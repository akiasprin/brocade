package mux

import "sync/atomic"

// WorkerPoolStats is a point-in-time snapshot for one outbound picker. The
// field names are intentionally stable so Xray's runtime stats integration can
// expose them without changing the pool implementation later.
type WorkerPoolStats struct {
	WorkerCreatedDemandTotal uint64 `json:"mux_worker_created_demand_total"`
	WorkerCreatedWarmTotal   uint64 `json:"mux_worker_created_warm_total"`
	ProbeSentTotal           uint64 `json:"mux_probe_sent_total"`
	ProbeAckTotal            uint64 `json:"mux_probe_ack_total"`
	ProbeTimeoutTotal        uint64 `json:"mux_probe_timeout_total"`
	WorkerClosedIdleTTLTotal uint64 `json:"mux_worker_closed_idle_ttl_total"`
	WorkerClosedProbeTotal   uint64 `json:"mux_worker_closed_probe_total"`
	WorkerClosedMaxIdleTotal uint64 `json:"mux_worker_closed_max_idle_total"`
	WorkersActive            uint32 `json:"mux_workers_active"`
	WorkersIdleReady         uint32 `json:"mux_workers_idle_ready"`
	WorkersProbeQueued       uint32 `json:"mux_workers_probe_queued"`
	WorkersProbing           uint32 `json:"mux_workers_probing"`
	WorkersDraining          uint32 `json:"mux_workers_draining"`
	WorkersWarmDialing       uint32 `json:"mux_workers_warm_dialing"`
}

type workerPoolStats struct {
	workerCreatedDemand atomic.Uint64
	workerCreatedWarm   atomic.Uint64
	probeSent           atomic.Uint64
	probeAck            atomic.Uint64
	probeTimeout        atomic.Uint64
	workerClosedIdleTTL atomic.Uint64
	workerClosedProbe   atomic.Uint64
	workerClosedMaxIdle atomic.Uint64
}

// WorkerPoolStats returns counters and gauges for this picker. It intentionally
// contains no destination, user, UUID, or traffic content.
func (p *IncrementalWorkerPicker) WorkerPoolStats() WorkerPoolStats {
	snapshot := WorkerPoolStats{
		WorkerCreatedDemandTotal: p.poolStats.workerCreatedDemand.Load(),
		WorkerCreatedWarmTotal:   p.poolStats.workerCreatedWarm.Load(),
		ProbeSentTotal:           p.poolStats.probeSent.Load(),
		ProbeAckTotal:            p.poolStats.probeAck.Load(),
		ProbeTimeoutTotal:        p.poolStats.probeTimeout.Load(),
		WorkerClosedIdleTTLTotal: p.poolStats.workerClosedIdleTTL.Load(),
		WorkerClosedProbeTotal:   p.poolStats.workerClosedProbe.Load(),
		WorkerClosedMaxIdleTotal: p.poolStats.workerClosedMaxIdle.Load(),
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
