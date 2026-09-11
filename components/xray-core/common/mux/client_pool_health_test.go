package mux

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func dispatchHealthTestSession(t *testing.T, w *ClientWorker) *pipe.Writer {
	t.Helper()
	r, input := pipe.New(pipe.WithoutSizeLimit())
	_, output := pipe.New(pipe.WithoutSizeLimit())
	t.Cleanup(func() { input.Close() })
	if !w.Dispatch(lifecycleTestContext(context.Background()), &transport.Link{Reader: r, Writer: output}) {
		t.Fatal("dispatch rejected")
	}
	return input
}

func healthProbeForTest(w *ClientWorker) uint64 {
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	return w.healthProbeID
}

func activeHealthFixture(t *testing.T) (*fakePoolClock, *IncrementalWorkerPicker, *ClientWorker) {
	t.Helper()
	clock, cfg := newFakePoolClock(), testPoolConfig()
	cfg.ReuseThreshold = 1
	p := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return newBlackholeWorker(t, cfg, 8), nil }})
	t.Cleanup(func() { p.Close() })
	w, err := p.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	dispatchHealthTestSession(t, w)
	return clock, p, w
}

func TestActiveHealthLeaseProbeRecoveryAndDrain(t *testing.T) {
	clock, p, w := activeHealthFixture(t)
	clock.Advance(p.config.ProbeInterval)
	id := healthProbeForTest(w)
	if id == 0 || w.IsFull() {
		t.Fatal("routine probe must run without quarantining a valid lease")
	}
	w.acceptPong(id + 1)
	if p.WorkerPoolStats().ProbeAckTotal != 0 {
		t.Fatal("unmatched ACK accepted")
	}
	clock.Advance(p.config.ProbeTimeout)
	if !w.IsFull() || w.Closed() || w.ActiveConnections() != 1 {
		t.Fatal("suspect must stop new admission, not existing business")
	}
	first := healthProbeForTest(w)
	w.acceptPong(id) // late routine ACK cannot recover.
	if healthProbeForTest(w) != first {
		t.Fatal("late ACK changed confirmation")
	}
	w.acceptPong(first)
	if !w.IsFull() {
		t.Fatal("one confirmation ACK is not recovery")
	}
	second := healthProbeForTest(w)
	if second == 0 || second == first {
		t.Fatal("missing second independent confirmation")
	}
	w.acceptPong(second)
	if w.IsFull() || p.WorkerPoolStats().HealthRecoveredTotal != 1 {
		t.Fatal("two ACKs did not recover")
	}
	_, snapshots := p.muxSnapshot(clock.Now())
	if snapshots[0].State != "READY" || snapshots[0].LeaseRemainingMS != p.config.healthLease().Milliseconds() {
		t.Fatalf("bad recovered snapshot: %+v", snapshots[0])
	}
	clock.Advance(p.config.ProbeInterval)
	clock.Advance(p.config.ProbeTimeout)
	clock.Advance(p.config.confirmTimeout())
	if workerStateForTest(w) != workerDraining || w.Closed() || w.ActiveConnections() != 1 {
		t.Fatal("confirmation expiry killed existing business or retained admission")
	}
	if p.WorkerPoolStats().HealthDrainingTotal != 1 {
		t.Fatal("missing draining counter")
	}
	w.acceptPong(second)
	if workerStateForTest(w) != workerDraining {
		t.Fatal("late ACK resurrected draining worker")
	}
}

