package scenarios

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	stdnet "net"
	"os/exec"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/commander"
	"github.com/xtls/xray-core/app/log"
	"github.com/xtls/xray-core/app/router"
	"github.com/xtls/xray-core/app/stats"
	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/buf"
	clog "github.com/xtls/xray-core/common/log"
	"github.com/xtls/xray-core/common/mux"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/uuid"
	core "github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/proxy/blackhole"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"github.com/xtls/xray-core/proxy/freedom"
	"github.com/xtls/xray-core/proxy/vless"
	vlessinbound "github.com/xtls/xray-core/proxy/vless/inbound"
	vlessoutbound "github.com/xtls/xray-core/proxy/vless/outbound"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"golang.org/x/sync/errgroup"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/proto"
)

const (
	vlessReversePortalTag = "vless-reverse-portal"
	vlessReverseBridgeTag = "vless-reverse-bridge"
)

func testVLESSReverseHealth() *vless.ReverseHealth {
	return &vless.ReverseHealth{
		ProbeIntervalMs:           250,
		ProbeTimeoutMs:            125,
		ConfirmTimeoutMs:          250,
		HealthLeaseMs:             750,
		MinHealthyWorkers:         2,
		MaxIdleReadyWorkers:       2,
		MaxParallelDialsPerPair:   2,
		DialReadyTimeoutMs:        1500,
		ReconnectBackoffCapMs:     500,
		DisconnectOnHealthFailure: false,
		Tuning: &vless.ReverseHealthTuning{
			ProbeJitterPercent:     0,
			RecoverySuccesses:      2,
			SpareWorkers:           0,
			MaxHealthyWorkers:      2,
			MaxSessionsPerWorker:   2,
			ReconnectBackoffBaseMs: 75,
			ReconnectStableResetMs: 750,
		},
	}
}

func testVLESSReverseApps(apiPort net.Port, rules ...*router.RoutingRule) []*serial.TypedMessage {
	return []*serial.TypedMessage{
		serial.ToTypedMessage(&log.Config{ErrorLogLevel: clog.Severity_Warning, ErrorLogType: log.LogType_Console}),
		serial.ToTypedMessage(&stats.Config{}),
		serial.ToTypedMessage(&commander.Config{
			Tag:     "api",
			Listen:  stdnet.JoinHostPort("127.0.0.1", apiPort.String()),
			Service: []*serial.TypedMessage{serial.ToTypedMessage(&statscmd.Config{})},
		}),
		serial.ToTypedMessage(&router.Config{Rule: rules}),
	}
}

type realVLESSReverseScenario struct {
	externalPort    net.Port
	externalUDPPort net.Port
	portalAPI       net.Port
	bridgeAPI       net.Port
	proxy           *muxHopProxy
	portal          *exec.Cmd
	bridge          *exec.Cmd
	portalConfig    *core.Config
	bridgeConfig    *core.Config
}

type vlessReverseUDPEchoServer struct {
	connection *stdnet.UDPConn
	done       chan struct{}
	closeOnce  sync.Once
}

