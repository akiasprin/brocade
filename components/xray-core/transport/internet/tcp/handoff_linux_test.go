//go:build linux

package tcp

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
	"golang.org/x/sys/unix"
	"google.golang.org/protobuf/proto"
)

func handoffTestListener(t *testing.T) *net.TCPListener {
	t.Helper()
	var lc net.ListenConfig
	lc.SetMultipathTCP(false)
	listener, err := lc.Listen(context.Background(), "tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	l := listener.(*net.TCPListener)
	t.Cleanup(func() { l.Close() })
	return l
}

func handoffTestManifest(t *testing.T, l *net.TCPListener) handoffManifest {
	t.Helper()
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	return handoffManifest{Version: 1, GenerationID: "test-generation-0001", ControlPath: filepath.Join(dir, "control.sock"), Listeners: []handoffEndpoint{{Address: l.Addr().String(), Protocol: unix.IPPROTO_TCP}}}
}

func handoffTestDigest(t *testing.T, config *internet.SocketConfig) string {
	t.Helper()
	digest, err := handoffSocketConfigDigest(config)
	if err != nil {
		t.Fatal(err)
	}
	return digest
}

func handoffTestControl(t *testing.T, manifest handoffManifest) *tcpHandoff {
	t.Helper()
	c, err := newTCPHandoff(manifest)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.close() })
	return c
}

func handoffTestOpen(t *testing.T, c *tcpHandoff, l *net.TCPListener) (*net.TCPListener, handoffGate) {
	t.Helper()
	actual, gate, err := c.open(l.Addr().String(), false, nil, func() (net.Listener, error) { return l, nil })
	if err != nil {
		t.Fatal(err)
	}
	return actual.(*net.TCPListener), gate
}

func handoffTestRPC(t *testing.T, c *tcpHandoff, operation string, epoch uint64, intent string) (handoffResponse, []int) {
	t.Helper()
	conn, err := net.DialUnix("unixpacket", nil, &net.UnixAddr{Name: c.manifest.ControlPath, Net: "unixpacket"})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatal(err)
	}
	body, err := json.Marshal(handoffRequest{Operation: operation, GenerationID: c.manifest.GenerationID, Epoch: epoch, Intent: intent})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Write(body); err != nil {
		t.Fatal(err)
	}
	data, ancillary := make([]byte, 16384), make([]byte, unix.CmsgSpace(32*4))
	n, oobn, flags, _, err := conn.ReadMsgUnix(data, ancillary)
	if err != nil || flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
		t.Fatalf("read handoff reply: %v flags=%d", err, flags)
	}
	var result handoffResponse
	if err := json.Unmarshal(data[:n], &result); err != nil {
		t.Fatal(err)
	}
	var fds []int
	if oobn > 0 {
		messages, err := unix.ParseSocketControlMessage(ancillary[:oobn])
		if err != nil {
			t.Fatal(err)
		}
		for _, message := range messages {
			rights, err := unix.ParseUnixRights(&message)
			if err != nil {
				t.Fatal(err)
			}
			fds = append(fds, rights...)
		}
	}
	return result, fds
}

func requireHandoffRPC(t *testing.T, c *tcpHandoff, operation string, epoch uint64) handoffStatus {
	t.Helper()
	response, fds := handoffTestRPC(t, c, operation, epoch, fmt.Sprintf("test-intent-%020d", epoch))
	for _, fd := range fds {
		unix.Close(fd)
	}
	if response.Error != "" || response.Status == nil {
		t.Fatalf("%s: %+v", operation, response)
	}
	return *response.Status
}

func TestHandoffSmallDeploymentFencingIntents(t *testing.T) {
	listener := handoffTestListener(t)
	c := handoffTestControl(t, handoffTestManifest(t, listener))
	handoffTestOpen(t, c, listener)
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	for index, operation := range []string{"seal", "resume", "pause"} {
		response, fds := handoffTestRPC(t, c, operation, uint64(index+1), "config-2-1-"+operation)
		if response.Error != "" || response.Status == nil || len(fds) != 0 {
			t.Fatalf("small deployment %s rejected: %+v fds=%v", operation, response, fds)
		}
	}
	for _, request := range []handoffRequest{
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 2, Intent: "config-2-1-resume"},
		{Operation: "pause", GenerationID: c.manifest.GenerationID, Epoch: 3, Intent: "config-2-2-pause"},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 3, Intent: "config-2-1-pause"},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 4},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 4, Intent: "bad intent"},
	} {
		if _, _, err := c.execute(request); err == nil {
			t.Fatalf("accepted invalid or conflicting fence: %+v", request)
		}
	}
	manifest := c.manifest
	manifest.GenerationID = "short"
	if err := validateHandoffManifest(manifest); err == nil {
		t.Fatal("short operation intents must not relax generation identity validation")
	}
}

func serveHandoffEcho(t *testing.T, listener *net.TCPListener, gate handoffGate, prefix byte) {
	t.Helper()
	var handlers sync.WaitGroup
	done := make(chan struct{})
	go func() {
		defer close(done)
		for gate.beforeAccept() {
			conn, err := listener.AcceptTCP()
			gate.afterAccept(err == nil)
			if err != nil {
				if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
					continue
				}
				return
			}
			if err := gate.prepareConnection(conn); err != nil {
				gate.finishConnection()
				conn.Close()
				return
			}
			gate.trackConnection(conn)
			handlers.Add(1)
			go func() {
				defer handlers.Done()
				defer FinishHandoffConnection(conn)
				defer conn.Close()
				conn.SetDeadline(time.Now().Add(10 * time.Second))
				buffer := make([]byte, 1)
				for {
					if _, err := io.ReadFull(conn, buffer); err != nil {
						return
					}
					if _, err := conn.Write([]byte{prefix, buffer[0]}); err != nil {
						return
					}
				}
			}()
		}
	}()
	t.Cleanup(func() {
		gate.close()
		listener.Close()
		select {
		case <-done:
		case <-time.After(2 * time.Second):
			t.Error("listener admission did not stop")
		}
		handlers.Wait()
	})
}

