package scenarios

import (
	"fmt"
	stdnet "net"
	"os/exec"
	"runtime"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/proxyman"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/uuid"
	core "github.com/xtls/xray-core/core"
	proxyanytls "github.com/xtls/xray-core/proxy/anytls"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"github.com/xtls/xray-core/proxy/freedom"
	hyproxy "github.com/xtls/xray-core/proxy/hysteria"
	hyaccount "github.com/xtls/xray-core/proxy/hysteria/account"
	"github.com/xtls/xray-core/proxy/vless"
	vlessinbound "github.com/xtls/xray-core/proxy/vless/inbound"
	vlessoutbound "github.com/xtls/xray-core/proxy/vless/outbound"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"github.com/xtls/xray-core/transport/internet"
	hytransport "github.com/xtls/xray-core/transport/internet/hysteria"
	"github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sync/errgroup"
)

const realProcessProbeTimeout = 3 * time.Second

func testWorkerPoolConfig() *proxyman.WorkerPoolConfig {
	return &proxyman.WorkerPoolConfig{
		MinIdleWorkers:       0,
		MaxIdleWorkers:       2,
		MaxProbingWorkers:    1,
		ProbeIntervalSecs:    5,
		ProbeTimeoutMs:       uint32(realProcessProbeTimeout / time.Millisecond),
		IdleTtlSecs:          24,
		MaxRequestsPerWorker: 128,
	}
}

func testMuxSender(stream *internet.StreamConfig, concurrency int32) *serial.TypedMessage {
	return serial.ToTypedMessage(&proxyman.SenderConfig{
		StreamSettings: stream,
		MultiplexSettings: &proxyman.MultiplexingConfig{
			Enabled:     true,
			Concurrency: concurrency,
			WorkerPool:  testWorkerPoolConfig(),
		},
	})
}

func testDirectSender(stream *internet.StreamConfig) *serial.TypedMessage {
	return serial.ToTypedMessage(&proxyman.SenderConfig{StreamSettings: stream})
}

func testMuxReceiver(port net.Port, stream *internet.StreamConfig) *serial.TypedMessage {
	return serial.ToTypedMessage(&proxyman.ReceiverConfig{
		PortList:       &net.PortList{Range: []*net.PortRange{net.SinglePortRange(port)}},
		Listen:         net.NewIPOrDomain(net.LocalHostIP),
		StreamSettings: stream,
	})
}

type muxHopConnection struct {
	client    stdnet.Conn
	server    stdnet.Conn
	blackhole atomic.Bool
	closeOnce sync.Once
	owner     *muxHopProxy
}

func (c *muxHopConnection) close() {
	c.closeOnce.Do(func() {
		_ = c.client.Close()
		_ = c.server.Close()
		c.owner.active.Add(-1)
	})
}

func (c *muxHopConnection) forward(dst, src stdnet.Conn) {
	defer c.close()
	payload := make([]byte, 32*1024)
	for {
		n, err := src.Read(payload)
		if n > 0 && !c.blackhole.Load() {
			if delay := time.Duration(c.owner.delay.Load()); delay > 0 {
				time.Sleep(delay)
			}
			if _, writeErr := dst.Write(payload[:n]); writeErr != nil {
				return
			}
		}
		if err != nil {
			return
		}
	}
}

type muxHopProxy struct {
	listener  stdnet.Listener
	target    string
	mu        sync.Mutex
	conns     []*muxHopConnection
	active    atomic.Int32
	delay     atomic.Int64
	done      chan struct{}
	closeOnce sync.Once
}

func newMuxHopProxy(t *testing.T, targetPort net.Port) *muxHopProxy {
	t.Helper()
	listener, err := stdnet.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	p := &muxHopProxy{
		listener: listener,
		target:   stdnet.JoinHostPort("127.0.0.1", targetPort.String()),
		done:     make(chan struct{}),
	}
	go p.accept()
	t.Cleanup(p.close)
	return p
}

func (p *muxHopProxy) port() net.Port {
	return net.Port(p.listener.Addr().(*stdnet.TCPAddr).Port)
}