func startVLESSReverseUDPEchoServer(t *testing.T) (*vlessReverseUDPEchoServer, net.Destination) {
	t.Helper()
	connection, err := stdnet.ListenUDP("udp4", &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	server := &vlessReverseUDPEchoServer{connection: connection, done: make(chan struct{})}
	go server.serve()
	t.Cleanup(server.close)
	address := connection.LocalAddr().(*stdnet.UDPAddr)
	return server, net.UDPDestination(net.IPAddress(address.IP), net.Port(address.Port))
}

func (s *vlessReverseUDPEchoServer) serve() {
	defer close(s.done)
	payload := make([]byte, 65535)
	for {
		n, peer, err := s.connection.ReadFromUDP(payload)
		if err != nil {
			return
		}
		if _, err := s.connection.WriteToUDP(xor(payload[:n]), peer); err != nil {
			return
		}
	}
}

func (s *vlessReverseUDPEchoServer) close() {
	s.closeOnce.Do(func() {
		_ = s.connection.Close()
		<-s.done
	})
}

func pickVLESSReverseUDPPort(t *testing.T) net.Port {
	t.Helper()
	connection, err := stdnet.ListenUDP("udp4", &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	port := net.Port(connection.LocalAddr().(*stdnet.UDPAddr).Port)
	_ = connection.Close()
	return port
}

func startRealVLESSReverseScenario(t *testing.T) *realVLESSReverseScenario {
	t.Helper()
	backend := tcp.Server{MsgProcessor: xor}
	destination, err := backend.Start()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { backend.Close() })
	_, udpDestination := startVLESSReverseUDPEchoServer(t)

	userID := protocol.NewID(uuid.New())
	reversePort := tcp.PickPort()
	externalPort := tcp.PickPort()
	externalUDPPort := pickVLESSReverseUDPPort(t)
	portalAPI := tcp.PickPort()
	bridgeAPI := tcp.PickPort()
	proxy := newMuxHopProxy(t, reversePort)

	portalConfig := &core.Config{
		App: testVLESSReverseApps(portalAPI, &router.RoutingRule{
			InboundTag: []string{"external", "external-udp"},
			TargetTag:  &router.RoutingRule_Tag{Tag: vlessReversePortalTag},
		}),
		Inbound: []*core.InboundHandlerConfig{
			{
				Tag:              "external",
				ReceiverSettings: testMuxReceiver(externalPort, nil),
				ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
					Address: net.NewIPOrDomain(destination.Address), Port: uint32(destination.Port), Networks: []net.Network{net.Network_TCP},
				}),
			},
			{
				Tag:              "reverse-carrier",
				ReceiverSettings: testMuxReceiver(reversePort, nil),
				ProxySettings: serial.ToTypedMessage(&vlessinbound.Config{Clients: []*protocol.User{{
					Email: "reverse@example.test",
					Account: serial.ToTypedMessage(&vless.Account{Id: userID.String(), Reverse: &vless.Reverse{
						Tag: vlessReversePortalTag, Health: testVLESSReverseHealth(),
					}}),
				}}}),
			},
			{
				Tag:              "external-udp",
				ReceiverSettings: testMuxReceiver(externalUDPPort, nil),
				ProxySettings: serial.ToTypedMessage(&dokodemo.Config{
					Address: net.NewIPOrDomain(udpDestination.Address), Port: uint32(udpDestination.Port), Networks: []net.Network{net.Network_UDP},
				}),
			},
		},
		Outbound: []*core.OutboundHandlerConfig{{
			Tag: "fail-closed", ProxySettings: serial.ToTypedMessage(&blackhole.Config{}),
		}},
	}
	bridgeConfig := &core.Config{
		App: testVLESSReverseApps(bridgeAPI, &router.RoutingRule{
			InboundTag: []string{vlessReverseBridgeTag},
			TargetTag:  &router.RoutingRule_Tag{Tag: "freedom"},
		}),
		Outbound: []*core.OutboundHandlerConfig{
			{Tag: "fail-closed", ProxySettings: serial.ToTypedMessage(&blackhole.Config{})},
			{Tag: "freedom", ProxySettings: serial.ToTypedMessage(&freedom.Config{IpsBlocked: &freedom.IPRules{}})},
			{
				Tag: "reverse",
				ProxySettings: serial.ToTypedMessage(&vlessoutbound.Config{Vnext: &protocol.ServerEndpoint{
					Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(proxy.port()),
					User: &protocol.User{Email: "reverse@example.test", Account: serial.ToTypedMessage(&vless.Account{
						Id: userID.String(), Reverse: &vless.Reverse{Tag: vlessReverseBridgeTag, Health: testVLESSReverseHealth()},
					})},
				}}),
			},
		},
	}

	scenario := &realVLESSReverseScenario{
		externalPort:    externalPort,
		externalUDPPort: externalUDPPort,
		portalAPI:       portalAPI,
		bridgeAPI:       bridgeAPI,
		proxy:           proxy,
		portalConfig:    portalConfig,
		bridgeConfig:    bridgeConfig,
	}
	if err := scenario.startPortal(); err != nil {
		t.Fatal(err)
	}
	if err := scenario.startBridge(); err != nil {
		_ = scenario.stopPortal(5 * time.Second)
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := scenario.stopAll(5 * time.Second); err != nil {
			t.Errorf("stop VLESS reverse processes: %v", err)
		}
	})
	return scenario
}

