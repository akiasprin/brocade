package mux

import "time"

// Health is independent of business occupancy. A pending routine probe does not
// quarantine a leased worker. Reads/writes/allocations never renew this lease.
type poolHealthState uint8

const (
	poolHealthReady poolHealthState = iota
	poolHealthSuspect
	poolHealthDraining
	poolRecoverySuccesses = 2
)

func (c *WorkerPoolConfig) healthLease() time.Duration    { return 3 * c.ProbeInterval }
func (c *WorkerPoolConfig) confirmTimeout() time.Duration { return 2 * c.ProbeTimeout }

func (w *ClientWorker) healthLeaseFreshLocked(now time.Time) bool {
	return w.healthLeaseUntil.IsZero() || now.Before(w.healthLeaseUntil)
}

// Caller holds poolAccess, not necessarily the picker lock. Checking on both
// reservation and actual Dispatch makes timer scheduling latency irrelevant.
func (w *ClientWorker) activeHealthUsableLocked(now time.Time) bool {
	if w.poolOwner == nil || w.strategy.WorkerPool == nil {
		return true
	}
	if w.poolState != workerActive {
		return w.healthState == poolHealthReady
	}
	w.checkActiveHealthLocked(now)
	return w.healthState == poolHealthReady && w.poolState == workerActive
}

func (w *ClientWorker) suspectHealthLocked(now, expiredAt time.Time, reason string) {
	if w.healthState != poolHealthReady {
		return
	}
	w.healthState, w.healthReason = poolHealthSuspect, reason
	w.healthRecoveryAcks = 0
	w.healthProbeID = 0
	w.healthNextProbe = now
	w.healthConfirmUntil = expiredAt.Add(w.strategy.WorkerPool.confirmTimeout())
	if p := w.poolOwner; p != nil {
		p.poolStats.healthSuspect.Add(1)
		w.recordHealthEventLocked(p, "READY", "SUSPECT", reason)
	}
}

func (w *ClientWorker) drainHealthLocked() {
	if w.healthState == poolHealthDraining {
		return
	}
	w.healthState, w.healthReason = poolHealthDraining, "confirmation_timeout"
	w.healthProbeID = 0
	w.setPoolStateLocked(workerDraining, w.healthReason)
	// Do not close the carrier or erase outstanding reservations here. A delayed
	// Dispatch consumes its reservation and is rejected; established sessions own
	// their own cancellation/ending lifecycle. onWorkerIdle reaps the last one.
	if p := w.poolOwner; p != nil {
		p.poolStats.healthDraining.Add(1)
		w.recordHealthEventLocked(p, "SUSPECT", "DRAINING", w.healthReason)
	}
}

func (w *ClientWorker) checkActiveHealthLocked(now time.Time) {
	if w.poolState != workerActive || w.Closed() {
		return
	}
	if w.healthState == poolHealthSuspect {
		if !now.Before(w.healthConfirmUntil) {
			w.drainHealthLocked()
		}
		return
	}
	if w.healthState != poolHealthReady {
		return
	}
	expiredAt, reason := w.healthLeaseUntil, "health_lease_expired"
	if w.healthProbeID != 0 && (expiredAt.IsZero() || w.healthProbeDeadline.Before(expiredAt)) {
		expiredAt, reason = w.healthProbeDeadline, "probe_timeout"
	}
	if !expiredAt.IsZero() && !now.Before(expiredAt) {
		if reason == "probe_timeout" {
			w.poolTimeouts++
			w.poolOwner.poolStats.probeTimeout.Add(1)
		}
		w.suspectHealthLocked(now, expiredAt, reason)
		if !now.Before(w.healthConfirmUntil) {
			w.drainHealthLocked()
		}
	}
}

func (w *ClientWorker) acceptActivePongLocked(id uint64) {
	if w.poolClock == nil || w.poolOwner == nil {
		return
	}
	now := w.poolClock.Now()
	w.checkActiveHealthLocked(now)
	if w.poolState != workerActive || w.Closed() || w.healthProbeID != id ||
		!now.Before(w.healthProbeDeadline) || (w.healthState == poolHealthSuspect && !now.Before(w.healthConfirmUntil)) {
		return
	}
	w.healthProbeID = 0
	w.poolAcks++
	w.lastProbeAck = now
	w.lastProbeRTT = now.Sub(w.healthProbeSentAt)
	w.poolOwner.poolStats.probeAck.Add(1)
	recovered := false
	if w.healthState == poolHealthSuspect {
		w.healthRecoveryAcks++
		if w.healthRecoveryAcks < poolRecoverySuccesses {
			w.healthNextProbe = now
			return
		}
		w.healthState, w.healthReason = poolHealthReady, ""
		w.poolOwner.poolStats.healthRecovered.Add(1)
		recovered = true
	}
	w.healthLeaseUntil = now.Add(w.strategy.WorkerPool.healthLease())
	w.healthNextProbe = now.Add(w.strategy.WorkerPool.ProbeInterval)
	if recovered {
		w.recordHealthEventLocked(w.poolOwner, "SUSPECT", "READY", "confirmed_recovery")
	}
}

