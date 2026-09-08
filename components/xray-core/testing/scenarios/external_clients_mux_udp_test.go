package scenarios

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	stdnet "net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/proxyman"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/uuid"
	core "github.com/xtls/xray-core/core"
	proxyanytls "github.com/xtls/xray-core/proxy/anytls"
	"github.com/xtls/xray-core/proxy/freedom"
	hyproxy "github.com/xtls/xray-core/proxy/hysteria"
	hyaccount "github.com/xtls/xray-core/proxy/hysteria/account"
	"github.com/xtls/xray-core/proxy/vless"
	vlessinbound "github.com/xtls/xray-core/proxy/vless/inbound"
	vlessoutbound "github.com/xtls/xray-core/proxy/vless/outbound"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"github.com/xtls/xray-core/testing/servers/udp"
	"github.com/xtls/xray-core/transport/internet"
	hytransport "github.com/xtls/xray-core/transport/internet/hysteria"
	xtls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sync/errgroup"
)

const (
	externalAnyTLSPassword = "external-anytls-udp-password"
	externalHysteriaAuth   = "external-hysteria2-udp-password"
)

type externalClientKind string

const (
	externalSingBox externalClientKind = "sing-box"
	externalMihomo  externalClientKind = "mihomo"
)

type externalMuxMode struct {
	name            string
	xudpConcurrency int32
}

type externalIngressSpec struct {
	name          string
	port          xnet.Port
	inbound       *serial.TypedMessage
	stream        *internet.StreamConfig
	clientAddress string
	clientPort    int
	uuid          string
}

type externalUDPTopology struct {
	servers  []*exec.Cmd
	hopProxy *muxHopProxy
	ingress  externalIngressSpec
}

func externalMuxSender(maxRequests uint32, xudpConcurrency int32) *serial.TypedMessage {
	pool := testWorkerPoolConfig()
	pool.MaxRequestsPerWorker = maxRequests
	return serial.ToTypedMessage(&proxyman.SenderConfig{
		MultiplexSettings: &proxyman.MultiplexingConfig{
			Enabled:         true,
			Concurrency:     1,
			XudpConcurrency: xudpConcurrency,
			WorkerPool:      pool,
		},
	})
}

func makeExternalIngress(t *testing.T, name string) externalIngressSpec {
	t.Helper()
	spec := externalIngressSpec{name: name, clientAddress: "127.0.0.1"}
	switch name {
	case "AnyTLS":
		spec.port = tcp.PickPort()
		certificate, _ := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		spec.inbound = serial.ToTypedMessage(&proxyanytls.ServerConfig{Users: []*protocol.User{{
			Email: "external-anytls@example.com",
			Account: serial.ToTypedMessage(&proxyanytls.Account{
				Password: externalAnyTLSPassword,
			}),
		}}})
		spec.stream = &internet.StreamConfig{
			ProtocolName: "tcp",
			SecurityType: serial.GetMessageType(&xtls.Config{}),
			SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&xtls.Config{
				Certificate: []*xtls.Certificate{xtls.ParseCertificate(certificate)},
			})},
		}
	case "VLESS":
		spec.port = tcp.PickPort()
		userID := uuid.New()
		spec.uuid = userID.String()
		spec.inbound = serial.ToTypedMessage(&vlessinbound.Config{Clients: []*protocol.User{{
			Email:   "external-vless@example.com",
			Account: serial.ToTypedMessage(&vless.Account{Id: spec.uuid}),
		}}})
	case "Hysteria2":
		spec.port = udp.PickPort()
		certificate, _ := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		spec.inbound = serial.ToTypedMessage(&hyproxy.ServerConfig{Users: []*protocol.User{{
			Email:   "external-hysteria2@example.com",
			Account: serial.ToTypedMessage(&hyaccount.Account{Auth: externalHysteriaAuth}),
		}}})
		spec.stream = &internet.StreamConfig{
			ProtocolName: "hysteria",
			TransportSettings: []*internet.TransportConfig{{
				ProtocolName: "hysteria",
				Settings: serial.ToTypedMessage(&hytransport.Config{
					Version: 2, Auth: externalHysteriaAuth, UdpIdleTimeout: 60,
				}),
			}},
			SecurityType: serial.GetMessageType(&xtls.Config{}),
			SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&xtls.Config{
				Certificate:  []*xtls.Certificate{xtls.ParseCertificate(certificate)},
				NextProtocol: []string{"h3"},
			})},
		}
	default:
		t.Fatalf("unknown external ingress protocol %q", name)
	}
	spec.clientPort = int(spec.port)
	return spec
}

