package mux

import (
	"context"
	"fmt"
	"testing"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func TestPickerChoosesIdleBeforeAnyActiveLoad(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	picker := newPoolPickerForTest(clock, cfg, nil)
	idle := newBlackholeWorker(t, cfg, 4)
	addWorkerForTest(t, picker, idle, workerIdleReady, clock.Now())
	for _, load := range []int{3, 1, 2} {
		worker := newBlackholeWorker(t, cfg, 4)
		addWorkerForTest(t, picker, worker, workerActive, clock.Now())
		for range load {
			if worker.sessionManager.Allocate(&worker.strategy) == nil {
				t.Fatal("failed to seed active worker")
			}
		}
	}
	t.Cleanup(func() { picker.Close() })
	chosen, err := picker.PickAvailable()
	if err != nil || chosen != idle {
		t.Fatalf("picker chose %p, want idle worker %p: %v", chosen, idle, err)
	}
}

func TestPickerConsumesIdleThenBalancesReservations(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	picker := newPoolPickerForTest(clock, cfg, nil)
	idle := newBlackholeWorker(t, cfg, 4)
	active := newBlackholeWorker(t, cfg, 4)
	addWorkerForTest(t, picker, idle, workerIdleReady, clock.Now())
	addWorkerForTest(t, picker, active, workerActive, clock.Now())
	if active.sessionManager.Allocate(&active.strategy) == nil {
		t.Fatal("failed to seed active worker")
	}
	t.Cleanup(func() { picker.Close() })
	chosen, err := picker.PickAvailable()
	if err != nil || chosen != idle {
		t.Fatalf("idle spare was not selected first: worker=%p err=%v", chosen, err)
	}
	for range 6 {
		if _, err := picker.PickAvailable(); err != nil {
			t.Fatal(err)
		}
	}
	if !idle.IsFull() || !active.IsFull() {
		t.Fatal("reservations did not fill exactly the remaining seven slots")
	}
}

func TestWarmPoolDoesNotAmplifyLongLivedWorkerCount(t *testing.T) {
	for _, test := range []struct {
		minimum  uint32
		requests int
	}{{0, 8}, {1, 8}, {1, 100}} {
		t.Run(fmt.Sprintf("min-%d/sessions-%d", test.minimum, test.requests), func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			cfg.PrewarmWorkers = test.minimum
			cfg.ReuseThreshold = 1
			factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
				return newBlackholeWorker(t, cfg, 4), nil
			}}
			picker := newPoolPickerForTest(clock, cfg, factory)
			t.Cleanup(func() { picker.Close() })
			manager := &ClientManager{Picker: picker}
			ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
				Target: net.TCPDestination(net.DomainAddress("echo.test"), 443),
			}})
			inputs := make([]*pipe.Writer, 0, test.requests)
			ackPending := func() {
				picker.access.Lock()
				workers := append([]*ClientWorker(nil), picker.workers...)
				picker.access.Unlock()
				for _, worker := range workers {
					if isProbingState(workerStateForTest(worker)) {
						if id := pendingProbeForTest(worker); id != 0 {
							worker.acceptPong(id)
						}
					}
				}
			}
			for range test.requests {
				reader, input := pipe.New(pipe.WithoutSizeLimit())
				_, output := pipe.New(pipe.WithoutSizeLimit())
				inputs = append(inputs, input)
				t.Cleanup(func() { input.Close() })
				if err := manager.Dispatch(ctx, &transport.Link{Reader: reader, Writer: output}); err != nil {
					t.Fatal(err)
				}
				if test.minimum > 0 {
					waitForTest(t, "warm budget check completes", func() bool {
						picker.access.Lock()
						defer picker.access.Unlock()
						return !picker.warmRunning
					})
				}
			}
			stats := picker.WorkerPoolStats()
			wantActive := uint32((test.requests + 3) / 4)
			if stats.WorkersActive != wantActive || stats.WorkersIdleReady != 0 || stats.WorkerCreatedWarmTotal != 0 || factory.count() != int(wantActive) {
				t.Fatalf("warm replenishment amplified connection count: created=%d stats=%+v", factory.count(), stats)
			}
			wantIdleReuses := uint64(0)
			wantDemandDials := uint64(wantActive)
			if stats.DispatchActiveReuseTotal != uint64(test.requests)-uint64(wantActive) || stats.DispatchIdleReuseTotal != wantIdleReuses || stats.DispatchDemandDialTotal != wantDemandDials {
				t.Fatalf("unexpected packing dispatch counters: %+v", stats)
			}
			t.Logf("sessions=%d slots=4 prewarm=%d: active=%d idle=%d created=%d", test.requests, test.minimum, stats.WorkersActive, stats.WorkersIdleReady, factory.count())
			for _, input := range inputs {
				input.Close()
			}
			waitForTest(t, "pool converges after all sessions finish", func() bool {
				ackPending()
				s := picker.WorkerPoolStats()
				reserved := s.WorkersIdleReady + s.WorkersProbing + s.WorkersProbeQueued + s.WorkersWarmDialing
				return s.WorkersActive == 0 && s.WorkersDraining == 0 && reserved == 1 && s.WorkersIdleReady == 1
			})
		})
	}
}
