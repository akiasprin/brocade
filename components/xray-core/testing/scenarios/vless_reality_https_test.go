package scenarios

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	gotls "crypto/tls"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"io"
	mathrand "math/rand"
	stdnet "net"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/commander"
	"github.com/xtls/xray-core/app/proxyman"
	"github.com/xtls/xray-core/app/router"
	"github.com/xtls/xray-core/app/stats"
	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/uuid"
	core "github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/proxy/blackhole"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"github.com/xtls/xray-core/proxy/freedom"
	"github.com/xtls/xray-core/proxy/vless"
	"github.com/xtls/xray-core/proxy/vless/inbound"
	"github.com/xtls/xray-core/proxy/vless/outbound"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/reality"
	transtcp "github.com/xtls/xray-core/transport/internet/tcp"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sync/errgroup"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	visionStatsPrefixForScenario = "vless>>>vision>>>"
	innerHTTPSBodySize           = 4 << 20
)

// TestVlessXtlsVisionRealityLocalCoverHTTPS reproduces the production shape:
//
//	VLESS + Vision + REALITY -> local TLS/dokodemo cover -> HTTP blackhole
//
// The authenticated business stream carries a real TLS 1.3 HTTP exchange to a
// separate local origin. The origin and client fragment their early TLS writes
// so the process-level test also exercises record boundaries split across TCP
// writes. Exact record-boundary permutations are covered by the proxy package's
// deterministic fragmentation test.
func TestVlessXtlsVisionRealityLocalCoverHTTPS(t *testing.T) {
	if testing.Short() {
		t.Skip("full process REALITY/HTTPS scenario")
	}
	options := localPerfOptionsFromEnv(t, 20, innerHTTPSBodySize)
	flow := vless.XRV
	flowName := "vision"
	switch value := os.Getenv(localPerfVLESSFlowEnv); value {
	case "", "vision":
	case "raw":
		flow = ""
		flowName = value
	default:
		t.Fatalf("%s must be vision or raw, got %q", localPerfVLESSFlowEnv, value)
	}

	origin, expectedBodyHash := startFragmentedHTTPSOrigin(t, options.bodyBytes)
	defer origin.Close()
	originPort := uint32(origin.Listener.Addr().(*stdnet.TCPAddr).Port)

	const coverName = "local-cover.test"
	coverCertificate, _ := cert.MustGenerate(nil, cert.CommonName(coverName), cert.DNSNames(coverName))
	privateKey := mustDecodeRawURLBase64(t, "aGSYystUbf59_9_6LKRxD27rmSW_-2_nyd9YG_Gwbks")
	publicKey := mustDecodeRawURLBase64(t, "E59WjnvZcQMu7tR7_BgyhycuEdBS-CtKxfImRCdAvFM")
	shortID := mustDecodeHex(t, "0123456789abcdef")

	userID := protocol.NewID(uuid.New())
	serverPort := tcp.PickPort()
	coverPort := tcp.PickPort()
	statsPort := tcp.PickPort()
	serverConfig := &core.Config{
		App: []*serial.TypedMessage{
			serial.ToTypedMessage(&stats.Config{}),
			serial.ToTypedMessage(&commander.Config{
				Listen: fmt.Sprintf("127.0.0.1:%d", statsPort),
				Service: []*serial.TypedMessage{
					serial.ToTypedMessage(&statscmd.Config{}),
				},
			}),
			serial.ToTypedMessage(&router.Config{
				Rule: []*router.RoutingRule{
					{
						InboundTag: []string{"cover"},
						TargetTag: &router.RoutingRule_Tag{
							Tag: "cover-response",
						},
					},
				},
			}),
		},
		Inbound: []*core.InboundHandlerConfig{
			{
				Tag: "vless-reality",
				ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
					PortList: &net.PortList{Range: []*net.PortRange{net.SinglePortRange(serverPort)}},
					Listen:   net.NewIPOrDomain(net.LocalHostIP),
					SniffingSettings: &proxyman.SniffingConfig{
						Enabled:             true,
						DestinationOverride: []string{"tls", "http", "quic"},
						RouteOnly:           true,
					},
					StreamSettings: &internet.StreamConfig{
						ProtocolName: "tcp",
						SecurityType: serial.GetMessageType(&reality.Config{}),
						SecuritySettings: []*serial.TypedMessage{
							serial.ToTypedMessage(&reality.Config{
								Dest:        fmt.Sprintf("127.0.0.1:%d", coverPort),
								ServerNames: []string{coverName},
								PrivateKey:  privateKey,
								ShortIds:    [][]byte{shortID},
								Type:        "tcp",
							}),
						},
					},
				}),
				ProxySettings: serial.ToTypedMessage(&inbound.Config{
					Clients: []*protocol.User{
						{
							Account: serial.ToTypedMessage(&vless.Account{
								Id:   userID.String(),
								Flow: flow,
							}),
						},
					},
					Decryption: "none",
				}),
			},
			{
				Tag: "cover",
				ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
					PortList: &net.PortList{Range: []*net.PortRange{net.SinglePortRange(coverPort)}},
					Listen:   net.NewIPOrDomain(net.LocalHostIP),
					StreamSettings: &internet.StreamConfig{
						ProtocolName: "tcp",
						SecurityType: serial.GetMessageType(&xraytls.Config{}),
						SecuritySettings: []*serial.TypedMessage{
							serial.ToTypedMessage(&xraytls.Config{
								Certificate:      []*xraytls.Certificate{xraytls.ParseCertificate(coverCertificate)},
								NextProtocol:     []string{"http/1.1"},
								RejectUnknownSni: true,
							}),
						},
					},
				}),
				ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
					Address:  net.NewIPOrDomain(net.LocalHostIP),
					Port:     1,
					Networks: []net.Network{net.Network_TCP},
				}),
			},
		},
		Outbound: []*core.OutboundHandlerConfig{
			{
				Tag: "direct",
				ProxySettings: serial.ToTypedMessage(&freedom.Config{
					IpsBlocked: &freedom.IPRules{},
				}),
			},
			{
				Tag: "cover-response",
				ProxySettings: serial.ToTypedMessage(&blackhole.Config{
					Response: serial.ToTypedMessage(&blackhole.HTTPResponse{}),
				}),
			},
		},
	}

	clientPort := tcp.PickPort()
	clientConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{
			{
				ReceiverSettings: serial.ToTypedMessage(&proxyman.ReceiverConfig{
					PortList: &net.PortList{Range: []*net.PortRange{net.SinglePortRange(clientPort)}},
					Listen:   net.NewIPOrDomain(net.LocalHostIP),
				}),
				ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
					Address:  net.NewIPOrDomain(net.LocalHostIP),
					Port:     originPort,
					Networks: []net.Network{net.Network_TCP},
				}),
			},
		},
		Outbound: []*core.OutboundHandlerConfig{
			{
				ProxySettings: serial.ToTypedMessage(&outbound.Config{
					Vnext: &protocol.ServerEndpoint{
						Address: net.NewIPOrDomain(net.LocalHostIP),
						Port:    uint32(serverPort),
						User: &protocol.User{
							Account: serial.ToTypedMessage(&vless.Account{
								Id:   userID.String(),
								Flow: flow,
							}),
						},
					},
				}),
				SenderSettings: serial.ToTypedMessage(&proxyman.SenderConfig{
					StreamSettings: &internet.StreamConfig{
						ProtocolName: "tcp",
						TransportSettings: []*internet.TransportConfig{
							{
								ProtocolName: "tcp",
								Settings:     serial.ToTypedMessage(&transtcp.Config{}),
							},
						},
						SecurityType: serial.GetMessageType(&reality.Config{}),
						SecuritySettings: []*serial.TypedMessage{
							serial.ToTypedMessage(&reality.Config{
								Fingerprint: "chrome",
								ServerName:  coverName,
								PublicKey:   publicKey,
								ShortId:     shortID,
								SpiderX:     "/",
							}),
						},
					},
				}),
			},
		},
	}
	profilePorts := make(map[string]net.Port)
	if options.profileDir != "" {
		profilePorts["server"] = tcp.PickPort()
		profilePorts["client"] = tcp.PickPort()
		addLocalCPUProfileEndpoint(serverConfig, profilePorts["server"])
		addLocalCPUProfileEndpoint(clientConfig, profilePorts["client"])
	}

	servers, err := InitializeServerConfigs(serverConfig, clientConfig)
	if err != nil {
		t.Fatal(err)
	}
	defer CloseAllServers(servers)
	applyLocalPerfCPUAffinity(t, options, servers)

	timeout := time.Duration(max(30, options.profileSeconds+15)) * time.Second
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	statsConn, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", statsPort),
		grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		t.Fatal(err)
	}
	defer statsConn.Close()
	statsClient := statscmd.NewStatsServiceClient(statsConn)
	before, err := statsClient.GetSysStats(ctx, &statscmd.SysStatsRequest{})
	if err != nil {
		t.Fatal(err)
	}

	waitProfiles := startLocalCPUProfiles(t, ctx, options.profileDir, "vless", options.profileSeconds, profilePorts)
	logProcessUsage := startLocalProcessUsage(t, os.Getenv(localPerfEnabledEnv) == "1", servers)
	started := time.Now()
	var group errgroup.Group
	for index := 0; index < options.concurrency; index++ {
		index := index
		group.Go(func() error {
			return fetchFragmentedHTTPS(ctx, clientPort, index+1, expectedBodyHash, options.bodyBytes)
		})
	}
	if err := group.Wait(); err != nil {
		t.Fatal(err)
	}
	elapsed := time.Since(started)
	logProcessUsage()
	waitProfiles()
	after, err := statsClient.GetSysStats(ctx, &statscmd.SysStatsRequest{})
	if err != nil {
		t.Fatal(err)
	}
	transferred := int64(options.concurrency) * int64(options.bodyBytes)
	if flow != vless.XRV {
		t.Logf("flow=%s transferred=%d elapsed=%s throughput=%.2f GiB/s alloc_delta=%d gc_delta=%d",
			flowName, transferred, elapsed, float64(transferred)/elapsed.Seconds()/(1<<30),
			after.TotalAlloc-before.TotalAlloc, after.NumGC-before.NumGC)
		return
	}

	connections := int64(options.concurrency)
	values := waitForCompletedVisionConnections(t, statsClient, connections)
	if got := values["completed_direct_connections"]; got != connections {
		t.Fatalf("completed direct connections = %d, want %d; all stats: %v", got, connections, values)
	}
	if got := values["completed_splice_connections"]; got != connections {
		t.Fatalf("completed splice connections = %d, want %d; all stats: %v", got, connections, values)
	}
	if got := values["not_spliced_connections"]; got != 0 {
		t.Fatalf("connections that missed splice = %d, want 0; all stats: %v", got, values)
	}
	minimumSpliceBytes := connections * int64(options.bodyBytes/2)
	if got := values["splice_bytes"]; got < minimumSpliceBytes {
		t.Fatalf("splice bytes = %d, want at least %d; all stats: %v", got, minimumSpliceBytes, values)
	}
	t.Logf("flow=%s completed=%d direct=%d splice=%d splice_bytes=%d transferred=%d elapsed=%s throughput=%.2f GiB/s alloc_delta=%d gc_delta=%d",
		flowName,
		values["completed_connections"], values["completed_direct_connections"], values["completed_splice_connections"],
		values["splice_bytes"], transferred, elapsed, float64(transferred)/elapsed.Seconds()/(1<<30),
		after.TotalAlloc-before.TotalAlloc, after.NumGC-before.NumGC)
}

