package mux

import (
	"context"
	"errors"
	"runtime"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/proxy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/pipe"
)

type fakePoolTimer struct {
	clock   *fakePoolClock
	at      time.Time
	fn      func()
	stopped bool
	fired   bool
}

func (t *fakePoolTimer) Stop() bool {
	t.clock.mu.Lock()
	defer t.clock.mu.Unlock()
	if t.stopped || t.fired {
		return false
	}
	t.stopped = true
	return true
}

type fakePoolClock struct {
	mu     sync.Mutex
	now    time.Time
	timers []*fakePoolTimer
}

func newFakePoolClock() *fakePoolClock {
	return &fakePoolClock{now: time.Unix(1_700_000_000, 0)}
}

func (c *fakePoolClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakePoolClock) AfterFunc(d time.Duration, fn func()) poolTimer {
	c.mu.Lock()
	defer c.mu.Unlock()
	timer := &fakePoolTimer{clock: c, at: c.now.Add(d), fn: fn}
	c.timers = append(c.timers, timer)
	return timer
}

func (c *fakePoolClock) Advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
	for {
		var due []*fakePoolTimer
		c.mu.Lock()
		for _, timer := range c.timers {
			if !timer.stopped && !timer.fired && !timer.at.After(c.now) {
				timer.fired = true
				due = append(due, timer)
			}
		}
		c.mu.Unlock()
		if len(due) == 0 {
			return
		}
		for _, timer := range due {
			timer.fn()
		}
	}
}

type blockingProbeWriter struct {
	started     chan struct{}
	interrupted chan struct{}
	startOnce   sync.Once
	stopOnce    sync.Once
}

func newBlockingProbeWriter() *blockingProbeWriter {
	return &blockingProbeWriter{
		started:     make(chan struct{}),
		interrupted: make(chan struct{}),
	}
}

func (w *blockingProbeWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	w.startOnce.Do(func() { close(w.started) })
	<-w.interrupted
	buf.ReleaseMulti(mb)
	return errors.New("probe writer interrupted")
}

func (w *blockingProbeWriter) Interrupt() {
	w.stopOnce.Do(func() { close(w.interrupted) })
}

func (c *fakePoolClock) pending() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	count := 0
	for _, timer := range c.timers {
		if !timer.stopped && !timer.fired {
			count++
		}
	}
	return count
}

type testWorkerFactory struct {
	mu      sync.Mutex
	create  func() (*ClientWorker, error)
	created int
}

type contextBlockingOutbound struct {
	started sync.Once
	start   chan struct{}
	stop    chan struct{}
}

func newContextBlockingOutbound() *contextBlockingOutbound {
	return &contextBlockingOutbound{start: make(chan struct{}), stop: make(chan struct{})}
}

func (o *contextBlockingOutbound) Process(ctx context.Context, _ *transport.Link, _ internet.Dialer) error {
	o.started.Do(func() { close(o.start) })
	<-ctx.Done()
	close(o.stop)
	return ctx.Err()
}

var _ proxy.Outbound = (*contextBlockingOutbound)(nil)

func (f *testWorkerFactory) Create() (*ClientWorker, error) {
	f.mu.Lock()
	f.created++
	create := f.create
	f.mu.Unlock()
	return create()
}

func (f *testWorkerFactory) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.created
}

func testPoolConfig() *WorkerPoolConfig {
	return &WorkerPoolConfig{
		MinIdleWorkers:    0,
		MaxIdleWorkers:    2,
		MaxProbingWorkers: 1,
		ProbeInterval:     5 * time.Second,
		ProbeTimeout:      2 * time.Second,
		IdleTTL:           24 * time.Second,
	}
}

func TestDefaultPoolJitterNeverShortensInterval(t *testing.T) {
	const interval = 5 * time.Second
	for i := 0; i < 10_000; i++ {
		got := defaultPoolJitter(interval)
		if got < interval || got >= interval+interval/5 {
			t.Fatalf("defaultPoolJitter(%s) = %s, want [%s, %s)", interval, got, interval, interval+interval/5)
		}
	}
}

func newBlackholeWorker(t *testing.T, cfg *WorkerPoolConfig, concurrency uint32) *ClientWorker {
	t.Helper()
	inputReader, _ := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	worker, err := NewClientWorker(transport.Link{Reader: inputReader, Writer: outputWriter}, ClientStrategy{
		MaxConcurrency: concurrency,
		MaxConnection:  128,
		WorkerPool:     cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	return worker
}

func newPoolPickerForTest(clock *fakePoolClock, cfg *WorkerPoolConfig, factory ClientWorkerFactory) *IncrementalWorkerPicker {
	return &IncrementalWorkerPicker{
		Factory: factory,
		Pool:    cfg,
		config:  cfg,
		clock:   clock,
		jitter:  func(d time.Duration) time.Duration { return d },
	}
}

func waitForTest(t *testing.T, what string, condition func() bool) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for !condition() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		runtime.Gosched()
	}
}

func workerStateForTest(worker *ClientWorker) clientWorkerState {
	state, _ := worker.poolStateSnapshot()
	return state
}