func handoffTestDial(t *testing.T, address string) *net.TCPConn {
	t.Helper()
	conn, err := net.DialTimeout("tcp", address, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	return conn.(*net.TCPConn)
}

func handoffTestExchange(t *testing.T, conn *net.TCPConn, prefix, value byte) {
	t.Helper()
	conn.SetDeadline(time.Now().Add(2 * time.Second))
	if _, err := conn.Write([]byte{value}); err != nil {
		t.Fatal(err)
	}
	buffer := make([]byte, 2)
	if _, err := io.ReadFull(conn, buffer); err != nil {
		t.Fatal(err)
	}
	if buffer[0] != prefix || buffer[1] != value {
		t.Fatalf("unexpected echo: %x", buffer)
	}
}

func TestHandoffPauseExportImportPreservesAcceptedConnection(t *testing.T) {
	original := handoffTestListener(t)
	source := handoffTestControl(t, handoffTestManifest(t, original))
	oldListener, oldGate := handoffTestOpen(t, source, original)
	serveHandoffEcho(t, oldListener, oldGate, 'o')
	if err := source.markReady(); err != nil {
		t.Fatal(err)
	}
	status := requireHandoffRPC(t, source, "status", 0)
	if !status.Paused || status.Accepting || status.Scope != "selected_tcp_listeners" || status.RetirementSupported {
		t.Fatalf("unsafe initial state: %+v", status)
	}
	queued := handoffTestDial(t, original.Addr().String())
	queued.SetReadDeadline(time.Now().Add(30 * time.Millisecond))
	if _, err := queued.Read(make([]byte, 1)); err == nil {
		t.Fatal("paused listener unexpectedly returned data")
	}
	requireHandoffRPC(t, source, "resume", 1)
	handoffTestExchange(t, queued, 'o', '1')
	paused := requireHandoffRPC(t, source, "pause", 2)
	if !paused.Paused || paused.Listeners[0].InAccept {
		t.Fatalf("pause barrier did not complete: %+v", paused)
	}
	handoffTestExchange(t, queued, 'o', '2')
	exported, fds := handoffTestRPC(t, source, "export", 3, "export-intent-0003")
	if exported.Error != "" || exported.Status == nil || len(fds) != 1 {
		t.Fatalf("export failed: %+v fds=%v", exported, fds)
	}
	identity := exported.Status.Listeners[0].handoffSocketIdentity
	manifest := handoffTestManifest(t, original)
	manifest.GenerationID = "test-generation-0002"
	manifest.Listeners[0] = handoffEndpoint{Address: identity.Address, FD: fds[0], SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie, Protocol: identity.Protocol, SocketConfigSHA256: exported.Status.Listeners[0].SocketConfigSHA256}
	candidate := handoffTestControl(t, manifest)
	listener, gate, err := candidate.open(identity.Address, false, nil, func() (net.Listener, error) {
		t.Fatal("import must never bind a replacement socket")
		return nil, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	serveHandoffEcho(t, listener.(*net.TCPListener), gate, 'n')
	if err := candidate.markReady(); err != nil {
		t.Fatal(err)
	}
	if candidate.status().Listeners[0].handoffSocketIdentity != identity {
		t.Fatal("import changed kernel socket identity")
	}
	requireHandoffRPC(t, candidate, "resume", 1)
	newConn := handoffTestDial(t, identity.Address)
	handoffTestExchange(t, newConn, 'n', '3')
	for i := 0; i < 25; i++ {
		handoffTestExchange(t, queued, 'o', byte(i))
		handoffTestExchange(t, newConn, 'n', byte(i))
	}
	// Closing admission/control is not closing a connection. Keep both process
	// generations alive in the real deployment until independently drained.
	if err := source.close(); err != nil {
		t.Fatal(err)
	}
	handoffTestExchange(t, queued, 'o', '4')
	newConn.Close()
	queued.Close()
}

func TestHandoffExportKeepsRealityCoverAdmissionLive(t *testing.T) {
	public := handoffTestListener(t)
	cover := handoffTestListener(t)
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "cover-export-generation-0001",
		ControlPath:  filepath.Join(dir, "control.sock"),
		Listeners: []handoffEndpoint{
			{Address: public.Addr().String(), BindAddress: public.Addr().String(), Network: "tcp", Role: "public", Protocol: unix.IPPROTO_TCP},
			{Address: cover.Addr().String(), BindAddress: cover.Addr().String(), Network: "tcp", Role: "cover", Protocol: unix.IPPROTO_TCP},
		},
	}
	control := handoffTestControl(t, manifest)
	publicListener, publicGate := handoffTestOpen(t, control, public)
	coverListener, coverGate := handoffTestOpen(t, control, cover)
	serveHandoffEcho(t, publicListener, publicGate, 'p')
	serveHandoffEcho(t, coverListener, coverGate, 'c')
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	requireHandoffRPC(t, control, "seal", 1)
	requireHandoffRPC(t, control, "resume", 2)

	paused := requireHandoffRPC(t, control, "pause", 3)
	if !paused.Paused || paused.Accepting {
		t.Fatalf("public pause barrier did not complete: %+v", paused)
	}
	coverInAccept := false
	for _, listener := range paused.Listeners {
		if listener.Role == "cover" {
			coverInAccept = listener.InAccept
		}
	}
	if !coverInAccept {
		t.Fatalf("test did not keep REALITY cover admission live: %+v", paused)
	}
	coverConn := handoffTestDial(t, cover.Addr().String())
	handoffTestExchange(t, coverConn, 'c', '1')

	exported, fds := handoffTestRPC(t, control, "export", 4, "cover-export-intent-0004")
	defer func() {
		for _, fd := range fds {
			unix.Close(fd)
		}
	}()
	if exported.Error != "" || exported.Status == nil || len(fds) != 2 {
		t.Fatalf("export rejected live REALITY cover admission: %+v fds=%v", exported, fds)
	}
}

func TestWholeGenerationIPv4WildcardPreservesLogicalAddressAcrossDualStackImport(t *testing.T) {
	var lc net.ListenConfig
	listener, err := lc.Listen(context.Background(), "tcp", "[::]:0")
	if err != nil {
		t.Skipf("dual-stack listener unavailable: %v", err)
	}
	original := listener.(*net.TCPListener)
	defer original.Close()
	actual, err := inspectHandoffListener(original)
	if err != nil {
		t.Fatal(err)
	}
	actualAddress, err := net.ResolveTCPAddr("tcp", actual.Address)
	if err != nil || actualAddress.IP.To4() != nil {
		t.Skipf("kernel did not create an IPv6 wildcard listener: address=%s err=%v", actual.Address, err)
	}
	logical := (&net.TCPAddr{IP: net.IPv4zero, Port: actualAddress.Port}).String()
	if !handoffListenerMatchesRequestedAddress(original, actual.Address, logical) {
		t.Fatal("default IPv6 wildcard listener did not satisfy the IPv4 wildcard manifest")
	}
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "dual-stack-source-0001",
		ControlPath:  filepath.Join(dir, "source.sock"),
		Listeners: []handoffEndpoint{{
			Address: logical, BindAddress: logical, Network: "tcp", Role: "public",
		}},
	}
	source := handoffTestControl(t, manifest)
	opened, _, err := source.open(logical, false, nil, func() (net.Listener, error) { return original, nil })
	if err != nil {
		t.Fatal(err)
	}
	if opened != original {
		t.Fatal("bootstrap replaced the listener")
	}
	if err := source.markReady(); err != nil {
		t.Fatal(err)
	}
	requireHandoffRPC(t, source, "seal", 1)
	status := source.status()
	if len(status.Listeners) != 1 || status.Listeners[0].Address != logical || status.Listeners[0].BindAddress != actual.Address {
		t.Fatalf("logical and kernel bind addresses were not preserved separately: %+v", status.Listeners)
	}
	exported, fds := handoffTestRPC(t, source, "export", 2, "dual-stack-export-0002")
	if exported.Error != "" || exported.Status == nil || len(fds) != 1 {
		t.Fatalf("export failed: %+v fds=%v", exported, fds)
	}
	defer unix.Close(fds[0])
	if err := source.close(); err != nil {
		t.Fatal(err)
	}
	exportedListener := exported.Status.Listeners[0]
	manifest.GenerationID = "dual-stack-candidate-0002"
	manifest.ControlPath = filepath.Join(dir, "candidate.sock")
	manifest.Listeners[0] = handoffEndpoint{
		Address: logical, BindAddress: exportedListener.BindAddress, Network: "tcp", Role: "public",
		FD: fds[0], SocketInode: exportedListener.SocketInode, SocketCookie: exportedListener.SocketCookie,
		Protocol: exportedListener.Protocol, SocketConfigSHA256: exportedListener.SocketConfigSHA256,
	}
	candidate := handoffTestControl(t, manifest)
	imported, _, err := candidate.open(logical, false, nil, func() (net.Listener, error) {
		t.Fatal("inherited dual-stack listener must not bind")
		return nil, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	defer imported.Close()
	if got := candidate.status().Listeners[0]; got.Address != logical || got.BindAddress != actual.Address {
		t.Fatalf("import lost logical or kernel bind address: %+v", got)
	}
}

func TestWholeGenerationDropsUDPRatherThanDrainingIt(t *testing.T) {
	tcpListener := handoffTestListener(t)
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	reserved, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.ParseIP("127.0.0.1")})
	if err != nil {
		t.Fatal(err)
	}
	udpAddress := reserved.LocalAddr().(*net.UDPAddr)
	logicalUDP := udpAddress.String()
	reserved.Close()
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "whole-generation-0001",
		ControlPath:  filepath.Join(dir, "control.sock"),
		Listeners: []handoffEndpoint{
			{Address: tcpListener.Addr().String(), BindAddress: tcpListener.Addr().String(), Network: "tcp", Role: "public", Protocol: unix.IPPROTO_TCP},
			{Address: logicalUDP, BindAddress: logicalUDP, Network: "udp", Role: "public", Protocol: unix.IPPROTO_UDP},
		},
	}
	control := handoffTestControl(t, manifest)
	handoffTestOpen(t, control, tcpListener)
	actualPacket, gate, err := control.openPacket(logicalUDP, nil, func(address string) (net.PacketConn, error) {
		if address != logicalUDP {
			t.Fatalf("bound UDP address %q, want the public address %q", address, logicalUDP)
		}
		return net.ListenUDP("udp4", udpAddress)
	})
	if err != nil || actualPacket == nil || gate == nil {
		t.Fatalf("open UDP generation listener: conn=%v gate=%v err=%v", actualPacket, gate, err)
	}
	t.Cleanup(func() { actualPacket.Close() })
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := control.seal(); err != nil {
		t.Fatal(err)
	}
	initial := control.status()
	if !initial.RetirementSupported || initial.Scope != "whole-generation" || initial.RetireReady {
		t.Fatalf("whole-generation capability was not certified: %+v", initial)
	}
	if initial.Listeners[1].BindAddress != logicalUDP {
		t.Fatalf("status did not preserve the public UDP bind: %+v", initial.Listeners[1])
	}
	if err := control.resume(); err != nil {
		t.Fatal(err)
	}
	acceptCtx, proceed := gate.BeforeAccept(context.Background())
	if !proceed {
		t.Fatal("UDP admission did not resume")
	}
	acceptDone := make(chan struct{})
	go func() {
		<-acceptCtx.Done()
		gate.AfterAccept(false)
		close(acceptDone)
	}()
	pauseCtx, cancelPause := context.WithTimeout(context.Background(), time.Second)
	defer cancelPause()
	if err := control.pause(pauseCtx); err != nil {
		t.Fatal(err)
	}
	<-acceptDone

	if err := control.resume(); err != nil {
		t.Fatal(err)
	}
	_, proceed = gate.BeforeAccept(context.Background())
	if !proceed {
		t.Fatal("UDP admission did not resume for accepted QUIC connection")
	}
	gate.AfterAccept(true)
	if err := control.pause(pauseCtx); err != nil {
		t.Fatal(err)
	}
	if err := control.drain(pauseCtx); err == nil {
		t.Fatal("generation retired before reconnect-only UDP was dropped")
	}
	response, _, err := control.execute(handoffRequest{
		Operation: "drop_udp", GenerationID: manifest.GenerationID, Epoch: 1, Intent: "drop-udp-reconnect",
	})
	if err != nil {
		t.Fatal(err)
	}
	if response.Status == nil || !response.Status.UDPDropped {
		t.Fatalf("drop_udp did not report a completed UDP reset: %+v", response.Status)
	}
	if gate.StartConnection() {
		t.Fatal("dropped UDP listener admitted another connection")
	}
	if err := control.drain(pauseCtx); err != nil {
		t.Fatal(err)
	}
	retired := control.status()
	if !retired.RetirementSupported || !retired.UDPDropped || !retired.RetireReady || retired.PublicActive != 0 || !retired.DrainRequested {
		t.Fatalf("TCP generation did not become retire-ready after UDP reset: %+v", retired)
	}
	gate.FinishConnection()
}