func startFragmentedHTTPSOrigin(t *testing.T, bodySize int) (*httptest.Server, [sha256.Size]byte) {
	t.Helper()
	chunk := bytes.Repeat([]byte("brocade-vision-real-https-"), 2048)
	hash := sha256.New()
	remaining := bodySize
	for remaining > 0 {
		length := min(len(chunk), remaining)
		_, _ = hash.Write(chunk[:length])
		remaining -= length
	}
	var expected [sha256.Size]byte
	copy(expected[:], hash.Sum(nil))

	server := httptest.NewUnstartedServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/large" {
			http.NotFound(writer, request)
			return
		}
		writer.Header().Set("Content-Length", strconv.Itoa(bodySize))
		writer.Header().Set("Content-Type", "application/octet-stream")
		writer.Header().Set("Connection", "close")
		remaining := bodySize
		for remaining > 0 {
			length := min(len(chunk), remaining)
			if _, err := writer.Write(chunk[:length]); err != nil {
				return
			}
			remaining -= length
		}
	}))
	server.EnableHTTP2 = false
	server.TLS = &gotls.Config{
		MinVersion: gotls.VersionTLS13,
		MaxVersion: gotls.VersionTLS13,
		NextProtos: []string{"http/1.1"},
	}
	server.Listener = &fragmentingListener{Listener: server.Listener}
	server.StartTLS()
	return server, expected
}

