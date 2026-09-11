package mux

import (
	"context"
	"errors"
	"sync"
	"testing"

	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func TestPickerSpreadsToBaseThenReusesThenOverflows(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.ReuseThreshold = 32
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 2), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() { picker.Close() })
	for request := 1; request <= 100; request++ {
		if _, err := picker.PickAvailable(); err != nil {
			t.Fatal(err)
		}
		want := request
		if request > 32 {
			want = max(32, (request+1)/2)
		}
		if got := factory.count(); got != want {
			t.Fatalf("request %d created %d workers, want %d (base=32 slots=2)", request, got, want)
		}
	}
	stats := picker.WorkerPoolStats()
	if stats.DispatchDemandDialTotal != 50 || stats.DispatchActiveReuseTotal != 50 || stats.DispatchRejectedTotal != 0 {
		t.Fatalf("unexpected dispatch counters: %+v", stats)
	}
}

func TestMinEqualsBaseDoesNotReplenishConsumedWorkers(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.PrewarmWorkers, cfg.ReuseThreshold, cfg.MaxProbingWorkers = 32, 32, 4
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) { return newBlackholeWorker(t, cfg, 2), nil }}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() { picker.Close() })
	ackPending := func() {
		picker.access.Lock()
		workers := append([]*ClientWorker(nil), picker.workers...)
		picker.access.Unlock()
		for _, worker := range workers {
			if id := pendingProbeForTest(worker); id != 0 {
				worker.acceptPong(id)
			}
		}
	}
	picker.poolUsed = true
	picker.requestEnsurePrewarm()
	waitForTest(t, "32 initial warm workers", func() bool { ackPending(); return picker.WorkerPoolStats().WorkersIdleReady == 32 })
	manager := &ClientManager{Picker: picker}
	var inputs []*pipe.Writer
	for request := 1; request <= 100; request++ {
		reader, input := pipe.New(pipe.WithoutSizeLimit())
		_, output := pipe.New(pipe.WithoutSizeLimit())
		inputs = append(inputs, input)
		t.Cleanup(func() { input.Close() })
		if err := manager.Dispatch(lifecycleTestContext(context.Background()), &transport.Link{Reader: reader, Writer: output}); err != nil {
			t.Fatal(err)
		}
		waitForTest(t, "bounded warm replenishment", func() bool {
			picker.access.Lock()
			defer picker.access.Unlock()
			return !picker.warmRunning
		})
		want := max(32, (request+1)/2)
		if got := factory.count(); got != want {
			t.Fatalf("request %d created %d workers, want %d", request, got, want)
		}
	}
	stats := picker.WorkerPoolStats()
	if stats.WorkerCreatedWarmTotal != 32 || stats.DispatchIdleReuseTotal != 32 || stats.DispatchActiveReuseTotal != 50 || stats.DispatchDemandDialTotal != 18 {
		t.Fatalf("unexpected warmed spread/reuse/overflow counters: %+v", stats)
	}
	for _, input := range inputs {
		input.Close()
	}
	waitForTest(t, "overflow recovers to 32 idle workers", func() bool {
		ackPending()
		s := picker.WorkerPoolStats()
		return s.WorkersActive == 0 && s.WorkersIdleReady == 32 && s.WorkersProbing+s.WorkersProbeQueued+s.WorkersWarmDialing == 0
	})
	if factory.count() != 50 || picker.WorkerPoolStats().WorkerClosedCapacityTotal != 18 {
		t.Fatalf("returning overflow triggered new warm churn: created=%d stats=%+v", factory.count(), picker.WorkerPoolStats())
	}
}

func TestBaseGrowthFailureFallsBackToExistingCapacity(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) { return nil, errors.New("dial unavailable") }}
	picker := newPoolPickerForTest(clock, cfg, factory)
	active := newBlackholeWorker(t, cfg, 2)
	addWorkerForTest(t, picker, active, workerActive, clock.Now())
	active.sessionManager.Allocate(&active.strategy)
	t.Cleanup(func() { picker.Close() })
	chosen, err := picker.PickAvailable()
	if err != nil || chosen != active || factory.count() != 1 {
		t.Fatalf("failed growth discarded usable capacity: %v", err)
	}
	if picker.WorkerPoolStats().DispatchRejectedTotal != 0 {
		t.Fatal("fallback counted as rejected")
	}
}