func pendingProbeForTest(worker *ClientWorker) uint64 {
	worker.poolAccess.Lock()
	defer worker.poolAccess.Unlock()
	return worker.pendingProbeID
}

func addWorkerForTest(t *testing.T, picker *IncrementalWorkerPicker, worker *ClientWorker, state clientWorkerState, now time.Time) {
	t.Helper()
	picker.access.Lock()
	defer picker.access.Unlock()
	if !worker.attachPool(picker, state, now) {
		t.Fatal("attachPool() failed")
	}
	if state == workerIdleReady {
		worker.poolAccess.Lock()
		worker.idleSince = now
		worker.nextProbeAt = now.Add(picker.config.ProbeInterval)
		worker.poolAccess.Unlock()
	}
	picker.workers = append(picker.workers, worker)
}

func TestPooledWorkerProbeSuccessAndStalePong(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "probe start", func() bool { return workerStateForTest(worker) == workerProbing && clock.pending() > 0 })
	probeID := pendingProbeForTest(worker)
	worker.acceptPong(probeID - 1)
	if state := workerStateForTest(worker); state != workerProbing {
		t.Fatalf("stale Pong changed state to %v", state)
	}
	worker.acceptPong(probeID)
	waitForTest(t, "idle-ready", func() bool { return workerStateForTest(worker) == workerIdleReady })
	if worker.IsFull() {
		t.Fatal("verified idle worker is unavailable")
	}
	worker.acceptPong(probeID)
	if state := workerStateForTest(worker); state != workerIdleReady {
		t.Fatalf("duplicate Pong changed state to %v", state)
	}
	stats := picker.WorkerPoolStats()
	if stats.ProbeSentTotal != 1 || stats.ProbeAckTotal != 1 || stats.WorkersIdleReady != 1 {
		t.Fatalf("probe stats = %+v", stats)
	}
}

func TestStaleProbeGenerationCannotCompleteCurrentProbe(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "probe start", func() bool { return workerStateForTest(worker) == workerProbing })
	worker.poolAccess.Lock()
	stale := probeRun{
		worker:     worker,
		id:         worker.pendingProbeID,
		generation: worker.probeGeneration,
		result:     worker.pendingProbe,
	}
	worker.probeGeneration++
	worker.pendingProbe = make(chan struct{}, 1)
	worker.poolAccess.Unlock()

	picker.onProbeFinished(stale, probeSucceeded)
	if state := workerStateForTest(worker); state != workerProbing {
		t.Fatalf("stale generation changed state to %v", state)
	}
}

func TestPooledWorkerProbeTimeoutClosesWorker(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "probe timeout timer", func() bool { return clock.pending() > 0 })
	clock.Advance(cfg.ProbeTimeout)
	waitForTest(t, "worker close", worker.Closed)
	if state := workerStateForTest(worker); state != workerClosed {
		t.Fatalf("state after timeout = %v", state)
	}
	stats := picker.WorkerPoolStats()
	if stats.ProbeTimeoutTotal != 1 || stats.WorkerClosedProbeTotal != 1 {
		t.Fatalf("timeout stats = %+v", stats)
	}
}

func TestProbeTimeoutInterruptsBlockedPingWrite(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	reader, _ := pipe.New(pipe.WithoutSizeLimit())
	writer := newBlockingProbeWriter()
	worker, err := NewClientWorker(transport.Link{Reader: reader, Writer: writer}, ClientStrategy{
		MaxConcurrency: 1,
		MaxConnection:  128,
		WorkerPool:     cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "blocked Ping write", func() bool {
		select {
		case <-writer.started:
			return clock.pending() > 0
		default:
			return false
		}
	})
	clock.Advance(cfg.ProbeTimeout)
	waitForTest(t, "blocked writer interruption", worker.Closed)
	waitForTest(t, "probe writer return", func() bool {
		select {
		case <-writer.interrupted:
			return true
		default:
			return false
		}
	})
	stats := picker.WorkerPoolStats()
	if stats.ProbeTimeoutTotal != 1 || stats.WorkerClosedProbeTotal != 1 {
		t.Fatalf("blocked-write timeout stats = %+v", stats)
	}
}

func TestPongJustBeforeTimeoutKeepsWorkerReady(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "probe timer", func() bool { return pendingProbeForTest(worker) != 0 && clock.pending() > 0 })
	clock.Advance(cfg.ProbeTimeout - time.Millisecond)
	worker.acceptPong(pendingProbeForTest(worker))
	waitForTest(t, "idle-ready before timeout", func() bool { return workerStateForTest(worker) == workerIdleReady })
	if worker.Closed() || picker.WorkerPoolStats().ProbeTimeoutTotal != 0 {
		t.Fatal("Pong before the deadline was treated as a timeout")
	}
}

func TestAcceptedPongWinsBeforeTimeoutCallback(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerProbing, clock.Now())
	t.Cleanup(func() { picker.Close() })

	worker.poolAccess.Lock()
	worker.idleSince = clock.Now()
	run := worker.beginProbeLocked(workerProbing)
	worker.poolAccess.Unlock()
	worker.acceptPong(run.id)
	picker.onProbeFinished(run, probeTimedOut)
	if state := workerStateForTest(worker); state != workerProbing {
		t.Fatalf("accepted Pong lost to timeout callback: state=%v", state)
	}
	picker.onProbeFinished(run, probeSucceeded)
	if state := workerStateForTest(worker); state != workerIdleReady {
		t.Fatalf("accepted Pong did not complete probe: state=%v", state)
	}
	stats := picker.WorkerPoolStats()
	if stats.ProbeAckTotal != 1 || stats.ProbeTimeoutTotal != 0 {
		t.Fatalf("Pong/timeout race stats = %+v", stats)
	}
}

