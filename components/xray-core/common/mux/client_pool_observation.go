package mux

import (
	"sort"
	"sync"
	"time"
)

// MuxWorkerPoolConfigSnapshot is the effective policy owned by one outbound picker. Keeping it
// beside the readings makes a hot-swapped, draining picker distinguishable from its replacement.
type MuxWorkerPoolConfigSnapshot struct {
	Concurrency          uint32 `json:"concurrency"`
	PrewarmWorkers       uint32 `json:"prewarm_workers"`
	ReuseThreshold       uint32 `json:"reuse_threshold"`
	MaxProbingWorkers    uint32 `json:"max_probing_workers"`
	ProbeIntervalMS      int64  `json:"probe_interval_ms"`
	ProbeTimeoutMS       int64  `json:"probe_timeout_ms"`
	IdleTTLMS            int64  `json:"idle_ttl_ms"`
	MaxSessionsPerWorker uint32 `json:"max_sessions_per_worker"`
	HealthLeaseMS        int64  `json:"health_lease_ms"`
	ConfirmTimeoutMS     int64  `json:"confirm_timeout_ms"`
	RecoverySuccesses    uint32 `json:"recovery_successes"`
	SessionEndTimeoutMS  int64  `json:"session_end_timeout_ms"`
}

// MuxPoolSnapshot is the pair-level view. Counter names deliberately use the same short,
// cumulative vocabulary as ReverseHealthSnapshot; the Agent and browser derive observation-window
// deltas without resetting data-plane counters.
type MuxPoolSnapshot struct {
	PoolID                 uint64                      `json:"pool_id,string"`
	Pair                   string                      `json:"pair"`
	Role                   string                      `json:"role"`
	Kind                   string                      `json:"kind"`
	Used                   bool                        `json:"used"`
	Draining               bool                        `json:"draining"`
	Config                 MuxWorkerPoolConfigSnapshot `json:"config"`
	ActiveSessions         uint32                      `json:"active_sessions"`
	AvailableSlots         uint32                      `json:"available_slots"`
	ReadyWorkers           uint32                      `json:"ready_workers"`
	TotalWorkers           uint32                      `json:"total_workers"`
	Dispatches             uint64                      `json:"dispatches"`
	ActiveReuses           uint64                      `json:"active_reuses"`
	IdleReuses             uint64                      `json:"idle_reuses"`
	DemandDials            uint64                      `json:"demand_dials"`
	RejectedDispatches     uint64                      `json:"rejected_dispatches"`
	Probes                 uint64                      `json:"probes"`
	Acks                   uint64                      `json:"acks"`
	Timeouts               uint64                      `json:"timeouts"`
	WorkersCreatedDemand   uint64                      `json:"workers_created_demand"`
	WorkersCreatedWarm     uint64                      `json:"workers_created_warm"`
	WorkersWarmReady       uint64                      `json:"workers_warm_ready"`
	WorkersWarmFailed      uint64                      `json:"workers_warm_failed"`
	WorkersClosedIdleTTL   uint64                      `json:"workers_closed_idle_ttl"`
	WorkersClosedProbe     uint64                      `json:"workers_closed_probe"`
	WorkersClosedCapacity  uint64                      `json:"workers_closed_capacity"`
	WorkersClosedRequests  uint64                      `json:"workers_closed_requests"`
	WorkersClosedTransport uint64                      `json:"workers_closed_transport"`
	HealthSuspects         uint64                      `json:"health_suspects"`
	HealthRecoveries       uint64                      `json:"health_recoveries"`
	HealthDraining         uint64                      `json:"health_draining"`
	HealthQueueFailures    uint64                      `json:"health_queue_failures"`
	HealthDialThrottled    uint64                      `json:"health_dial_throttled"`
}

// MuxWorkerSnapshot intentionally shares the reverse-health dimensions first. Mux-specific
// capacity and lifetime fields follow them, so the frontend can reuse the same worker table and
// event vocabulary without pretending the two state machines are identical.
type MuxWorkerSnapshot struct {
	PoolID            uint64 `json:"pool_id,string"`
	WorkerID          uint64 `json:"worker_id,string"`
	Pair              string `json:"pair"`
	Role              string `json:"role"`
	Kind              string `json:"kind"`
	State             string `json:"state"`
	Reason            string `json:"reason"`
	Phase             string `json:"phase"`
	ActiveSessions    uint32 `json:"active_sessions"`
	AffectedSessions  uint32 `json:"affected_sessions"`
	AvailableSlots    uint32 `json:"available_slots"`
	LifetimeSessions  uint32 `json:"lifetime_sessions"`
	AckAgeMS          int64  `json:"ack_age_ms"`
	RTTMS             int64  `json:"rtt_ms"`
	Probes            uint64 `json:"probes"`
	Acks              uint64 `json:"acks"`
	Timeouts          uint64 `json:"timeouts"`
	LeaseRemainingMS  int64  `json:"lease_remaining_ms"`
	ControlQueueDepth int    `json:"control_queue_depth"`
	QueueDelayMS      int64  `json:"queue_delay_ms"`
}

