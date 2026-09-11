package mux

import (
	"context"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func ioWorkerForTest(t *testing.T) (*fakePoolClock, *IncrementalWorkerPicker, *ClientWorker, *pipe.Writer) {
	t.Helper()
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	incoming, inject := pipe.New(pipe.WithoutSizeLimit())
	outgoing, writer := pipe.New(pipe.WithoutSizeLimit())
	worker, err := NewClientWorker(transport.Link{Reader: incoming, Writer: writer}, ClientStrategy{
		MaxConcurrency: 1, MaxConnection: 128, WorkerPool: cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() {
		picker.Close()
		inject.Close()
		outgoing.Interrupt()
	})
	return clock, picker, worker, inject
}

func controlReadForTest(t *testing.T, w *ClientWorker, inject *pipe.Writer) {
	t.Helper()
	w.poolAccess.Lock()
	id := w.pendingProbeID + 1 // Never acknowledge an outstanding probe.
	now := w.poolClock.Now()
	w.poolAccess.Unlock()
	if err := writeProbeFrame(inject, id, true); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "inbound control frame recorded", func() bool {
		w.poolAccess.Lock()
		defer w.poolAccess.Unlock()
		return w.lastRead.Equal(now)
	})
}

func TestRecentReceivedControlFrameReusesIdleWorkerWithoutProbe(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	controlReadForTest(t, worker, inject)
	clock.Advance(2 * time.Second)
	picker.onWorkerIdle(worker)
	if workerStateForTest(worker) != workerIdleReady || picker.WorkerPoolStats().ProbeSentTotal != 0 {
		t.Fatal("recent received control frame triggered a redundant probe")
	}
	reused, err := picker.PickAvailable()
	if err != nil || reused != worker {
		t.Fatalf("recently responsive worker was not reused: worker=%p err=%v", reused, err)
	}
}

func TestReceiveFreshnessExpiresFromLastResponse(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	lastResponse := clock.Now()
	controlReadForTest(t, worker, inject)
	clock.Advance(2 * time.Second)
	picker.onWorkerIdle(worker)
	worker.poolAccess.Lock()
	due := worker.nextProbeAt
	worker.poolAccess.Unlock()
	if !due.Equal(lastResponse.Add(picker.config.ProbeInterval)) {
		t.Fatalf("probe period restarted at session closure: due=%s", due)
	}
	clock.Advance(3*time.Second - time.Nanosecond)
	if worker.IsFull() || picker.WorkerPoolStats().ProbeSentTotal != 0 {
		t.Fatal("worker was not reusable within the I/O freshness window")
	}
	clock.Advance(time.Nanosecond)
	waitForTest(t, "probe at I/O freshness expiry", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	if !worker.IsFull() {
		t.Fatal("probing worker remained selectable")
	}
}

func TestRecentPongAllowsReuseWithoutRestartingWindow(t *testing.T) {
	clock, picker, worker, _ := ioWorkerForTest(t)
	picker.onWorkerIdle(worker)
	waitForTest(t, "initial Ping", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	ackedAt := clock.Now()
	worker.acceptPong(pendingProbeForTest(worker))
	waitForTest(t, "initial Pong", func() bool { return workerStateForTest(worker) == workerIdleReady })
	business := openFaultEchoSession(t, &ClientManager{Picker: picker})
	t.Cleanup(func() { business.input.Close() })
	clock.Advance(2 * time.Second)
	business.input.Close()
	waitForTest(t, "reuse based on recent Pong", func() bool { return workerStateForTest(worker) == workerIdleReady })
	worker.poolAccess.Lock()
	due := worker.nextProbeAt
	worker.poolAccess.Unlock()
	if !due.Equal(ackedAt.Add(picker.config.ProbeInterval)) || picker.WorkerPoolStats().ProbeSentTotal != 1 {
		t.Fatal("local End restarted the Pong window or caused a redundant probe")
	}
}

func TestStaleIOOrNoIOStillRequiresProbe(t *testing.T) {
	for _, age := range []time.Duration{0, 5 * time.Second, time.Hour} {
		t.Run(age.String(), func(t *testing.T) {
			clock, picker, worker, inject := ioWorkerForTest(t)
			if age != 0 {
				controlReadForTest(t, worker, inject)
				clock.Advance(age)
			}
			picker.onWorkerIdle(worker)
			waitForTest(t, "probe without recent I/O", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
		})
	}
}

func TestIdleReadRefreshesDeadlineButNotIdleTTL(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	controlReadForTest(t, worker, inject)
	picker.onWorkerIdle(worker)
	for range 5 {
		clock.Advance(4 * time.Second)
		if workerStateForTest(worker) == workerProbing {
			// Passive reads may postpone routine idle probes, but not the hard
			// bidirectional lease. Renew it with an actual matching Pong.
			worker.poolAccess.Lock()
			id := worker.pendingProbeID
			worker.poolAccess.Unlock()
			worker.acceptPong(id)
			waitForTest(t, "lease validated", func() bool { return workerStateForTest(worker) == workerIdleReady })
		}
		controlReadForTest(t, worker, inject)
	}
	if picker.WorkerPoolStats().ProbeAckTotal != 1 {
		t.Fatal("passive reads bypassed the bidirectional lease")
	}
	clock.Advance(4 * time.Second)
	waitForTest(t, "original idle TTL", worker.Closed)
	if picker.WorkerPoolStats().WorkerClosedIdleTTLTotal != 1 {
		t.Fatal("I/O extended the original business idle TTL")
	}
}

func TestIOWhileProbingCannotExtendTimeoutOrAcknowledgeProbe(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	picker.onWorkerIdle(worker)
	waitForTest(t, "initial Ping", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	id := pendingProbeForTest(worker)
	clock.Advance(time.Second)
	controlReadForTest(t, worker, inject)
	if workerStateForTest(worker) != workerProbing {
		t.Fatal("unrelated I/O acknowledged the pending probe")
	}
	clock.Advance(time.Second)
	waitForTest(t, "original two-second timeout", worker.Closed)
	worker.acceptPong(id)
	if workerStateForTest(worker) != workerClosed || picker.WorkerPoolStats().ProbeTimeoutTotal != 1 {
		t.Fatal("I/O or late Pong revived a timed-out worker")
	}
}

func TestQueuedProbeBecomesReusableAfterRead(t *testing.T) {
	_, picker, worker, inject := ioWorkerForTest(t)
	other := newBlackholeWorker(t, picker.config, 1)
	addWorkerForTest(t, picker, other, workerActive, picker.now())
	picker.onWorkerIdle(other)
	picker.onWorkerIdle(worker)
	if workerStateForTest(worker) != workerProbeQueued {
		t.Fatal("second probe was not queued")
	}
	controlReadForTest(t, worker, inject)
	selected, err := picker.PickAvailable()
	if err != nil || selected != worker {
		t.Fatalf("fresh I/O did not release queued worker: worker=%p err=%v", selected, err)
	}
}

func TestExpiredIOWindowCannotBeReusedBeforeTimerRuns(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	controlReadForTest(t, worker, inject)
	picker.onWorkerIdle(worker)
	// Let time pass without running the timer callback.
	clock.mu.Lock()
	clock.now = clock.now.Add(picker.config.ProbeInterval)
	clock.mu.Unlock()
	if !worker.IsFull() {
		t.Fatal("expired idle worker was advertised as reusable")
	}
	selected, err := picker.PickAvailable()
	if err != nil || selected == worker {
		t.Fatalf("request reused expired I/O evidence: worker=%p err=%v", selected, err)
	}
}

func TestLocalEndRequiresProbeWithoutRecentReceive(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	// Use the production uplink capacity without any consumer or remote data.
	// Only local writes can succeed; none can prove that a peer is alive.
	uplinkReader, uplinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
	downlinkReader, downlinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
	worker, err := NewClientWorker(transport.Link{Reader: downlinkReader, Writer: uplinkWriter}, ClientStrategy{
		MaxConcurrency: 4, MaxConnection: 128, WorkerPool: cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() {
		picker.Close()
		uplinkReader.Interrupt()
		downlinkWriter.Close()
	})
	manager := &ClientManager{Picker: picker}
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("echo.test"), 443),
	}})
	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	t.Cleanup(func() { inputWriter.Close() })
	if err := inputWriter.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("payload"))}); err != nil {
		t.Fatal(err)
	}
	if err := manager.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}); err != nil {
		t.Fatal(err)
	}
	clock.Advance(cfg.ProbeInterval + time.Second)
	inputWriter.Close() // Actual fetchInput EOF -> End write -> onWorkerIdle.
	waitForTest(t, "idle transition after local EOF", func() bool {
		return workerStateForTest(worker) != workerActive
	})
	if workerStateForTest(worker) != workerProbing || !worker.IsFull() {
		t.Fatal("local End frame made an unresponsive worker reusable")
	}
	waitForTest(t, "active and idle validation Pings", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 2 })
	// The peer must actually return the matching Pong before this worker can
	// be offered again. Neither the End nor the outgoing Ping is sufficient.
	if err := writeProbeFrame(downlinkWriter, pendingProbeForTest(worker), true); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "matching Pong allows reuse", func() bool { return workerStateForTest(worker) == workerIdleReady })
	selected, err := picker.PickAvailable()
	if err != nil || selected != worker {
		t.Fatalf("validated worker was not reusable: worker=%p err=%v", selected, err)
	}
}
