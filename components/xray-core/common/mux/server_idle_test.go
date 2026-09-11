package mux

import (
	"bytes"
	"context"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/bitmask"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/signal/done"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func newServerIdleWorkerForTest(clock *fakePoolClock) *ServerWorker {
	w := &ServerWorker{
		sessionManager: NewSessionManager(),
		link:           &transport.Link{Writer: buf.Discard},
		done:           done.New(),
		idle:           serverIdleGuard{clock: clock.Now},
	}
	return w
}

func serverIdleFrameForTest(t *testing.T, w *ServerWorker, meta FrameMetadata) {
	t.Helper()
	wire := marshalMetadataForTest(t, meta)
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(wire))}
	if err := w.handleFrame(context.Background(), reader); err != nil {
		t.Fatal(err)
	}
}

func serverIdlePingForTest(t *testing.T, w *ServerWorker) {
	t.Helper()
	serverIdleFrameForTest(t, w, FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe, ProbeID: 42})
}

func TestServerPoolPingSurvivesLegacyIdleTick(t *testing.T) {
	uplinkReader, uplinkWriter := pipe.New(pipe.WithoutSizeLimit())
	downlinkReader, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
	server, err := NewServerWorker(context.Background(), nil, &transport.Link{Reader: uplinkReader, Writer: downlinkWriter})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { server.Close(); uplinkWriter.Close(); downlinkReader.Interrupt() })
	if err := writeProbeFrame(uplinkWriter, 42, false); err != nil {
		t.Fatal(err)
	}
	mb, err := downlinkReader.ReadMultiBufferTimeout(time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(mb)
	var pong FrameMetadata
	if err := pong.Unmarshal(bytes.NewReader([]byte(mb.String())), false); err != nil {
		t.Fatal(err)
	}
	if pong.Option != OptionProbe|OptionAck || pong.ProbeID != 42 {
		t.Fatalf("invalid Pong: %+v", pong)
	}
	// Exercise the real monitor without waiting a wall-clock minute. A normal
	// pool Ping must prevent the ordinary zero-business-session check from closing it.
	server.timer.Reset(time.Millisecond)
	select {
	case <-server.WaitClosed():
		t.Fatal("server closed a healthy, never-used warm worker at its first idle check")
	case <-time.After(20 * time.Millisecond):
	}
}

func TestServerPoolHeartbeatRetainsFiveHourIdle(t *testing.T) {
	// Include the full maximum interval + maximum jitter + response budget,
	// which crosses a 60-second check without a Ping in every window.
	for _, gap := range []time.Duration{8 * time.Second, maxWorkerPoolProbeInterval*12/10 + 10*time.Second} {
		t.Run(gap.String(), func(t *testing.T) {
			clock := newFakePoolClock()
			w := newServerIdleWorkerForTest(clock)
			serverIdlePingForTest(t, w)
			end := clock.Now().Add(5 * time.Hour)
			nextPing := clock.Now().Add(gap)
			nextTick := clock.Now().Add(serverIdleCheckInterval)
			checks := 0
			for next := minTime(nextPing, nextTick); !next.After(end); next = minTime(nextPing, nextTick) {
				clock.Advance(next.Sub(clock.Now()))
				if next.Equal(nextPing) {
					serverIdlePingForTest(t, w)
					nextPing = nextPing.Add(gap)
				}
				if next.Equal(nextTick) {
					if w.idle.closeIfIdle(w.sessionManager, 0, 0) {
						t.Fatalf("healthy idle worker closed at %v", next)
					}
					checks++
					nextTick = nextTick.Add(serverIdleCheckInterval)
				}
			}
			if checks != 300 || w.sessionManager.Count() != 0 {
				t.Fatalf("checks=%d sessions=%d; heartbeats must not allocate business sessions", checks, w.sessionManager.Count())
			}
			deadline := w.idle.lastReceived.Add(serverPeerIdleGrace)
			clock.Advance(deadline.Sub(clock.Now()) - time.Nanosecond)
			if w.idle.closeIfIdle(w.sessionManager, 0, 0) {
				t.Fatal("server reclaimed an idle peer before the silence deadline")
			}
			clock.Advance(time.Nanosecond)
			if !w.idle.closeIfIdle(w.sessionManager, 0, 0) {
				t.Fatal("server retained an orphan after heartbeats stopped")
			}
		})
	}
}

