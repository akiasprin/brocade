package mux

import (
	"context"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal/done"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func healthForTest(t *testing.T) *ReverseHealth {
	t.Helper()
	d := done.New()
	t.Cleanup(func() { d.Close() })
	return &ReverseHealth{config: DefaultReverseHealthConfig("test", "portal"), writer: newHealthWriter(buf.Discard, d), done: d, started: time.Now(), lastAck: time.Now(), nextID: 100, snapshot: ReverseHealthSnapshot{State: "READY"}}
}
func TestReverseExpiredDeadlineQuarantinesWithoutTimer(t *testing.T) {
	h := healthForTest(t)
	now := time.Now()
	h.pendingID = 10
	h.pendingDeadline = now.Add(-time.Millisecond)
	if h.Usable() {
		t.Fatal("expired worker was dispatchable")
	}
	if h.snapshot.State != "SUSPECT" {
		t.Fatal(h.snapshot)
	}
	h.mu.Lock()
	h.checkLocked(now.Add(time.Second))
	h.mu.Unlock()
	if !h.done.Done() {
		t.Fatal("confirmation deadline did not close worker")
	}
}
func TestReverseConfirmedFailureDrainsActiveRequestsByDefault(t *testing.T) {
	h := healthForTest(t)
	active := uint32(3)
	h.config.ActiveSessions = func() uint32 { return active }
	h.config.DrainIdle = func() bool { return active == 0 }
	now := time.Now()
	h.pendingID = 10
	h.pendingDeadline = now.Add(-time.Millisecond)
	h.Usable()
	h.mu.Lock()
	h.checkLocked(now.Add(time.Second))
	h.mu.Unlock()
	if h.done.Done() || h.snapshot.State != "DRAINING" {
		t.Fatalf("active requests were terminated: %+v", h.snapshot)
	}
	active = 0
	h.mu.Lock()
	h.checkLocked(now.Add(2 * time.Second))
	h.mu.Unlock()
	if !h.done.Done() || h.snapshot.State != "DEAD" {
		t.Fatalf("idle draining worker was not reclaimed: %+v", h.snapshot)
	}
}
func TestReverseConfirmedFailureCanDisconnectActiveRequests(t *testing.T) {
	h := healthForTest(t)
	h.config.DisconnectOnHealthFailure = true
	h.config.ActiveSessions = func() uint32 { return 3 }
	now := time.Now()
	h.pendingID = 10
	h.pendingDeadline = now.Add(-time.Millisecond)
	h.Usable()
	h.mu.Lock()
	h.checkLocked(now.Add(time.Second))
	h.mu.Unlock()
	if !h.done.Done() || h.snapshot.State != "DEAD" || h.snapshot.AffectedSessions != 3 {
		t.Fatalf("configured active disconnect was not applied: %+v", h.snapshot)
	}
}
func TestReversePendingProbeDoesNotQuarantineReadyWorker(t *testing.T) {
	h := healthForTest(t)
	h.pendingID = 10
	h.pendingDeadline = time.Now().Add(time.Second)
	if !h.Usable() {
		t.Fatal("normal pending ping must not stop traffic")
	}
}
func TestReverseLateDuplicateAndWrongPongCannotRevive(t *testing.T) {
	h := healthForTest(t)
	h.pendingID = 10
	h.pendingDeadline = time.Now().Add(-time.Millisecond)
	h.Usable()
	fresh := h.pendingID
	for _, id := range []uint64{10, 9, 0} {
		h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: id})
	}
	if h.snapshot.State != "SUSPECT" || h.snapshot.Acks != 0 {
		t.Fatal(h.snapshot)
	}
	h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: fresh})
	h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: fresh})
	if h.snapshot.State != "SUSPECT" || h.snapshot.Acks != 1 {
		t.Fatal(h.snapshot)
	}
	h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: h.pendingID})
	if !h.Usable() {
		t.Fatal(h.snapshot)
	}
	h.stop("test_close")
	h.done.Close()
	h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: h.pendingID})
	if h.snapshot.State != "CLOSED" {
		t.Fatal("closed worker resurrected")
	}
}
func TestReverseRequiresBothValidationDirections(t *testing.T) {
	h := healthForTest(t)
	h.snapshot.State = "VALIDATING"
	h.pendingID = 10
	h.pendingDeadline = time.Now().Add(time.Second)
	h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: 10})
	if h.Usable() {
		t.Fatal("own ACK alone accepted")
	}
	h.receive(&FrameMetadata{Option: OptionProbe, ProbeID: 22})
	h.receive(&FrameMetadata{Option: OptionProbe | OptionValidated, ProbeID: 21})
	if h.Usable() {
		t.Fatal("wrong peer generation accepted")
	}
	h.receive(&FrameMetadata{Option: OptionProbe | OptionValidated, ProbeID: 22})
	if !h.Usable() {
		t.Fatal(h.snapshot)
	}
}
func TestReverseControlWriteBlockedStillCloses(t *testing.T) {
	r, w := pipe.New(pipe.WithSizeLimit(0))
	defer r.Interrupt()
	d := done.New()
	c := DefaultReverseHealthConfig("blocked", "bridge")
	c.ReadyTimeout = 100 * time.Millisecond
	h := newReverseHealth(c, newHealthWriter(w, d), d)
	select {
	case <-d.Wait():
	case <-time.After(time.Second):
		t.Fatal("blocked control write suppressed deadline")
	}
	if h.Usable() {
		t.Fatal("closed writer dispatchable")
	}
}
func TestReversePairValidationAndAtomicDispatchClose(t *testing.T) {
	a, aw := pipe.New()
	b, bw := pipe.New()
	sc := DefaultReverseHealthConfig("pair", "bridge")
	server, err := NewReverseServerWorker(context.Background(), nil, &transport.Link{Reader: a, Writer: bw}, sc)
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	cc := DefaultReverseHealthConfig("pair", "portal")
	client, err := NewReverseClientWorker(transport.Link{Reader: b, Writer: aw}, cc)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	deadline := time.Now().Add(time.Second)
	for !client.health.Usable() || !server.health.Usable() {
		if time.Now().After(deadline) {
			t.Fatalf("pair not ready: %+v %+v", client.health.Snapshot(), server.health.Snapshot())
		}
		time.Sleep(time.Millisecond)
	}
	// All active and spare workers continue probing with no business sessions.
	if client.health.Snapshot().Acks == 0 || server.health.Snapshot().Acks == 0 {
		t.Fatal("one-way validation")
	}
	client.health.mu.Lock()
	client.health.transitionLocked("SUSPECT", "test")
	client.health.hardDeadline = time.Now().Add(time.Second)
	client.health.mu.Unlock()
	r, w := pipe.New()
	out, outw := pipe.New()
	defer r.Interrupt()
	defer out.Interrupt()
	defer w.Close()
	defer outw.Close()
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{Target: net.TCPDestination(net.LocalHostIP, 80)}})
	if client.Dispatch(ctx, &transport.Link{Reader: r, Writer: outw}) {
		t.Fatal("dispatch bypassed quarantine")
	}
}