func TestActiveControlQueueFailureQuarantinesWithoutClosingBusiness(t *testing.T) {
	clock, cfg := newFakePoolClock(), testPoolConfig()
	reader, input := pipe.New()
	blocked := newBlockingProbeWriter()
	w, err := NewClientWorker(transport.Link{Reader: reader, Writer: blocked}, ClientStrategy{MaxConcurrency: 8, MaxConnection: 128, WorkerPool: cfg})
	if err != nil {
		t.Fatal(err)
	}
	p := newPoolPickerForTest(clock, cfg, &testWorkerFactory{})
	addWorkerForTest(t, p, w, workerActive, clock.Now())
	w.sessionManager.Allocate(&w.strategy)
	t.Cleanup(func() { p.Close(); input.Close() })
	go w.poolControl.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("blocked complete frame"))})
	select {
	case <-blocked.started:
	case <-time.After(time.Second):
		t.Fatal("writer not blocked")
	}
	for range cap(w.poolControl.control) {
		if err := w.poolControl.enqueue(FrameMetadata{SessionStatus: SessionStatusKeepAlive}); err != nil {
			t.Fatal(err)
		}
	}
	clock.Advance(cfg.ProbeInterval)
	p.runActiveHealth()
	if !w.IsFull() || w.Closed() || p.WorkerPoolStats().HealthQueueFailuresTotal != 1 {
		t.Fatal("queue failure did not quarantine safely")
	}
	clock.Advance(cfg.confirmTimeout())
	if w.Closed() || w.ActiveConnections() != 1 || workerStateForTest(w) != workerDraining {
		t.Fatal("local queue pressure killed business")
	}
}

func TestActiveHealthRejectsStaleAdmissionEvenWithoutTimer(t *testing.T) {
	for _, failGrowth := range []bool{false, true} {
		t.Run(map[bool]string{false: "replacement", true: "failed-growth-fallback"}[failGrowth], func(t *testing.T) {
			clock, p, w := activeHealthFixture(t)
			if failGrowth {
				p.Factory = &testWorkerFactory{create: func() (*ClientWorker, error) { return nil, errors.New("growth failed") }}
			}
			// Delay timer callbacks deliberately; even continuous receive evidence
			// cannot extend the bidirectional lease in either picker path.
			clock.mu.Lock()
			clock.now = clock.now.Add(p.config.healthLease())
			clock.mu.Unlock()
			w.recordInboundActivity()
			next, err := p.PickAvailable()
			if next == w || (failGrowth && err == nil) || (!failGrowth && err != nil) {
				t.Fatalf("stale worker admitted: next=%p old=%p err=%v", next, w, err)
			}
			if w.Closed() || w.ActiveConnections() != 1 {
				t.Fatal("old business was terminated")
			}
		})
	}
}

func TestActiveHealthRechecksReservationAtDispatch(t *testing.T) {
	clock, p, w := activeHealthFixture(t)
	chosen, err := p.PickAvailable()
	if err != nil || chosen != w {
		t.Fatal(err)
	}
	clock.mu.Lock()
	clock.now = clock.now.Add(p.config.healthLease())
	clock.mu.Unlock()
	if w.Dispatch(lifecycleTestContext(context.Background()), &transport.Link{}) {
		t.Fatal("stale reservation admitted")
	}
	w.poolAccess.Lock()
	reserved := w.poolReservations
	w.poolAccess.Unlock()
	if reserved != 0 || w.ActiveConnections() != 1 {
		t.Fatal("reservation leaked or old session lost")
	}
}

func TestActiveHealthReplacementBackoffAndProbeBudget(t *testing.T) {
	clock, p, w := activeHealthFixture(t)
	clock.Advance(p.config.healthLease() + p.config.confirmTimeout())
	replacement, err := p.PickAvailable()
	if err != nil || replacement == w {
		t.Fatal("replacement unavailable", err)
	}
	dispatchHealthTestSession(t, replacement)
	// Exhaust healthy replacement slots; a dead route must not trigger an
	// unbounded burst of overflow dials while old business remains draining.
	for range 7 {
		next, err := p.PickAvailable()
		if err != nil || next != replacement {
			t.Fatal(err)
		}
		dispatchHealthTestSession(t, next)
	}
	before := p.Factory.(*testWorkerFactory).count()
	for range 20 {
		if _, err := p.PickAvailable(); err == nil {
			t.Fatal("replacement backoff bypassed")
		}
	}
	if p.Factory.(*testWorkerFactory).count() != before {
		t.Fatal("backoff created extra carriers")
	}
	p.access.Lock()
	_, probing := p.poolCountsLocked(nil)
	p.access.Unlock()
	if probing > p.config.MaxProbingWorkers {
		t.Fatal("active probes exceeded shared budget")
	}
	id := healthProbeForTest(replacement)
	if id == 0 {
		t.Fatal("replacement should validate immediately")
	}
	replacement.acceptPong(id)
	if _, err := p.PickAvailable(); err != nil {
		t.Fatal("confirmed replacement did not clear backoff", err)
	}
}