func TestSessionCloseGapCannotBypassIdleTransition(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	var replacement *ClientWorker
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		replacement = newBlackholeWorker(t, cfg, 1)
		return replacement, nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	manager := &ClientManager{Enabled: true, Picker: picker}
	first := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, first, workerIdleReady, clock.Now())
	t.Cleanup(func() { manager.Close() })

	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
	}})
	firstReader, firstInput := pipe.New(pipe.WithoutSizeLimit())
	_, firstOutput := pipe.New(pipe.WithoutSizeLimit())
	if err := manager.Dispatch(ctx, &transport.Link{Reader: firstReader, Writer: firstOutput}); err != nil {
		t.Fatal(err)
	}

	emptyReached := make(chan struct{})
	releaseEmpty := make(chan struct{})
	var emptyOnce sync.Once
	var releaseOnce sync.Once
	t.Cleanup(func() { releaseOnce.Do(func() { close(releaseEmpty) }) })
	first.sessionManager.SetOnEmpty(func() {
		emptyOnce.Do(func() { close(emptyReached) })
		<-releaseEmpty
		first.onSessionEmpty()
	})
	if err := firstInput.Close(); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "session removal before onEmpty", func() bool {
		select {
		case <-emptyReached:
			return first.ActiveConnections() == 0 && workerStateForTest(first) == workerActive
		default:
			return false
		}
	})

	secondReader, secondInput := pipe.New(pipe.WithoutSizeLimit())
	_, secondOutput := pipe.New(pipe.WithoutSizeLimit())
	if err := manager.Dispatch(ctx, &transport.Link{Reader: secondReader, Writer: secondOutput}); err != nil {
		t.Fatal(err)
	}
	if replacement == nil || replacement == first || factory.count() != 1 {
		t.Fatal("request reused the worker before its session-close callback completed")
	}
	if first.ActiveConnections() != 0 {
		t.Fatal("closing worker accepted a request before completing its idle transition")
	}
	releaseOnce.Do(func() { close(releaseEmpty) })
	waitForTest(t, "old worker idle", func() bool { return workerStateForTest(first) == workerIdleReady })
	_ = secondInput.Close()
}

func TestOrdinaryProbeSuccessDoesNotResetWarmBackoff(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerProbing, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.access.Lock()
	picker.warmFailures = 4
	picker.nextWarmAttempt = clock.Now().Add(8 * time.Second)
	picker.access.Unlock()
	worker.poolAccess.Lock()
	worker.idleSince = clock.Now()
	run := worker.beginProbeLocked(workerProbing)
	worker.poolAccess.Unlock()
	worker.acceptPong(run.id)
	picker.onProbeFinished(run, probeSucceeded)
	picker.access.Lock()
	failures := picker.warmFailures
	nextAttempt := picker.nextWarmAttempt
	picker.access.Unlock()
	if failures != 4 || nextAttempt.IsZero() {
		t.Fatalf("ordinary Pong reset warm backoff: failures=%d next=%s", failures, nextAttempt)
	}
}

func TestProbeTimeoutWarningIsRateLimitedPerPicker(t *testing.T) {
	clock := newFakePoolClock()
	picker := newPoolPickerForTest(clock, testPoolConfig(), &testWorkerFactory{})

	picker.access.Lock()
	log, suppressed := picker.probeTimeoutLogLocked(clock.Now())
	picker.access.Unlock()
	if !log || suppressed != 0 {
		t.Fatalf("first timeout log = (%v, %d), want (true, 0)", log, suppressed)
	}

	for range 3 {
		picker.access.Lock()
		log, suppressed = picker.probeTimeoutLogLocked(clock.Now())
		picker.access.Unlock()
		if log || suppressed != 0 {
			t.Fatalf("repeated timeout log = (%v, %d), want suppressed", log, suppressed)
		}
	}

	clock.Advance(probeTimeoutLogInterval)
	picker.access.Lock()
	log, suppressed = picker.probeTimeoutLogLocked(clock.Now())
	picker.access.Unlock()
	if !log || suppressed != 3 {
		t.Fatalf("summary timeout log = (%v, %d), want (true, 3)", log, suppressed)
	}
}

func TestPickerNeverReturnsProbingWorker(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	var replacement *ClientWorker
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		replacement = newBlackholeWorker(t, cfg, 1)
		return replacement, nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	probing := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, probing, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })
	picker.onWorkerIdle(probing)
	waitForTest(t, "probing state", func() bool { return workerStateForTest(probing) == workerProbing })

	chosen, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if chosen == probing || chosen != replacement {
		t.Fatalf("PickAvailable() chose probing worker: %p", chosen)
	}
}

