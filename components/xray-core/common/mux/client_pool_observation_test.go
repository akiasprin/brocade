package mux

import (
	"errors"
	"testing"
	"time"
)

func TestMuxDispatchDecisionCountersDistinguishReuseAndDial(t *testing.T) {
	tests := []struct {
		name  string
		state clientWorkerState
		seed  bool
		fail  bool
		want  WorkerPoolStats
	}{
		{name: "active reuse", state: workerActive, seed: true, want: WorkerPoolStats{DispatchTotal: 1, DispatchActiveReuseTotal: 1}},
		{name: "idle reuse", state: workerIdleReady, seed: true, want: WorkerPoolStats{DispatchTotal: 1, DispatchIdleReuseTotal: 1}},
		{name: "demand dial", want: WorkerPoolStats{DispatchTotal: 1, DispatchDemandDialTotal: 1, WorkerCreatedDemandTotal: 1}},
		{name: "rejected dial", fail: true, want: WorkerPoolStats{DispatchRejectedTotal: 1}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			cfg.ReuseThreshold = 1 // Reach the base threshold before testing active reuse.
			factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
				if tt.fail {
					return nil, errors.New("dial failed")
				}
				return newBlackholeWorker(t, cfg, 2), nil
			}}
			picker := newPoolPickerForTest(clock, cfg, factory)
			if tt.seed {
				worker := newBlackholeWorker(t, cfg, 2)
				addWorkerForTest(t, picker, worker, tt.state, clock.Now())
				if tt.state == workerActive && worker.sessionManager.Allocate(&worker.strategy) == nil {
					t.Fatal("allocate active session")
				}
			}
			t.Cleanup(func() { _ = picker.Close() })

			_, err := picker.PickAvailable()
			if tt.fail != (err != nil) {
				t.Fatalf("PickAvailable() error = %v, want failure %v", err, tt.fail)
			}
			got := picker.WorkerPoolStats()
			if got.DispatchTotal != tt.want.DispatchTotal ||
				got.DispatchActiveReuseTotal != tt.want.DispatchActiveReuseTotal ||
				got.DispatchIdleReuseTotal != tt.want.DispatchIdleReuseTotal ||
				got.DispatchDemandDialTotal != tt.want.DispatchDemandDialTotal ||
				got.DispatchRejectedTotal != tt.want.DispatchRejectedTotal ||
				got.WorkerCreatedDemandTotal != tt.want.WorkerCreatedDemandTotal {
				t.Fatalf("dispatch counters = %+v, want matching %+v", got, tt.want)
			}
		})
	}
}

func TestMuxSnapshotUsesReverseHealthVocabularyAndPoolDimensions(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	factory := &testWorkerFactory{create: func() (*ClientWorker, error) {
		return newBlackholeWorker(t, cfg, 4), nil
	}}
	picker := newPoolPickerForTest(clock, cfg, factory)
	picker.Tag = "out:app-a/chain-a>peer-a"
	picker.EnableObservation("tcp")
	t.Cleanup(func() {
		_ = picker.Close()
		picker.disableObservation()
	})

	worker := newBlackholeWorker(t, cfg, 4)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	if worker.sessionManager.Allocate(&worker.strategy) == nil {
		t.Fatal("allocate worker session")
	}
	worker.poolAccess.Lock()
	worker.lastProbeAck = time.Now().Add(-250 * time.Millisecond)
	worker.healthLeaseUntil = worker.lastProbeAck.Add(cfg.healthLease())
	worker.lastProbeRTT = 17 * time.Millisecond
	worker.poolProbes = 4
	worker.poolAcks = 3
	worker.poolTimeouts = 1
	worker.poolAccess.Unlock()
	picker.poolStats.dispatchTotal.Add(7)
	picker.poolStats.dispatchActiveReuse.Add(4)
	picker.poolStats.dispatchIdleReuse.Add(2)
	picker.poolStats.dispatchDemandDial.Add(1)
	picker.poolStats.probeSent.Add(4)
	picker.poolStats.probeAck.Add(3)
	picker.poolStats.probeTimeout.Add(1)
	recordMuxWorkerEvent(picker, worker, "DIALING", "VALIDATING", "warm_created", 0)
	// Registry membership is snapshot topology, not a hidden lifecycle event. It must not punch a
	// fake hole in the sequence used by the frontend event timeline.
	other := newPoolPickerForTest(clock, cfg, factory)
	other.EnableObservation("tcp")
	other.disableObservation()
	recordMuxWorkerEvent(picker, worker, "VALIDATING", "READY", "probe_ack", 0)

	report := GetMuxSnapshot()
	var found *MuxPoolSnapshot
	for i := range report.Pools {
		if report.Pools[i].PoolID == picker.observationID.Load() {
			found = &report.Pools[i]
			break
		}
	}
	if found == nil {
		t.Fatalf("picker %d missing from report: %+v", picker.observationID.Load(), report.Pools)
	}
	if found.Pair != picker.Tag || found.Role != "dialer" || found.Kind != "tcp" {
		t.Fatalf("pool identity = %+v", *found)
	}
	if found.Dispatches != 7 || found.ActiveReuses != 4 || found.IdleReuses != 2 || found.DemandDials != 1 {
		t.Fatalf("pool dispatch counters = %+v", *found)
	}
	if found.Probes != 4 || found.Acks != 3 || found.Timeouts != 1 {
		t.Fatalf("pool probe counters = %+v", *found)
	}
	if found.ActiveSessions != 1 || found.AvailableSlots != 3 || found.ReadyWorkers != 1 {
		t.Fatalf("pool capacity = %+v", *found)
	}

	var observed *MuxWorkerSnapshot
	for i := range report.Workers {
		if report.Workers[i].WorkerID == worker.workerID {
			observed = &report.Workers[i]
			break
		}
	}
	if observed == nil {
		t.Fatalf("worker %d missing from report", worker.workerID)
	}
	if observed.Kind != "tcp" || observed.State != "READY" || observed.Phase != "active" || observed.ActiveSessions != 1 || observed.AvailableSlots != 3 {
		t.Fatalf("worker dimensions = %+v", *observed)
	}
	if observed.RTTMS != 17 || observed.Probes != 4 || observed.Acks != 3 || observed.Timeouts != 1 {
		t.Fatalf("worker probe readings = %+v", *observed)
	}
	last := report.Events[len(report.Events)-1]
	if last.PoolID != picker.observationID.Load() || last.From != "VALIDATING" || last.State != "READY" || last.Reason != "probe_ack" {
		t.Fatalf("last event = %+v", last)
	}
	previous := report.Events[len(report.Events)-2]
	if previous.WorkerID != worker.workerID || last.Sequence != previous.Sequence+1 {
		t.Fatalf("pool registration created an event sequence gap: previous=%+v last=%+v", previous, last)
	}
}