func (p *muxHopProxy) accept() {
	defer close(p.done)
	for {
		client, err := p.listener.Accept()
		if err != nil {
			return
		}
		server, err := stdnet.DialTimeout("tcp", p.target, 2*time.Second)
		if err != nil {
			_ = client.Close()
			continue
		}
		connection := &muxHopConnection{client: client, server: server, owner: p}
		p.mu.Lock()
		p.conns = append(p.conns, connection)
		p.mu.Unlock()
		p.active.Add(1)
		go connection.forward(server, client)
		go connection.forward(client, server)
	}
}

func (p *muxHopProxy) connectionCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(p.conns)
}

func (p *muxHopProxy) setDelay(delay time.Duration) {
	p.delay.Store(int64(delay))
}

func (p *muxHopProxy) closeConnections(reset bool) {
	p.mu.Lock()
	connections := append([]*muxHopConnection(nil), p.conns...)
	p.mu.Unlock()
	for _, connection := range connections {
		if reset {
			if tcp, ok := connection.client.(*stdnet.TCPConn); ok {
				_ = tcp.SetLinger(0)
			}
		}
		connection.close()
	}
}

func (p *muxHopProxy) connection(index int) *muxHopConnection {
	p.mu.Lock()
	defer p.mu.Unlock()
	if index < 0 || index >= len(p.conns) {
		return nil
	}
	return p.conns[index]
}

func (p *muxHopProxy) close() {
	p.closeOnce.Do(func() {
		_ = p.listener.Close()
		<-p.done
		p.mu.Lock()
		connections := append([]*muxHopConnection(nil), p.conns...)
		p.mu.Unlock()
		for _, connection := range connections {
			connection.close()
		}
	})
}

func waitMuxScenario(t *testing.T, what string, timeout time.Duration, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

type muxIngressProtocol struct {
	inbound        *serial.TypedMessage
	inboundStream  *internet.StreamConfig
	outbound       *serial.TypedMessage
	outboundStream *internet.StreamConfig
}

func makeMuxIngressProtocol(t *testing.T, name string, serverPort net.Port) muxIngressProtocol {
	t.Helper()
	switch name {
	case "VLESS":
		userID := protocol.NewID(uuid.New())
		return muxIngressProtocol{
			inbound: serial.ToTypedMessage(&vlessinbound.Config{Clients: []*protocol.User{{
				Email: "mux-ingress-vless@example.com", Account: serial.ToTypedMessage(&vless.Account{Id: userID.String()}),
			}}}),
			outbound: serial.ToTypedMessage(&vlessoutbound.Config{Vnext: &protocol.ServerEndpoint{
				Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(serverPort),
				User: &protocol.User{Email: "mux-ingress-vless@example.com", Account: serial.ToTypedMessage(&vless.Account{Id: userID.String()})},
			}}),
		}
	case "AnyTLS":
		certificate, certificateHash := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		password := "mux-anytls-password"
		return muxIngressProtocol{
			inbound: serial.ToTypedMessage(&proxyanytls.ServerConfig{Users: []*protocol.User{{
				Email: "mux-ingress-anytls@example.com", Account: serial.ToTypedMessage(&proxyanytls.Account{Password: password}),
			}}}),
			inboundStream: &internet.StreamConfig{
				ProtocolName: "tcp",
				SecurityType: serial.GetMessageType(&tls.Config{}),
				SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&tls.Config{
					Certificate: []*tls.Certificate{tls.ParseCertificate(certificate)},
				})},
			},
			outbound: serial.ToTypedMessage(&proxyanytls.ClientConfig{Server: &protocol.ServerEndpoint{
				Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(serverPort),
				User: &protocol.User{Email: "mux-ingress-anytls@example.com", Account: serial.ToTypedMessage(&proxyanytls.Account{Password: password})},
			}}),
			outboundStream: &internet.StreamConfig{
				ProtocolName: "tcp",
				SecurityType: serial.GetMessageType(&tls.Config{}),
				SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&tls.Config{
					ServerName: "localhost", PinnedPeerCertSha256: [][]byte{certificateHash[:]},
				})},
			},
		}
	case "Hysteria2":
		certificate, certificateHash := cert.MustGenerate(nil, cert.CommonName("localhost"), cert.DNSNames("localhost"))
		auth := "mux-hysteria2-auth"
		return muxIngressProtocol{
			inbound: serial.ToTypedMessage(&hyproxy.ServerConfig{Users: []*protocol.User{{
				Email: "mux-ingress-hysteria2@example.com", Account: serial.ToTypedMessage(&hyaccount.Account{Auth: auth}),
			}}}),
			inboundStream: &internet.StreamConfig{
				ProtocolName: "hysteria",
				TransportSettings: []*internet.TransportConfig{{ProtocolName: "hysteria", Settings: serial.ToTypedMessage(&hytransport.Config{
					Version: 2, Auth: auth, UdpIdleTimeout: 60,
				})}},
				SecurityType: serial.GetMessageType(&tls.Config{}),
				SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&tls.Config{
					Certificate: []*tls.Certificate{tls.ParseCertificate(certificate)}, NextProtocol: []string{"h3"},
				})},
			},
			outbound: serial.ToTypedMessage(&hyproxy.ClientConfig{
				Version: 2,
				Server: &protocol.ServerEndpoint{
					Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(serverPort),
					User: &protocol.User{Email: "mux-ingress-hysteria2@example.com", Account: serial.ToTypedMessage(&hyaccount.Account{Auth: auth})},
				},
			}),
			outboundStream: &internet.StreamConfig{
				ProtocolName: "hysteria",
				TransportSettings: []*internet.TransportConfig{{ProtocolName: "hysteria", Settings: serial.ToTypedMessage(&hytransport.Config{
					Version: 2, Auth: auth, UdpIdleTimeout: 60,
				})}},
				SecurityType: serial.GetMessageType(&tls.Config{}),
				SecuritySettings: []*serial.TypedMessage{serial.ToTypedMessage(&tls.Config{
					ServerName: "localhost", PinnedPeerCertSha256: [][]byte{certificateHash[:]}, NextProtocol: []string{"h3"},
				})},
			},
		}
	default:
		t.Fatalf("unknown ingress protocol %q", name)
		return muxIngressProtocol{}
	}
}