func TestWholeGenerationUDPIPv4WildcardUsesDirectPublicBind(t *testing.T) {
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	reserved, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4zero})
	if err != nil {
		t.Fatal(err)
	}
	logical := fmt.Sprintf("0.0.0.0:%d", reserved.LocalAddr().(*net.UDPAddr).Port)
	reserved.Close()
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "udp-dual-stack-0001",
		ControlPath:  filepath.Join(dir, "control.sock"),
		Listeners: []handoffEndpoint{{
			Address: logical, BindAddress: logical, Network: "udp", Role: "public", Protocol: unix.IPPROTO_UDP,
		}},
	}
	control := handoffTestControl(t, manifest)
	packet, _, err := control.openPacket(logical, nil, func(address string) (net.PacketConn, error) {
		resolved, err := net.ResolveUDPAddr("udp", address)
		if err != nil {
			return nil, err
		}
		var lc net.ListenConfig
		return lc.ListenPacket(context.Background(), resolved.Network(), resolved.String())
	})
	if err != nil {
		t.Fatal(err)
	}
	defer packet.Close()
	actual := packet.LocalAddr().String()
	actualAddress, err := net.ResolveUDPAddr("udp", actual)
	if err != nil {
		t.Fatal(err)
	}
	if actualAddress.IP.To4() == nil && !handoffPacketMatchesRequestedAddress(packet.(*net.UDPConn), actual, logical) {
		t.Fatalf("dual-stack UDP wildcard did not satisfy the IPv4 wildcard manifest: %s", actual)
	}
	status := control.status()
	if len(status.Listeners) != 1 || status.Listeners[0].Address != logical || status.Listeners[0].BindAddress != actual {
		t.Fatalf("logical and kernel UDP bind addresses were not preserved separately: %+v", status.Listeners)
	}
}

func TestHandoffPacketAddressMatchRejectsNonEquivalentSocket(t *testing.T) {
	conn, err := net.ListenUDP("udp6", &net.UDPAddr{IP: net.IPv6zero})
	if err != nil {
		t.Skipf("IPv6 UDP unavailable: %v", err)
	}
	defer conn.Close()
	actual := conn.LocalAddr().String()
	if handoffPacketMatchesRequestedAddress(conn, actual, "0.0.0.0:0") {
		t.Fatal("IPv6-only UDP wildcard satisfied an IPv4 wildcard manifest")
	}
	actualAddress := conn.LocalAddr().(*net.UDPAddr)
	wrongPort := (&net.UDPAddr{IP: actualAddress.IP, Port: actualAddress.Port + 1}).String()
	if handoffPacketMatchesRequestedAddress(conn, actual, wrongPort) {
		t.Fatal("UDP listener with a different fixed port satisfied the manifest")
	}
}

func TestWholeGenerationRetirementRejectsUnregisteredListener(t *testing.T) {
	listener := handoffTestListener(t)
	manifest := handoffTestManifest(t, listener)
	manifest.Version = 2
	manifest.Scope = "whole-generation"
	manifest.Listeners[0] = handoffEndpoint{Address: listener.Addr().String(), BindAddress: listener.Addr().String(), Network: "tcp", Role: "public", Protocol: unix.IPPROTO_TCP}
	control := handoffTestControl(t, manifest)
	handoffTestOpen(t, control, listener)
	untracked, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer untracked.Close()
	if _, _, err := control.open(untracked.Addr().String(), false, nil, func() (net.Listener, error) { return untracked, nil }); err != nil {
		t.Fatal(err)
	}
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	status := control.status()
	if status.RetirementSupported || len(status.UnsupportedReasons) != 1 || status.UnsupportedReasons[0] != "unregistered_tcp_listener" {
		t.Fatalf("unregistered listener did not fail retirement closed: %+v", status)
	}
}

func TestHandoffFencingAndScope(t *testing.T) {
	l := handoffTestListener(t)
	c := handoffTestControl(t, handoffTestManifest(t, l))
	handoffTestOpen(t, c, l)
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	requireHandoffRPC(t, c, "resume", 10)
	requireHandoffRPC(t, c, "pause", 11)
	for _, request := range []handoffRequest{
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 10, Intent: "test-intent-00000000000000000010"},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 11, Intent: "test-intent-00000000000000000011"},
		{Operation: "pause", GenerationID: c.manifest.GenerationID, Epoch: 11, Intent: "different-intent-11"},
		{Operation: "resume", GenerationID: "wrong-generation-0001", Epoch: 12, Intent: "test-intent-00012"},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 0, Intent: "test-intent-00012"},
		{Operation: "resume", GenerationID: c.manifest.GenerationID, Epoch: 12, Intent: "bad intent"},
		{Operation: "retire", GenerationID: c.manifest.GenerationID, Epoch: 12, Intent: "test-intent-00012"},
	} {
		if _, _, err := c.execute(request); err == nil {
			t.Fatalf("unsafe request accepted: %+v", request)
		}
	}
	requireHandoffRPC(t, c, "pause", 11)
	if s := c.status(); !s.Paused || s.Epoch != 11 {
		t.Fatalf("stale request changed admission: %+v", s)
	}
	requireHandoffRPC(t, c, "resume", 12)
	if _, _, err := c.execute(handoffRequest{Operation: "export", GenerationID: c.manifest.GenerationID, Epoch: 13, Intent: "export-intent-0013"}); err == nil {
		t.Fatal("export while accepting must fail")
	}
}

func TestHandoffIncompleteAndUnsupportedGroupFailClosed(t *testing.T) {
	l := handoffTestListener(t)
	manifest := handoffTestManifest(t, l)
	c := handoffTestControl(t, manifest)
	if err := c.markReady(); err == nil {
		t.Fatal("incomplete group became ready")
	}
	bound := false
	bind := func() (net.Listener, error) { bound = true; return l, nil }
	if _, _, err := c.open(l.Addr().String(), true, nil, bind); err == nil || bound {
		t.Fatal("wrapped listener must fail before bind")
	}
	handoffTestOpen(t, c, l)
	if _, _, err := c.open(l.Addr().String(), false, nil, bind); err == nil || bound {
		t.Fatal("duplicate listener accepted")
	}
	response, _ := handoffTestRPC(t, c, "resume", 1, "resume-intent-0001")
	if response.Error == "" || !c.status().Paused {
		t.Fatal("incomplete generation resumed")
	}
}