func fetchFragmentedHTTPS(ctx context.Context, port net.Port, seed int, expectedHash [sha256.Size]byte, expectedSize int) error {
	dialer := stdnet.Dialer{Timeout: 5 * time.Second}
	raw, err := dialer.DialContext(ctx, "tcp", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return err
	}
	defer raw.Close()
	if tcpConn, ok := raw.(*stdnet.TCPConn); ok {
		_ = tcpConn.SetNoDelay(true)
	}
	fragmented := &fragmentingConn{
		Conn:      raw,
		rng:       mathrand.New(mathrand.NewSource(int64(seed))),
		remaining: 32 << 10,
	}
	tlsConn := gotls.Client(fragmented, &gotls.Config{
		InsecureSkipVerify: true, // The origin certificate exists only for this local scenario.
		MinVersion:         gotls.VersionTLS13,
		MaxVersion:         gotls.VersionTLS13,
		ServerName:         "inner-origin.test",
		NextProtos:         []string{"http/1.1"},
	})
	deadline, ok := ctx.Deadline()
	if ok {
		_ = tlsConn.SetDeadline(deadline)
	}
	if err := tlsConn.HandshakeContext(ctx); err != nil {
		return fmt.Errorf("TLS handshake: %w", err)
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://inner-origin.test/large", nil)
	if err != nil {
		return err
	}
	request.Close = true
	if err := request.Write(tlsConn); err != nil {
		return fmt.Errorf("write HTTP request: %w", err)
	}
	response, err := http.ReadResponse(bufio.NewReader(tlsConn), request)
	if err != nil {
		return fmt.Errorf("read HTTP response: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("HTTP status = %s, want 200 OK", response.Status)
	}
	hash := sha256.New()
	written, err := io.Copy(hash, response.Body)
	if err != nil {
		return fmt.Errorf("read HTTP body: %w", err)
	}
	if written != int64(expectedSize) {
		return fmt.Errorf("HTTP body size = %d, want %d", written, expectedSize)
	}
	if !bytes.Equal(hash.Sum(nil), expectedHash[:]) {
		return fmt.Errorf("HTTP body hash mismatch")
	}
	return nil
}

func waitForCompletedVisionConnections(t *testing.T, client statscmd.StatsServiceClient, want int64) map[string]int64 {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		response, err := client.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: visionStatsPrefixForScenario})
		cancel()
		if err != nil {
			t.Fatal(err)
		}
		values := make(map[string]int64, len(response.Stat))
		for _, stat := range response.Stat {
			name := stat.Name
			if len(name) >= len(visionStatsPrefixForScenario) {
				name = name[len(visionStatsPrefixForScenario):]
			}
			values[name] = stat.Value
		}
		if values["completed_connections"] >= want {
			return values
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %d completed Vision connections; stats: %v", want, values)
		}
		time.Sleep(25 * time.Millisecond)
	}
}