func (s *realVLESSReverseScenario) startPortal() error {
	if s.portal != nil {
		return fmt.Errorf("VLESS reverse portal is already running")
	}
	portal, err := InitializeServerConfig(proto.Clone(s.portalConfig).(*core.Config))
	if err != nil {
		return err
	}
	s.portal = portal
	return nil
}

func (s *realVLESSReverseScenario) stopPortal(timeout time.Duration) error {
	if s.portal == nil {
		return nil
	}
	portal := s.portal
	s.portal = nil
	return stopMuxProcessesWithin([]*exec.Cmd{portal}, timeout)
}

func (s *realVLESSReverseScenario) startBridge() error {
	if s.bridge != nil {
		return fmt.Errorf("VLESS reverse bridge is already running")
	}
	bridge, err := InitializeServerConfig(proto.Clone(s.bridgeConfig).(*core.Config))
	if err != nil {
		return err
	}
	s.bridge = bridge
	return nil
}

func (s *realVLESSReverseScenario) stopBridge(timeout time.Duration) error {
	if s.bridge == nil {
		return nil
	}
	bridge := s.bridge
	s.bridge = nil
	return stopMuxProcessesWithin([]*exec.Cmd{bridge}, timeout)
}

func (s *realVLESSReverseScenario) stopAll(timeout time.Duration) error {
	commands := make([]*exec.Cmd, 0, 2)
	if s.bridge != nil {
		commands = append(commands, s.bridge)
		s.bridge = nil
	}
	if s.portal != nil {
		commands = append(commands, s.portal)
		s.portal = nil
	}
	if len(commands) == 0 {
		return nil
	}
	return stopMuxProcessesWithin(commands, timeout)
}

func fetchVLESSReverseReport(apiPort net.Port) (mux.ReverseHealthReport, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	connection, err := grpc.DialContext(
		ctx,
		stdnet.JoinHostPort("127.0.0.1", apiPort.String()),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithBlock(),
	)
	if err != nil {
		return mux.ReverseHealthReport{}, err
	}
	defer connection.Close()
	response, err := statscmd.NewStatsServiceClient(connection).GetReverseHealthSnapshot(ctx, &statscmd.ReverseHealthRequest{})
	if err != nil {
		return mux.ReverseHealthReport{}, err
	}
	var report mux.ReverseHealthReport
	if err := json.Unmarshal(response.Json, &report); err != nil {
		return mux.ReverseHealthReport{}, err
	}
	return report, nil
}