func TestPickerPrefersActiveWorkerBeforeIdleReady(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 2), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	idle := newBlackholeWorker(t, cfg, 2)
	active := newBlackholeWorker(t, cfg, 2)
	addWorkerForTest(t, picker, idle, workerIdleReady, clock.Now())
	addWorkerForTest(t, picker, active, workerActive, clock.Now())
	if session := active.sessionManager.Allocate(&active.strategy); session == nil {
		t.Fatal("failed to make active worker carry a session")
	}
	t.Cleanup(func() { picker.Close() })

	chosen, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if chosen != active {
		t.Fatalf("picker chose %p, want active worker %p", chosen, active)
	}
}

func TestPickerReturnsTheWorkerItReservedAfterReordering(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	selected := newBlackholeWorker(t, cfg, 1)
	unavailable := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, selected, workerIdleReady, clock.Now())
	addWorkerForTest(t, picker, unavailable, workerProbing, clock.Now())
	t.Cleanup(func() { picker.Close() })

	worker, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if worker != selected {
		t.Fatal("picker returned a different worker after moving the reserved worker to the back")
	}
	selected.poolAccess.Lock()
	reservations := selected.poolReservations
	selected.poolAccess.Unlock()
	if reservations != 1 {
		t.Fatalf("selected worker reservations = %d, want 1", reservations)
	}
}

func TestMaxProbingQueuesAndPromotes(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	first := newBlackholeWorker(t, cfg, 1)
	second := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, first, workerActive, clock.Now())
	addWorkerForTest(t, picker, second, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(first)
	picker.onWorkerIdle(second)
	if state := workerStateForTest(first); state != workerProbing {
		t.Fatalf("first state = %v", state)
	}
	if state := workerStateForTest(second); state != workerProbeQueued {
		t.Fatalf("second state = %v", state)
	}
	first.acceptPong(pendingProbeForTest(first))
	waitForTest(t, "queued worker promotion", func() bool { return workerStateForTest(second) == workerProbing })
}

func TestTransportClosePromotesQueuedProbe(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	first := newBlackholeWorker(t, cfg, 1)
	second := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, first, workerActive, clock.Now())
	addWorkerForTest(t, picker, second, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(first)
	picker.onWorkerIdle(second)
	if state := workerStateForTest(second); state != workerProbeQueued {
		t.Fatalf("second state = %v, want probe-queued", state)
	}
	if err := first.Close(); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "queued probe promotion after transport close", func() bool {
		return workerStateForTest(second) == workerProbing
	})
}

func TestMaxIdleEvictsOldestReadyWorker(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MaxProbingWorkers = 2
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() { picker.Close() })

	makeReady := func() *ClientWorker {
		worker := newBlackholeWorker(t, cfg, 1)
		addWorkerForTest(t, picker, worker, workerActive, clock.Now())
		picker.onWorkerIdle(worker)
		waitForTest(t, "probe start", func() bool { return workerStateForTest(worker) == workerProbing })
		worker.acceptPong(pendingProbeForTest(worker))
		waitForTest(t, "worker ready", func() bool { return workerStateForTest(worker) == workerIdleReady })
		return worker
	}
	first := makeReady()
	clock.Advance(time.Second)
	second := makeReady()
	clock.Advance(time.Second)

	third := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, third, workerActive, clock.Now())
	picker.onWorkerIdle(third)
	waitForTest(t, "oldest worker close", first.Closed)
	if second.Closed() {
		t.Fatal("newer idle worker was evicted")
	}
	if state := workerStateForTest(third); state != workerProbing {
		t.Fatalf("replacement state = %v", state)
	}
	picker.access.Lock()
	reserved, _ := picker.poolCountsLocked(nil)
	picker.access.Unlock()
	if reserved != cfg.MaxIdleWorkers {
		t.Fatalf("reserved idle workers = %d, want %d", reserved, cfg.MaxIdleWorkers)
	}
	if got := picker.WorkerPoolStats().WorkerClosedMaxIdleTotal; got != 1 {
		t.Fatalf("max-idle close count = %d, want 1", got)
	}
}

func TestLargeProbeLimitUsesActualWorkerCount(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MaxIdleWorkers = ^uint32(0)
	cfg.MaxProbingWorkers = ^uint32(0)
	if err := cfg.Validate(65535); err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerProbeQueued, clock.Now())
	t.Cleanup(func() { picker.Close() })
	worker.poolAccess.Lock()
	worker.idleSince = clock.Now()
	worker.poolAccess.Unlock()
	picker.access.Lock()
	runs := picker.promoteQueuedLocked(clock.Now())
	picker.access.Unlock()
	if len(runs) != 1 || cap(runs) != 1 {
		t.Fatalf("probe queue allocation followed the configured ceiling: len=%d cap=%d", len(runs), cap(runs))
	}
}

