package scenarios

import (
	"bytes"
	"context"
	"fmt"
	"io"
	stdnet "net"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/commander"
	"github.com/xtls/xray-core/app/policy"
	"github.com/xtls/xray-core/app/proxyman/command"
	"github.com/xtls/xray-core/app/stats"
	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/uuid"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/proxy/dokodemo"
	"github.com/xtls/xray-core/proxy/freedom"
	"github.com/xtls/xray-core/proxy/vless"
	"github.com/xtls/xray-core/proxy/vless/encoding"
	"github.com/xtls/xray-core/proxy/vless/inbound"
	"github.com/xtls/xray-core/proxy/vless/outbound"
	"github.com/xtls/xray-core/testing/servers/tcp"
	"github.com/xtls/xray-core/transport/internet"
	grpcTransport "github.com/xtls/xray-core/transport/internet/grpc"
	grpcEncoding "github.com/xtls/xray-core/transport/internet/grpc/encoding"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// Exercise the same gRPC RemoveUser used by SyncGrants against real Xray processes. Bob must
// continuously transfer through the ORIGINAL socket while Alice's plain and multiplexed flows
// stop. A fresh Bob connection would not prove that the shared listener wasn't interrupted.
func TestQuotaRevocationRealProcessesKeepsOtherUserAndCounters(t *testing.T) {
	for _, transport := range []string{"tcp", "shared-grpc"} {
		t.Run(transport, func(t *testing.T) { quotaRevocationRealProcesses(t, transport) })
	}
}

func quotaRevocationRealProcesses(t *testing.T, transportName string) {
	echo, err := stdnet.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	var echoWorkers sync.WaitGroup
	var echoMu sync.Mutex
	echoConnections := make(map[stdnet.Conn]struct{})
	echoWorkers.Add(1)
	go func() {
		defer echoWorkers.Done()
		for {
			conn, err := echo.Accept()
			if err != nil {
				return
			}
			echoMu.Lock()
			echoConnections[conn] = struct{}{}
			echoMu.Unlock()
			echoWorkers.Add(1)
			go func() {
				defer echoWorkers.Done()
				defer func() { _ = conn.Close(); echoMu.Lock(); delete(echoConnections, conn); echoMu.Unlock() }()
				_, _ = io.Copy(conn, conn)
			}()
		}
	}()
	defer func() {
		_ = echo.Close()
		echoMu.Lock()
		for conn := range echoConnections {
			_ = conn.Close()
		}
		echoMu.Unlock()
		echoWorkers.Wait()
	}()
	targetPort := net.Port(echo.Addr().(*stdnet.TCPAddr).Port)
	aliceID, bobID := uuid.New(), uuid.New()
	alice := &protocol.User{Email: "alice-quota-test", Level: 1, Account: serial.ToTypedMessage(&vless.Account{Id: aliceID.String()})}
	bob := &protocol.User{Email: "bob-quota-test", Level: 1, Account: serial.ToTypedMessage(&vless.Account{Id: bobID.String()})}
	serverPort, apiPort, muxPort := tcp.PickPort(), tcp.PickPort(), tcp.PickPort()
	var wire *internet.StreamConfig
	if transportName == "shared-grpc" {
		wire = &internet.StreamConfig{ProtocolName: "grpc", TransportSettings: []*internet.TransportConfig{{ProtocolName: "grpc", Settings: serial.ToTypedMessage(&grpcTransport.Config{ServiceName: "quota-test"})}}}
	}
	server := &core.Config{
		App: []*serial.TypedMessage{
			serial.ToTypedMessage(&commander.Config{Listen: fmt.Sprintf("127.0.0.1:%d", apiPort), Service: []*serial.TypedMessage{serial.ToTypedMessage(&command.Config{}), serial.ToTypedMessage(&statscmd.Config{})}}),
			serial.ToTypedMessage(&stats.Config{}),
			serial.ToTypedMessage(&policy.Config{Level: map[uint32]*policy.Policy{1: {Stats: &policy.Policy_Stats{UserUplink: true, UserDownlink: true}}}}),
		},
		Inbound:  []*core.InboundHandlerConfig{{Tag: "quota-entry", ReceiverSettings: testMuxReceiver(serverPort, wire), ProxySettings: serial.ToTypedMessage(&inbound.Config{Users: []*protocol.User{alice, bob}, Decryption: "none"})}},
		Outbound: []*core.OutboundHandlerConfig{{ProxySettings: serial.ToTypedMessage(&freedom.Config{})}},
	}
	client := &core.Config{
		Inbound:  []*core.InboundHandlerConfig{{ReceiverSettings: testMuxReceiver(muxPort, nil), ProxySettings: serial.ToTypedMessage(&dokodemo.Config{RewriteAddress: net.NewIPOrDomain(net.LocalHostIP), RewritePort: uint32(targetPort), AllowedNetworks: []net.Network{net.Network_TCP}})}},
		Outbound: []*core.OutboundHandlerConfig{{SenderSettings: testMuxSender(wire, 4), ProxySettings: serial.ToTypedMessage(&outbound.Config{Vnext: &protocol.ServerEndpoint{Address: net.NewIPOrDomain(net.LocalHostIP), Port: uint32(serverPort), User: alice}})}},
	}
	processes, err := InitializeServerConfigs(server, client)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		for _, process := range processes {
			_ = process.Process.Signal(syscall.SIGTERM)
		}
		for _, process := range processes {
			done := make(chan error, 1)
			go func() { done <- process.Wait() }()
			select {
			case err := <-done:
				if err != nil {
					t.Errorf("Xray child exited unsuccessfully (including race failures): %v", err)
				}
			case <-time.After(3 * time.Second):
				_ = process.Process.Kill()
				<-done
				t.Error("Xray child shutdown exceeded its deadline")
			}
		}
	}()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	api, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", apiPort), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	cancel()
	if err != nil {
		t.Fatal(err)
	}
	defer api.Close()
	dialEntry := func() (stdnet.Conn, error) {
		return stdnet.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", serverPort), time.Second)
	}
	if transportName == "shared-grpc" {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		carrier, err := grpc.DialContext(ctx, fmt.Sprintf("127.0.0.1:%d", serverPort), grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
		cancel()
		if err != nil {
			t.Fatal(err)
		}
		defer carrier.Close()
		service := grpcEncoding.NewGRPCServiceClient(carrier).(grpcEncoding.GRPCServiceClientX)
		dialEntry = func() (stdnet.Conn, error) {
			ctx, cancel := context.WithCancel(context.Background())
			stream, err := service.TunCustomName(ctx, "quota-test", "Tun")
			if err != nil {
				cancel()
				return nil, err
			}
			return grpcEncoding.NewHunkConn(stream, cancel, nil), nil
		}
	}
	handler := command.NewHandlerServiceClient(api)
	statistics := statscmd.NewStatsServiceClient(api)
	usage := func() int64 {
		t.Helper()
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		response, err := statistics.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: "user>>>alice-quota-test>>>traffic>>>", Reset_: false})
		if err != nil {
			t.Fatal(err)
		}
		var total int64
		for _, counter := range response.Stat {
			total += counter.Value
		}
		return total
	}
	exchange := func(conn stdnet.Conn) error {
		payload := []byte("continuous quota isolation")
		_ = conn.SetDeadline(time.Now().Add(time.Second))
		if _, err := conn.Write(payload); err != nil {
			return err
		}
		got := make([]byte, len(payload))
		if _, err := io.ReadFull(conn, got); err != nil {
			return err
		}
		if !bytes.Equal(payload, got) {
			return fmt.Errorf("echo payload changed")
		}
		return nil
	}
	dialMux := func() (stdnet.Conn, error) {
		return stdnet.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", muxPort), time.Second)
	}
	dialRaw := func(user *protocol.User) stdnet.Conn {
		t.Helper()
		conn, err := dialEntry()
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = conn.Close() })
		_ = conn.SetDeadline(time.Now().Add(3 * time.Second))
		memory, err := user.ToMemoryUser()
		if err != nil {
			t.Fatal(err)
		}
		var header bytes.Buffer
		if err := encoding.EncodeRequestHeader(&header, &protocol.RequestHeader{Version: encoding.Version, User: memory, Command: protocol.RequestCommandTCP, Address: net.LocalHostIP, Port: targetPort}, &encoding.Addons{}); err != nil {
			t.Fatal(err)
		}
		if _, err := conn.Write(append(header.Bytes(), 'q')); err != nil {
			t.Fatal(err)
		}
		response := make([]byte, 3)
		if _, err := io.ReadFull(conn, response); err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(response, []byte{0, 0, 'q'}) {
			t.Fatalf("invalid VLESS response: %x", response)
		}
		return conn
	}
	aliceTCP, bobTCP := dialRaw(alice), dialRaw(bob)
	aliceMux, err := dialMux()
	if err != nil {
		t.Fatal(err)
	}
	defer aliceMux.Close()
	if err := exchange(aliceTCP); err != nil {
		t.Fatal(err)
	}
	if err := exchange(aliceMux); err != nil {
		t.Fatal(err)
	}
	beforeUsage := usage()
	if beforeUsage == 0 {
		t.Fatal("traffic was not accounted before revocation")
	}
	var transferred atomic.Int32
	failed := make(chan error, 1)
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() {
		defer close(done)
		ticker := time.NewTicker(5 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-stop:
				return
			case <-ticker.C:
				if err := exchange(bobTCP); err != nil {
					failed <- err
					return
				}
				transferred.Add(1)
			}
		}
	}()
	defer func() { close(stop); <-done }()
	waitBob := func(min int32) {
		t.Helper()
		timer := time.NewTimer(3 * time.Second)
		defer timer.Stop()
		ticker := time.NewTicker(time.Millisecond)
		defer ticker.Stop()
		for transferred.Load() < min {
			select {
			case err := <-failed:
				t.Fatalf("unrelated user's original socket disconnected: %v", err)
			case <-timer.C:
				t.Fatal("other user's flow stopped")
			case <-ticker.C:
			}
		}
	}
	waitBob(3)
	before := transferred.Load()
	ctx, cancel = context.WithTimeout(context.Background(), 3*time.Second)
	_, err = handler.AlterInbound(ctx, &command.AlterInboundRequest{Tag: "quota-entry", Operation: serial.ToTypedMessage(&command.RemoveUserOperation{Email: alice.Email})})
	cancel()
	if err != nil {
		t.Fatal(err)
	}
	for name, conn := range map[string]stdnet.Conn{"TCP": aliceTCP, "Mux": aliceMux} {
		_ = conn.SetReadDeadline(time.Now().Add(3 * time.Second))
		if _, err := conn.Read(make([]byte, 1)); err == nil {
			t.Fatalf("revoked %s carrier stayed open", name)
		} else if timeout, ok := err.(stdnet.Error); ok && timeout.Timeout() {
			t.Fatalf("revoked %s waited for a timeout instead of closing", name)
		}
	}
	waitBob(before + 3)
	afterUsage := usage()
	if afterUsage < beforeUsage {
		t.Fatal("revocation reset cumulative accounting")
	}
	denied, err := dialMux()
	if err != nil {
		t.Fatal(err)
	}
	err = exchange(denied)
	_ = denied.Close()
	if err == nil {
		t.Fatal("revoked user opened a fresh stream on old mux authorization")
	}
	ctx, cancel = context.WithTimeout(context.Background(), 3*time.Second)
	_, err = handler.AlterInbound(ctx, &command.AlterInboundRequest{Tag: "quota-entry", Operation: serial.ToTypedMessage(&command.AddUserOperation{User: alice})})
	cancel()
	if err != nil {
		t.Fatal(err)
	}
	restored := dialRaw(alice)
	if err := exchange(restored); err != nil {
		t.Fatal(err)
	}
	var restoredMuxErr error
	for attempt := 0; attempt < 5; attempt++ {
		conn, err := dialMux()
		if err == nil {
			err = exchange(conn)
			_ = conn.Close()
		}
		restoredMuxErr = err
		if err == nil {
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if restoredMuxErr != nil {
		t.Fatalf("restored mux could not reconnect: %v", restoredMuxErr)
	}
	waitBob(before + 6)
	if total := usage(); total <= afterUsage {
		t.Fatalf("restored accounting did not advance: %d <= %d", total, afterUsage)
	}
}