func TestServerControlWithoutPoolPingDoesNotOptIntoPoolRetention(t *testing.T) {
	for _, option := range []bitmask.Byte{0, OptionProbe | OptionAck, OptionProbe | OptionValidated, OptionProbe | OptionDrain} {
		clock := newFakePoolClock()
		w := newServerIdleWorkerForTest(clock)
		serverIdleFrameForTest(t, w, FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: option, ProbeID: 1})
		clock.Advance(serverIdleCheckInterval)
		if !w.idle.closeIfIdle(w.sessionManager, 0, 0) {
			t.Fatalf("control option %#x changed no-Ping idle cleanup", option)
		}
	}
}

func TestServerLocalWritesAndUnsolicitedControlDoNotRenewPeerGrace(t *testing.T) {
	clock := newFakePoolClock()
	w := newServerIdleWorkerForTest(clock)
	serverIdlePingForTest(t, w)
	clock.Advance(serverPeerIdleGrace - time.Nanosecond)
	for _, option := range []bitmask.Byte{0, OptionProbe | OptionAck, OptionProbe | OptionValidated, OptionProbe | OptionDrain} {
		serverIdleFrameForTest(t, w, FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: option, ProbeID: 1})
	}
	if err := writeProbeFrame(w.link.Writer, 42, true); err != nil {
		t.Fatal(err)
	}
	clock.Advance(time.Nanosecond)
	if !w.idle.closeIfIdle(w.sessionManager, 0, 0) {
		t.Fatal("local Pong or unsolicited control extended peer silence grace")
	}
}

func TestServerBusinessFrameRenewsPeerGrace(t *testing.T) {
	clock := newFakePoolClock()
	w := newServerIdleWorkerForTest(clock)
	serverIdlePingForTest(t, w)
	clock.Advance(serverPeerIdleGrace - time.Second)
	serverIdleFrameForTest(t, w, FrameMetadata{SessionStatus: SessionStatusKeep, SessionID: 1})
	clock.Advance(time.Second)
	if w.idle.closeIfIdle(w.sessionManager, 0, 0) {
		t.Fatal("server ignored a parsed business frame from a pool peer")
	}
	clock.Advance(serverPeerIdleGrace - time.Second)
	if !w.idle.closeIfIdle(w.sessionManager, 0, 0) {
		t.Fatal("business frame disabled bounded orphan cleanup")
	}
}

func TestServerActiveSessionAndNewIdleTransitionRemainProtected(t *testing.T) {
	clock := newFakePoolClock()
	w := newServerIdleWorkerForTest(clock)
	serverIdlePingForTest(t, w)
	s := w.sessionManager.Allocate(&ClientStrategy{MaxConcurrency: 2})
	if s == nil {
		t.Fatal("allocate business session")
	}
	clock.Advance(6 * time.Hour)
	if w.idle.closeIfIdle(w.sessionManager, 0, 0) || w.idle.closeIfIdle(w.sessionManager, 1, 1) {
		t.Fatal("peer silence fallback terminated an active business session")
	}
	if err := s.Close(false); err != nil {
		t.Fatal(err)
	}
	clock.Advance(maxWorkerPoolProbeInterval*12/10 + 10*time.Second)
	if w.idle.closeIfIdle(w.sessionManager, 0, 1) {
		t.Fatal("long-lived business completion did not allow the next idle probe")
	}
	serverIdlePingForTest(t, w)
	clock.Advance(serverPeerIdleGrace)
	if !w.idle.closeIfIdle(w.sessionManager, 0, 1) {
		t.Fatal("session-end grace prevented eventual orphan cleanup")
	}
}