func waitVLESSReverseReport(t *testing.T, apiPort net.Port, what string, timeout time.Duration, condition func(mux.ReverseHealthReport) bool) mux.ReverseHealthReport {
	t.Helper()
	deadline := time.Now().Add(timeout)
	var last mux.ReverseHealthReport
	var lastErr error
	for time.Now().Before(deadline) {
		last, lastErr = fetchVLESSReverseReport(apiPort)
		if lastErr == nil && condition(last) {
			return last
		}
		time.Sleep(25 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s: last report=%+v, last error=%v", what, last, lastErr)
	return mux.ReverseHealthReport{}
}

func readyVLESSReverseWorkers(report mux.ReverseHealthReport) []mux.ReverseHealthSnapshot {
	ready := make([]mux.ReverseHealthSnapshot, 0, len(report.Workers))
	for _, worker := range report.Workers {
		if worker.State == "READY" {
			ready = append(ready, worker)
		}
	}
	return ready
}

func waitVLESSReverseReady(t *testing.T, scenario *realVLESSReverseScenario) (mux.ReverseHealthReport, mux.ReverseHealthReport) {
	t.Helper()
	portal := waitVLESSReverseReport(t, scenario.portalAPI, "two READY portal workers", 10*time.Second, func(report mux.ReverseHealthReport) bool {
		return len(readyVLESSReverseWorkers(report)) == 2
	})
	bridge := waitVLESSReverseReport(t, scenario.bridgeAPI, "two READY bridge workers", 10*time.Second, func(report mux.ReverseHealthReport) bool {
		return len(readyVLESSReverseWorkers(report)) == 2
	})
	waitMuxScenario(t, "two active physical reverse carriers", 2*time.Second, func() bool {
		return scenario.proxy.active.Load() == 2
	})
	return portal, bridge
}

func vlessReverseWorkerIDs(workers []mux.ReverseHealthSnapshot) map[uint64]struct{} {
	ids := make(map[uint64]struct{}, len(workers))
	for _, worker := range workers {
		ids[worker.WorkerID] = struct{}{}
	}
	return ids
}

func vlessReverseHasNewReady(report mux.ReverseHealthReport, old map[uint64]struct{}, count int) bool {
	newReady := 0
	for _, worker := range readyVLESSReverseWorkers(report) {
		if _, exists := old[worker.WorkerID]; !exists {
			newReady++
		}
	}
	return newReady >= count
}

func vlessReverseObserved(report mux.ReverseHealthReport, workerID uint64, state string) bool {
	for _, worker := range report.Workers {
		if worker.WorkerID == workerID && worker.State == state {
			return true
		}
	}
	for _, event := range report.Events {
		if event.WorkerID == workerID && event.State == state {
			return true
		}
	}
	return false
}

func vlessReverseActiveWorker(report mux.ReverseHealthReport) (uint64, bool) {
	for _, worker := range report.Workers {
		if worker.ActiveSessions > 0 {
			return worker.WorkerID, true
		}
	}
	return 0, false
}

func snapshotMuxHopConnections(proxy *muxHopProxy) []*muxHopConnection {
	proxy.mu.Lock()
	defer proxy.mu.Unlock()
	return append([]*muxHopConnection(nil), proxy.conns...)
}

func deterministicVLESSReversePayload(stream, round, size int) []byte {
	payload := make([]byte, size)
	for i := range payload {
		payload[i] = byte((stream*53 + round*97 + i*31 + i/251) % 251)
	}
	return payload
}

func exchangeVLESSReversePayload(connection stdnet.Conn, payload []byte, timeout time.Duration) error {
	if err := connection.SetDeadline(time.Now().Add(timeout)); err != nil {
		return err
	}
	defer connection.SetDeadline(time.Time{})
	for written := 0; written < len(payload); {
		n, err := connection.Write(payload[written:])
		if err != nil {
			return err
		}
		written += n
	}
	received := make([]byte, len(payload))
	if _, err := io.ReadFull(connection, received); err != nil {
		return err
	}
	if expected := xor(payload); !bytes.Equal(received, expected) {
		return fmt.Errorf("payload corruption: got %x, want %x", received[:min(len(received), 32)], expected[:min(len(expected), 32)])
	}
	return nil
}

func dialVLESSReverseExternal(port net.Port) (stdnet.Conn, error) {
	return stdnet.DialTimeout("tcp", stdnet.JoinHostPort("127.0.0.1", port.String()), 2*time.Second)
}

func dialVLESSReverseUDP(port net.Port) (*stdnet.UDPConn, error) {
	return stdnet.DialUDP("udp4", nil, &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1), Port: int(port)})
}

func exchangeVLESSReverseDatagram(connection *stdnet.UDPConn, payload []byte, timeout time.Duration) error {
	if err := connection.SetDeadline(time.Now().Add(timeout)); err != nil {
		return err
	}
	defer connection.SetDeadline(time.Time{})
	written, err := connection.Write(payload)
	if err != nil {
		return err
	}
	if written != len(payload) {
		return fmt.Errorf("short UDP write: got %d, want %d", written, len(payload))
	}
	received := make([]byte, len(payload)+1)
	n, err := connection.Read(received)
	if err != nil {
		return err
	}
	expected := xor(payload)
	if n != len(expected) || !bytes.Equal(received[:n], expected) {
		return fmt.Errorf("UDP payload corruption: got %x, want %x", received[:min(n, 32)], expected[:min(len(expected), 32)])
	}
	return nil
}

func assertVLESSReverseCapacity(t *testing.T, report mux.ReverseHealthReport, sessions int) {
	t.Helper()
	ready := readyVLESSReverseWorkers(report)
	if len(ready) != 2 {
		t.Fatalf("got %d READY workers, want 2: %+v", len(ready), report.Workers)
	}
	total := 0
	for _, worker := range ready {
		if worker.ActiveSessions > 2 {
			t.Fatalf("worker %d exceeded max_sessions_per_worker: %+v", worker.WorkerID, worker)
		}
		total += int(worker.ActiveSessions)
	}
	if total != sessions {
		t.Fatalf("READY workers report %d active sessions, want %d: %+v", total, sessions, ready)
	}
}