func startExternalUDPTopology(t *testing.T, protocolName string, mode externalMuxMode) *externalUDPTopology {
	t.Helper()
	ingress := makeExternalIngress(t, protocolName)
	relayID := protocol.NewID(uuid.New())
	relayPort := tcp.PickPort()
	hopProxy := newMuxHopProxy(t, relayPort)

	relayConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: testMuxReceiver(relayPort, nil),
			ProxySettings: serial.ToTypedMessage(&vlessinbound.Config{Clients: []*protocol.User{{
				Email:   "external-hop@example.com",
				Account: serial.ToTypedMessage(&vless.Account{Id: relayID.String()}),
			}}}),
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			ProxySettings: serial.ToTypedMessage(&freedom.Config{IpsBlocked: &freedom.IPRules{}}),
		}},
	}
	ingressConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: testMuxReceiver(ingress.port, ingress.stream),
			ProxySettings:    ingress.inbound,
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			Tag:            "external-hop",
			SenderSettings: externalMuxSender(8, mode.xudpConcurrency),
			ProxySettings: serial.ToTypedMessage(&vlessoutbound.Config{Vnext: &protocol.ServerEndpoint{
				Address: xnet.NewIPOrDomain(xnet.LocalHostIP),
				Port:    uint32(hopProxy.port()),
				User: &protocol.User{
					Email:   "external-hop@example.com",
					Account: serial.ToTypedMessage(&vless.Account{Id: relayID.String()}),
				},
			}}),
		}},
	}

	servers, err := InitializeServerConfigs(relayConfig, ingressConfig)
	if err != nil {
		t.Fatal(err)
	}
	topology := &externalUDPTopology{servers: servers, hopProxy: hopProxy, ingress: ingress}
	t.Cleanup(func() {
		if topology.servers != nil {
			CloseAllServers(topology.servers)
			topology.servers = nil
		}
	})
	return topology
}

type dnsUDPServer struct {
	conn      *stdnet.UDPConn
	answer    [4]byte
	done      chan struct{}
	queries   atomic.Uint64
	dropEvery atomic.Uint64
	baseDelay atomic.Int64
	jitter    atomic.Int64
	closeOnce sync.Once
}