func TestRoutineMuxProbeStaysReadyButHasNoDispatchCapacity(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{})
	picker.Tag = "out:chain-a>peer-a"
	worker := newBlackholeWorker(t, cfg, 2)
	addWorkerForTest(t, picker, worker, workerProbing, clock.Now())
	t.Cleanup(func() { _ = picker.Close() })

	snapshot := worker.muxSnapshot(picker, time.Now())
	if snapshot.State != "READY" || snapshot.Phase != "probing" || snapshot.AvailableSlots != 0 {
		t.Fatalf("probing snapshot = %+v", snapshot)
	}
}

func TestMuxTransportCloseReportsAffectedSessions(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{})
	picker.Tag = "out:chain-a>peer-a"
	picker.EnableObservation("tcp")
	worker := newBlackholeWorker(t, cfg, 2)
	addWorkerForTest(t, picker, worker, workerActive, clock.Now())
	if worker.sessionManager.Allocate(&worker.strategy) == nil {
		t.Fatal("allocate worker session")
	}
	t.Cleanup(func() {
		_ = picker.Close()
		picker.disableObservation()
	})

	if err := worker.Close(); err != nil {
		t.Fatal(err)
	}
	waitForTest(t, "transport close observation", func() bool {
		return picker.WorkerPoolStats().WorkerClosedTransportTotal == 1
	})
	var observed *MuxWorkerEvent
	waitForTest(t, "transport close event", func() bool {
		report := GetMuxSnapshot()
		for i := len(report.Events) - 1; i >= 0; i-- {
			event := report.Events[i]
			if event.WorkerID == worker.workerID && event.State == "DEAD" {
				observed = &event
				return true
			}
		}
		return false
	})
	if observed.Reason != "transport_closed" || observed.AffectedSessions != 1 {
		t.Fatalf("transport event = %+v", *observed)
	}
}

func TestMuxLifecycleReasonMatchesSnapshotAndEvent(t *testing.T) {
	for _, tt := range []struct {
		name   string
		reason string
		run    func(*testing.T, *IncrementalWorkerPicker, *ClientWorker)
	}{
		{
			name:   "request limit",
			reason: "request_limit",
			run: func(t *testing.T, picker *IncrementalWorkerPicker, worker *ClientWorker) {
				worker.strategy.MaxConnection = 1
				business := openFaultEchoSession(t, &ClientManager{Picker: picker})
				t.Cleanup(func() {
					business.input.Close()
					business.output.Interrupt()
				})
			},
		},
		{
			name:   "planned rotation after recovery",
			reason: "planned_rotation",
			run: func(t *testing.T, picker *IncrementalWorkerPicker, worker *ClientWorker) {
				if worker.sessionManager.Allocate(&worker.strategy) == nil {
					t.Fatal("allocate active session")
				}
				worker.poolAccess.Lock()
				// A prior health transition must never become the lifecycle reason.
				worker.healthReason = "confirmed_recovery"
				worker.poolAccess.Unlock()
				if err := picker.Drain(); err != nil {
					t.Fatal(err)
				}
			},
		},
	} {
		t.Run(tt.name, func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			cfg.ReuseThreshold = 1
			picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{})
			picker.Tag = "out:chain-a>peer-a"
			picker.EnableObservation("tcp")
			worker := newBlackholeWorker(t, cfg, 2)
			state := workerActive
			if tt.reason == "request_limit" {
				state = workerIdleReady
			}
			addWorkerForTest(t, picker, worker, state, clock.Now())
			t.Cleanup(func() {
				_ = picker.Close()
				picker.disableObservation()
			})

			tt.run(t, picker, worker)
			waitForTest(t, "worker draining", func() bool { return workerStateForTest(worker) == workerDraining })

			report := GetMuxSnapshot()
			var snapshot *MuxWorkerSnapshot
			for i := range report.Workers {
				if report.Workers[i].WorkerID == worker.workerID {
					snapshot = &report.Workers[i]
					break
				}
			}
			if snapshot == nil || snapshot.State != "DRAINING" || snapshot.Reason != tt.reason {
				t.Fatalf("draining snapshot = %+v", snapshot)
			}
			for i := len(report.Events) - 1; i >= 0; i-- {
				event := report.Events[i]
				if event.WorkerID == worker.workerID && event.State == "DRAINING" {
					if event.Reason != tt.reason {
						t.Fatalf("event reason = %q, snapshot reason = %q", event.Reason, snapshot.Reason)
					}
					return
				}
			}
			t.Fatal("draining event was not recorded")
		})
	}
}