func TestNonPipeDispatchPrewarmsBeforeBusinessEnds(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.PrewarmWorkers = 1
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) { return newBlackholeWorker(t, cfg, 2), nil }}
	picker := newPoolPickerForTest(clock, cfg, factory)
	reader, input := pipe.New(pipe.WithoutSizeLimit())
	_, output := pipe.New(pipe.WithoutSizeLimit())
	t.Cleanup(func() { input.Close(); picker.Close() })
	finished := make(chan error, 1)
	go func() {
		// Keep Reader interruptible without exposing the concrete *pipe.Reader.
		finished <- (&ClientManager{Picker: picker}).Dispatch(lifecycleTestContext(context.Background()), &transport.Link{Reader: struct{ *pipe.Reader }{reader}, Writer: output})
	}()
	waitForTest(t, "non-pipe active business triggers warm creation", func() bool { return factory.count() == 2 })
	select {
	case err := <-finished:
		t.Fatalf("business ended before prewarm check: %v", err)
	default:
	}
	input.Close()
	if err := <-finished; err != nil {
		t.Fatal(err)
	}
}

func TestConcurrentPickerReservesBaseAndOverflowCapacity(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.ReuseThreshold = 32
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 2), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	t.Cleanup(func() { picker.Close() })
	var group sync.WaitGroup
	for range 100 {
		group.Add(1)
		go func() {
			defer group.Done()
			if _, err := picker.PickAvailable(); err != nil {
				t.Error(err)
			}
		}()
	}
	group.Wait()
	if got := factory.count(); got != 50 {
		t.Fatalf("concurrent reservations created %d workers, want 50", got)
	}
}

func TestWarmCreationReservesBaseBudgetBeforeFactoryReturns(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.PrewarmWorkers = 1
	started, release := make(chan struct{}), make(chan struct{})
	var once sync.Once
	var unblock sync.Once
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		once.Do(func() { close(started); <-release })
		return newBlackholeWorker(t, cfg, 2), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	active := newBlackholeWorker(t, cfg, 2)
	addWorkerForTest(t, picker, active, workerActive, clock.Now())
	active.sessionManager.Allocate(&active.strategy)
	t.Cleanup(func() { unblock.Do(func() { close(release) }); picker.Close() })
	picker.poolUsed = true
	picker.requestEnsurePrewarm()
	<-started
	// Pick in another goroutine so the regression fails promptly even if it
	// incorrectly enters a second Factory.Create while the warm dial is held.
	picked := make(chan *ClientWorker, 1)
	go func() { worker, _ := picker.PickAvailable(); picked <- worker }()
	var chosen *ClientWorker
	waitForTest(t, "reuse active capacity while warm creation owns the last base slot", func() bool {
		select {
		case chosen = <-picked:
			return true
		default:
			return false
		}
	})
	unblock.Do(func() { close(release) })
	if chosen != active || factory.count() != 1 {
		t.Fatalf("warm base reservation ignored: chosen=%p active=%p creates=%d", chosen, active, factory.count())
	}
	waitForTest(t, "warm factory completes", func() bool {
		picker.access.Lock()
		defer picker.access.Unlock()
		return len(picker.workers) == 2
	})
}

func TestClosingPoolDuringWarmCreationStopsAllReplenishment(t *testing.T) {
	for _, failed := range []bool{false, true} {
		t.Run(map[bool]string{false: "created", true: "failed"}[failed], func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			cfg.PrewarmWorkers = 1
			started, release := make(chan struct{}), make(chan struct{})
			var unblock sync.Once
			created := make(chan *ClientWorker, 1)
			factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
				close(started)
				<-release
				if failed {
					return nil, errors.New("warm creation failed after close")
				}
				worker := newBlackholeWorker(t, cfg, 2)
				created <- worker
				return worker, nil
			}}
			picker := newPoolPickerForTest(clock, cfg, factory)
			t.Cleanup(func() { unblock.Do(func() { close(release) }); picker.Close() })
			picker.poolUsed = true
			picker.requestEnsurePrewarm()
			<-started
			picker.Close()
			unblock.Do(func() { close(release) })
			waitForTest(t, "warm creation stops after pool closes", func() bool {
				picker.access.Lock()
				defer picker.access.Unlock()
				return !picker.warmRunning && !picker.warmCreating && picker.warmTimer == nil && len(picker.workers) == 0
			})
			if !failed {
				waitForTest(t, "late-created warm worker is closed", (<-created).Closed)
			}
			clock.Advance(cfg.IdleTTL)
			if factory.count() != 1 {
				t.Fatal("closed pool restarted warm creation")
			}
		})

	}
}