func TestVLESSReverseRealProcessConcurrencyAndRestart(t *testing.T) {
	scenario := startRealVLESSReverseScenario(t)
	initialPortal, _ := waitVLESSReverseReady(t, scenario)
	initialPortalIDs := vlessReverseWorkerIDs(initialPortal.Workers)

	connections := make([]stdnet.Conn, 4)
	defer func() {
		for _, connection := range connections {
			if connection != nil {
				_ = connection.Close()
			}
		}
	}()
	type openedStream struct {
		connection stdnet.Conn
		index      int
		err        error
	}
	start := make(chan struct{})
	opened := make(chan openedStream, len(connections))
	for stream := range connections {
		go func() {
			<-start
			connection, err := dialVLESSReverseExternal(scenario.externalPort)
			if err == nil {
				err = exchangeVLESSReversePayload(connection, deterministicVLESSReversePayload(stream, 0, 257), 3*time.Second)
			}
			if err != nil && connection != nil {
				_ = connection.Close()
				connection = nil
			}
			opened <- openedStream{connection: connection, index: stream, err: err}
		}()
	}
	close(start)
	var openErr error
	for range connections {
		result := <-opened
		connections[result.index] = result.connection
		if result.err != nil && openErr == nil {
			openErr = fmt.Errorf("prime stream %d: %w", result.index, result.err)
		}
	}
	if openErr != nil {
		t.Fatal(openErr)
	}

	portal := waitVLESSReverseReport(t, scenario.portalAPI, "four active portal sessions", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		total := uint32(0)
		for _, worker := range readyVLESSReverseWorkers(report) {
			total += worker.ActiveSessions
		}
		return total == 4
	})
	bridge := waitVLESSReverseReport(t, scenario.bridgeAPI, "four active bridge sessions", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		total := uint32(0)
		for _, worker := range readyVLESSReverseWorkers(report) {
			total += worker.ActiveSessions
		}
		return total == 4
	})
	assertVLESSReverseCapacity(t, portal, 4)
	assertVLESSReverseCapacity(t, bridge, 4)
	if got := scenario.proxy.active.Load(); got != 2 {
		t.Fatalf("four logical streams used %d physical carriers, want 2", got)
	}
	overflow, err := dialVLESSReverseExternal(scenario.externalPort)
	if err == nil {
		defer overflow.Close()
		if err := exchangeVLESSReversePayload(overflow, []byte("over capacity"), 750*time.Millisecond); err == nil {
			t.Fatal("fifth stream exceeded the configured two sessions per carrier")
		}
	}

	frameBoundarySizes := []int{1, buf.Size - 1, buf.Size, buf.Size + 1, 8*buf.Size + 17}
	var group errgroup.Group
	for stream, connection := range connections {
		stream, connection := stream, connection
		group.Go(func() error {
			for round, size := range frameBoundarySizes {
				payload := deterministicVLESSReversePayload(stream, round+1, size)
				if err := exchangeVLESSReversePayload(connection, payload, 5*time.Second); err != nil {
					return fmt.Errorf("stream %d round %d size %d: %w", stream, round, size, err)
				}
			}
			return nil
		})
	}
	if err := group.Wait(); err != nil {
		t.Fatal(err)
	}

	if err := scenario.stopBridge(5 * time.Second); err != nil {
		t.Fatal(err)
	}
	waitMuxScenario(t, "reverse carriers to close after bridge shutdown", 3*time.Second, func() bool {
		return scenario.proxy.active.Load() == 0
	})
	for index, connection := range connections {
		if err := exchangeVLESSReversePayload(connection, []byte{byte(index)}, 500*time.Millisecond); err == nil {
			t.Fatalf("stream %d survived bridge process termination", index)
		}
	}
	waitVLESSReverseReport(t, scenario.portalAPI, "portal to remove stopped bridge workers", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		return len(readyVLESSReverseWorkers(report)) == 0
	})
	failed, err := dialVLESSReverseExternal(scenario.externalPort)
	if err == nil {
		defer failed.Close()
		if err := exchangeVLESSReversePayload(failed, []byte("must fail closed"), 750*time.Millisecond); err == nil {
			t.Fatal("new traffic succeeded with no reverse carrier")
		}
	}

	if err := scenario.startBridge(); err != nil {
		t.Fatal(err)
	}
	restartedPortal, _ := waitVLESSReverseReady(t, scenario)
	if !vlessReverseHasNewReady(restartedPortal, initialPortalIDs, 2) {
		t.Fatalf("bridge restart did not create a fresh READY generation: %+v", restartedPortal.Workers)
	}
	fresh, err := dialVLESSReverseExternal(scenario.externalPort)
	if err != nil {
		t.Fatal(err)
	}
	defer fresh.Close()
	if err := exchangeVLESSReversePayload(fresh, deterministicVLESSReversePayload(9, 9, 32*1024+3), 3*time.Second); err != nil {
		t.Fatalf("fresh traffic after bridge restart: %v", err)
	}
}