func TestMaxRequestsClosesWorkerAfterLastSession(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	worker.strategy.MaxConnection = 2
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
	}})

	dispatchAndFinish := func() {
		inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
		_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
		state, _ := worker.poolStateSnapshot()
		if !worker.reserveForDispatch(state, state == workerActive) {
			t.Fatal("failed to reserve worker for Dispatch()")
		}
		if !worker.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}) {
			t.Fatal("Dispatch() failed")
		}
		inputWriter.Close()
	}
	dispatchAndFinish()
	waitForTest(t, "first session ready after recent I/O", func() bool { return workerStateForTest(worker) == workerIdleReady })
	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	if !worker.reserveForDispatch(workerIdleReady, false) {
		t.Fatal("failed to reserve worker for second Dispatch()")
	}
	if !worker.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}) {
		t.Fatal("second Dispatch() failed")
	}
	if state := workerStateForTest(worker); state != workerDraining {
		t.Fatalf("worker at request limit state = %v, want draining", state)
	}
	if !worker.IsFull() {
		t.Fatal("draining worker remained selectable")
	}
	inputWriter.Close()
	waitForTest(t, "max requests close", worker.Closed)
}

func Test24HourIdleTTLExpiresAtItsOriginalDeadline(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.IdleTTL = 24 * time.Hour
	if err := cfg.Validate(128); err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })
	picker.onWorkerIdle(worker)
	waitForTest(t, "initial Ping", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 1 })
	worker.acceptPong(pendingProbeForTest(worker))
	waitForTest(t, "initial Pong", func() bool { return workerStateForTest(worker) == workerIdleReady })

	// Advance the fake scheduler beyond uint16 seconds, then just short of 24h.
	// Periodic callbacks run when the fake clock advances, not in real time.
	elapsed := time.Duration(0)
	for index, age := range []time.Duration{20 * time.Hour, 24*time.Hour - time.Second} {
		clock.Advance(age - elapsed)
		elapsed = age
		waitForTest(t, "periodic Ping before 24h", func() bool {
			return picker.WorkerPoolStats().ProbeSentTotal == uint64(index+2)
		})
		worker.acceptPong(pendingProbeForTest(worker))
		waitForTest(t, "periodic Pong before 24h", func() bool { return workerStateForTest(worker) == workerIdleReady })
		if worker.Closed() {
			t.Fatal("long idle TTL was shortened or truncated")
		}
	}
	clock.Advance(time.Second)
	waitForTest(t, "24h idle expiry", worker.Closed)
	if picker.WorkerPoolStats().WorkerClosedIdleTTLTotal != 1 {
		t.Fatal("healthy probes extended the original 24h idle deadline")
	}
}

func TestIdleTTLIsNotExtendedByPong(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	t.Cleanup(func() { picker.Close() })

	picker.onWorkerIdle(worker)
	waitForTest(t, "first probe", func() bool { return pendingProbeForTest(worker) != 0 })
	worker.acceptPong(pendingProbeForTest(worker))
	waitForTest(t, "first Pong", func() bool { return workerStateForTest(worker) == workerIdleReady })

	for elapsed := 5 * time.Second; elapsed <= 20*time.Second; elapsed += 5 * time.Second {
		clock.Advance(5 * time.Second)
		waitForTest(t, "periodic probe", func() bool {
			return workerStateForTest(worker) == workerProbing || worker.Closed()
		})
		if worker.Closed() {
			break
		}
		worker.acceptPong(pendingProbeForTest(worker))
		waitForTest(t, "periodic Pong", func() bool { return workerStateForTest(worker) == workerIdleReady })
	}
	clock.Advance(3999 * time.Millisecond)
	if worker.Closed() {
		t.Fatal("Pong shortened the original idle TTL")
	}
	clock.Advance(time.Millisecond)
	waitForTest(t, "idle TTL close", worker.Closed)
	if got := picker.WorkerPoolStats().WorkerClosedIdleTTLTotal; got != 1 {
		t.Fatalf("idle-TTL close count = %d, want 1", got)
	}
}

func TestDefaultIdleProbeRateIsBounded(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MaxProbingWorkers = 2
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	workers := []*ClientWorker{
		newBlackholeWorker(t, cfg, 1),
		newBlackholeWorker(t, cfg, 1),
	}
	for _, worker := range workers {
		addWorkerForTest(t, picker, worker, workerActive, clock.Now())
		picker.onWorkerIdle(worker)
	}
	t.Cleanup(func() { picker.Close() })

	waitForTest(t, "initial probes", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == 2 })
	for _, worker := range workers {
		worker.acceptPong(pendingProbeForTest(worker))
	}
	waitForTest(t, "initial probe acknowledgements", func() bool { return picker.WorkerPoolStats().WorkersIdleReady == 2 })
	initial := picker.WorkerPoolStats().ProbeSentTotal

	for cycle := uint64(1); cycle <= 4; cycle++ {
		clock.Advance(cfg.ProbeInterval)
		wantSent := initial + cycle*uint64(len(workers))
		waitForTest(t, "periodic probes", func() bool { return picker.WorkerPoolStats().ProbeSentTotal == wantSent })
		for _, worker := range workers {
			worker.acceptPong(pendingProbeForTest(worker))
		}
		waitForTest(t, "periodic probe acknowledgements", func() bool { return picker.WorkerPoolStats().WorkersIdleReady == 2 })
	}

	periodic := picker.WorkerPoolStats().ProbeSentTotal - initial
	if periodic != 8 {
		t.Fatalf("two idle workers sent %d probes over 20 seconds, want 8 (0.4/s)", periodic)
	}
}