func reverseEchoPair(t *testing.T) (*ClientWorker, *ServerWorker) {
	t.Helper()
	a, aw := pipe.New()
	b, bw := pipe.New()
	sc := DefaultReverseHealthConfig("echo", "bridge")
	server, err := NewReverseServerWorker(context.Background(), &faultEchoDispatcher{}, &transport.Link{Reader: a, Writer: bw}, sc)
	if err != nil {
		t.Fatal(err)
	}
	cc := DefaultReverseHealthConfig("echo", "portal")
	client, err := NewReverseClientWorker(transport.Link{Reader: b, Writer: aw}, cc)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { client.Close(); server.Close() })
	deadline := time.Now().Add(time.Second)
	for !client.health.Usable() || !server.health.Usable() {
		if time.Now().After(deadline) {
			t.Fatal("echo worker validation timed out")
		}
		time.Sleep(time.Millisecond)
	}
	if client.ActiveConnections() != 0 || server.ActiveConnections() != 0 {
		t.Fatal("probes became business sessions")
	}
	return client, server
}

func TestReversePairDoesNotInstallOrdinaryIdleReaper(t *testing.T) {
	client, server := reverseEchoPair(t)
	if client.timer != nil || server.timer != nil {
		t.Fatal("reverse pair inherited the ordinary idle reaper")
	}
	if client.strategy.WorkerPool != nil || client.strategy.MaxConcurrency != client.health.config.MaxSessionsPerWorker || client.strategy.MaxConnection != uint32(^uint16(0)) {
		t.Fatalf("reverse client inherited or lost strategy parameters: %+v", client.strategy)
	}
	select {
	case <-server.WaitClosed():
		t.Fatal("ordinary idle reaper closed healthy reverse worker")
	case <-time.After(25 * time.Millisecond):
	}
	if client.Closed() || !client.health.Usable() || !server.health.Usable() {
		t.Fatal("idle reverse pair did not remain healthy")
	}
}