func (w *ClientWorker) beginActiveProbeLocked(now time.Time) {
	w.nextProbeID++
	if w.nextProbeID == 0 {
		w.nextProbeID++
	}
	w.healthProbeID = w.nextProbeID
	w.healthProbeSentAt = now
	w.healthProbeDeadline = now.Add(w.strategy.WorkerPool.ProbeTimeout)
	if w.healthState == poolHealthSuspect {
		// Both recovery exchanges share the original confirmation deadline.
		w.healthProbeDeadline = w.healthConfirmUntil
	}
	meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe, ProbeID: w.healthProbeID}
	if err := w.poolControl.enqueue(meta); err != nil {
		w.healthProbeID = 0
		w.poolOwner.poolStats.healthQueueFailures.Add(1)
		w.suspectHealthLocked(now, now, "control_queue_full_or_closed")
		w.healthNextProbe = now.Add(w.strategy.WorkerPool.ProbeTimeout)
		return
	}
	w.poolProbes++
	w.poolOwner.poolStats.probeSent.Add(1)
}

func (w *ClientWorker) activeProbeDueLocked(now time.Time) bool {
	return w.poolState == workerActive && !w.Closed() && w.poolControl != nil &&
		w.healthProbeID == 0 && !now.Before(w.healthNextProbe) &&
		(w.sessionManager.Size() != 0 || w.poolReservations != 0)
}

func (p *IncrementalWorkerPicker) runActiveHealth() {
	p.access.Lock()
	defer p.access.Unlock()
	p.serviceActiveHealthLocked()
}

// One scheduled callback per picker, no blocking probe goroutine per active
// worker. The bounded priority writer owns any transport-blocked control write.
func (p *IncrementalWorkerPicker) serviceActiveHealthLocked() {
	p.activeHealthGeneration++
	if p.activeHealthTimer != nil {
		p.activeHealthTimer.Stop()
		p.activeHealthTimer = nil
	}
	if p.config == nil || p.poolClosed {
		return
	}
	now := p.nowLocked()
	for _, w := range p.workers {
		w.poolAccess.Lock()
		w.checkActiveHealthLocked(now)
		if w.healthReplacement && !w.lastProbeAck.IsZero() && w.healthState == poolHealthReady {
			w.healthReplacement = false
			p.healthDialAttempts, p.nextHealthDial = 0, time.Time{}
		}
		w.poolAccess.Unlock()
	}
	// Active and queued-idle probes share oldest-due ordering and one budget.
	for _, run := range p.promoteQueuedLocked(now) {
		go run.worker.runProbe(run, p)
	}
	var next time.Time
	for _, w := range p.workers {
		w.poolAccess.Lock()
		if w.poolState == workerActive && !w.Closed() && (w.sessionManager.Size() != 0 || w.poolReservations != 0) {
			at := w.healthNextProbe
			if w.healthProbeID != 0 {
				at = w.healthProbeDeadline
			} else if !at.After(now) {
				// Waiting for the shared probe budget is event-driven: ACK/idle
				// probe completion returns the slot. Do not poll all workers at
				// 40 Hz under load. The hard lease remains the outer deadline.
				at = w.healthLeaseUntil
				if w.healthState == poolHealthSuspect {
					at = w.healthConfirmUntil
				}
			}
			if w.healthState == poolHealthSuspect {
				at = minTime(at, w.healthConfirmUntil)
			} else {
				at = minTime(at, w.healthLeaseUntil)
			}
			if !at.After(now) {
				at = now.Add(25 * time.Millisecond)
			}
			if next.IsZero() || at.Before(next) {
				next = at
			}
		}
		w.poolAccess.Unlock()
	}
	if !next.IsZero() {
		token := p.activeHealthGeneration
		p.activeHealthTimer = p.afterFuncLocked(next.Sub(now), func() {
			p.access.Lock()
			defer p.access.Unlock()
			if token == p.activeHealthGeneration {
				p.serviceActiveHealthLocked()
			}
		})
	}
}

func (p *IncrementalWorkerPicker) hasUnhealthyWorkerLocked() bool {
	for _, w := range p.workers {
		w.poolAccess.Lock()
		bad := w.healthState != poolHealthReady && !w.Closed()
		w.poolAccess.Unlock()
		if bad {
			return true
		}
	}
	return false
}

func (p *IncrementalWorkerPicker) noteHealthDialLocked() {
	p.healthDialAttempts++
	delays := [...]time.Duration{1, 2, 4, 8, 16, 30}
	d := delays[min(p.healthDialAttempts-1, len(delays)-1)] * time.Second
	p.nextHealthDial = p.nowLocked().Add(p.jitterLocked(d))
}