func startDNSUDPServer(t *testing.T, answer [4]byte) *dnsUDPServer {
	t.Helper()
	conn, err := stdnet.ListenUDP("udp4", &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	server := &dnsUDPServer{conn: conn, answer: answer, done: make(chan struct{})}
	go server.serve()
	t.Cleanup(server.close)
	return server
}

func (s *dnsUDPServer) address() *stdnet.UDPAddr {
	return s.conn.LocalAddr().(*stdnet.UDPAddr)
}

func (s *dnsUDPServer) serve() {
	defer close(s.done)
	payload := make([]byte, 4096)
	for {
		n, peer, err := s.conn.ReadFromUDP(payload)
		if err != nil {
			return
		}
		response, ok := dnsResponse(payload[:n], s.answer)
		if !ok {
			continue
		}
		sequence := s.queries.Add(1)
		if dropEvery := s.dropEvery.Load(); dropEvery > 0 && sequence%dropEvery == 0 {
			continue
		}
		delay := time.Duration(s.baseDelay.Load())
		if jitter := time.Duration(s.jitter.Load()); jitter > 0 {
			delay += time.Duration(sequence%3) * jitter
		}
		if delay > 0 {
			time.Sleep(delay)
		}
		_, _ = s.conn.WriteToUDP(response, peer)
	}
}

func (s *dnsUDPServer) setImpairment(dropEvery uint64, baseDelay, jitter time.Duration) {
	s.dropEvery.Store(dropEvery)
	s.baseDelay.Store(int64(baseDelay))
	s.jitter.Store(int64(jitter))
}

func (s *dnsUDPServer) close() {
	s.closeOnce.Do(func() {
		_ = s.conn.Close()
		<-s.done
	})
}

func dnsResponse(query []byte, answer [4]byte) ([]byte, bool) {
	if len(query) < 17 || query[2]&0x80 != 0 || binary.BigEndian.Uint16(query[4:6]) != 1 {
		return nil, false
	}
	position := 12
	for {
		if position >= len(query) {
			return nil, false
		}
		length := int(query[position])
		position++
		if length == 0 {
			break
		}
		if length > 63 || position+length > len(query) {
			return nil, false
		}
		position += length
	}
	if position+4 > len(query) {
		return nil, false
	}
	questionEnd := position + 4
	response := append([]byte(nil), query[:questionEnd]...)
	response[2] = 0x81
	response[3] = 0x80
	binary.BigEndian.PutUint16(response[6:8], 1)
	response = append(response,
		0xc0, 0x0c, // compressed owner name
		0x00, 0x01, // A
		0x00, 0x01, // IN
		0x00, 0x00, 0x00, 0x1e, // TTL
		0x00, 0x04,
		answer[0], answer[1], answer[2], answer[3],
	)
	return response, true
}

func dnsQuery(id uint16, label string) []byte {
	query := make([]byte, 12)
	binary.BigEndian.PutUint16(query[:2], id)
	binary.BigEndian.PutUint16(query[2:4], 0x0100)
	binary.BigEndian.PutUint16(query[4:6], 1)
	for _, part := range strings.Split(label, ".") {
		query = append(query, byte(len(part)))
		query = append(query, part...)
	}
	return append(query, 0, 0, 1, 0, 1)
}

type externalClientProcess struct {
	kind      externalClientKind
	cmd       *exec.Cmd
	socksPort int
	logPath   string
	logFile   *os.File
	waited    bool
}

func externalClientBinary(kind externalClientKind) string {
	switch kind {
	case externalSingBox:
		return os.Getenv("BROCADE_SING_BOX_BIN")
	case externalMihomo:
		return os.Getenv("BROCADE_MIHOMO_BIN")
	default:
		return ""
	}
}

func singBoxConfig(spec externalIngressSpec, socksPort int) map[string]any {
	var outbound map[string]any
	switch spec.name {
	case "AnyTLS":
		outbound = map[string]any{
			"type": "anytls", "tag": "proxy", "server": spec.clientAddress,
			"server_port": spec.clientPort, "password": externalAnyTLSPassword,
			"tls": map[string]any{"enabled": true, "server_name": "localhost", "insecure": true},
		}
	case "VLESS":
		outbound = map[string]any{
			"type": "vless", "tag": "proxy", "server": spec.clientAddress,
			"server_port": spec.clientPort, "uuid": spec.uuid,
		}
	case "Hysteria2":
		outbound = map[string]any{
			"type": "hysteria2", "tag": "proxy", "server": spec.clientAddress,
			"server_port": spec.clientPort, "password": externalHysteriaAuth,
			"tls": map[string]any{"enabled": true, "server_name": "localhost", "insecure": true},
		}
	}
	return map[string]any{
		"log": map[string]any{"level": "warn"},
		"inbounds": []any{map[string]any{
			"type": "socks", "tag": "socks-in", "listen": "127.0.0.1", "listen_port": socksPort,
		}},
		"outbounds": []any{outbound},
		"route":     map[string]any{"final": "proxy"},
	}
}

func mihomoConfig(spec externalIngressSpec, socksPort int) string {
	var proxy string
	switch spec.name {
	case "AnyTLS":
		proxy = fmt.Sprintf(`  - name: proxy
    type: anytls
    server: %s
    port: %d
    password: %s
    sni: localhost
    skip-cert-verify: true
    udp: true`, spec.clientAddress, spec.clientPort, externalAnyTLSPassword)
	case "VLESS":
		proxy = fmt.Sprintf(`  - name: proxy
    type: vless
    server: %s
    port: %d
    uuid: %s
    network: tcp
    tls: false
    udp: true`, spec.clientAddress, spec.clientPort, spec.uuid)
	case "Hysteria2":
		proxy = fmt.Sprintf(`  - name: proxy
    type: hysteria2
    server: %s
    port: %d
    password: %s
    sni: localhost
    skip-cert-verify: true
    udp: true`, spec.clientAddress, spec.clientPort, externalHysteriaAuth)
	}
	return fmt.Sprintf(`socks-port: %d
bind-address: 127.0.0.1
allow-lan: false
mode: rule
log-level: warning
ipv6: false
proxies:
%s
proxy-groups:
  - name: E2E
    type: select
    proxies:
      - proxy
rules:
  - MATCH,E2E
`, socksPort, proxy)
}

func startExternalClient(t *testing.T, kind externalClientKind, spec externalIngressSpec) *externalClientProcess {
	t.Helper()
	binaryPath := externalClientBinary(kind)
	if binaryPath == "" {
		t.Skipf("set %s to run the real-client UDP E2E", map[externalClientKind]string{
			externalSingBox: "BROCADE_SING_BOX_BIN", externalMihomo: "BROCADE_MIHOMO_BIN",
		}[kind])
	}
	if _, err := os.Stat(binaryPath); err != nil {
		t.Fatalf("%s binary %q: %v", kind, binaryPath, err)
	}
	dir := t.TempDir()
	socksPort := int(tcp.PickPort())
	client := &externalClientProcess{kind: kind, socksPort: socksPort, logPath: filepath.Join(dir, "client.log")}
	var configPath string
	switch kind {
	case externalSingBox:
		configPath = filepath.Join(dir, "sing-box.json")
		encoded, err := json.MarshalIndent(singBoxConfig(spec, socksPort), "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(configPath, encoded, 0o600); err != nil {
			t.Fatal(err)
		}
		checked := exec.Command(binaryPath, "check", "-c", configPath)
		if output, err := checked.CombinedOutput(); err != nil {
			t.Fatalf("sing-box rejected %s config: %v\n%s", spec.name, err, output)
		}
		client.cmd = exec.Command(binaryPath, "run", "-c", configPath)
	case externalMihomo:
		configPath = filepath.Join(dir, "config.yaml")
		if err := os.WriteFile(configPath, []byte(mihomoConfig(spec, socksPort)), 0o600); err != nil {
			t.Fatal(err)
		}
		checked := exec.Command(binaryPath, "-t", "-d", dir, "-f", configPath)
		if output, err := checked.CombinedOutput(); err != nil {
			t.Fatalf("mihomo rejected %s config: %v\n%s", spec.name, err, output)
		}
		client.cmd = exec.Command(binaryPath, "-d", dir, "-f", configPath)
	}
	logFile, err := os.Create(client.logPath)
	if err != nil {
		t.Fatal(err)
	}
	client.logFile = logFile
	client.cmd.Stdout = logFile
	client.cmd.Stderr = logFile
	if err := client.cmd.Start(); err != nil {
		_ = logFile.Close()
		t.Fatal(err)
	}
	t.Cleanup(func() { client.stop() })
	if !waitTCPPort(socksPort, 5*time.Second) {
		client.stop()
		content, _ := os.ReadFile(client.logPath)
		t.Fatalf("%s SOCKS port did not become ready for %s:\n%s", kind, spec.name, content)
	}
	return client
}

func waitTCPPort(port int, timeout time.Duration) bool {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		connection, err := stdnet.DialTimeout("tcp", stdnet.JoinHostPort("127.0.0.1", strconv.Itoa(port)), 100*time.Millisecond)
		if err == nil {
			_ = connection.Close()
			return true
		}
		time.Sleep(25 * time.Millisecond)
	}
	return false
}