func TestVLESSReverseRealProcessUDPDataPlane(t *testing.T) {
	scenario := startRealVLESSReverseScenario(t)
	initialPortal, _ := waitVLESSReverseReady(t, scenario)
	initialPortalIDs := vlessReverseWorkerIDs(initialPortal.Workers)

	connections := make([]*stdnet.UDPConn, 4)
	defer func() {
		for _, connection := range connections {
			if connection != nil {
				_ = connection.Close()
			}
		}
	}()
	for stream := range connections {
		connection, err := dialVLESSReverseUDP(scenario.externalUDPPort)
		if err != nil {
			t.Fatal(err)
		}
		connections[stream] = connection
		if err := exchangeVLESSReverseDatagram(connection, deterministicVLESSReversePayload(stream, 0, 257), 3*time.Second); err != nil {
			t.Fatalf("prime UDP stream %d: %v", stream, err)
		}
	}

	portal := waitVLESSReverseReport(t, scenario.portalAPI, "four active portal UDP sessions", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		total := uint32(0)
		for _, worker := range readyVLESSReverseWorkers(report) {
			total += worker.ActiveSessions
		}
		return total == 4
	})
	bridge := waitVLESSReverseReport(t, scenario.bridgeAPI, "four active bridge UDP sessions", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		total := uint32(0)
		for _, worker := range readyVLESSReverseWorkers(report) {
			total += worker.ActiveSessions
		}
		return total == 4
	})
	assertVLESSReverseCapacity(t, portal, 4)
	assertVLESSReverseCapacity(t, bridge, 4)

	packetSizes := []int{1, 1200, buf.Size - 1, buf.Size + 1, 4*buf.Size + 17}
	var group errgroup.Group
	for stream, connection := range connections {
		stream, connection := stream, connection
		group.Go(func() error {
			for round, size := range packetSizes {
				payload := deterministicVLESSReversePayload(stream, round+1, size)
				if err := exchangeVLESSReverseDatagram(connection, payload, 5*time.Second); err != nil {
					return fmt.Errorf("UDP stream %d round %d size %d: %w", stream, round, size, err)
				}
			}
			return nil
		})
	}
	if err := group.Wait(); err != nil {
		t.Fatal(err)
	}

	overflow, err := dialVLESSReverseUDP(scenario.externalUDPPort)
	if err != nil {
		t.Fatal(err)
	}
	defer overflow.Close()
	if err := exchangeVLESSReverseDatagram(overflow, []byte("over UDP capacity"), 750*time.Millisecond); err == nil {
		t.Fatal("fifth UDP stream exceeded the configured two sessions per carrier")
	}
	_ = overflow.Close()

	if err := scenario.stopBridge(5 * time.Second); err != nil {
		t.Fatal(err)
	}
	waitMuxScenario(t, "UDP reverse carriers to close after bridge shutdown", 3*time.Second, func() bool {
		return scenario.proxy.active.Load() == 0
	})
	waitVLESSReverseReport(t, scenario.portalAPI, "portal to remove stopped UDP workers", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		return len(readyVLESSReverseWorkers(report)) == 0
	})
	if err := exchangeVLESSReverseDatagram(connections[0], []byte("must not cross a stopped bridge"), 500*time.Millisecond); err == nil {
		t.Fatal("UDP association survived bridge process termination")
	}

	if err := scenario.startBridge(); err != nil {
		t.Fatal(err)
	}
	restartedPortal, _ := waitVLESSReverseReady(t, scenario)
	if !vlessReverseHasNewReady(restartedPortal, initialPortalIDs, 2) {
		t.Fatalf("bridge restart did not create fresh READY workers for UDP: %+v", restartedPortal.Workers)
	}
	// Reusing the same source socket proves the inbound association was detached
	// from the failed carrier and that no stale datagram crossed generations.
	if err := exchangeVLESSReverseDatagram(connections[0], deterministicVLESSReversePayload(0, 99, 32*1024+19), 3*time.Second); err != nil {
		t.Fatalf("UDP association did not recover on fresh reverse workers: %v", err)
	}
}