type fragmentingListener struct {
	stdnet.Listener
	nextSeed atomic.Int64
}

func (listener *fragmentingListener) Accept() (stdnet.Conn, error) {
	conn, err := listener.Listener.Accept()
	if err != nil {
		return nil, err
	}
	if tcpConn, ok := conn.(*stdnet.TCPConn); ok {
		_ = tcpConn.SetNoDelay(true)
	}
	return &fragmentingConn{
		Conn:      conn,
		rng:       mathrand.New(mathrand.NewSource(listener.nextSeed.Add(1))),
		remaining: 64 << 10,
	}, nil
}

type fragmentingConn struct {
	stdnet.Conn
	rng       *mathrand.Rand
	remaining int
}

func (conn *fragmentingConn) Write(payload []byte) (int, error) {
	if conn.remaining <= 0 {
		return conn.Conn.Write(payload)
	}
	written := 0
	for len(payload) > 0 {
		if conn.remaining <= 0 {
			count, err := conn.Conn.Write(payload)
			written += count
			return written, err
		}
		length := 1 + conn.rng.Intn(257)
		if length > conn.remaining {
			length = conn.remaining
		}
		if length > len(payload) {
			length = len(payload)
		}
		count, err := conn.Conn.Write(payload[:length])
		written += count
		conn.remaining -= count
		payload = payload[count:]
		if err != nil {
			return written, err
		}
		if count != length {
			return written, io.ErrShortWrite
		}
	}
	return written, nil
}

func mustDecodeRawURLBase64(t *testing.T, value string) []byte {
	t.Helper()
	decoded, err := base64.RawURLEncoding.DecodeString(value)
	if err != nil {
		t.Fatal(err)
	}
	return decoded
}

func mustDecodeHex(t *testing.T, value string) []byte {
	t.Helper()
	decoded, err := hex.DecodeString(value)
	if err != nil {
		t.Fatal(err)
	}
	return decoded
}