func (c *externalClientProcess) stop() {
	if c == nil || c.waited {
		return
	}
	c.waited = true
	if c.cmd != nil && c.cmd.Process != nil {
		_ = c.cmd.Process.Signal(os.Interrupt)
		doneWaiting := make(chan struct{})
		go func() {
			_ = c.cmd.Wait()
			close(doneWaiting)
		}()
		select {
		case <-doneWaiting:
		case <-time.After(2 * time.Second):
			_ = c.cmd.Process.Kill()
			<-doneWaiting
		}
	}
	if c.logFile != nil {
		_ = c.logFile.Close()
	}
}

type socksUDPAssociation struct {
	control *stdnet.TCPConn
	packet  *stdnet.UDPConn
	relay   *stdnet.UDPAddr
}

func openSocksUDPAssociation(socksPort int) (*socksUDPAssociation, error) {
	address := stdnet.JoinHostPort("127.0.0.1", strconv.Itoa(socksPort))
	raw, err := stdnet.DialTimeout("tcp", address, 2*time.Second)
	if err != nil {
		return nil, err
	}
	control := raw.(*stdnet.TCPConn)
	fail := func(err error) (*socksUDPAssociation, error) {
		_ = control.Close()
		return nil, err
	}
	if _, err := control.Write([]byte{5, 1, 0}); err != nil {
		return fail(err)
	}
	method := make([]byte, 2)
	if _, err := io.ReadFull(control, method); err != nil {
		return fail(err)
	}
	if !bytes.Equal(method, []byte{5, 0}) {
		return fail(fmt.Errorf("SOCKS method reply = %v", method))
	}
	if _, err := control.Write([]byte{5, 3, 0, 1, 0, 0, 0, 0, 0, 0}); err != nil {
		return fail(err)
	}
	header := make([]byte, 4)
	if _, err := io.ReadFull(control, header); err != nil {
		return fail(err)
	}
	if header[0] != 5 || header[1] != 0 {
		return fail(fmt.Errorf("SOCKS UDP ASSOCIATE reply = %v", header))
	}
	relayHost, err := readSocksAddress(control, header[3])
	if err != nil {
		return fail(err)
	}
	portBytes := make([]byte, 2)
	if _, err := io.ReadFull(control, portBytes); err != nil {
		return fail(err)
	}
	if relayHost == "0.0.0.0" || relayHost == "::" {
		relayHost = "127.0.0.1"
	}
	relay, err := stdnet.ResolveUDPAddr("udp", stdnet.JoinHostPort(relayHost, strconv.Itoa(int(binary.BigEndian.Uint16(portBytes)))))
	if err != nil {
		return fail(err)
	}
	packet, err := stdnet.ListenUDP("udp", &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		return fail(err)
	}
	return &socksUDPAssociation{control: control, packet: packet, relay: relay}, nil
}