func TestWholeGenerationPreparationRestoresDynamicListenerBeforeSeal(t *testing.T) {
	api := handoffTestListener(t)
	dynamic := handoffTestListener(t)
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "prepared-generation-0001",
		ControlPath:  filepath.Join(dir, "control.sock"),
		Listeners: []handoffEndpoint{
			{Address: api.Addr().String(), Network: "tcp", Role: "control", Prepare: true, Protocol: 0},
			{Address: dynamic.Addr().String(), Network: "tcp", Role: "public", Protocol: 0},
		},
	}
	control := handoffTestControl(t, manifest)
	apiListener, apiGate := handoffTestOpen(t, control, api)
	serveHandoffEcho(t, apiListener, apiGate, 'a')
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	if status := control.status(); status.Ready || status.Accepting || status.RetirementSupported {
		t.Fatalf("incomplete generation was certified: %+v", status)
	}
	apiClient := handoffTestDial(t, api.Addr().String())
	handoffTestExchange(t, apiClient, 'a', '1')
	if status := control.status(); status.Accepting || status.Active != 0 || status.PublicActive != 0 {
		t.Fatalf("preparation control traffic was reported as public admission: %+v", status)
	}

	dynamicListener, dynamicGate := handoffTestOpen(t, control, dynamic)
	serveHandoffEcho(t, dynamicListener, dynamicGate, 'd')
	if _, _, err := control.execute(handoffRequest{Operation: "resume", GenerationID: manifest.GenerationID, Epoch: 1, Intent: "prepare-resume-0000000001"}); err == nil {
		t.Fatal("unsealed complete generation resumed")
	}
	sealed, _, err := control.execute(handoffRequest{Operation: "seal", GenerationID: manifest.GenerationID, Epoch: 2, Intent: "prepare-seal-000000000002"})
	if err != nil || sealed.Status == nil || !sealed.Status.Ready || !sealed.Status.RetirementSupported || !sealed.Status.Paused {
		t.Fatalf("seal did not certify complete generation: status=%+v response_error=%q err=%v", sealed.Status, sealed.Error, err)
	}
	resumed, _, err := control.execute(handoffRequest{Operation: "resume", GenerationID: manifest.GenerationID, Epoch: 3, Intent: "prepare-resume-0000000003"})
	if err != nil || resumed.Status == nil || !resumed.Status.Accepting {
		t.Fatalf("sealed generation did not resume: response=%+v err=%v", resumed, err)
	}
	dynamicClient := handoffTestDial(t, dynamic.Addr().String())
	handoffTestExchange(t, dynamicClient, 'd', '2')
}

func TestWholeGenerationControlConnectionDoesNotBlockBusinessRetirement(t *testing.T) {
	api := handoffTestListener(t)
	public := handoffTestListener(t)
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := handoffManifest{
		Version:      2,
		Scope:        "whole-generation",
		GenerationID: "control-retirement-0001",
		ControlPath:  filepath.Join(dir, "control.sock"),
		Listeners: []handoffEndpoint{
			{Address: api.Addr().String(), Network: "tcp", Role: "control", Prepare: true, Protocol: 0},
			{Address: public.Addr().String(), Network: "tcp", Role: "public", Protocol: 0},
		},
	}
	control := handoffTestControl(t, manifest)
	_, apiGate := handoffTestOpen(t, control, api)
	handoffTestOpen(t, control, public)
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := control.seal(); err != nil {
		t.Fatal(err)
	}
	if err := control.resume(); err != nil {
		t.Fatal(err)
	}
	if !apiGate.beforeAccept() {
		t.Fatal("control listener did not enter accept")
	}
	apiGate.afterAccept(true)
	if status := control.status(); status.Active != 0 || status.PublicActive != 0 {
		t.Fatalf("control connection entered business totals: %+v", status)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := control.pause(ctx); err != nil {
		t.Fatal(err)
	}
	if err := control.drain(ctx); err != nil {
		t.Fatalf("control connection blocked business retirement: %v", err)
	}
	if !control.status().RetireReady {
		t.Fatal("business generation was not retire-ready")
	}
	apiGate.finishConnection()
}

func TestPreparationAdmissionRejectsNonControlAndNonLoopback(t *testing.T) {
	l := handoffTestListener(t)
	manifest := handoffTestManifest(t, l)
	manifest.Version = 2
	manifest.Scope = "whole-generation"
	manifest.Listeners[0] = handoffEndpoint{Address: l.Addr().String(), Network: "tcp", Role: "public", Prepare: true, Protocol: 0}
	if _, err := newTCPHandoff(manifest); err == nil {
		t.Fatal("public preparation listener accepted")
	}
	manifest.Listeners[0] = handoffEndpoint{Address: "0.0.0.0:12345", Network: "tcp", Role: "control", Prepare: true, Protocol: 0}
	if _, err := newTCPHandoff(manifest); err == nil {
		t.Fatal("non-loopback preparation control accepted")
	}
}

func TestHandoffManifestPermissionsAndBoundedJSON(t *testing.T) {
	l := handoffTestListener(t)
	manifest := handoffTestManifest(t, l)
	path := filepath.Join(filepath.Dir(manifest.ControlPath), "manifest.json")
	body, _ := json.Marshal(manifest)
	if err := os.WriteFile(path, body, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := readHandoffManifest(path); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0400); err != nil {
		t.Fatal(err)
	}
	parent := filepath.Dir(path)
	defer os.Chmod(parent, 0700)
	if err := os.Chmod(parent, 0500); err != nil {
		t.Fatal(err)
	}
	if _, err := readHandoffManifest(path); err != nil {
		t.Fatalf("private read-only manifest must remain readable: %v", err)
	}
	if err := os.Chmod(parent, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0640); err != nil {
		t.Fatal(err)
	}
	if _, err := readHandoffManifest(path); err == nil {
		t.Fatal("readable-by-group manifest accepted")
	}
	os.Chmod(path, 0600)
	link := filepath.Join(filepath.Dir(path), "symlink.json")
	if err := os.Symlink(path, link); err != nil {
		t.Fatal(err)
	}
	if _, err := readHandoffManifest(link); err == nil {
		t.Fatal("symlink manifest accepted")
	}
	if err := os.Link(path, filepath.Join(filepath.Dir(path), "hardlink.json")); err != nil {
		t.Fatal(err)
	}
	if _, err := readHandoffManifest(path); err == nil {
		t.Fatal("hardlinked manifest accepted")
	}
	if err := os.Chmod(filepath.Dir(path), 0750); err != nil {
		t.Fatal(err)
	}
	if err := validateHandoffManifest(manifest); err == nil {
		t.Fatal("nonprivate control directory accepted")
	}
	var extended handoffRequest
	if err := decodeHandoffJSON([]byte(`{"operation":"status","future_field":true}`), &extended); err != nil {
		t.Fatalf("additional fields rejected: %v", err)
	}
	if extended.Operation != "status" {
		t.Fatal("additional field changed the operation")
	}
	for _, raw := range []string{`{"operation":1}`, `{} {}`, strings.Repeat(" ", handoffMaxMessage+1)} {
		var request handoffRequest
		if err := decodeHandoffJSON([]byte(raw), &request); err == nil {
			t.Fatalf("invalid JSON accepted: %.20q", raw)
		}
	}
}

func TestHandoffManifestRejectsWrongSocketIdentity(t *testing.T) {
	l := handoffTestListener(t)
	id, err := inspectHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	fd, err := duplicateHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	defer unix.Close(fd)
	base := handoffTestManifest(t, l)
	base.Listeners[0] = handoffEndpoint{Address: id.Address, FD: fd, SocketInode: id.SocketInode, SocketCookie: id.SocketCookie, Protocol: id.Protocol, SocketConfigSHA256: handoffTestDigest(t, nil)}
	if err := validateHandoffManifest(base); err != nil {
		t.Fatal(err)
	}
	for _, change := range []func(*handoffEndpoint){
		func(e *handoffEndpoint) { e.SocketCookie++ },
		func(e *handoffEndpoint) { e.SocketInode++ },
		func(e *handoffEndpoint) { e.SocketCookie = 0 },
		func(e *handoffEndpoint) { e.SocketInode = 0 },
		func(e *handoffEndpoint) { e.FD = 1 },
		func(e *handoffEndpoint) { e.Protocol = unix.IPPROTO_UDP },
		func(e *handoffEndpoint) { e.Address = "127.0.0.2:12345" },
		func(e *handoffEndpoint) { e.SocketConfigSHA256 = "" },
		func(e *handoffEndpoint) { e.SocketConfigSHA256 = "not-a-digest" },
		func(e *handoffEndpoint) { e.SocketConfigSHA256 = strings.ToUpper(e.SocketConfigSHA256) },
	} {
		manifest := base
		manifest.Listeners = append([]handoffEndpoint(nil), base.Listeners...)
		change(&manifest.Listeners[0])
		if err := validateHandoffManifest(manifest); err == nil {
			t.Fatalf("bad identity accepted: %+v", manifest.Listeners[0])
		}
	}
	manifest := base
	manifest.Listeners = append(append([]handoffEndpoint(nil), base.Listeners...), base.Listeners[0])
	if err := validateHandoffManifest(manifest); err == nil {
		t.Fatal("duplicate FD accepted")
	}
	if err := unix.SetNonblock(fd, false); err != nil {
		t.Fatal(err)
	}
	if _, err := inspectHandoffFD(fd); err == nil {
		t.Fatal("blocking FD accepted")
	}
	if err := unix.SetNonblock(fd, true); err != nil {
		t.Fatal(err)
	}
	if _, err := inspectHandoffListener(l); err != nil {
		t.Fatal("duplicate changed source flags", err)
	}
}

func TestHandoffWrongInheritedFDNeverBindsFallback(t *testing.T) {
	l := handoffTestListener(t)
	id, _ := inspectHandoffListener(l)
	fd, err := duplicateHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	manifest := handoffTestManifest(t, l)
	manifest.Listeners[0] = handoffEndpoint{Address: id.Address, FD: fd, SocketInode: id.SocketInode, SocketCookie: id.SocketCookie, Protocol: id.Protocol, SocketConfigSHA256: handoffTestDigest(t, nil)}
	c := handoffTestControl(t, manifest)
	if err := unix.Close(fd); err != nil {
		t.Fatal(err)
	}
	bound := false
	if _, _, err := c.open(id.Address, false, nil, func() (net.Listener, error) { bound = true; return l, nil }); err == nil || bound {
		t.Fatal("invalid imported FD fell back to bind")
	}
}

func TestHandoffConcurrentFencingNeverRevivesOldIntent(t *testing.T) {
	l := handoffTestListener(t)
	c := handoffTestControl(t, handoffTestManifest(t, l))
	handoffTestOpen(t, c, l)
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	for epoch := uint64(1); epoch <= 100; epoch++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			operation := "resume"
			if epoch == 100 {
				operation = "pause"
			}
			c.execute(handoffRequest{Operation: operation, GenerationID: c.manifest.GenerationID, Epoch: epoch, Intent: fmt.Sprintf("concurrent-intent-%020d", epoch)})
		}()
	}
	wg.Wait()
	if s := c.status(); s.Epoch != 100 || !s.Paused || s.Operation != "pause" {
		t.Fatalf("stale mutation superseded newest pause: %+v", s)
	}
}