func TestServerPoolIdleCheckAndPingAreSerialized(t *testing.T) {
	for range 100 {
		clock := newFakePoolClock()
		w := newServerIdleWorkerForTest(clock)
		serverIdlePingForTest(t, w)
		clock.Advance(serverPeerIdleGrace - time.Nanosecond)
		var group sync.WaitGroup
		group.Add(2)
		var closed bool
		go func() { defer group.Done(); w.idle.received(true) }()
		go func() { defer group.Done(); closed = w.idle.closeIfIdle(w.sessionManager, 0, 0) }()
		group.Wait()
		if closed {
			t.Fatal("idle checker raced a Ping before expiry")
		}
		clock.Advance(time.Nanosecond)
		if w.idle.closeIfIdle(w.sessionManager, 0, 0) {
			t.Fatal("idle checker ignored the newly published Ping")
		}
	}
}

func TestServerPoolClientCloseIsImmediate(t *testing.T) {
	uplinkReader, uplinkWriter := pipe.New(pipe.WithoutSizeLimit())
	server, err := NewServerWorker(context.Background(), nil, &transport.Link{Reader: uplinkReader, Writer: buf.Discard})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { server.Close(); uplinkWriter.Close() })
	if err := writeProbeFrame(uplinkWriter, 42, false); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "server entered pool peer retention", func() bool {
		server.idle.mu.Lock()
		defer server.idle.mu.Unlock()
		return server.idle.peerProbes
	})
	if err := uplinkWriter.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case <-server.WaitClosed():
	case <-time.After(time.Second):
		t.Fatal("client TTL/transport close waited for the server peer grace")
	}
}

func TestServerPoolSessionBetweenIdleChecksReceivesGrace(t *testing.T) {
	clock := newFakePoolClock()
	w := newServerIdleWorkerForTest(clock)
	serverIdlePingForTest(t, w)
	clock.Advance(serverPeerIdleGrace)
	s := w.sessionManager.Allocate(&ClientStrategy{MaxConcurrency: 1})
	if s == nil {
		t.Fatal("allocate business session")
	}
	s.Close(false)
	// Even a snapshot taken after removal, before any on-empty callback could
	// run, must not reclaim a peer that only just became idle.
	if w.idle.closeIfIdle(w.sessionManager, 0, 1) {
		t.Fatal("short session between idle checks lost its next-probe grace")
	}
	clock.Advance(serverPeerIdleGrace)
	if !w.idle.closeIfIdle(w.sessionManager, 0, 1) {
		t.Fatal("idle transition grace was restarted on every check")
	}
}

func TestServerPoolOrphanCleanupInterruptsBlockedPong(t *testing.T) {
	clock := newFakePoolClock()
	w := newServerIdleWorkerForTest(clock)
	reader, writer := pipe.New(pipe.WithSizeLimit(64 * 1024))
	blocked := newBlockingProbeWriter()
	w.link = &transport.Link{Reader: reader, Writer: blocked}
	finished := make(chan struct{})
	go func() { defer close(finished); w.run(context.Background()) }()
	t.Cleanup(func() { w.Close(); writer.Close(); reader.Interrupt(); blocked.Interrupt() })
	if err := writeProbeFrame(writer, 42, false); err != nil {
		t.Fatal(err)
	}
	select {
	case <-blocked.started:
	case <-time.After(time.Second):
		t.Fatal("Pong write did not start")
	}
	// Both fields are published before the monitor starts. No need to wait
	// two real minutes to verify that cleanup interrupts the blocked writer.
	w.timer = time.NewTicker(time.Millisecond)
	go w.monitor()
	clock.Advance(serverPeerIdleGrace)
	select {
	case <-finished:
	case <-time.After(time.Second):
		t.Fatal("expired idle peer left its blocked Pong writer running")
	}
}