type MuxWorkerEvent struct {
	Sequence uint64 `json:"sequence"`
	AtUnixMS int64  `json:"at_unix_ms"`
	From     string `json:"from"`
	MuxWorkerSnapshot
}

type MuxReport struct {
	BootID          uint64              `json:"boot_id,string"`
	Sequence        uint64              `json:"sequence"`
	SampledAtUnixMS int64               `json:"sampled_at_unix_ms"`
	Pools           []MuxPoolSnapshot   `json:"pools"`
	Workers         []MuxWorkerSnapshot `json:"workers"`
	Events          []MuxWorkerEvent    `json:"events"`
}

var muxObservationRegistry = struct {
	sync.Mutex
	boot   uint64
	seq    uint64
	pools  map[uint64]*IncrementalWorkerPicker
	events []MuxWorkerEvent
}{boot: newProbeSeed(), pools: make(map[uint64]*IncrementalWorkerPicker)}

// EnableObservation publishes this picker under its outbound tag. Ordinary unit-test pickers and
// third-party embedders opt out simply by not calling it.
func (p *IncrementalWorkerPicker) EnableObservation(kind string) {
	p.access.Lock()
	if p.observationID.Load() == 0 {
		p.observationKind = kind
		// Publish the immutable descriptive fields before the identity. Readers
		// use the atomic identity as the publication barrier.
		p.observationID.Store(newProbeSeed())
	}
	id := p.observationID.Load()
	p.access.Unlock()

	r := &muxObservationRegistry
	r.Lock()
	r.pools[id] = p
	r.Unlock()
}

func (p *IncrementalWorkerPicker) disableObservation() {
	id := p.observationID.Load()
	if id == 0 {
		return
	}
	r := &muxObservationRegistry
	r.Lock()
	if r.pools[id] == p {
		delete(r.pools, id)
	}
	r.Unlock()
}

func observableWorkerState(state clientWorkerState) string {
	switch state {
	case workerWarmDialing:
		return "VALIDATING"
	case workerDraining:
		return "DRAINING"
	case workerClosed:
		return "CLOSED"
	default:
		// Active, idle-ready and routine probe phases are all transport-healthy. Availability is
		// expressed separately by available_slots instead of overloading health state.
		return "READY"
	}
}

func observableWorkerPhase(state clientWorkerState) string {
	switch state {
	case workerActive:
		return "active"
	case workerIdleReady:
		return "idle_ready"
	case workerProbeQueued:
		return "probe_queued"
	case workerProbing:
		return "probing"
	case workerWarmDialing:
		return "warm_dialing"
	case workerDraining:
		return "draining"
	default:
		return "closed"
	}
}

func (w *ClientWorker) muxSnapshot(p *IncrementalWorkerPicker, now time.Time) MuxWorkerSnapshot {
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	return w.muxSnapshotLocked(p, now)
}