func TestSessionEmptyCallbackDoesNotTakePickerLockWhileOtherBusinessRuns(t *testing.T) {
	_, p, w := activeHealthFixture(t)
	chosen, err := p.PickAvailable()
	if err != nil || chosen != w {
		t.Fatal(err)
	}
	dispatchHealthTestSession(t, w)
	// Holding picker.access must not delay a per-session finish while another
	// business stream is still present. The single map-empty transition owns it.
	p.access.Lock()
	finished := make(chan struct{})
	go func() { w.onSessionEmpty(); close(finished) }()
	select {
	case <-finished:
	case <-time.After(time.Second):
		p.access.Unlock()
		t.Fatal("per-session completion acquired picker lock")
	}
	p.access.Unlock()
}

func TestActiveAndIdleProbeBudgetUsesOldestDue(t *testing.T) {
	clock, p, first := activeHealthFixture(t)
	clock.Advance(p.config.ProbeInterval)
	id := healthProbeForTest(first)
	second := newBlackholeWorker(t, p.config, 8)
	addWorkerForTest(t, p, second, workerActive, clock.Now())
	second.sessionManager.Allocate(&second.strategy)
	second.poolAccess.Lock()
	second.healthNextProbe = clock.Now()
	second.poolAccess.Unlock()
	idle := newBlackholeWorker(t, p.config, 8)
	addWorkerForTest(t, p, idle, workerProbeQueued, clock.Now())
	idle.poolAccess.Lock()
	idle.idleSince = clock.Now()
	idle.nextProbeAt = clock.Now().Add(-time.Second)
	idle.poolAccess.Unlock()
	p.runActiveHealth()
	if healthProbeForTest(second) != 0 || workerStateForTest(idle) != workerProbeQueued {
		t.Fatal("shared probe budget exceeded")
	}
	first.acceptPong(id)
	waitForTest(t, "older idle validation starts first", func() bool { return workerStateForTest(idle) == workerProbing })
	if healthProbeForTest(second) != 0 {
		t.Fatal("active probe jumped older idle queue")
	}
	waitForTest(t, "idle Ping written", func() bool { idle.poolAccess.Lock(); defer idle.poolAccess.Unlock(); return idle.poolProbes == 1 })
	idle.acceptPong(pendingProbeForTest(idle))
	waitForTest(t, "active probe receives released budget", func() bool { return healthProbeForTest(second) != 0 })
}

func TestDrainingReleasesActiveProbeBudget(t *testing.T) {
	clock, p, w := activeHealthFixture(t)
	clock.Advance(p.config.ProbeInterval)
	if healthProbeForTest(w) == 0 {
		t.Fatal("missing routine probe")
	}
	w.poolAccess.Lock()
	w.strategy.MaxConnection = 2
	w.poolAccess.Unlock()
	chosen, err := p.PickAvailable()
	if err != nil || chosen != w {
		t.Fatal(err)
	}
	dispatchHealthTestSession(t, w)
	if workerStateForTest(w) != workerDraining || healthProbeForTest(w) != 0 {
		t.Fatal("request-limit drain retained active probe budget")
	}
}

var _ buf.Writer = (*sessionFrameWriter)(nil)