func readSocksAddress(reader io.Reader, addressType byte) (string, error) {
	switch addressType {
	case 1:
		value := make([]byte, 4)
		_, err := io.ReadFull(reader, value)
		return stdnet.IP(value).String(), err
	case 4:
		value := make([]byte, 16)
		_, err := io.ReadFull(reader, value)
		return stdnet.IP(value).String(), err
	case 3:
		length := make([]byte, 1)
		if _, err := io.ReadFull(reader, length); err != nil {
			return "", err
		}
		value := make([]byte, int(length[0]))
		_, err := io.ReadFull(reader, value)
		return string(value), err
	default:
		return "", fmt.Errorf("unsupported SOCKS address type %d", addressType)
	}
}

func (a *socksUDPAssociation) close() {
	if a == nil {
		return
	}
	_ = a.packet.Close()
	_ = a.control.Close()
}

func (a *socksUDPAssociation) queryDNS(target *stdnet.UDPAddr, id uint16, answer [4]byte, timeout time.Duration) error {
	query := dnsQuery(id, "normal-udp.example")
	packet := []byte{0, 0, 0, 1}
	packet = append(packet, target.IP.To4()...)
	port := make([]byte, 2)
	binary.BigEndian.PutUint16(port, uint16(target.Port))
	packet = append(packet, port...)
	packet = append(packet, query...)
	if err := a.packet.SetDeadline(time.Now().Add(timeout)); err != nil {
		return err
	}
	if _, err := a.packet.WriteToUDP(packet, a.relay); err != nil {
		return err
	}
	response := make([]byte, 4096)
	n, _, err := a.packet.ReadFromUDP(response)
	if err != nil {
		return err
	}
	payload, err := socksUDPPayload(response[:n])
	if err != nil {
		return err
	}
	if len(payload) < 16 || binary.BigEndian.Uint16(payload[:2]) != id || payload[2]&0x80 == 0 {
		return fmt.Errorf("invalid DNS response for id %d: %x", id, payload)
	}
	if !bytes.Equal(payload[len(payload)-4:], answer[:]) {
		return fmt.Errorf("DNS answer = %v, want %v", payload[len(payload)-4:], answer)
	}
	return nil
}