func TestPooledWorkerExchangesProbeWithServer(t *testing.T) {
	cfg := testPoolConfig()
	clock := newFakePoolClock()
	uplinkReader, uplinkWriter := pipe.New(pipe.WithoutSizeLimit())
	downlinkReader, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
	server, err := NewServerWorker(context.Background(), nil, &transport.Link{
		Reader: uplinkReader,
		Writer: downlinkWriter,
	})
	if err != nil {
		t.Fatal(err)
	}
	client, err := NewClientWorker(transport.Link{
		Reader: downlinkReader,
		Writer: uplinkWriter,
	}, ClientStrategy{MaxConcurrency: 1, MaxConnection: 128, WorkerPool: cfg})
	if err != nil {
		t.Fatal(err)
	}
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	addWorkerForTest(t, picker, client, workerActive, clock.Now())
	t.Cleanup(func() {
		picker.Close()
		server.Close()
	})

	picker.onWorkerIdle(client)
	waitForTest(t, "server Pong", func() bool { return workerStateForTest(client) == workerIdleReady })
}

func TestMinIdlePrewarmsOnlyAfterUse(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 2
	cfg.MaxProbingWorkers = 2
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() { picker.Close() })

	picker.requestEnsureMinIdle()
	if factory.count() != 0 {
		t.Fatal("unused outbound prewarmed workers")
	}
	picker.poolUsed = true
	picker.requestEnsureMinIdle()
	waitForTest(t, "two warm workers", func() bool {
		picker.access.Lock()
		count := len(picker.workers)
		picker.access.Unlock()
		return factory.count() == 2 && count == 2
	})
	picker.access.Lock()
	workers := append([]*ClientWorker(nil), picker.workers...)
	picker.access.Unlock()
	if len(workers) != 2 {
		t.Fatalf("worker count = %d", len(workers))
	}
	for _, worker := range workers {
		if state := workerStateForTest(worker); state != workerWarmDialing {
			t.Fatalf("warm worker state = %v", state)
		}
		worker.acceptPong(pendingProbeForTest(worker))
	}
	waitForTest(t, "warm workers ready", func() bool {
		for _, worker := range workers {
			if workerStateForTest(worker) != workerIdleReady {
				return false
			}
		}
		return true
	})
	if got := picker.WorkerPoolStats().WorkerCreatedWarmTotal; got != 2 {
		t.Fatalf("warm worker count = %d, want 2", got)
	}
}

func TestRepeatedWarmProbeTimeoutsRespectBackoffWithLargeMinimum(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 100000
	cfg.MaxIdleWorkers = 100000
	cfg.MaxProbingWorkers = 1
	created := make(chan *ClientWorker, 16)
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		worker := newBlackholeWorker(t, cfg, 1)
		created <- worker
		return worker, nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	picker.poolUsed = true
	t.Cleanup(func() { picker.Close() })
	picker.requestEnsureMinIdle()

	for attempt, delay := range []time.Duration{1, 2, 4, 8, 16, 30, 30} {
		var worker *ClientWorker
		select {
		case worker = <-created:
		case <-time.After(2 * time.Second):
			t.Fatal("background retry did not start")
		}
		waitForTest(t, "warm Ping timer", func() bool {
			return picker.WorkerPoolStats().ProbeSentTotal == uint64(attempt+1) && clock.pending() > 0
		})
		if got := factory.count(); got != attempt+1 {
			t.Fatalf("large minimum bypassed probe concurrency: attempts=%d", got)
		}
		clock.Advance(cfg.ProbeTimeout)
		waitForTest(t, "failed warm worker cleanup and retry timer", func() bool {
			picker.access.Lock()
			defer picker.access.Unlock()
			return worker.Closed() && len(picker.workers) == 0 && !picker.warmRunning && picker.warmTimer != nil
		})
		clock.Advance(delay*time.Second - time.Nanosecond)
		picker.requestEnsureMinIdle()
		waitForTest(t, "backoff check", func() bool {
			picker.access.Lock()
			defer picker.access.Unlock()
			return !picker.warmRunning
		})
		if got := factory.count(); got != attempt+1 {
			t.Fatalf("timeout retry bypassed %s backoff: attempts=%d", delay*time.Second, got)
		}
		clock.Advance(time.Nanosecond)
	}
}

func TestWarmFailureUsesBackoff(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 1
	var fail atomic.Bool
	fail.Store(true)
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		if fail.Load() {
			return nil, errors.New("dial failed")
		}
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	picker.poolUsed = true
	t.Cleanup(func() { picker.Close() })
	picker.requestEnsureMinIdle()
	waitForTest(t, "first failed warm dial", func() bool { return factory.count() == 1 && clock.pending() > 0 })
	picker.requestEnsureMinIdle()
	if got := factory.count(); got != 1 {
		t.Fatalf("backoff allowed %d immediate attempts", got)
	}
	fail.Store(false)
	clock.Advance(time.Second)
	waitForTest(t, "warm retry", func() bool { return factory.count() == 2 })
}