func TestVLESSReverseRealProcessOneWayBlackholeRecovery(t *testing.T) {
	scenario := startRealVLESSReverseScenario(t)
	initialPortal, initialBridge := waitVLESSReverseReady(t, scenario)
	initialPortalIDs := vlessReverseWorkerIDs(initialPortal.Workers)
	initialBridgeIDs := vlessReverseWorkerIDs(initialBridge.Workers)
	initialCarriers := snapshotMuxHopConnections(scenario.proxy)
	if len(initialCarriers) != 2 {
		t.Fatalf("got %d initial physical carriers, want 2", len(initialCarriers))
	}

	longLived, err := dialVLESSReverseExternal(scenario.externalPort)
	if err != nil {
		t.Fatal(err)
	}
	defer longLived.Close()
	if err := exchangeVLESSReversePayload(longLived, deterministicVLESSReversePayload(1, 0, 16*1024+7), 3*time.Second); err != nil {
		t.Fatal(err)
	}
	initialPortal = waitVLESSReverseReport(t, scenario.portalAPI, "active portal session", 2*time.Second, func(report mux.ReverseHealthReport) bool {
		_, ok := vlessReverseActiveWorker(report)
		return ok
	})
	initialBridge = waitVLESSReverseReport(t, scenario.bridgeAPI, "active bridge session", 2*time.Second, func(report mux.ReverseHealthReport) bool {
		_, ok := vlessReverseActiveWorker(report)
		return ok
	})
	portalActiveID, _ := vlessReverseActiveWorker(initialPortal)
	bridgeActiveID, _ := vlessReverseActiveWorker(initialBridge)

	// The bridge-to-portal direction remains open. Silently dropping only the
	// portal-to-bridge direction models an asymmetric NAT or middlebox failure.
	for _, carrier := range initialCarriers {
		carrier.serverToClientBlackhole.Store(true)
	}
	if err := exchangeVLESSReversePayload(longLived, []byte("dropped while one-way blackholed"), 350*time.Millisecond); err == nil {
		t.Fatal("old stream unexpectedly crossed a one-way blackhole")
	}

	portalRecovered := waitVLESSReverseReport(t, scenario.portalAPI, "portal quarantine and replacement", 5*time.Second, func(report mux.ReverseHealthReport) bool {
		return vlessReverseObserved(report, portalActiveID, "SUSPECT") &&
			vlessReverseObserved(report, portalActiveID, "DRAINING") &&
			vlessReverseHasNewReady(report, initialPortalIDs, 2)
	})
	bridgeRecovered := waitVLESSReverseReport(t, scenario.bridgeAPI, "bridge quarantine and replacement", 5*time.Second, func(report mux.ReverseHealthReport) bool {
		return vlessReverseObserved(report, bridgeActiveID, "SUSPECT") &&
			vlessReverseObserved(report, bridgeActiveID, "DRAINING") &&
			vlessReverseHasNewReady(report, initialBridgeIDs, 2)
	})
	if len(readyVLESSReverseWorkers(portalRecovered)) < 2 || len(readyVLESSReverseWorkers(bridgeRecovered)) < 2 {
		t.Fatal("replacement generation was not READY on both sides")
	}
	if got := scenario.proxy.connectionCount(); got > 6 {
		t.Fatalf("one failure caused a reconnect storm: %d physical dials", got)
	}

	fresh, err := dialVLESSReverseExternal(scenario.externalPort)
	if err != nil {
		t.Fatal(err)
	}
	if err := exchangeVLESSReversePayload(fresh, deterministicVLESSReversePayload(2, 0, 48*1024+5), 3*time.Second); err != nil {
		_ = fresh.Close()
		t.Fatalf("new traffic did not recover through replacement carriers: %v", err)
	}
	_ = fresh.Close()
	dialsAfterRecovery := scenario.proxy.connectionCount()
	time.Sleep(750 * time.Millisecond)
	if got := scenario.proxy.connectionCount(); got != dialsAfterRecovery {
		t.Fatalf("healthy replacement pool kept dialing: before=%d after=%d", dialsAfterRecovery, got)
	}

	for _, carrier := range initialCarriers {
		carrier.serverToClientBlackhole.Store(false)
	}
	if err := exchangeVLESSReversePayload(longLived, deterministicVLESSReversePayload(1, 2, 24*1024+11), 3*time.Second); err != nil {
		t.Fatalf("pre-failure business stream did not resume after path recovery: %v", err)
	}
	_ = longLived.Close()
	waitVLESSReverseReport(t, scenario.portalAPI, "drained portal worker removal", 3*time.Second, func(report mux.ReverseHealthReport) bool {
		for _, worker := range report.Workers {
			if worker.WorkerID == portalActiveID {
				return false
			}
		}
		return true
	})
	waitMuxScenario(t, "old drained physical carrier cleanup", 3*time.Second, func() bool {
		return scenario.proxy.active.Load() == 2
	})
}