func TestReverseTCPAndUDPFailClosedAndFreshGenerationWorks(t *testing.T) {
	for _, network := range []net.Network{net.Network_TCP, net.Network_UDP} {
		t.Run(network.String(), func(t *testing.T) {
			// Both generations use the same UDP source (hence the same XUDP GlobalID).
			ctx := session.ContextWithInbound(context.WithValue(context.Background(), "cone", true), &session.Inbound{Name: "socks", Source: net.UDPDestination(net.LocalHostIP, 49152)})
			target := net.Destination{Network: network, Address: net.LocalHostIP, Port: 1234}
			ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: target}})
			for generation := 0; generation < 2; generation++ {
				client, server := reverseEchoPair(t)
				input, inputWriter := pipe.New()
				output, outputWriter := pipe.New()
				if !client.Dispatch(ctx, &transport.Link{Reader: input, Writer: outputWriter}) {
					t.Fatal("healthy worker rejected stream")
				}
				for n := 0; n < 20; n++ {
					payload := buf.New()
					payload.Write([]byte{byte(generation), byte(n)})
					if network == net.Network_UDP {
						dest := target
						dest.Port = net.Port(1234 + n%2)
						payload.UDP = &dest
					}
					if err := inputWriter.WriteMultiBuffer(buf.MultiBuffer{payload}); err != nil {
						t.Fatal(err)
					}
					replies, err := output.ReadMultiBufferTimeout(time.Second)
					if err != nil {
						t.Fatal(err)
					}
					if len(replies) != 1 || replies[0].Len() != 2 || replies[0].Byte(0) != byte(generation) || replies[0].Byte(1) != byte(n) {
						t.Fatalf("reply crossed generation: %v", replies)
					}
					if network == net.Network_UDP && (replies[0].UDP == nil || replies[0].UDP.Port != net.Port(1234+n%2)) {
						t.Fatalf("UDP destination changed: %v", replies[0].UDP)
					}
					buf.ReleaseMulti(replies)
				}
				// Existing TCP streams and UDP associations must terminate, never hang or replay.
				client.Close()
				server.Close()
				if replies, err := output.ReadMultiBufferTimeout(time.Second); err == nil || err == buf.ErrReadTimeout {
					buf.ReleaseMulti(replies)
					t.Fatalf("failed worker did not terminate response link: %v", err)
				}
				deadline := time.Now().Add(time.Second)
				for client.ActiveConnections() != 0 || server.ActiveConnections() != 0 {
					if time.Now().After(deadline) {
						t.Fatal("sessions leaked after worker close")
					}
					time.Sleep(time.Millisecond)
				}
				inputWriter.Close()
				output.Interrupt()
			}
		})
	}
}

func TestReverseWorkerDoesNotMutateSharedInbound(t *testing.T) {
	inbound := &session.Inbound{}
	inbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
	ctx := session.ContextWithInbound(context.Background(), inbound)
	reader, writer := pipe.New()
	worker, err := NewReverseServerWorker(ctx, nil, &transport.Link{Reader: reader, Writer: buf.Discard}, DefaultReverseHealthConfig("metadata-test", "bridge"))
	if err != nil {
		t.Fatal(err)
	}
	defer worker.Close()
	defer writer.Close()
	if inbound.CanSpliceCopy.Load() != session.SpliceCopyDirect {
		t.Fatal("worker modified shared inbound metadata")
	}
}

func TestReverseTuningJitterAndRecoveryThreshold(t *testing.T) {
	h := healthForTest(t)
	h.config.ProbeJitterPercent = 0
	if h.probeDelay() != h.config.ProbeInterval {
		t.Fatal("zero jitter changed interval")
	}
	h.config.ProbeJitterPercent = 25
	for id := uint64(0); id < 1000; id++ {
		h.nextID = id
		d := h.probeDelay()
		if d < 750*time.Millisecond || d > 1250*time.Millisecond {
			t.Fatalf("jitter out of range: %v", d)
		}
	}
	h.config.RecoverySuccesses = 3
	h.ownValidated = true
	h.pendingID = 10
	h.pendingDeadline = time.Now().Add(-time.Millisecond)
	h.Usable()
	for i := 1; i <= 3; i++ {
		if err := h.receive(&FrameMetadata{Option: OptionProbe | OptionAck, ProbeID: h.pendingID}); err != nil {
			t.Fatal(err)
		}
		if (h.snapshot.State == "READY") != (i == 3) {
			t.Fatalf("ack %d, state %s", i, h.snapshot.State)
		}
	}
}

func TestReverseCanaryConfiguredSuccessCountAndWindow(t *testing.T) {
	config := DefaultReverseCanaryConfig()
	config.Successes = 3
	config.StableWindow = time.Second
	config.Interval = time.Minute
	c := NewReverseCanary("canary-tuning-test", config)
	defer c.Close()
	if c.FreshnessBudgetMS != 120750 {
		t.Fatal("canary freshness ignored interval")
	}
	for i := 0; i < 3; i++ {
		c.Record(time.Now(), "success")
	}
	if c.StableSinceUnixMS != 0 {
		t.Fatal("success count bypassed stable window")
	}
	reverseHealthRegistry.Lock()
	c.FirstOKUnixMS = time.Now().Add(-2 * time.Second).UnixMilli()
	reverseHealthRegistry.Unlock()
	c.Record(time.Now(), "success")
	if c.StableSinceUnixMS == 0 {
		t.Fatal("configured threshold did not mark stable")
	}
	c.Record(time.Now(), "request_failed")
	if c.StableSinceUnixMS != 0 || c.ConsecutiveSuccesses != 0 {
		t.Fatal("failure did not reset stability")
	}
}