func TestDemandWorkerBreaksWarmBackoff(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 1
	var fail atomic.Bool
	fail.Store(true)
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		if fail.Load() {
			return nil, errors.New("dial failed")
		}
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	picker.poolUsed = true
	t.Cleanup(func() { picker.Close() })
	picker.requestEnsureMinIdle()
	waitForTest(t, "warm backoff", func() bool {
		picker.access.Lock()
		defer picker.access.Unlock()
		return factory.count() == 1 && !picker.nextWarmAttempt.IsZero()
	})

	fail.Store(false)
	worker, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if worker == nil || factory.count() != 2 {
		t.Fatal("demand dial did not bypass warm backoff")
	}
	picker.access.Lock()
	nextAttempt := picker.nextWarmAttempt
	picker.access.Unlock()
	if !nextAttempt.IsZero() {
		t.Fatalf("demand success retained warm backoff until %s", nextAttempt)
	}
}

func TestWarmTransportFailureUsesBackoff(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 1
	created := make(chan *ClientWorker, 2)
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		worker := newBlackholeWorker(t, cfg, 1)
		created <- worker
		return worker, nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	picker.poolUsed = true
	t.Cleanup(func() { picker.Close() })

	picker.requestEnsureMinIdle()
	worker := <-created
	waitForTest(t, "warm worker attach", func() bool { return workerStateForTest(worker) == workerWarmDialing })
	worker.Close()
	waitForTest(t, "failed worker removal", func() bool {
		picker.access.Lock()
		defer picker.access.Unlock()
		return len(picker.workers) == 0 && !picker.nextWarmAttempt.IsZero() && !picker.warmRunning && picker.warmTimer != nil
	})
	picker.requestEnsureMinIdle()
	if got := factory.count(); got != 1 {
		t.Fatalf("transport failure backoff allowed %d immediate attempts", got)
	}
	clock.Advance(time.Second)
	waitForTest(t, "warm retry after transport failure", func() bool { return factory.count() == 2 })
}

func TestPoolCloseCancelsProbeTimers(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	picker.onWorkerIdle(worker)
	waitForTest(t, "probe timer", func() bool { return clock.pending() > 0 })

	if err := picker.Close(); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "worker close", worker.Closed)
	waitForTest(t, "probe timer cancellation", func() bool { return clock.pending() == 0 })
}

func TestDrainBeforeFirstPickClosesLazyPool(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MinIdleWorkers = 1
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := &IncrementalWorkerPicker{
		Factory: factory,
		Pool:    cfg,
		clock:   clock,
		jitter:  func(d time.Duration) time.Duration { return d },
	}

	// Model an outbound removal that wins the race with its first Dispatch.
	if err := picker.Drain(); err != nil {
		t.Fatal(err)
	}
	if _, err := picker.PickAvailable(); err == nil {
		t.Fatal("first pick succeeded after the lazy pool was drained")
	}
	if got := factory.count(); got != 0 {
		t.Fatalf("drained lazy pool created %d workers", got)
	}
	clock.Advance(time.Minute)
	if got := factory.count(); got != 0 {
		t.Fatalf("drained lazy pool replenished %d workers", got)
	}
}

func TestPoolShutdownCancelsDialingWorkerProcess(t *testing.T) {
	for _, shutdown := range []struct {
		name string
		run  func(*IncrementalWorkerPicker) error
	}{
		{name: "Drain", run: (*IncrementalWorkerPicker).Drain},
		{name: "Close", run: (*IncrementalWorkerPicker).Close},
	} {
		t.Run(shutdown.name, func(t *testing.T) {
			cfg := testPoolConfig()
			outbound := newContextBlockingOutbound()
			picker := &IncrementalWorkerPicker{
				Pool: cfg,
				Factory: &DialingWorkerFactory{
					Proxy: outbound,
					Strategy: ClientStrategy{
						MaxConcurrency: 1,
						MaxConnection:  128,
						WorkerPool:     cfg,
					},
				},
			}
			if _, err := picker.PickAvailable(); err != nil {
				t.Fatal(err)
			}
			select {
			case <-outbound.start:
			case <-time.After(2 * time.Second):
				t.Fatal("Proxy.Process did not start")
			}
			if err := shutdown.run(picker); err != nil {
				t.Fatal(err)
			}
			select {
			case <-outbound.stop:
			case <-time.After(2 * time.Second):
				t.Fatal("pool shutdown did not cancel Proxy.Process")
			}
		})
	}
}

func TestDrainStopsPoolAndLetsActiveSessionFinish(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	worker := newBlackholeWorker(t, cfg, 1)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())

	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
	}})
	if !worker.reserveForDispatch(workerActive, true) {
		t.Fatal("failed to reserve active worker for Dispatch()")
	}
	if !worker.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}) {
		t.Fatal("Dispatch() failed")
	}
	if err := picker.Drain(); err != nil {
		t.Fatal(err)
	}
	if worker.Closed() || workerStateForTest(worker) != workerDraining {
		t.Fatalf("active worker was not left draining: state=%v closed=%v", workerStateForTest(worker), worker.Closed())
	}
	inputWriter.Close()
	waitForTest(t, "drained worker close", worker.Closed)
	if factory.count() != 0 {
		t.Fatalf("drained pool created %d workers", factory.count())
	}
}