type realMuxScenario struct {
	clientPort net.Port
	proxy      *muxHopProxy
	servers    []*exec.Cmd
}

func stopMuxProcessesWithin(commands []*exec.Cmd, timeout time.Duration) error {
	for _, command := range commands {
		if runtime.GOOS == "windows" {
			_ = command.Process.Kill()
		} else {
			_ = command.Process.Signal(syscall.SIGTERM)
		}
	}
	done := make(chan struct{})
	go func() {
		for _, command := range commands {
			_, _ = command.Process.Wait()
		}
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-time.After(timeout):
		for _, command := range commands {
			_ = command.Process.Kill()
		}
		<-done
		return fmt.Errorf("Xray processes did not stop within %s", timeout)
	}
}

func startRealMuxScenario(t *testing.T, protocolName string, concurrency int32, destination net.Destination) *realMuxScenario {
	t.Helper()
	relayID := protocol.NewID(uuid.New())
	relayPort := tcp.PickPort()
	proxy := newMuxHopProxy(t, relayPort)
	ingressPort := tcp.PickPort()
	clientPort := tcp.PickPort()
	ingressProtocol := makeMuxIngressProtocol(t, protocolName, ingressPort)

	relayConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: testMuxReceiver(relayPort, nil),
			ProxySettings: serial.ToTypedMessage(&vlessinbound.Config{Clients: []*protocol.User{{
				Email: "mux-hop@example.com", Account: serial.ToTypedMessage(&vless.Account{Id: relayID.String()}),
			}}}),
		}},
		Outbound: []*core.OutboundHandlerConfig{{ProxySettings: serial.ToTypedMessage(&freedom.Config{IpsBlocked: &freedom.IPRules{}})}},
	}
	ingressConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: testMuxReceiver(ingressPort, ingressProtocol.inboundStream),
			ProxySettings:    ingressProtocol.inbound,
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			Tag:            "mux-hop",
			SenderSettings: testMuxSender(nil, concurrency),
			ProxySettings: serial.ToTypedMessage(&vlessoutbound.Config{Vnext: &protocol.ServerEndpoint{
				Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(proxy.port()),
				User: &protocol.User{Email: "mux-hop@example.com", Account: serial.ToTypedMessage(&vless.Account{Id: relayID.String()})},
			}}),
		}},
	}
	clientConfig := &core.Config{
		Inbound: []*core.InboundHandlerConfig{{
			ReceiverSettings: testMuxReceiver(clientPort, nil),
			ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
				Address: net.NewIPOrDomain(destination.Address), Port: uint32(destination.Port), Networks: []net.Network{net.Network_TCP},
			}),
		}},
		Outbound: []*core.OutboundHandlerConfig{{
			SenderSettings: testDirectSender(ingressProtocol.outboundStream),
			ProxySettings:  ingressProtocol.outbound,
		}},
	}

	servers, err := InitializeServerConfigs(relayConfig, ingressConfig, clientConfig)
	if err != nil {
		t.Fatal(err)
	}
	scenario := &realMuxScenario{clientPort: clientPort, proxy: proxy, servers: servers}
	t.Cleanup(func() {
		if scenario.servers != nil {
			CloseAllServers(scenario.servers)
			scenario.servers = nil
		}
		proxy.close()
	})
	return scenario
}