func socksUDPPayload(packet []byte) ([]byte, error) {
	if len(packet) < 4 || packet[0] != 0 || packet[1] != 0 || packet[2] != 0 {
		return nil, fmt.Errorf("invalid SOCKS UDP header: %x", packet)
	}
	position := 4
	switch packet[3] {
	case 1:
		position += 4
	case 4:
		position += 16
	case 3:
		if position >= len(packet) {
			return nil, io.ErrUnexpectedEOF
		}
		position += 1 + int(packet[position])
	default:
		return nil, fmt.Errorf("unsupported SOCKS UDP address type %d", packet[3])
	}
	position += 2
	if position > len(packet) {
		return nil, io.ErrUnexpectedEOF
	}
	return packet[position:], nil
}

func runPersistentDNSWorkload(socksPort int, servers []*dnsUDPServer, queries int) error {
	association, err := openSocksUDPAssociation(socksPort)
	if err != nil {
		return err
	}
	defer association.close()
	for index := 0; index < queries; index++ {
		server := servers[index%len(servers)]
		if err := association.queryDNS(server.address(), uint16(index+1), server.answer, 5*time.Second); err != nil {
			return fmt.Errorf("query %d/%d: %w", index+1, queries, err)
		}
	}
	return nil
}

func runConcurrentDNSWorkload(socksPort int, servers []*dnsUDPServer, associations, queries int) error {
	var group errgroup.Group
	for associationIndex := 0; associationIndex < associations; associationIndex++ {
		associationIndex := associationIndex
		group.Go(func() error {
			association, err := openSocksUDPAssociation(socksPort)
			if err != nil {
				return err
			}
			defer association.close()
			for queryIndex := 0; queryIndex < queries; queryIndex++ {
				server := servers[(associationIndex+queryIndex)%len(servers)]
				id := uint16(1000 + associationIndex*queries + queryIndex)
				if err := association.queryDNS(server.address(), id, server.answer, 8*time.Second); err != nil {
					return fmt.Errorf("association %d query %d: %w", associationIndex, queryIndex, err)
				}
			}
			return nil
		})
	}
	return group.Wait()
}

func runImpairedDNSWorkload(socksPort int, servers []*dnsUDPServer, queries int) error {
	association, err := openSocksUDPAssociation(socksPort)
	if err != nil {
		return err
	}
	defer association.close()
	for index := 0; index < queries; index++ {
		server := servers[index%len(servers)]

		// UDP loss is normal. Keep the association alive and retry the same
		// ordinary DNS query so a missing datagram cannot poison later frames.
		var queryErr error
		for attempt := 0; attempt < 3; attempt++ {
			queryErr = association.queryDNS(server.address(), uint16(2000+index), server.answer, 250*time.Millisecond)
			if queryErr == nil {
				break
			}
		}
		if queryErr != nil {
			return fmt.Errorf("impaired query %d/%d: %w", index+1, queries, queryErr)
		}
	}
	return nil
}