func TestClientWorkerConcurrentDispatchHonorsConcurrency(t *testing.T) {
	cfg := testPoolConfig()
	worker := newBlackholeWorker(t, cfg, 8)
	t.Cleanup(func() { worker.Close() })
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
	}})

	const attempts = 100
	var successes atomic.Int32
	inputs := make([]*pipe.Writer, attempts)
	var wg sync.WaitGroup
	for i := 0; i < attempts; i++ {
		inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
		_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
		inputs[i] = inputWriter
		wg.Add(1)
		go func() {
			defer wg.Done()
			if worker.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}) {
				successes.Add(1)
			}
		}()
	}
	wg.Wait()
	if got := successes.Load(); got != 8 {
		t.Fatalf("successful concurrent Dispatch calls = %d, want 8", got)
	}
	if got := worker.ActiveConnections(); got != 8 {
		t.Fatalf("active sessions = %d, want 8", got)
	}
	for _, writer := range inputs {
		writer.Close()
	}
}

func TestConcurrentRequestsBypassAllProbingWorkers(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.MaxIdleWorkers = 4
	cfg.MaxProbingWorkers = 4
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 1), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	manager := &ClientManager{Enabled: true, Picker: picker}
	t.Cleanup(func() { manager.Close() })

	probing := make([]*ClientWorker, 4)
	for i := range probing {
		probing[i] = newBlackholeWorker(t, cfg, 1)
		addWorkerForTest(t, picker, probing[i], workerActive, clock.Now())
		picker.onWorkerIdle(probing[i])
	}
	waitForTest(t, "all workers probing", func() bool {
		for _, worker := range probing {
			if workerStateForTest(worker) != workerProbing {
				return false
			}
		}
		return true
	})

	const requests = 100
	inputs := make([]*pipe.Writer, requests)
	errorsSeen := make(chan error, requests)
	start := make(chan struct{})
	var wg sync.WaitGroup
	for i := 0; i < requests; i++ {
		inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
		_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
		inputs[i] = inputWriter
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
				Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
			}})
			errorsSeen <- manager.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter})
		}()
	}
	close(start)
	wg.Wait()
	close(errorsSeen)
	for err := range errorsSeen {
		if err != nil {
			t.Fatal(err)
		}
	}
	for _, worker := range probing {
		if worker.ActiveConnections() != 0 || workerStateForTest(worker) != workerProbing {
			t.Fatal("a probing worker accepted a request")
		}
	}
	if got := factory.count(); got != requests {
		t.Fatalf("demand workers created = %d, want %d", got, requests)
	}
	if got := picker.WorkerPoolStats().WorkerCreatedDemandTotal; got != requests {
		t.Fatalf("demand worker stat = %d, want %d", got, requests)
	}
	for _, input := range inputs {
		input.Close()
	}
	waitForTest(t, "burst idle-pool convergence", func() bool {
		picker.access.Lock()
		defer picker.access.Unlock()
		reserved, _ := picker.poolCountsLocked(nil)
		open := 0
		for _, worker := range picker.workers {
			if !worker.Closed() {
				open++
			}
		}
		return reserved == cfg.MaxIdleWorkers && open == int(cfg.MaxIdleWorkers)
	})
	if got := picker.WorkerPoolStats().WorkerClosedMaxIdleTotal; got != requests {
		t.Fatalf("burst max-idle closures = %d, want %d", got, requests)
	}
}

func TestPickerCreatesSecondWorkerAtConcurrencyLimit(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 8), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	manager := &ClientManager{Enabled: true, Picker: picker}
	t.Cleanup(func() { manager.Close() })
	first := newBlackholeWorker(t, cfg, 8)
	addWorkerForTest(t, picker, first, workerIdleReady, clock.Now())

	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.DomainAddress("example.com"), 443),
	}})
	inputs := make([]*pipe.Writer, 9)
	for i := range inputs {
		inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
		_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
		inputs[i] = inputWriter
		if err := manager.Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}); err != nil {
			t.Fatal(err)
		}
	}
	if first.ActiveConnections() != 8 {
		t.Fatalf("first worker active sessions = %d, want 8", first.ActiveConnections())
	}
	if got := factory.count(); got != 1 {
		t.Fatalf("replacement worker count = %d, want 1", got)
	}
	for _, input := range inputs {
		input.Close()
	}
}

func BenchmarkPooledWorkerIsFull(b *testing.B) {
	cfg := testPoolConfig()
	inputReader, _ := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	worker, err := NewClientWorker(transport.Link{Reader: inputReader, Writer: outputWriter}, ClientStrategy{
		MaxConcurrency: 8,
		MaxConnection:  128,
		WorkerPool:     cfg,
	})
	if err != nil {
		b.Fatal(err)
	}
	defer worker.Close()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_ = worker.IsFull()
	}
}

func BenchmarkLegacyWorkerIsFull(b *testing.B) {
	inputReader, _ := pipe.New(pipe.WithoutSizeLimit())
	_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	worker, err := NewClientWorker(transport.Link{Reader: inputReader, Writer: outputWriter}, ClientStrategy{
		MaxConcurrency: 8,
		MaxConnection:  128,
	})
	if err != nil {
		b.Fatal(err)
	}
	defer worker.Close()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_ = worker.IsFull()
	}
}