func (w *ClientWorker) muxSnapshotLocked(p *IncrementalWorkerPicker, now time.Time) MuxWorkerSnapshot {
	id := p.observationID.Load()
	state := w.poolState
	reservations := w.poolReservations
	lastAck := w.lastProbeAck
	lastRTT := w.lastProbeRTT
	probes := w.poolProbes
	acks := w.poolAcks
	timeouts := w.poolTimeouts
	reason := w.poolReason
	if reason == "" {
		reason = defaultPoolStateReason(state)
	}
	idleFresh := state != workerIdleReady || (w.poolClock != nil && w.idleReadyFreshLocked(now))

	active := uint32(w.sessionManager.Size())
	lifetime := uint32(w.sessionManager.Count())
	available := uint32(0)
	if state == workerActive || state == workerIdleReady {
		available = w.strategy.MaxConcurrency
		if used := active + reservations; used >= available {
			available = 0
		} else {
			available -= used
		}
		if w.strategy.MaxConnection > 0 {
			remaining := uint32(0)
			if used := lifetime + reservations; used < w.strategy.MaxConnection {
				remaining = w.strategy.MaxConnection - used
			}
			if remaining < available {
				available = remaining
			}
		}
		if !idleFresh {
			available = 0
		}
	}
	ending := w.endingSessions.Load() != 0
	if ending {
		available = 0
	}
	ackAge := int64(-1)
	if !lastAck.IsZero() {
		ackAge = now.Sub(lastAck).Milliseconds()
	}
	snapshot := MuxWorkerSnapshot{
		PoolID: id, WorkerID: w.workerID, Pair: p.Tag, Role: "dialer", Kind: p.observationKind,
		State: observableWorkerState(state), Reason: reason, Phase: observableWorkerPhase(state),
		ActiveSessions: active, AvailableSlots: available, LifetimeSessions: lifetime,
		AckAgeMS: ackAge, RTTMS: lastRTT.Milliseconds(), Probes: probes, Acks: acks, Timeouts: timeouts,
	}
	if ending && state == workerActive {
		snapshot.Phase, snapshot.Reason = "ending", "session_ending"
	}
	probeExpired := w.healthProbeID != 0 && !now.Before(w.healthProbeDeadline)
	if (w.healthState == poolHealthSuspect && state != workerClosed && state != workerDraining) ||
		(state == workerActive && (!w.healthLeaseFreshLocked(now) || probeExpired)) {
		snapshot.State, snapshot.AvailableSlots = "SUSPECT", 0
		snapshot.Reason = w.healthReason
		if w.healthState != poolHealthSuspect || snapshot.Reason == "" {
			snapshot.Reason = "health_lease_expired"
			if probeExpired {
				snapshot.Reason = "probe_timeout"
			}
		}
	}
	snapshot.LeaseRemainingMS = max(0, w.healthLeaseUntil.Sub(now).Milliseconds())
	if w.poolControl != nil {
		snapshot.ControlQueueDepth = len(w.poolControl.control)
		snapshot.QueueDelayMS = w.poolControl.queueDelayMS.Load()
	}
	return snapshot
}

func (p *IncrementalWorkerPicker) muxSnapshot(now time.Time) (MuxPoolSnapshot, []MuxWorkerSnapshot) {
	p.access.Lock()
	cfg := p.config
	if cfg == nil {
		cfg = p.Pool
	}
	workers := append([]*ClientWorker(nil), p.workers...)
	id, kind, pair := p.observationID.Load(), p.observationKind, p.Tag
	used, draining := p.poolUsed, p.poolClosed
	p.access.Unlock()

	config := MuxWorkerPoolConfigSnapshot{}
	if cfg != nil {
		config = MuxWorkerPoolConfigSnapshot{
			PrewarmWorkers: cfg.PrewarmWorkers, ReuseThreshold: cfg.ReuseThreshold,
			MaxProbingWorkers: cfg.MaxProbingWorkers, ProbeIntervalMS: cfg.ProbeInterval.Milliseconds(),
			ProbeTimeoutMS: cfg.ProbeTimeout.Milliseconds(), IdleTTLMS: cfg.IdleTTL.Milliseconds(),
			HealthLeaseMS: cfg.healthLease().Milliseconds(), ConfirmTimeoutMS: cfg.confirmTimeout().Milliseconds(),
			RecoverySuccesses: poolRecoverySuccesses, SessionEndTimeoutMS: sessionEndTimeout.Milliseconds(),
		}
	}
	if len(workers) > 0 {
		config.Concurrency = workers[0].strategy.MaxConcurrency
		config.MaxSessionsPerWorker = workers[0].strategy.MaxConnection
	} else if factory, ok := p.Factory.(*DialingWorkerFactory); ok {
		config.Concurrency = factory.Strategy.MaxConcurrency
		config.MaxSessionsPerWorker = factory.Strategy.MaxConnection
	}

	workerSnapshots := make([]MuxWorkerSnapshot, 0, len(workers))
	var activeSessions, availableSlots, readyWorkers uint32
	for _, worker := range workers {
		snapshot := worker.muxSnapshot(p, now)
		workerSnapshots = append(workerSnapshots, snapshot)
		activeSessions += snapshot.ActiveSessions
		availableSlots += snapshot.AvailableSlots
		if snapshot.State == "READY" {
			readyWorkers++
		}
	}
	stats := p.WorkerPoolStats()
	pool := MuxPoolSnapshot{
		PoolID: id, Pair: pair, Role: "dialer", Kind: kind, Used: used, Draining: draining, Config: config,
		ActiveSessions: activeSessions, AvailableSlots: availableSlots, ReadyWorkers: readyWorkers,
		TotalWorkers: uint32(len(workerSnapshots)), Dispatches: stats.DispatchTotal,
		ActiveReuses: stats.DispatchActiveReuseTotal, IdleReuses: stats.DispatchIdleReuseTotal,
		DemandDials: stats.DispatchDemandDialTotal, RejectedDispatches: stats.DispatchRejectedTotal,
		Probes: stats.ProbeSentTotal, Acks: stats.ProbeAckTotal, Timeouts: stats.ProbeTimeoutTotal,
		WorkersCreatedDemand: stats.WorkerCreatedDemandTotal, WorkersCreatedWarm: stats.WorkerCreatedWarmTotal,
		WorkersWarmReady: stats.WorkerWarmReadyTotal, WorkersWarmFailed: stats.WorkerWarmFailedTotal,
		WorkersClosedIdleTTL: stats.WorkerClosedIdleTTLTotal, WorkersClosedProbe: stats.WorkerClosedProbeTotal,
		WorkersClosedCapacity: stats.WorkerClosedCapacityTotal, WorkersClosedRequests: stats.WorkerClosedRequestsTotal,
		WorkersClosedTransport: stats.WorkerClosedTransportTotal,
		HealthSuspects:         stats.HealthSuspectTotal, HealthRecoveries: stats.HealthRecoveredTotal,
		HealthDraining: stats.HealthDrainingTotal, HealthQueueFailures: stats.HealthQueueFailuresTotal,
		HealthDialThrottled: stats.HealthDialThrottledTotal,
	}
	return pool, workerSnapshots
}