func retryDNSWorkload(timeout time.Duration, run func() error) error {
	deadline := time.Now().Add(timeout)
	var last error
	for time.Now().Before(deadline) {
		if err := run(); err == nil {
			return nil
		} else {
			last = err
		}
		time.Sleep(100 * time.Millisecond)
	}
	return last
}

func TestExternalClientsMuxUDPDataPlane(t *testing.T) {
	if testing.Short() {
		t.Skip("real external-client process test")
	}
	clients := []externalClientKind{externalSingBox, externalMihomo}
	protocols := []string{"AnyTLS", "VLESS", "Hysteria2"}
	modes := []externalMuxMode{
		{name: "shared-mux", xudpConcurrency: 0},
		{name: "dedicated-xudp", xudpConcurrency: 1},
	}
	for _, mode := range modes {
		for _, protocolName := range protocols {
			for _, clientKind := range clients {
				t.Run(mode.name+"/"+protocolName+"/"+string(clientKind), func(t *testing.T) {
					if externalClientBinary(clientKind) == "" {
						t.Skipf("real %s client binary is not configured", clientKind)
					}
					topology := startExternalUDPTopology(t, protocolName, mode)
					firstDNS := startDNSUDPServer(t, [4]byte{192, 0, 2, 10})
					secondDNS := startDNSUDPServer(t, [4]byte{198, 51, 100, 20})
					dnsServers := []*dnsUDPServer{firstDNS, secondDNS}
					client := startExternalClient(t, clientKind, topology.ingress)

					if err := runPersistentDNSWorkload(client.socksPort, dnsServers, 32); err != nil {
						content, _ := os.ReadFile(client.logPath)
						t.Fatalf("persistent multi-target UDP failed: %v\n%s", err, content)
					}
					if err := runConcurrentDNSWorkload(client.socksPort, dnsServers, 6, 8); err != nil {
						content, _ := os.ReadFile(client.logPath)
						t.Fatalf("concurrent UDP failed: %v\n%s", err, content)
					}

					// Exercise normal UDP behavior under deterministic response loss
					// and jitter. Every fourth response is dropped; application-level
					// retries must continue on the same long-lived association.
					for _, server := range dnsServers {
						server.setImpairment(4, 5*time.Millisecond, 5*time.Millisecond)
					}
					if err := runImpairedDNSWorkload(client.socksPort, dnsServers, 16); err != nil {
						content, _ := os.ReadFile(client.logPath)
						t.Fatalf("lossy/jittered UDP failed: %v\n%s", err, content)
					}
					for _, server := range dnsServers {
						server.setImpairment(0, 0, 0)
					}

					// Add RTT on the physical relay hop without changing application bytes.
					topology.hopProxy.setDelay(15 * time.Millisecond)
					if err := runPersistentDNSWorkload(client.socksPort, dnsServers, 8); err != nil {
						t.Fatalf("delayed relay-hop UDP failed: %v", err)
					}

					// Reset every existing relay TCP connection. A fresh normal UDP
					// association must recover through a replacement Worker.
					topology.hopProxy.closeConnections(true)
					if err := retryDNSWorkload(8*time.Second, func() error {
						return runPersistentDNSWorkload(client.socksPort, dnsServers, 4)
					}); err != nil {
						content, _ := os.ReadFile(client.logPath)
						t.Fatalf("UDP did not recover after relay reset: %v\n%s", err, content)
					}
					if firstDNS.queries.Load() == 0 || secondDNS.queries.Load() == 0 {
						t.Fatalf("multi-target DNS was not observed: first=%d second=%d", firstDNS.queries.Load(), secondDNS.queries.Load())
					}
				})
			}
		}
	}
}