func (s *realMuxScenario) close(t *testing.T) {
	t.Helper()
	if s.servers != nil {
		CloseAllServers(s.servers)
		s.servers = nil
	}
	waitMuxScenario(t, "mux-hop sockets to close", 5*time.Second, func() bool { return s.proxy.active.Load() == 0 })
	s.proxy.close()
}

func (s *realMuxScenario) closeWithin(t *testing.T, timeout time.Duration) {
	t.Helper()
	if s.servers != nil {
		if err := stopMuxProcessesWithin(s.servers, timeout); err != nil {
			t.Fatal(err)
		}
		s.servers = nil
	}
	waitMuxScenario(t, "mux-hop sockets to close", timeout, func() bool { return s.proxy.active.Load() == 0 })
	s.proxy.close()
}

func runMuxProcessDataPlane(t *testing.T, protocolName string, concurrency int32) {
	t.Helper()
	backend := tcp.Server{MsgProcessor: xor}
	destination, err := backend.Start()
	common.Must(err)
	defer backend.Close()
	scenario := startRealMuxScenario(t, protocolName, concurrency, destination)

	for range 3 {
		if err := testTCPConn(scenario.clientPort, 16*1024, 20*time.Second)(); err != nil {
			t.Fatal(err)
		}
		time.Sleep(1500 * time.Millisecond)
	}
	waitMuxScenario(t, "first physical mux connection", 2*time.Second, func() bool { return scenario.proxy.connectionCount() >= 1 })
	if got := scenario.proxy.connectionCount(); got != 1 {
		t.Fatalf("healthy sequential streams used %d physical mux connections, want 1", got)
	}

	var group errgroup.Group
	for range 16 {
		group.Go(testTCPConn(scenario.clientPort, 256*1024, 20*time.Second))
	}
	if err := group.Wait(); err != nil {
		t.Fatal(err)
	}
	maxConnections := (16 + int(concurrency) - 1) / int(concurrency)
	if got := scenario.proxy.connectionCount(); got > maxConnections {
		t.Fatalf("concurrent streams used %d physical mux connections, want at most %d", got, maxConnections)
	}
	waitMuxScenario(t, "idle pool bound", 5*time.Second, func() bool { return scenario.proxy.active.Load() <= 2 })
	scenario.close(t)
}

func TestMuxWorkerPoolRealProcesses(t *testing.T) {
	for _, concurrency := range []int32{1, 8} {
		concurrency := concurrency
		t.Run(map[int32]string{1: "concurrency-1", 8: "concurrency-8"}[concurrency], func(t *testing.T) {
			for _, protocolName := range []string{"VLESS", "AnyTLS", "Hysteria2"} {
				protocolName := protocolName
				t.Run(protocolName, func(t *testing.T) {
					runMuxProcessDataPlane(t, protocolName, concurrency)
				})
			}
		})
	}
}