func TestHandoffPauseCancellationKeepsAdmissionClosed(t *testing.T) {
	l := handoffTestListener(t)
	c := handoffTestControl(t, handoffTestManifest(t, l))
	_, gate := handoffTestOpen(t, c, l)
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
	if !gate.beforeAccept() {
		t.Fatal("not accepting")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := c.pause(ctx); err != context.Canceled {
		t.Fatalf("cancelled pause=%v", err)
	}
	if s := c.status(); !s.PauseRequested || s.Paused || !s.Accepting {
		t.Fatalf("cancelled pause falsely reported a completed barrier: %+v", s)
	}
	gate.afterAccept(false)
	if s := c.status(); !s.PauseRequested || !s.Paused || s.Accepting {
		t.Fatalf("completed pause barrier has wrong status: %+v", s)
	}
}

func TestHandoffImportedKeepAliveMatchesSocket(t *testing.T) {
	l := handoffTestListener(t)
	raw, err := l.SyscallConn()
	if err != nil {
		t.Fatal(err)
	}
	var optionErr error
	raw.Control(func(fd uintptr) { optionErr = unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_KEEPALIVE, 0) })
	if optionErr != nil {
		t.Fatal(optionErr)
	}
	id, _ := inspectHandoffListener(l)
	fd, err := duplicateHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	manifest := handoffTestManifest(t, l)
	manifest.Listeners[0] = handoffEndpoint{Address: id.Address, FD: fd, SocketInode: id.SocketInode, SocketCookie: id.SocketCookie, Protocol: id.Protocol, SocketConfigSHA256: handoffTestDigest(t, nil)}
	c := handoffTestControl(t, manifest)
	listener, gate, err := c.open(id.Address, false, nil, func() (net.Listener, error) { t.Fatal("unexpected bind"); return nil, nil })
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	client := handoffTestDial(t, id.Address)
	defer client.Close()
	conn, err := listener.(*net.TCPListener).AcceptTCP()
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := gate.prepareConnection(conn); err != nil {
		t.Fatal(err)
	}
	actual := -1
	connRaw, _ := conn.SyscallConn()
	connRaw.Control(func(fd uintptr) { actual, optionErr = unix.GetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_KEEPALIVE) })
	if optionErr != nil || actual != 0 {
		t.Fatalf("import enabled keepalive: value=%d err=%v", actual, optionErr)
	}
}

func TestHandoffExplicitKeepAliveMatchesNormalAccept(t *testing.T) {
	for _, testCase := range []struct {
		name    string
		options *internet.SocketConfig
	}{
		{name: "unspecified"},
		{name: "zero", options: &internet.SocketConfig{}},
		{name: "disabled", options: &internet.SocketConfig{TcpKeepAliveIdle: -1, TcpKeepAliveInterval: -1}},
		{name: "idle_only", options: &internet.SocketConfig{TcpKeepAliveIdle: 41}},
		{name: "interval_only", options: &internet.SocketConfig{TcpKeepAliveInterval: 17}},
		{name: "idle_and_interval", options: &internet.SocketConfig{TcpKeepAliveIdle: 41, TcpKeepAliveInterval: 17}},
		{name: "brocade_inbound", options: &internet.SocketConfig{
			TcpKeepAliveIdle: 300, TcpKeepAliveInterval: 90,
			CustomSockopt: []*internet.CustomSockopt{{System: "linux", Network: "tcp", Level: "6", Opt: "6", Type: "int", Value: "3"}},
		}},
	} {
		t.Run(testCase.name, func(t *testing.T) {
			ordinary, err := internet.ListenSystem(context.Background(), &net.TCPAddr{IP: net.ParseIP("127.0.0.1")}, testCase.options)
			if err != nil {
				t.Fatal(err)
			}
			defer ordinary.Close()
			listener := ordinary.(*net.TCPListener)
			identity, err := inspectHandoffListener(listener)
			if err != nil {
				t.Fatal(err)
			}
			listenerConfig, err := handoffKeepAlive(listener)
			if err != nil {
				t.Fatal(err)
			}
			ordinaryClient := handoffTestDial(t, identity.Address)
			defer ordinaryClient.Close()
			ordinaryAccepted, err := listener.AcceptTCP()
			if err != nil {
				t.Fatal(err)
			}
			defer ordinaryAccepted.Close()
			readAccepted := func(conn *net.TCPConn) [4]int {
				t.Helper()
				var values [4]int
				raw, err := conn.SyscallConn()
				if err != nil {
					t.Fatal(err)
				}
				var optionErr error
				err = raw.Control(func(fd uintptr) {
					for index, option := range []struct{ level, option int }{{unix.SOL_SOCKET, unix.SO_KEEPALIVE}, {unix.IPPROTO_TCP, unix.TCP_KEEPIDLE}, {unix.IPPROTO_TCP, unix.TCP_KEEPINTVL}, {unix.IPPROTO_TCP, unix.TCP_KEEPCNT}} {
						values[index], optionErr = unix.GetsockoptInt(int(fd), option.level, option.option)
						if optionErr != nil {
							return
						}
					}
				})
				if err != nil || optionErr != nil {
					t.Fatalf("read accepted socket options: raw=%v option=%v", err, optionErr)
				}
				return values
			}
			normal := readAccepted(ordinaryAccepted)
			fd, err := duplicateHandoffListener(listener)
			if err != nil {
				t.Fatal(err)
			}
			manifest := handoffTestManifest(t, listener)
			manifest.Listeners[0] = handoffEndpoint{Address: identity.Address, FD: fd, SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie, Protocol: identity.Protocol, SocketConfigSHA256: handoffTestDigest(t, testCase.options)}
			control := handoffTestControl(t, manifest)
			imported, gate, err := control.open(identity.Address, false, testCase.options, func() (net.Listener, error) { t.Fatal("unexpected bind"); return nil, nil })
			if err != nil {
				t.Fatal(err)
			}
			defer imported.Close()
			importedClient := handoffTestDial(t, identity.Address)
			defer importedClient.Close()
			importedAccepted, err := imported.(*net.TCPListener).AcceptTCP()
			if err != nil {
				t.Fatal(err)
			}
			defer importedAccepted.Close()
			beforeRestore := readAccepted(importedAccepted)
			if err := gate.prepareConnection(importedAccepted); err != nil {
				t.Fatal(err)
			}
			afterRestore := readAccepted(importedAccepted)
			t.Logf("listener=%+v normal=%v imported_before=%v imported_after=%v (enabled,idle,interval,count)", listenerConfig, normal, beforeRestore, afterRestore)
			if normal != afterRestore {
				t.Fatalf("import changed effective keepalive: ordinary=%v imported=%v", normal, afterRestore)
			}
			if testCase.options != nil {
				if testCase.options.TcpKeepAliveIdle > 0 && (normal[0] != 1 || normal[1] != int(testCase.options.TcpKeepAliveIdle)) {
					t.Fatalf("ordinary listener did not apply explicit keepalive idle: %v", normal)
				}
				if testCase.options.TcpKeepAliveInterval > 0 && (normal[0] != 1 || normal[2] != int(testCase.options.TcpKeepAliveInterval)) {
					t.Fatalf("ordinary listener did not apply explicit keepalive interval: %v", normal)
				}
				if testCase.name == "brocade_inbound" && normal != [4]int{1, 300, 90, 3} {
					t.Fatalf("Brocade keepalive must not inherit the host probe count: %v", normal)
				}
			}
		})
	}
}