func recordMuxWorkerEvent(p *IncrementalWorkerPicker, worker *ClientWorker, from, state, reason string, affected uint32) {
	if p == nil || p.observationID.Load() == 0 {
		return
	}
	now := time.Now()
	snapshot := worker.muxSnapshot(p, now)
	snapshot.State = state
	snapshot.Reason = reason
	snapshot.AffectedSessions = affected
	appendMuxWorkerEvent(snapshot, from, now)
}

func (w *ClientWorker) recordHealthEventLocked(p *IncrementalWorkerPicker, from, state, reason string) {
	if p == nil || p.observationID.Load() == 0 {
		return
	}
	now := time.Now()
	snapshot := w.muxSnapshotLocked(p, w.poolClock.Now())
	snapshot.State, snapshot.Reason = state, reason
	appendMuxWorkerEvent(snapshot, from, now)
}

func appendMuxWorkerEvent(snapshot MuxWorkerSnapshot, from string, now time.Time) {
	r := &muxObservationRegistry
	r.Lock()
	r.seq++
	event := MuxWorkerEvent{Sequence: r.seq, AtUnixMS: now.UnixMilli(), From: from, MuxWorkerSnapshot: snapshot}
	if len(r.events) == 256 {
		copy(r.events, r.events[1:])
		r.events = r.events[:255]
	}
	r.events = append(r.events, event)
	r.Unlock()
}

// GetMuxSnapshot mirrors GetReverseHealthSnapshot: current workers plus a bounded transition tail.
func GetMuxSnapshot() MuxReport {
	r := &muxObservationRegistry
	r.Lock()
	pickers := make([]*IncrementalWorkerPicker, 0, len(r.pools))
	for _, picker := range r.pools {
		pickers = append(pickers, picker)
	}
	report := MuxReport{
		BootID: r.boot, Sequence: r.seq, SampledAtUnixMS: time.Now().UnixMilli(),
		Pools: []MuxPoolSnapshot{}, Workers: []MuxWorkerSnapshot{}, Events: append([]MuxWorkerEvent{}, r.events...),
	}
	r.Unlock()
	for _, picker := range pickers {
		pool, workers := picker.muxSnapshot(time.Now())
		report.Pools = append(report.Pools, pool)
		report.Workers = append(report.Workers, workers...)
	}
	sort.Slice(report.Pools, func(i, j int) bool { return report.Pools[i].PoolID < report.Pools[j].PoolID })
	sort.Slice(report.Workers, func(i, j int) bool { return report.Workers[i].WorkerID < report.Workers[j].WorkerID })
	return report
}