func TestMuxWorkerPoolRealProcessFaultMatrix(t *testing.T) {
	for _, concurrency := range []int32{1, 8} {
		concurrency := concurrency
		t.Run(map[int32]string{1: "concurrency-1", 8: "concurrency-8"}[concurrency], func(t *testing.T) {
			for _, protocolName := range []string{"VLESS", "AnyTLS", "Hysteria2"} {
				protocolName := protocolName
				t.Run(protocolName, func(t *testing.T) {
					backend := tcp.Server{MsgProcessor: xor}
					destination, err := backend.Start()
					common.Must(err)
					defer backend.Close()
					scenario := startRealMuxScenario(t, protocolName, concurrency, destination)

					first, err := stdnet.DialTimeout("tcp", stdnet.JoinHostPort("127.0.0.1", scenario.clientPort.String()), 5*time.Second)
					if err != nil {
						t.Fatal(err)
					}
					if err := testTCPConn2(first, 16*1024, 10*time.Second)(); err != nil {
						t.Fatal(err)
					}
					waitMuxScenario(t, "first physical mux connection", 2*time.Second, func() bool { return scenario.proxy.connectionCount() == 1 })
					scenario.proxy.connection(0).blackhole.Store(true)
					_ = first.Close()
					// VLESS may keep the just-closed logical stream in teardown for
					// about one second. Let the configured recent-I/O window expire
					// after teardown, while remaining inside the probe timeout.
					time.Sleep(time.Duration(testWorkerPoolConfig().ProbeIntervalSecs)*time.Second + 1800*time.Millisecond)

					started := time.Now()
					longLived, err := stdnet.DialTimeout("tcp", stdnet.JoinHostPort("127.0.0.1", scenario.clientPort.String()), 5*time.Second)
					if err != nil {
						t.Fatal(err)
					}
					defer longLived.Close()
					if err := testTCPConn2(longLived, 64*1024, 10*time.Second)(); err != nil {
						t.Fatal(err)
					}
					if elapsed := time.Since(started); elapsed >= realProcessProbeTimeout {
						t.Fatalf("new stream waited for the failed worker probe: %s", elapsed)
					}
					waitMuxScenario(t, "replacement mux connection", 2*time.Second, func() bool { return scenario.proxy.connectionCount() == 2 })

					time.Sleep(realProcessProbeTimeout + 300*time.Millisecond)
					if err := testTCPConn2(longLived, 64*1024, 10*time.Second)(); err != nil {
						t.Fatalf("active long stream was interrupted by another worker probe failure: %v", err)
					}
					_ = longLived.Close()
					scenario.close(t)
				})
			}
		})
	}
}

func TestMuxWorkerPoolShutdownDuringBlackholedProbe(t *testing.T) {
	for _, protocolName := range []string{"VLESS", "AnyTLS", "Hysteria2"} {
		protocolName := protocolName
		t.Run(protocolName, func(t *testing.T) {
			backend := tcp.Server{MsgProcessor: xor}
			destination, err := backend.Start()
			common.Must(err)
			defer backend.Close()
			scenario := startRealMuxScenario(t, protocolName, 8, destination)

			first, err := stdnet.DialTimeout("tcp", stdnet.JoinHostPort("127.0.0.1", scenario.clientPort.String()), 5*time.Second)
			if err != nil {
				t.Fatal(err)
			}
			if err := testTCPConn2(first, 16*1024, 10*time.Second)(); err != nil {
				t.Fatal(err)
			}
			waitMuxScenario(t, "first physical mux connection", 2*time.Second, func() bool { return scenario.proxy.connectionCount() == 1 })
			scenario.proxy.connection(0).blackhole.Store(true)
			_ = first.Close()
			// Shutdown must interrupt an actual in-flight probe, after recent I/O expires.
			time.Sleep(time.Duration(testWorkerPoolConfig().ProbeIntervalSecs)*time.Second + 1800*time.Millisecond)

			scenario.closeWithin(t, 5*time.Second)
		})
	}
}
