package mux

import "sync/atomic"

// WorkerPoolStats is a point-in-time snapshot for one outbound picker. The
// field names are intentionally stable so Xray's runtime stats integration can
// expose them without changing the pool implementation later.
type WorkerPoolStats struct {
	DispatchTotal              uint64 `json:"mux_dispatch_total"`
	DispatchActiveReuseTotal   uint64 `json:"mux_dispatch_active_reuse_total"`
	DispatchIdleReuseTotal     uint64 `json:"mux_dispatch_idle_reuse_total"`
	DispatchDemandDialTotal    uint64 `json:"mux_dispatch_demand_dial_total"`
	DispatchRejectedTotal      uint64 `json:"mux_dispatch_rejected_total"`
	WorkerCreatedDemandTotal   uint64 `json:"mux_worker_created_demand_total"`
	WorkerCreatedWarmTotal     uint64 `json:"mux_worker_created_warm_total"`
	WorkerWarmReadyTotal       uint64 `json:"mux_worker_warm_ready_total"`
	WorkerWarmFailedTotal      uint64 `json:"mux_worker_warm_failed_total"`
	ProbeSentTotal             uint64 `json:"mux_probe_sent_total"`
	ProbeAckTotal              uint64 `json:"mux_probe_ack_total"`
	ProbeTimeoutTotal          uint64 `json:"mux_probe_timeout_total"`
	WorkerClosedIdleTTLTotal   uint64 `json:"mux_worker_closed_idle_ttl_total"`
	WorkerClosedProbeTotal     uint64 `json:"mux_worker_closed_probe_total"`
	WorkerClosedCapacityTotal  uint64 `json:"mux_worker_closed_capacity_total"`
	WorkerClosedRequestsTotal  uint64 `json:"mux_worker_closed_requests_total"`
	WorkerClosedTransportTotal uint64 `json:"mux_worker_closed_transport_total"`
	HealthSuspectTotal         uint64 `json:"mux_health_suspect_total"`
	HealthRecoveredTotal       uint64 `json:"mux_health_recovered_total"`
	HealthDrainingTotal        uint64 `json:"mux_health_draining_total"`
	HealthQueueFailuresTotal   uint64 `json:"mux_health_control_queue_failures_total"`
	HealthDialThrottledTotal   uint64 `json:"mux_health_dial_throttled_total"`
	WorkersActive              uint32 `json:"mux_workers_active"`
	WorkersIdleReady           uint32 `json:"mux_workers_idle_ready"`
	WorkersProbeQueued         uint32 `json:"mux_workers_probe_queued"`
	WorkersProbing             uint32 `json:"mux_workers_probing"`
	WorkersDraining            uint32 `json:"mux_workers_draining"`
	WorkersWarmDialing         uint32 `json:"mux_workers_warm_dialing"`
}

type workerPoolStats struct {
	dispatchTotal         atomic.Uint64
	dispatchActiveReuse   atomic.Uint64
	dispatchIdleReuse     atomic.Uint64
	dispatchDemandDial    atomic.Uint64
	dispatchRejected      atomic.Uint64
	workerCreatedDemand   atomic.Uint64
	workerCreatedWarm     atomic.Uint64
	workerWarmReady       atomic.Uint64
	workerWarmFailed      atomic.Uint64
	probeSent             atomic.Uint64
	probeAck              atomic.Uint64
	probeTimeout          atomic.Uint64
	workerClosedIdleTTL   atomic.Uint64
	workerClosedProbe     atomic.Uint64
	workerClosedCapacity  atomic.Uint64
	workerClosedRequests  atomic.Uint64
	workerClosedTransport atomic.Uint64
	healthSuspect         atomic.Uint64
	healthRecovered       atomic.Uint64
	healthDraining        atomic.Uint64
	healthQueueFailures   atomic.Uint64
	healthDialThrottled   atomic.Uint64
}

// WorkerPoolStats returns counters and gauges for this picker. It intentionally
// contains no destination, user, UUID, or traffic content.
func (p *IncrementalWorkerPicker) WorkerPoolStats() WorkerPoolStats {
	snapshot := WorkerPoolStats{
		DispatchTotal:              p.poolStats.dispatchTotal.Load(),
		DispatchActiveReuseTotal:   p.poolStats.dispatchActiveReuse.Load(),
		DispatchIdleReuseTotal:     p.poolStats.dispatchIdleReuse.Load(),
		DispatchDemandDialTotal:    p.poolStats.dispatchDemandDial.Load(),
		DispatchRejectedTotal:      p.poolStats.dispatchRejected.Load(),
		WorkerCreatedDemandTotal:   p.poolStats.workerCreatedDemand.Load(),
		WorkerCreatedWarmTotal:     p.poolStats.workerCreatedWarm.Load(),
		WorkerWarmReadyTotal:       p.poolStats.workerWarmReady.Load(),
		WorkerWarmFailedTotal:      p.poolStats.workerWarmFailed.Load(),
		ProbeSentTotal:             p.poolStats.probeSent.Load(),
		ProbeAckTotal:              p.poolStats.probeAck.Load(),
		ProbeTimeoutTotal:          p.poolStats.probeTimeout.Load(),
		WorkerClosedIdleTTLTotal:   p.poolStats.workerClosedIdleTTL.Load(),
		WorkerClosedProbeTotal:     p.poolStats.workerClosedProbe.Load(),
		WorkerClosedCapacityTotal:  p.poolStats.workerClosedCapacity.Load(),
		WorkerClosedRequestsTotal:  p.poolStats.workerClosedRequests.Load(),
		WorkerClosedTransportTotal: p.poolStats.workerClosedTransport.Load(),
		HealthSuspectTotal:         p.poolStats.healthSuspect.Load(),
		HealthRecoveredTotal:       p.poolStats.healthRecovered.Load(),
		HealthDrainingTotal:        p.poolStats.healthDraining.Load(),
		HealthQueueFailuresTotal:   p.poolStats.healthQueueFailures.Load(),
		HealthDialThrottledTotal:   p.poolStats.healthDialThrottled.Load(),
	}
	p.access.Lock()
	defer p.access.Unlock()
	for _, worker := range p.workers {
		state, _ := worker.poolStateSnapshot()
		switch state {
		case workerActive:
			snapshot.WorkersActive++
		case workerIdleReady:
			snapshot.WorkersIdleReady++
		case workerProbeQueued:
			snapshot.WorkersProbeQueued++
		case workerProbing:
			snapshot.WorkersProbing++
		case workerDraining:
			snapshot.WorkersDraining++
		case workerWarmDialing:
			snapshot.WorkersWarmDialing++
		}
	}
	return snapshot
}