func TestVLESSReverseRealProcessPortalOutageBackoff(t *testing.T) {
	scenario := startRealVLESSReverseScenario(t)
	_, initialBridge := waitVLESSReverseReady(t, scenario)
	initialBridgeIDs := vlessReverseWorkerIDs(initialBridge.Workers)

	primer, err := dialVLESSReverseExternal(scenario.externalPort)
	if err != nil {
		t.Fatal(err)
	}
	if err := exchangeVLESSReversePayload(primer, deterministicVLESSReversePayload(3, 0, 8*1024+3), 3*time.Second); err != nil {
		_ = primer.Close()
		t.Fatal(err)
	}
	_ = primer.Close()

	attemptsBeforeOutage := scenario.proxy.accepted.Load()
	if err := scenario.stopPortal(5 * time.Second); err != nil {
		t.Fatal(err)
	}
	waitMuxScenario(t, "portal outage to close all carriers", 3*time.Second, func() bool {
		return scenario.proxy.active.Load() == 0
	})
	time.Sleep(2 * time.Second)
	waitVLESSReverseReport(t, scenario.bridgeAPI, "bridge to quarantine portal outage", 2*time.Second, func(report mux.ReverseHealthReport) bool {
		return len(readyVLESSReverseWorkers(report)) == 0
	})
	attemptsDuringOutage := scenario.proxy.accepted.Load() - attemptsBeforeOutage
	if attemptsDuringOutage < 2 {
		t.Fatalf("bridge made only %d carrier attempts during portal outage, want at least one full pair", attemptsDuringOutage)
	}
	// Two parallel attempts with a 500 ms capped exponential backoff should
	// remain well below a 100 ms monitor-loop storm (40 attempts in two seconds).
	if attemptsDuringOutage > 24 {
		t.Fatalf("portal outage caused a reconnect storm: %d attempts in two seconds", attemptsDuringOutage)
	}

	if err := scenario.startPortal(); err != nil {
		t.Fatal(err)
	}
	_, recoveredBridge := waitVLESSReverseReady(t, scenario)
	if !vlessReverseHasNewReady(recoveredBridge, initialBridgeIDs, 2) {
		t.Fatalf("portal restart did not yield a fresh READY bridge generation: %+v", recoveredBridge.Workers)
	}
	recovered, err := dialVLESSReverseExternal(scenario.externalPort)
	if err != nil {
		t.Fatal(err)
	}
	defer recovered.Close()
	if err := exchangeVLESSReversePayload(recovered, deterministicVLESSReversePayload(3, 1, 32*1024+13), 3*time.Second); err != nil {
		t.Fatalf("traffic did not recover after portal restart: %v", err)
	}
	attemptsAfterRecovery := scenario.proxy.accepted.Load()
	time.Sleep(time.Second)
	if got := scenario.proxy.accepted.Load(); got != attemptsAfterRecovery {
		t.Fatalf("bridge kept redialing after portal recovery: before=%d after=%d", attemptsAfterRecovery, got)
	}
}