func TestHandoffTCPFallbackWithMPTCPPreference(t *testing.T) {
	t.Setenv("GODEBUG", "multipathtcp=0")
	config := &internet.SocketConfig{
		TcpMptcp: true, TcpKeepAliveIdle: 300, TcpKeepAliveInterval: 90,
		CustomSockopt: []*internet.CustomSockopt{{System: "linux", Network: "tcp", Level: "6", Opt: "6", Type: "int", Value: "3"}},
	}
	// Build the exact TCP result of a compatibility fallback independently of
	// this test host's MPTCP capabilities. Retain the requested MPTCP preference
	// in the source and candidate configuration digests.
	bindConfig := proto.Clone(config).(*internet.SocketConfig)
	bindConfig.TcpMptcp = false
	bound, err := internet.ListenSystem(context.Background(), &net.TCPAddr{IP: net.ParseIP("127.0.0.1")}, bindConfig)
	if err != nil {
		t.Fatal(err)
	}
	listener := bound.(*net.TCPListener)
	defer listener.Close()
	identity, err := inspectHandoffListener(listener)
	if err != nil || identity.Protocol != unix.IPPROTO_TCP {
		t.Fatalf("expected ordinary TCP: identity=%+v err=%v", identity, err)
	}
	manifest := handoffTestManifest(t, listener)
	source := handoffTestControl(t, manifest)
	if _, _, err := source.open(identity.Address, false, config, func() (net.Listener, error) { return listener, nil }); err != nil {
		t.Fatal(err)
	}
	fd, err := duplicateHandoffListener(listener)
	if err != nil {
		t.Fatal(err)
	}
	manifest = handoffTestManifest(t, listener)
	manifest.Listeners[0] = handoffEndpoint{
		Address: identity.Address, FD: fd, Protocol: unix.IPPROTO_TCP,
		SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie,
		SocketConfigSHA256: handoffTestDigest(t, config),
	}
	candidate := handoffTestControl(t, manifest)
	imported, gate, err := candidate.open(identity.Address, false, config, func() (net.Listener, error) {
		t.Fatal("fallback import must preserve its actual TCP socket, not bind another listener")
		return nil, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	defer imported.Close()
	client := handoffTestDial(t, identity.Address)
	defer client.Close()
	accepted, err := imported.(*net.TCPListener).AcceptTCP()
	if err != nil {
		t.Fatal(err)
	}
	defer accepted.Close()
	if err := gate.prepareConnection(accepted); err != nil {
		t.Fatal(err)
	}
	var got [4]int
	raw, err := accepted.SyscallConn()
	if err != nil {
		t.Fatal(err)
	}
	var optionErr error
	err = raw.Control(func(fd uintptr) {
		for i, option := range []struct{ level, option int }{
			{unix.SOL_SOCKET, unix.SO_KEEPALIVE}, {unix.IPPROTO_TCP, unix.TCP_KEEPIDLE},
			{unix.IPPROTO_TCP, unix.TCP_KEEPINTVL}, {unix.IPPROTO_TCP, unix.TCP_KEEPCNT},
		} {
			got[i], optionErr = unix.GetsockoptInt(int(fd), option.level, option.option)
			if optionErr != nil {
				return
			}
		}
	})
	if err != nil || optionErr != nil || got != [4]int{1, 300, 90, 3} {
		t.Fatalf("fallback handoff lost keepalive: got=%v raw=%v option=%v", got, err, optionErr)
	}
}

func TestHandoffKeepAliveRebindFromMPTCP(t *testing.T) {
	t.Setenv("GODEBUG", "multipathtcp=1")
	for _, ip := range []string{"127.0.0.1", "::1"} {
		t.Run(ip, func(t *testing.T) {
			bound, err := internet.ListenSystem(context.Background(), &net.TCPAddr{IP: net.ParseIP(ip)}, nil)
			if err != nil {
				t.Fatal(err)
			}
			old := bound.(*net.TCPListener)
			defer old.Close()
			identity, err := inspectHandoffListener(old)
			if err != nil {
				t.Fatal(err)
			}
			if identity.Protocol != unix.IPPROTO_MPTCP {
				t.Skip("kernel did not create an MPTCP listener")
			}
			manifest := handoffTestManifest(t, old)
			manifest.Listeners[0].Protocol = identity.Protocol
			source := handoffTestControl(t, manifest)
			listener, gate := handoffTestOpen(t, source, old)
			serveHandoffEcho(t, listener, gate, 'o')
			if err := source.markReady(); err != nil {
				t.Fatal(err)
			}
			requireHandoffRPC(t, source, "resume", 1)
			oldClient := handoffTestDial(t, identity.Address)
			handoffTestExchange(t, oldClient, 'o', '1')
			requireHandoffRPC(t, source, "pause", 2)
			// Changed socket options require a fresh bind while the old MPTCP
			// socket and its accepted connection still exist. Protocol 0 lets
			// this new endpoint record a compatibility fallback, not inherit
			// the source's protocol or mutate its shared FD.
			manifest = handoffTestManifest(t, old)
			manifest.Listeners[0].Protocol = 0
			candidate := handoffTestControl(t, manifest)
			config := &internet.SocketConfig{
				TcpMptcp: true, TcpKeepAliveIdle: 300, TcpKeepAliveInterval: 90,
				CustomSockopt: []*internet.CustomSockopt{{System: "linux", Network: "tcp", Level: "6", Opt: "6", Type: "int", Value: "3"}},
			}
			fresh, freshGate, err := candidate.open(identity.Address, false, config, func() (net.Listener, error) {
				return internet.ListenSystem(context.Background(), old.Addr(), config)
			})
			if err != nil {
				t.Fatal(err)
			}
			serveHandoffEcho(t, fresh.(*net.TCPListener), freshGate, 'n')
			if err := candidate.markReady(); err != nil {
				t.Fatal(err)
			}
			requireHandoffRPC(t, candidate, "resume", 1)
			gate.close()
			if err := old.Close(); err != nil { // retire admission, not accepted connections
				t.Fatal(err)
			}
			handoffTestExchange(t, oldClient, 'o', '2')
			newClient := handoffTestDial(t, identity.Address)
			handoffTestExchange(t, newClient, 'n', '3')
			keepAlive, err := handoffKeepAlive(fresh.(*net.TCPListener))
			if err != nil || !keepAlive.Enable || keepAlive.Idle != 300*time.Second || keepAlive.Interval != 90*time.Second || keepAlive.Count != 3 {
				t.Fatalf("new listener lost keepalive: config=%+v err=%v", keepAlive, err)
			}
			t.Logf("source protocol=%d candidate protocol=%d; old and new connections both usable", identity.Protocol, candidate.status().Listeners[0].Protocol)
		})
	}
}

func TestHandoffRejectsUnappliedKeepAlive(t *testing.T) {
	listener := handoffTestListener(t)
	config := &internet.SocketConfig{TcpKeepAliveIdle: 300, TcpKeepAliveInterval: 90}
	before, err := handoffKeepAlive(listener)
	if err != nil {
		t.Fatal(err)
	}
	identity, err := inspectHandoffListener(listener)
	if err != nil {
		t.Fatal(err)
	}
	fd, err := duplicateHandoffListener(listener)
	if err != nil {
		t.Fatal(err)
	}
	manifest := handoffTestManifest(t, listener)
	manifest.Listeners[0] = handoffEndpoint{
		Address: identity.Address, FD: fd, Protocol: identity.Protocol,
		SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie,
		SocketConfigSHA256: handoffTestDigest(t, config),
	}
	candidate := handoffTestControl(t, manifest)
	if _, _, err := candidate.open(identity.Address, false, config, func() (net.Listener, error) {
		t.Fatal("must not rebind a rejected inherited listener")
		return nil, nil
	}); err == nil || !strings.Contains(err.Error(), "keepalive") {
		t.Fatalf("matching digest must not certify unapplied keepalive: %v", err)
	}
	if after, err := handoffKeepAlive(listener); err != nil || before != after {
		t.Fatalf("rejecting candidate mutated source: before=%+v after=%+v err=%v", before, after, err)
	}
}

func TestHandoffMPTCPSameSocketExportImport(t *testing.T) {
	for _, explicitMPTCP := range []bool{false, true} {
		t.Run(fmt.Sprintf("explicit=%v", explicitMPTCP), func(t *testing.T) {
			var lc net.ListenConfig
			lc.SetMultipathTCP(true)
			listener, err := lc.Listen(context.Background(), "tcp4", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			l := listener.(*net.TCPListener)
			defer l.Close()
			identity, err := inspectHandoffListener(l)
			if err != nil {
				t.Fatal(err)
			}
			if identity.Protocol != unix.IPPROTO_MPTCP {
				t.Skip("kernel did not create an MPTCP listener; same-socket MPTCP check unavailable")
			}
			manifest := handoffTestManifest(t, l)
			manifest.Listeners[0].Protocol = unix.IPPROTO_MPTCP
			source := handoffTestControl(t, manifest)
			if _, _, err := source.open(identity.Address, false, &internet.SocketConfig{TcpMptcp: explicitMPTCP}, func() (net.Listener, error) { return l, nil }); err != nil {
				t.Fatal(err)
			}
			if err := source.markReady(); err != nil {
				t.Fatal(err)
			}
			exported, fds := handoffTestRPC(t, source, "export", 1, "mptcp-export-0001")
			if exported.Error != "" || len(fds) != 1 || exported.Status.Listeners[0].Protocol != unix.IPPROTO_MPTCP {
				t.Fatalf("MPTCP export failed: %+v fds=%v", exported, fds)
			}
			manifest = handoffTestManifest(t, l)
			manifest.GenerationID = "mptcp-candidate-0002"
			manifest.Listeners[0] = handoffEndpoint{Address: identity.Address, FD: fds[0], SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie, Protocol: unix.IPPROTO_MPTCP, SocketConfigSHA256: exported.Status.Listeners[0].SocketConfigSHA256}
			candidate := handoffTestControl(t, manifest)
			imported, gate, err := candidate.open(identity.Address, false, &internet.SocketConfig{TcpMptcp: explicitMPTCP}, func() (net.Listener, error) { t.Fatal("MPTCP import must not bind"); return nil, nil })
			if err != nil {
				t.Fatal(err)
			}
			if got := candidate.status().Listeners[0].handoffSocketIdentity; got != identity {
				t.Fatalf("MPTCP identity changed: got=%+v want=%+v", got, identity)
			}
			serveHandoffEcho(t, imported.(*net.TCPListener), gate, 'm')
			if err := candidate.markReady(); err != nil {
				t.Fatal(err)
			}
			requireHandoffRPC(t, candidate, "resume", 1)
			var dialer net.Dialer
			dialer.SetMultipathTCP(true)
			conn, err := dialer.DialContext(context.Background(), "tcp4", identity.Address)
			if err != nil {
				t.Fatal(err)
			}
			defer conn.Close()
			usedMPTCP, err := conn.(*net.TCPConn).MultipathTCP()
			if err != nil || !usedMPTCP {
				t.Fatalf("MPTCP connection unexpectedly fell back: used=%v err=%v", usedMPTCP, err)
			}
			handoffTestExchange(t, conn.(*net.TCPConn), 'm', '1')
			requireHandoffRPC(t, candidate, "pause", 2)
			handoffTestExchange(t, conn.(*net.TCPConn), 'm', '2')
		})
	}
}

func TestHandoffHubLifecycleKeepsRawTCPConnection(t *testing.T) {
	l := handoffTestListener(t)
	identity, err := inspectHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	fd, err := duplicateHandoffListener(l)
	if err != nil {
		t.Fatal(err)
	}
	manifest := handoffTestManifest(t, l)
	manifest.Listeners[0] = handoffEndpoint{Address: identity.Address, FD: fd, SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie, Protocol: identity.Protocol, SocketConfigSHA256: handoffTestDigest(t, nil)}
	path := filepath.Join(filepath.Dir(manifest.ControlPath), "manifest.json")
	body, _ := json.Marshal(manifest)
	if err := os.WriteFile(path, body, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("XRAY_TCP_HANDOFF_MANIFEST", path)
	if err := InitHandoff(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { CloseHandoff() })
	if err := InitHandoff(); err == nil {
		t.Fatal("double initialization accepted")
	}
	accepted := make(chan stat.Connection, 1)
	address := l.Addr().(*net.TCPAddr)
	hub, err := ListenTCP(context.Background(), xnet.IPAddress(address.IP), xnet.Port(address.Port), &internet.MemoryStreamConfig{ProtocolSettings: &Config{}}, func(conn stat.Connection) { accepted <- conn })
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { hub.Close() })
	if err := MarkHandoffReady(); err != nil {
		t.Fatal(err)
	}
	client := handoffTestDial(t, identity.Address)
	select {
	case conn := <-accepted:
		conn.Close()
		t.Fatal("hub accepted before explicit resume")
	case <-time.After(30 * time.Millisecond):
	}
	handoffRegistry.Lock()
	c := handoffRegistry.control
	handoffRegistry.Unlock()
	requireHandoffRPC(t, c, "resume", 1)
	var conn stat.Connection
	select {
	case conn = <-accepted:
	case <-time.After(time.Second):
		t.Fatal("hub did not accept after resume")
	}
	defer conn.Close()
	if _, ok := conn.(*net.TCPConn); !ok {
		t.Fatalf("handoff obscured raw TCP type: %T", conn)
	}
	if active := c.status().Listeners[0].Active; active != 1 {
		t.Fatalf("accepted connection was not pinned as active: %d", active)
	}
	requireHandoffRPC(t, c, "pause", 2)
	if err := hub.Close(); err != nil {
		t.Fatal(err)
	}
	if err := conn.SetDeadline(time.Now().Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Write([]byte("alive")); err != nil {
		t.Fatal(err)
	}
	buffer := make([]byte, 5)
	if _, err := io.ReadFull(conn, buffer); err != nil || string(buffer) != "alive" {
		t.Fatalf("listener close interrupted existing connection: %q %v", buffer, err)
	}
	FinishHandoffConnection(conn)
	if active := c.status().Listeners[0].Active; active != 0 {
		t.Fatalf("finished connection kept retirement pinned: %d", active)
	}
	// Completion is idempotent because multiple cleanup paths may observe the same close.
	FinishHandoffConnection(conn)
	if active := c.status().Listeners[0].Active; active != 0 {
		t.Fatalf("duplicate completion underflowed active connections: %d", active)
	}
}

func TestHandoffNoOptInDoesNotInstallControl(t *testing.T) {
	t.Setenv("XRAY_TCP_HANDOFF_MANIFEST", "")
	if err := InitHandoff(); err != nil {
		t.Fatal(err)
	}
	if err := MarkHandoffReady(); err != nil {
		t.Fatal(err)
	}
	if err := CloseHandoff(); err != nil {
		t.Fatal(err)
	}
	l := handoffTestListener(t)
	actual, gate, err := openHandoffListener(l.Addr().String(), true, nil, func() (net.Listener, error) { return l, nil })
	if err != nil || actual != l || gate != nil {
		t.Fatalf("disabled handoff changed listener: %v %v %v", actual, gate, err)
	}
}

func TestDirectControlListenerUsesPreparationAndPauseBarriers(t *testing.T) {
	reserved := handoffTestListener(t)
	address := reserved.Addr().String()
	manifest := handoffTestManifest(t, reserved)
	reserved.Close()
	manifest.Version, manifest.Scope = 2, "whole-generation"
	manifest.Listeners[0] = handoffEndpoint{Address: address, Network: "tcp", Role: "control", Prepare: true}
	path := filepath.Join(filepath.Dir(manifest.ControlPath), "manifest.json")
	body, err := json.Marshal(manifest)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, body, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("XRAY_TCP_HANDOFF_MANIFEST", path)
	if err := InitHandoff(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { CloseHandoff() })
	listener, err := ListenHandoffControl(context.Background(), address)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { listener.Close() })
	if err := MarkHandoffReady(); err != nil {
		t.Fatal(err)
	}
	accepted := make(chan net.Conn, 1)
	go func() { conn, _ := listener.Accept(); accepted <- conn }()
	client := handoffTestDial(t, address)
	defer client.Close()
	select {
	case conn := <-accepted:
		if conn == nil {
			t.Fatal("preparation API did not accept")
		}
		defer conn.Close()
	case <-time.After(time.Second):
		t.Fatal("preparation API blocked")
	}
	handoffRegistry.Lock()
	c := handoffRegistry.control
	handoffRegistry.Unlock()
	if status := c.status(); status.Active != 0 || status.Accepting {
		t.Fatalf("management traffic affected public admission: %+v", status)
	}
	if err := c.seal(); err != nil {
		t.Fatal(err)
	}
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
	go func() { conn, _ := listener.Accept(); accepted <- conn }()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := c.pause(ctx); err != nil {
		t.Fatal(err)
	}
	if err := c.drain(ctx); err != nil {
		t.Fatal(err)
	}
	if !c.status().RetireReady {
		t.Fatal("management connection blocked retirement")
	}
	if err := c.dropTCP(); err != nil {
		t.Fatal(err)
	}
	listener.Close()
	if !c.status().Ready || !c.status().RetireReady {
		t.Fatal("management server cleanup destroyed the retirement proof")
	}
	select {
	case conn := <-accepted:
		if conn != nil {
			conn.Close()
			t.Fatal("paused API admitted traffic")
		}
	case <-time.After(time.Second):
		t.Fatal("closed API accept did not return")
	}
}

func TestDropTCPKeepsAcceptedFlowsAndCoverAdmission(t *testing.T) {
	public, cover := handoffTestListener(t), handoffTestListener(t)
	manifest := handoffTestManifest(t, public)
	manifest.Version, manifest.Scope = 2, "whole-generation"
	manifest.Listeners = []handoffEndpoint{
		{Address: public.Addr().String(), Network: "tcp", Role: "public"},
		{Address: cover.Addr().String(), Network: "tcp", Role: "cover"},
	}
	c := handoffTestControl(t, manifest)
	pl, pg := handoffTestOpen(t, c, public)
	cl, cg := handoffTestOpen(t, c, cover)
	serveHandoffEcho(t, pl, pg, 'p')
	serveHandoffEcho(t, cl, cg, 'c')
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := c.seal(); err != nil {
		t.Fatal(err)
	}
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
	client := handoffTestDial(t, public.Addr().String())
	defer client.Close()
	handoffTestExchange(t, client, 'p', '1')
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := c.pause(ctx); err != nil {
		t.Fatal(err)
	}
	requireHandoffRPC(t, c, "drop_tcp", 1)
	requireHandoffRPC(t, c, "drop_tcp", 1)
	if s := c.status(); !s.Ready || !s.RetirementSupported || s.PublicActive != 1 || s.RetireReady {
		t.Fatalf("lost accepted connection accounting: %+v", s)
	}
	handoffTestExchange(t, client, 'p', '2')
	if conn, err := net.DialTimeout("tcp", public.Addr().String(), time.Second); err == nil {
		conn.Close()
		t.Fatal("retired listener still accepted new TCP")
	}
	coverClient := handoffTestDial(t, cover.Addr().String())
	defer coverClient.Close()
	handoffTestExchange(t, coverClient, 'c', '3')
	if err := c.resume(); err == nil {
		t.Fatal("irreversibly retired generation resumed")
	}
	if _, err := c.exportListeners(); err == nil {
		t.Fatal("closed listener exported")
	}
	if err := c.drain(ctx); err == nil {
		t.Fatal("active public connection was treated as drained")
	}
}

func TestHandoffSocketConfigDigestIsCanonicalAndComplete(t *testing.T) {
	empty := handoffTestDigest(t, nil)
	if empty != handoffTestDigest(t, &internet.SocketConfig{}) || empty != handoffTestDigest(t, &internet.SocketConfig{CustomSockopt: []*internet.CustomSockopt{}}) {
		t.Fatal("nil and empty socket configurations did not normalize")
	}
	base := &internet.SocketConfig{TcpKeepAliveIdle: 41, TcpKeepAliveInterval: 17, TcpMptcp: true}
	digest := handoffTestDigest(t, base)
	for i := 0; i < 32; i++ {
		if got := handoffTestDigest(t, proto.Clone(base).(*internet.SocketConfig)); got != digest {
			t.Fatal("protobuf socket configuration digest is not deterministic")
		}
	}
	for _, mutate := range []func(*internet.SocketConfig){
		func(c *internet.SocketConfig) { c.TcpKeepAliveIdle++ },
		func(c *internet.SocketConfig) { c.TcpKeepAliveInterval++ },
		func(c *internet.SocketConfig) { c.TcpMptcp = false },
		func(c *internet.SocketConfig) { c.Mark = 42 },
		func(c *internet.SocketConfig) { c.Tfo = 256 },
		func(c *internet.SocketConfig) { c.TcpCongestion = "bbr" },
		func(c *internet.SocketConfig) { c.V6Only = true },
		func(c *internet.SocketConfig) {
			c.CustomSockopt = []*internet.CustomSockopt{{Level: "6", Opt: "18", Value: "41", Type: "int"}}
		},
	} {
		changed := proto.Clone(base).(*internet.SocketConfig)
		mutate(changed)
		if handoffTestDigest(t, changed) == digest {
			t.Fatalf("socket configuration change omitted from digest: %v", changed)
		}
	}
}

func TestHandoffSocketConfigMismatchRejectsWithoutTouchingSharedFD(t *testing.T) {
	config := &internet.SocketConfig{TcpKeepAliveIdle: 41, TcpKeepAliveInterval: 17}
	listener, err := internet.ListenSystem(context.Background(), &net.TCPAddr{IP: net.ParseIP("127.0.0.1")}, config)
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	original := listener.(*net.TCPListener)
	identity, err := inspectHandoffListener(original)
	if err != nil {
		t.Fatal(err)
	}
	keepAlive, err := handoffKeepAlive(original)
	if err != nil {
		t.Fatal(err)
	}
	client := handoffTestDial(t, identity.Address)
	defer client.Close()
	accepted, err := original.AcceptTCP()
	if err != nil {
		t.Fatal(err)
	}
	defer accepted.Close()
	fd, err := duplicateHandoffListener(original)
	if err != nil {
		t.Fatal(err)
	}
	manifest := handoffTestManifest(t, original)
	manifest.Listeners[0] = handoffEndpoint{Address: identity.Address, FD: fd, SocketInode: identity.SocketInode, SocketCookie: identity.SocketCookie, Protocol: identity.Protocol, SocketConfigSHA256: handoffTestDigest(t, config)}
	control := handoffTestControl(t, manifest)
	changed := proto.Clone(config).(*internet.SocketConfig)
	changed.TcpKeepAliveIdle = 59
	bound := false
	_, _, err = control.open(identity.Address, false, changed, func() (net.Listener, error) { bound = true; return nil, nil })
	if err == nil || !strings.Contains(err.Error(), "socket configuration differs") || bound {
		t.Fatalf("changed socket settings did not fail before import: err=%v bound=%v", err, bound)
	}
	if err := control.markReady(); err == nil {
		t.Fatal("rejected socket settings reached readiness")
	}
	if got, err := inspectHandoffFD(fd); err != nil || got != identity {
		t.Fatalf("mismatch closed or replaced the inherited FD: got=%+v err=%v", got, err)
	}
	if got, err := handoffKeepAlive(original); err != nil || got != keepAlive {
		t.Fatalf("mismatch modified shared listener socket options: got=%+v err=%v", got, err)
	}
	client.SetDeadline(time.Now().Add(time.Second))
	accepted.SetDeadline(time.Now().Add(time.Second))
	if _, err := client.Write([]byte("alive")); err != nil {
		t.Fatal(err)
	}
	data := make([]byte, 5)
	if _, err := io.ReadFull(accepted, data); err != nil || string(data) != "alive" {
		t.Fatalf("existing connection did not survive rejection: data=%q err=%v", data, err)
	}
	// A matching request still consumes exactly the original FD, proving the
	// rejected candidate did not mutate, consume, or replace that descriptor.
	imported, _, err := control.open(identity.Address, false, config, func() (net.Listener, error) { t.Fatal("unexpected fallback bind"); return nil, nil })
	if err != nil {
		t.Fatal(err)
	}
	defer imported.Close()
	if got := control.status().Listeners[0].SocketConfigSHA256; got != manifest.Listeners[0].SocketConfigSHA256 {
		t.Fatal("import did not publish the verified socket configuration digest")
	}
}

func TestHandoffTimedOutPauseDoesNotClaimBarrierOrAllowStaleResume(t *testing.T) {
	listener := handoffTestListener(t)
	control := handoffTestControl(t, handoffTestManifest(t, listener))
	_, gate := handoffTestOpen(t, control, listener)
	if err := control.markReady(); err != nil {
		t.Fatal(err)
	}
	requireHandoffRPC(t, control, "resume", 1)
	if !gate.beforeAccept() {
		t.Fatal("listener did not enter accept")
	}
	defer gate.afterAccept(false)
	// Hold an admission operation beyond the bounded pause timeout. No actual
	// connection is dispatched; this deterministically exercises the barrier.
	response, _ := handoffTestRPC(t, control, "pause", 2, "timed-out-pause-0002")
	if response.Error == "" {
		t.Fatal("pause unexpectedly completed while accept remained in flight")
	}
	status := requireHandoffRPC(t, control, "status", 0)
	if status.Epoch != 2 || !status.PauseRequested || status.Paused || !status.Accepting {
		t.Fatalf("timed-out pause claimed a completed barrier: %+v", status)
	}
	stale, _ := handoffTestRPC(t, control, "resume", 1, "test-intent-00000000000000000001")
	if stale.Error == "" {
		t.Fatal("stale resume bypassed a timed-out pause fence")
	}
	gate.afterAccept(false)
	retried, _ := handoffTestRPC(t, control, "pause", 2, "timed-out-pause-0002")
	if retried.Error != "" || retried.Status == nil || !retried.Status.PauseRequested || !retried.Status.Paused || retried.Status.Accepting {
		t.Fatalf("same-intent retry did not confirm completed pause: %+v", retried)
	}
}
