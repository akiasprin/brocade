package mux

import (
	"testing"
	"time"

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

func controlIOForTest(t *testing.T, w *ClientWorker, inject *pipe.Writer, direction string) {
	t.Helper()
	w.poolAccess.Lock()
	id := w.pendingProbeID + 1 // Never acknowledge an outstanding probe.
	now := w.poolClock.Now()
	w.poolAccess.Unlock()
	if direction == "read" {
		if err := writeProbeFrame(inject, id, true); err != nil {
			t.Fatal(err)
		}
	} else if err := writeProbeFrame(w.link.Writer, id, true); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "control-frame I/O recorded", func() bool {
		w.poolAccess.Lock()
		defer w.poolAccess.Unlock()
		return w.lastIO.Equal(now)
	})
}

func TestRecentControlIOReusesIdleWorkerWithoutProbe(t *testing.T) {
	for _, direction := range []string{"read", "write"} {
		t.Run(direction, func(t *testing.T) {
			clock, picker, worker, inject := ioWorkerForTest(t)
			controlIOForTest(t, worker, inject, direction)
			clock.Advance(2 * time.Second)
			picker.onWorkerIdle(worker)
			if workerStateForTest(worker) != workerIdleReady || picker.WorkerPoolStats().ProbeSentTotal != 0 {
				t.Fatal("recent control-frame I/O triggered a redundant probe")
			}
			reused, err := picker.PickAvailable()
			if err != nil || reused != worker {
				t.Fatalf("recently active worker was not reused: worker=%p err=%v", reused, err)
			}
		})
	}
}

func TestIOFreshnessExpiresFromLastActivity(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	lastIO := clock.Now()
	controlIOForTest(t, worker, inject, "read")
	clock.Advance(2 * time.Second)
	picker.onWorkerIdle(worker)
	worker.poolAccess.Lock()
	due := worker.nextProbeAt
	worker.poolAccess.Unlock()
	if !due.Equal(lastIO.Add(picker.config.ProbeInterval)) {
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

func TestStaleIOOrNoIOStillRequiresProbe(t *testing.T) {
	for _, age := range []time.Duration{0, 5 * time.Second, time.Hour} {
		t.Run(age.String(), func(t *testing.T) {
			clock, picker, worker, inject := ioWorkerForTest(t)
			if age != 0 {
				controlIOForTest(t, worker, inject, "read")
				clock.Advance(age)
			}
			picker.onWorkerIdle(worker)
			waitForTest(t, "probe without recent I/O", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
		})
	}
}

func TestIdleIORefreshesDeadlineButNotIdleTTL(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	controlIOForTest(t, worker, inject, "read")
	picker.onWorkerIdle(worker)
	for range 5 {
		clock.Advance(4 * time.Second)
		controlIOForTest(t, worker, inject, "write")
	}
	if picker.WorkerPoolStats().ProbeSentTotal != 0 {
		t.Fatal("idle control traffic failed to postpone probing")
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
	controlIOForTest(t, worker, inject, "read")
	controlIOForTest(t, worker, inject, "write")
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

func TestQueuedProbeBecomesReusableAfterIO(t *testing.T) {
	_, picker, worker, inject := ioWorkerForTest(t)
	other := newBlackholeWorker(t, picker.config, 1)
	addWorkerForTest(t, picker, other, workerActive, picker.now())
	picker.onWorkerIdle(other)
	picker.onWorkerIdle(worker)
	if workerStateForTest(worker) != workerProbeQueued {
		t.Fatal("second probe was not queued")
	}
	controlIOForTest(t, worker, inject, "read")
	selected, err := picker.PickAvailable()
	if err != nil || selected != worker {
		t.Fatalf("fresh I/O did not release queued worker: worker=%p err=%v", selected, err)
	}
}

func TestExpiredIOWindowCannotBeReusedBeforeTimerRuns(t *testing.T) {
	clock, picker, worker, inject := ioWorkerForTest(t)
	controlIOForTest(t, worker, inject, "read")
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
