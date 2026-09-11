package mux

import (
	"context"
	cryptorand "crypto/rand"
	"encoding/binary"
	"math/rand/v2"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
)

// WorkerPoolConfig enables idle-first spreading with an application-level
// Ping/Pong or recent received connection data before offering an idle worker.
// Active workers also require a bounded bidirectional health lease. Neither
// local writes nor passive reads can extend that lease; only matching Pong can.
// IdleTTL reclaims capacity above PrewarmWorkers. A healthy, explicitly
// requested prewarm reserve is retained instead of periodically torn down and
// recreated at the same interval.
type WorkerPoolConfig struct {
	PrewarmWorkers uint32 // Best-effort idle reserve within the total base budget.
	// TOTAL worker reuse threshold: use idle,
	// grow to base, reuse active slots, then overflow. Warm creation shares this
	// budget. Overflow is reclaimed on return to idle, never by killing business.
	ReuseThreshold    uint32
	MaxProbingWorkers uint32
	ProbeInterval     time.Duration
	ProbeTimeout      time.Duration
	IdleTTL           time.Duration
}

const maxWorkerPoolProbeInterval = 60 * time.Second

func (c *WorkerPoolConfig) Validate(maxRequests uint32) error {
	if c == nil {
		return nil
	}
	if c.ReuseThreshold < 1 {
		return errors.New("worker reuse threshold (reuseThreshold) must be at least 1")
	}
	if c.PrewarmWorkers > c.ReuseThreshold {
		return errors.New("prewarm workers (prewarmWorkers) must not exceed worker reuse threshold (reuseThreshold)")
	}
	if c.MaxProbingWorkers < 1 || c.MaxProbingWorkers > c.ReuseThreshold {
		return errors.New("max probing workers must be between 1 and worker reuse threshold (reuseThreshold)")
	}
	if c.ProbeInterval < 2*time.Second || c.ProbeInterval > maxWorkerPoolProbeInterval {
		return errors.New("probe interval must be between 2 and 60 seconds")
	}
	if c.ProbeTimeout < 200*time.Millisecond || c.ProbeTimeout > 10*time.Second || c.ProbeTimeout >= c.ProbeInterval {
		return errors.New("probe timeout must be between 200 milliseconds and 10 seconds and shorter than probe interval")
	}
	if c.IdleTTL < time.Second {
		return errors.New("idle TTL must be at least 1 second")
	}
	if c.IdleTTL < c.ProbeInterval+c.ProbeTimeout {
		return errors.New("idle TTL must cover one probe interval and timeout")
	}
	if maxRequests < 1 || maxRequests > 65535 {
		return errors.New("max requests per worker must be between 1 and 65535")
	}
	return nil
}

type clientWorkerState uint8

const (
	workerActive clientWorkerState = iota
	workerIdleReady
	workerProbeQueued
	workerProbing
	workerWarmDialing
	workerDraining
	workerClosed
)

func defaultPoolStateReason(state clientWorkerState) string {
	switch state {
	case workerIdleReady:
		return "idle_ready"
	case workerProbeQueued:
		return "probe_queued"
	case workerProbing:
		return "probe_sent"
	case workerWarmDialing:
		return "initial_probe"
	case workerDraining:
		return "draining"
	case workerClosed:
		return "closed"
	default:
		return "serving"
	}
}

// setPoolStateLocked is the only lifecycle state writer. Pairing state and reason under the same
// lock prevents snapshots from inheriting an unrelated health or previous-transition reason.
func (w *ClientWorker) setPoolStateLocked(state clientWorkerState, reason string) {
	if reason == "" {
		reason = defaultPoolStateReason(state)
	}
	w.poolState, w.poolReason = state, reason
}

type poolTimer interface {
	Stop() bool
}

type poolClock interface {
	Now() time.Time
	AfterFunc(time.Duration, func()) poolTimer
}

type realPoolClock struct{}

func (realPoolClock) Now() time.Time { return time.Now() }
func (realPoolClock) AfterFunc(d time.Duration, f func()) poolTimer {
	return time.AfterFunc(d, f)
}

type probeRun struct {
	worker     *ClientWorker
	id         uint64
	generation uint64
	result     <-chan struct{}
	timeout    time.Duration
}

type probeOutcome uint8

const (
	probeSucceeded probeOutcome = iota
	probeTimedOut
	probeIdleTTLExpired
	probeTransportFailed
)

func newProbeSeed() uint64 {
	var seed [8]byte
	if _, err := cryptorand.Read(seed[:]); err == nil {
		return binary.BigEndian.Uint64(seed[:])
	}
	return uint64(time.Now().UnixNano())
}

func defaultPoolJitter(d time.Duration) time.Duration {
	if d <= 0 {
		return d
	}
	// Delay by up to 20% to keep links from probing on the same second without
	// ever exceeding the configured maximum probe rate.
	factor := 1.0 + rand.Float64()*0.2
	return time.Duration(float64(d) * factor)
}

func (w *ClientWorker) attachPool(owner *IncrementalWorkerPicker, state clientWorkerState, now time.Time) bool {
	// The caller holds the picker lock; keep its clock available without taking
	// that lock for active connection I/O.
	owner.initializePoolLocked()
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	if w.Closed() {
		w.setPoolStateLocked(workerClosed, "transport_closed")
		return false
	}
	w.poolOwner = owner
	w.poolClock = owner.clock
	w.strategy.WorkerPool = owner.config
	w.setPoolStateLocked(state, "")
	w.healthLeaseUntil = now.Add(owner.config.healthLease())
	w.healthNextProbe = now.Add(owner.jitterLocked(owner.config.ProbeInterval))
	if state == workerWarmDialing {
		w.idleSince = now
	}
	return true
}

func (w *ClientWorker) poolStateSnapshot() (clientWorkerState, time.Time) {
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	return w.poolState, w.idleSince
}

func (w *ClientWorker) stopPoolTimerLocked() {
	w.poolTimerGeneration++
	if w.poolTimer != nil {
		w.poolTimer.Stop()
		w.poolTimer = nil
	}
}

func (w *ClientWorker) schedulePoolTimerLocked(owner *IncrementalWorkerPicker, at time.Time) {
	w.stopPoolTimerLocked()
	token := w.poolTimerGeneration
	delay := at.Sub(owner.nowLocked())
	if delay < 0 {
		delay = 0
	}
	w.poolTimer = owner.afterFuncLocked(delay, func() {
		owner.onWorkerTimer(w, token)
	})
}

func (w *ClientWorker) beginProbeLocked(state clientWorkerState) probeRun {
	w.stopPoolTimerLocked()
	w.setPoolStateLocked(state, "")
	w.probeGeneration++
	w.nextProbeID++
	result := make(chan struct{}, 1)
	w.pendingProbeID = w.nextProbeID
	w.pendingProbe = result
	w.pendingProbeAcked = false
	return probeRun{
		worker:     w,
		id:         w.pendingProbeID,
		generation: w.probeGeneration,
		result:     result,
		timeout:    w.strategy.WorkerPool.ProbeTimeout,
	}
}

func (w *ClientWorker) acceptPong(probeID uint64) {
	w.poolAccess.Lock()
	if w.healthProbeID != 0 && w.healthProbeID == probeID {
		owner := w.poolOwner
		w.acceptActivePongLocked(probeID)
		w.poolAccess.Unlock()
		if owner != nil {
			owner.runActiveHealth()
		}
		return
	}
	defer w.poolAccess.Unlock()
	if (w.poolState != workerProbing && w.poolState != workerWarmDialing) || w.pendingProbeID != probeID || w.pendingProbe == nil || w.pendingProbeAcked {
		return
	}
	result := w.pendingProbe
	select {
	case result <- struct{}{}:
		w.pendingProbeAcked = true
		w.poolAcks++
		now := time.Now()
		if w.poolClock != nil {
			now = w.poolClock.Now()
		}
		w.lastProbeAck = now
		w.healthLeaseUntil = now.Add(w.strategy.WorkerPool.healthLease())
		w.healthNextProbe = now.Add(w.strategy.WorkerPool.ProbeInterval)
		wasSuspect := w.healthState == poolHealthSuspect
		w.healthState, w.healthReason = poolHealthReady, ""
		if !w.pendingProbeSentAt.IsZero() {
			w.lastProbeRTT = now.Sub(w.pendingProbeSentAt)
		}
		if w.poolOwner != nil {
			w.poolOwner.poolStats.probeAck.Add(1)
			if wasSuspect {
				w.poolOwner.poolStats.healthRecovered.Add(1)
				w.recordHealthEventLocked(w.poolOwner, "SUSPECT", "READY", "idle_revalidated")
			}
		}
	default:
	}
}

func (w *ClientWorker) poolIdleDeadlineLocked(cfg *WorkerPoolConfig) time.Time {
	return w.idleSince.Add(cfg.IdleTTL)
}

type poolActivityReader struct {
	buf.Reader
	worker *ClientWorker
}

func (r *poolActivityReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	mb, err := r.Reader.ReadMultiBuffer()
	if !mb.IsEmpty() {
		r.worker.recordInboundActivity()
	}
	return mb, err
}

func (r *poolActivityReader) Interrupt()   { common.Interrupt(r.Reader) }
func (r *poolActivityReader) Close() error { return common.Close(r.Reader) }

// Track completed non-empty reads as remote activity for idle reuse. Writes are deliberately not
// instrumented: the standard Writer is a local pipe, so a successful End, Ping or payload write
// says nothing about peer liveness. Reads do not acknowledge a probe or renew its health lease.
func (w *ClientWorker) recordInboundActivity() {
	w.poolAccess.Lock()
	owner := w.poolOwner
	if w.poolClock == nil || w.Closed() || w.poolState == workerClosed {
		w.poolAccess.Unlock()
		return
	}
	w.lastRead = w.poolClock.Now()
	refreshIdle := w.healthState == poolHealthReady && w.healthLeaseFreshLocked(w.lastRead) && (w.poolState == workerIdleReady || w.poolState == workerProbeQueued)
	if refreshIdle {
		// Publish freshness under the same lock used by timers and reservations.
		// A timer that is already due must see the new deadline before it probes.
		w.setPoolStateLocked(workerIdleReady, "remote_activity")
		w.nextProbeAt = w.lastRead.Add(w.strategy.WorkerPool.ProbeInterval)
	}
	w.poolAccess.Unlock()
	if refreshIdle && owner != nil {
		owner.onIdleRead(w)
	}
}

func (p *IncrementalWorkerPicker) onIdleRead(w *ClientWorker) {
	p.access.Lock()
	defer p.access.Unlock()
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	if p.poolClosed || p.config == nil || w.Closed() || (w.poolState != workerIdleReady && w.poolState != workerProbeQueued) {
		return
	}
	now := p.nowLocked()
	due := w.lastRead.Add(p.config.ProbeInterval)
	if !now.Before(due) || !now.Before(w.poolIdleDeadlineLocked(p.config)) {
		return
	}
	w.setPoolStateLocked(workerIdleReady, "remote_activity")
	w.nextProbeAt = due
	w.schedulePoolTimerLocked(p, minTime(minTime(due, w.healthLeaseUntil), w.poolIdleDeadlineLocked(p.config)))
}

func (w *ClientWorker) idleReadyFreshLocked(now time.Time) bool {
	return w.healthState == poolHealthReady && w.healthLeaseFreshLocked(now) && now.Before(w.nextProbeAt) && now.Before(w.poolIdleDeadlineLocked(w.strategy.WorkerPool))
}

func (w *ClientWorker) markPoolClosedLocked(reason string) bool {
	if w.poolState == workerClosed {
		return false
	}
	w.stopPoolTimerLocked()
	w.probeGeneration++
	w.pendingProbe = nil
	w.pendingProbeAcked = false
	w.pendingProbeSentAt = time.Time{}
	w.poolReservations = 0
	w.setPoolStateLocked(workerClosed, reason)
	w.healthProbeID = 0
	return true
}

func (w *ClientWorker) closeForPool(reason string) {
	w.poolAccess.Lock()
	closeDone := w.markPoolClosedLocked(reason)
	w.poolAccess.Unlock()
	if closeDone {
		common.Must(w.done.Close())
	}
}

func (w *ClientWorker) drainForPool(force bool) (clientWorkerState, clientWorkerState, uint32, bool) {
	w.poolAccess.Lock()
	previous := w.poolState
	if w.poolState == workerClosed {
		w.poolAccess.Unlock()
		return previous, previous, 0, false
	}
	w.stopPoolTimerLocked()
	w.probeGeneration++
	w.pendingProbe = nil
	w.pendingProbeAcked = false
	w.poolReservations = 0
	w.healthProbeID = 0
	active := uint32(w.sessionManager.Size())
	if !force && (active > 0 || w.endingSessions.Load() != 0) {
		w.setPoolStateLocked(workerDraining, "planned_rotation")
		w.poolAccess.Unlock()
		return previous, workerDraining, active, previous != workerDraining
	}
	w.setPoolStateLocked(workerClosed, "planned_rotation")
	w.poolAccess.Unlock()
	common.Must(w.done.Close())
	return previous, workerClosed, active, true
}

func (w *ClientWorker) runProbe(run probeRun, owner *IncrementalWorkerPicker) {
	now := owner.now()
	w.poolAccess.Lock()
	w.pendingProbeSentAt = now
	wait := run.timeout
	w.poolAccess.Unlock()
	if wait < 0 {
		wait = 0
	}
	// Start the deadline before writing the Ping. A size-limited outbound pipe
	// can itself block when the underlying transport is half-open; the timeout
	// callback closes the worker, whose monitor interrupts that blocked writer.
	timer := owner.afterFunc(wait, func() {
		owner.onProbeFinished(run, probeTimedOut)
	})
	defer timer.Stop()
	if err := writeProbeFrame(w.link.Writer, run.id, false); err != nil {
		owner.onProbeFinished(run, probeTransportFailed)
		return
	}
	w.poolAccess.Lock()
	w.poolProbes++
	w.poolAccess.Unlock()
	owner.poolStats.probeSent.Add(1)

	select {
	case <-run.result:
		owner.onProbeFinished(run, probeSucceeded)
	case <-w.done.Wait():
		owner.onProbeFinished(run, probeTransportFailed)
	}
}

func (p *IncrementalWorkerPicker) initializePoolLocked() {
	if p.clock == nil {
		p.clock = realPoolClock{}
	}
	if p.jitter == nil {
		p.jitter = defaultPoolJitter
	}
}

func (p *IncrementalWorkerPicker) now() time.Time {
	p.access.Lock()
	p.initializePoolLocked()
	clock := p.clock
	p.access.Unlock()
	return clock.Now()
}

func (p *IncrementalWorkerPicker) afterFunc(d time.Duration, f func()) poolTimer {
	p.access.Lock()
	p.initializePoolLocked()
	clock := p.clock
	p.access.Unlock()
	return clock.AfterFunc(d, f)
}

// The Locked variants are used while p.access is already held.
func (p *IncrementalWorkerPicker) nowLocked() time.Time {
	p.initializePoolLocked()
	return p.clock.Now()
}

func (p *IncrementalWorkerPicker) afterFuncLocked(d time.Duration, f func()) poolTimer {
	p.initializePoolLocked()
	return p.clock.AfterFunc(d, f)
}

func (p *IncrementalWorkerPicker) jitterLocked(d time.Duration) time.Duration {
	p.initializePoolLocked()
	return p.jitter(d)
}

func isReservedIdleState(state clientWorkerState) bool {
	switch state {
	case workerIdleReady, workerProbeQueued, workerProbing, workerWarmDialing:
		return true
	default:
		return false
	}
}

func isProbingState(state clientWorkerState) bool {
	return state == workerProbing || state == workerWarmDialing
}

func (p *IncrementalWorkerPicker) poolCountsLocked(except *ClientWorker) (reserved, probing uint32) {
	for _, worker := range p.workers {
		if worker == except {
			continue
		}
		worker.poolAccess.Lock()
		state, activeProbe := worker.poolState, worker.healthProbeID != 0
		worker.poolAccess.Unlock()
		// done may close just before monitor reports the terminal state. Keep the
		// reservation until that report is processed so warm failure backoff wins
		// the race with the replenishment loop.
		if state == workerClosed {
			continue
		}
		if isReservedIdleState(state) {
			reserved++
		}
		if isProbingState(state) || activeProbe {
			probing++
		}
	}
	return reserved, probing
}

// Caller holds p.access and, when except is non-nil, may also hold that
// worker's poolAccess. The configured prewarm target is the number of healthy
// idle carriers the operator explicitly asked to retain. Matching validation
// therefore renews their idle epoch; excess idle carriers keep their original
// deadline and are reclaimed normally.
func (p *IncrementalWorkerPicker) withinPrewarmTargetLocked(except *ClientWorker) bool {
	if p.config == nil || p.config.PrewarmWorkers == 0 {
		return false
	}
	reserved, _ := p.poolCountsLocked(except)
	return reserved < p.config.PrewarmWorkers
}

// Caller holds access. except avoids relocking onWorkerIdle's current worker.
// Count unavailable/probing/draining workers and in-flight warm creation too;
// they consume connections even when no business can be assigned to them.
func (p *IncrementalWorkerPicker) poolWorkerCountLocked(except *ClientWorker) uint64 {
	var count uint64
	if p.warmCreating {
		count++
	}
	for _, worker := range p.workers {
		if worker == except {
			continue
		}
		if state, _ := worker.poolStateSnapshot(); state != workerClosed {
			count++
		}
	}
	return count
}

func (p *IncrementalWorkerPicker) closeOldestIdleReadyLocked(except *ClientWorker) *ClientWorker {
	var oldest *ClientWorker
	var oldestSince time.Time
	for _, worker := range p.workers {
		if worker == except || worker.Closed() {
			continue
		}
		state, since := worker.poolStateSnapshot()
		if state != workerIdleReady || (!oldestSince.IsZero() && !since.Before(oldestSince)) {
			continue
		}
		oldest = worker
		oldestSince = since
	}
	if oldest == nil {
		return nil
	}
	oldest.poolAccess.Lock()
	if oldest.poolState != workerIdleReady {
		oldest.poolAccess.Unlock()
		return nil
	}
	closeDone := oldest.markPoolClosedLocked("capacity_reclaim")
	oldest.poolAccess.Unlock()
	if !closeDone {
		return nil
	}
	return oldest
}

func (p *IncrementalWorkerPicker) onWorkerUsed(worker *ClientWorker) {
	p.access.Lock()
	if p.poolClosed || p.config == nil {
		p.access.Unlock()
		return
	}
	p.poolUsed = true
	p.access.Unlock()
	p.runActiveHealth()
	p.requestEnsurePrewarm()
}

func (p *IncrementalWorkerPicker) onWorkerIdle(worker *ClientWorker) {
	var closeWorkers []*ClientWorker
	var run *probeRun

	p.access.Lock()
	p.initializePoolLocked()
	if worker.Closed() {
		// The monitor owns terminal accounting. An End completion after failure
		// must not relabel it as an idle transition or successful planned drain.
		p.access.Unlock()
		return
	}
	if p.poolClosed {
		worker.poolAccess.Lock()
		// Finishing one End can notify us while other sessions still run.
		// Planned drain must wait for all business and all ending writes.
		if worker.sessionManager.Size() != 0 || worker.poolReservations != 0 || worker.endingSessions.Load() != 0 {
			worker.poolAccess.Unlock()
			p.access.Unlock()
			return
		}
		previousState := worker.poolState
		closeDone := worker.markPoolClosedLocked("drain_complete")
		worker.poolAccess.Unlock()
		p.access.Unlock()
		if closeDone {
			common.Must(worker.done.Close())
			recordMuxWorkerEvent(p, worker, observableWorkerState(previousState), "CLOSED", "drain_complete", 0)
		}
		return
	}
	if p.config == nil {
		p.access.Unlock()
		worker.closeForPool("closed")
		return
	}
	now := p.nowLocked()
	worker.poolAccess.Lock()
	if worker.poolState == workerDraining && worker.sessionManager.Size() == 0 && worker.poolReservations == 0 && worker.endingSessions.Load() == 0 {
		if worker.markPoolClosedLocked("drain_complete") {
			closeWorkers = append(closeWorkers, worker)
		}
		worker.poolAccess.Unlock()
		p.access.Unlock()
		for _, item := range closeWorkers {
			common.Must(item.done.Close())
			recordMuxWorkerEvent(p, item, "DRAINING", "CLOSED", "drain_complete", 0)
		}
		return
	}
	if worker.poolState != workerActive || worker.poolReservations != 0 || worker.sessionManager.Size() != 0 || worker.endingSessions.Load() != 0 {
		worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	if worker.strategy.MaxConnection > 0 && worker.sessionManager.Count() >= int(worker.strategy.MaxConnection) {
		if worker.markPoolClosedLocked("request_limit") {
			closeWorkers = append(closeWorkers, worker)
		}
		worker.poolAccess.Unlock()
		p.access.Unlock()
		for _, item := range closeWorkers {
			common.Must(item.done.Close())
			recordMuxWorkerEvent(p, item, "READY", "CLOSED", "request_limit", 0)
		}
		p.requestEnsurePrewarm()
		return
	}

	_, probing := p.poolCountsLocked(worker)
	if p.poolWorkerCountLocked(worker)+1 > uint64(p.config.ReuseThreshold) {
		if victim := p.closeOldestIdleReadyLocked(worker); victim != nil {
			closeWorkers = append(closeWorkers, victim)
			p.poolStats.workerClosedCapacity.Add(1)
		} else {
			if worker.markPoolClosedLocked("capacity_reclaim") {
				closeWorkers = append(closeWorkers, worker)
				p.poolStats.workerClosedCapacity.Add(1)
			}
			worker.poolAccess.Unlock()
			p.access.Unlock()
			for _, item := range closeWorkers {
				common.Must(item.done.Close())
				recordMuxWorkerEvent(p, item, "READY", "CLOSED", "capacity_reclaim", 0)
			}
			return
		}
	}
	worker.idleSince = now
	worker.healthProbeID = 0
	worker.nextProbeAt = now
	// Keep local activity separate from remote evidence. In particular, the
	// End frame written just before this callback must not renew idle reuse.
	lastResponse := worker.lastRead
	if worker.lastProbeAck.After(lastResponse) {
		lastResponse = worker.lastProbeAck
	}
	if worker.healthState == poolHealthReady && worker.healthLeaseFreshLocked(now) && !lastResponse.IsZero() && now.Before(lastResponse.Add(p.config.ProbeInterval)) {
		worker.setPoolStateLocked(workerIdleReady, "idle_ready")
		worker.nextProbeAt = lastResponse.Add(p.config.ProbeInterval)
		worker.schedulePoolTimerLocked(p, minTime(minTime(worker.nextProbeAt, worker.healthLeaseUntil), worker.poolIdleDeadlineLocked(p.config)))
	} else if probing < p.config.MaxProbingWorkers {
		started := worker.beginProbeLocked(workerProbing)
		run = &started
	} else {
		worker.setPoolStateLocked(workerProbeQueued, "probe_queued")
		worker.schedulePoolTimerLocked(p, worker.poolIdleDeadlineLocked(p.config))
	}
	worker.poolAccess.Unlock()
	p.access.Unlock()

	for _, item := range closeWorkers {
		common.Must(item.done.Close())
		recordMuxWorkerEvent(p, item, "READY", "CLOSED", "capacity_reclaim", 0)
	}
	if run != nil {
		go worker.runProbe(*run, p)
	}
	p.requestEnsurePrewarm()
}

func (p *IncrementalWorkerPicker) onWorkerTimer(worker *ClientWorker, token uint64) {
	var closeWorker bool
	var run *probeRun

	p.access.Lock()
	if p.poolClosed || p.config == nil {
		p.access.Unlock()
		worker.closeForPool("planned_rotation")
		return
	}
	now := p.nowLocked()
	worker.poolAccess.Lock()
	if token != worker.poolTimerGeneration || (worker.poolState != workerIdleReady && worker.poolState != workerProbeQueued) {
		worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	if !now.Before(worker.poolIdleDeadlineLocked(p.config)) && p.withinPrewarmTargetLocked(worker) {
		// Do not rotate a healthy carrier solely to recreate the same configured
		// prewarm reserve. A due health/validation probe still runs below.
		worker.idleSince = now
	}
	if !now.Before(worker.poolIdleDeadlineLocked(p.config)) {
		closeWorker = worker.markPoolClosedLocked("idle_ttl")
		if closeWorker {
			p.poolStats.workerClosedIdleTTL.Add(1)
		}
	} else if worker.poolState == workerIdleReady && worker.idleReadyFreshLocked(now) {
		worker.schedulePoolTimerLocked(p, minTime(minTime(worker.nextProbeAt, worker.healthLeaseUntil), worker.poolIdleDeadlineLocked(p.config)))
	} else {
		_, probing := p.poolCountsLocked(worker)
		if probing < p.config.MaxProbingWorkers {
			started := worker.beginProbeLocked(workerProbing)
			run = &started
		} else {
			worker.setPoolStateLocked(workerProbeQueued, "probe_queued")
			worker.schedulePoolTimerLocked(p, worker.poolIdleDeadlineLocked(p.config))
		}
	}
	worker.poolAccess.Unlock()
	p.access.Unlock()

	if closeWorker {
		common.Must(worker.done.Close())
		recordMuxWorkerEvent(p, worker, "READY", "CLOSED", "idle_ttl", 0)
		p.requestEnsurePrewarm()
	}
	if run != nil {
		go worker.runProbe(*run, p)
	}
}

func minTime(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}

func (p *IncrementalWorkerPicker) onProbeFinished(run probeRun, outcome probeOutcome) {
	var closeWorker bool
	var starts []probeRun
	var logTimeout bool
	var suppressed uint64

	p.access.Lock()
	if p.config == nil {
		p.access.Unlock()
		return
	}
	now := p.nowLocked()
	run.worker.poolAccess.Lock()
	if run.generation != run.worker.probeGeneration || run.id != run.worker.pendingProbeID || (run.worker.poolState != workerProbing && run.worker.poolState != workerWarmDialing) {
		run.worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	// A Pong that won the worker lock before the timeout is authoritative even
	// if the probe goroutine has not consumed its notification yet.
	if outcome == probeTimedOut && run.worker.pendingProbeAcked {
		run.worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	wasWarm := run.worker.poolState == workerWarmDialing
	run.worker.pendingProbe = nil
	run.worker.pendingProbeAcked = false
	if !p.poolClosed && outcome == probeSucceeded && p.withinPrewarmTargetLocked(run.worker) {
		// A matching Pong retains only the explicitly requested warm reserve.
		// It never extends excess idle capacity.
		run.worker.idleSince = now
	}
	if outcome == probeSucceeded && !now.Before(run.worker.poolIdleDeadlineLocked(p.config)) {
		outcome = probeIdleTTLExpired
	}
	if p.poolClosed || outcome != probeSucceeded {
		if outcome == probeTimedOut {
			run.worker.poolTimeouts++
		}
		if wasWarm && outcome != probeSucceeded {
			p.poolStats.workerWarmFailed.Add(1)
		}
		closeReason := "planned_rotation"
		switch outcome {
		case probeTimedOut:
			closeReason = "probe_timeout"
			p.poolStats.probeTimeout.Add(1)
			p.poolStats.workerClosedProbe.Add(1)
			p.noteWarmFailureLocked(now)
			logTimeout, suppressed = p.probeTimeoutLogLocked(now)
		case probeTransportFailed:
			closeReason = "transport_closed"
			p.poolStats.workerClosedProbe.Add(1)
			p.poolStats.workerClosedTransport.Add(1)
			p.noteWarmFailureLocked(now)
		case probeIdleTTLExpired:
			closeReason = "idle_ttl"
			p.poolStats.workerClosedIdleTTL.Add(1)
		}
		closeWorker = run.worker.markPoolClosedLocked(closeReason)
	} else {
		run.worker.setPoolStateLocked(workerIdleReady, "probe_ack")
		run.worker.nextProbeAt = now.Add(p.jitterLocked(p.config.ProbeInterval))
		run.worker.schedulePoolTimerLocked(p, minTime(run.worker.nextProbeAt, run.worker.poolIdleDeadlineLocked(p.config)))
		if wasWarm {
			p.poolStats.workerWarmReady.Add(1)
			p.warmFailures = 0
			p.nextWarmAttempt = time.Time{}
		}
	}
	run.worker.poolAccess.Unlock()
	if !p.poolClosed {
		starts = append(starts, p.promoteQueuedLocked(now)...)
	}
	p.access.Unlock()
	if wasWarm && outcome == probeSucceeded && !closeWorker {
		recordMuxWorkerEvent(p, run.worker, "VALIDATING", "READY", "probe_ack", 0)
	}

	if closeWorker {
		state := "DEAD"
		reason := "transport_closed"
		from := "READY"
		if wasWarm {
			from = "VALIDATING"
		}
		switch outcome {
		case probeTimedOut:
			reason = "probe_timeout"
		case probeIdleTTLExpired:
			state = "CLOSED"
			reason = "idle_ttl"
		case probeSucceeded:
			state = "CLOSED"
			reason = "planned_rotation"
		}
		recordMuxWorkerEvent(p, run.worker, from, state, reason, run.worker.ActiveConnections())
		common.Must(run.worker.done.Close())
	}
	for _, next := range starts {
		go next.worker.runProbe(next, p)
	}
	if logTimeout {
		if suppressed == 0 {
			errors.LogWarning(context.Background(), "mux worker probe timed out for outbound ", p.Tag)
		} else {
			errors.LogWarning(context.Background(), "mux worker probe timed out for outbound ", p.Tag, "; suppressed ", suppressed, " repeated warnings")
		}
	}
	p.requestEnsurePrewarm()
}

const probeTimeoutLogInterval = 30 * time.Second

// probeTimeoutLogLocked rate-limits warnings per outbound picker. Counters are
// still updated for every timeout, so suppressing logs never loses telemetry.
func (p *IncrementalWorkerPicker) probeTimeoutLogLocked(now time.Time) (bool, uint64) {
	if p.lastTimeoutLog.IsZero() || now.Sub(p.lastTimeoutLog) >= probeTimeoutLogInterval {
		suppressed := p.suppressedLogs
		p.lastTimeoutLog = now
		p.suppressedLogs = 0
		return true, suppressed
	}
	p.suppressedLogs++
	return false, 0
}

func (p *IncrementalWorkerPicker) promoteQueuedLocked(now time.Time) []probeRun {
	_, probing := p.poolCountsLocked(nil)
	if probing >= p.config.MaxProbingWorkers {
		return nil
	}
	// Size scratch space by actual workers, not a potentially very large configured
	// ceiling. Bound before converting to int so this also works on 32-bit systems.
	available := int(min(uint64(p.config.MaxProbingWorkers-probing), uint64(len(p.workers))))
	if available <= 0 {
		return nil
	}
	starts := make([]probeRun, 0, available)
	for available > 0 {
		var candidate *ClientWorker
		var oldest time.Time
		for _, worker := range p.workers {
			worker.poolAccess.Lock()
			worker.checkActiveHealthLocked(now)
			due := worker.nextProbeAt
			eligible := worker.poolState == workerProbeQueued
			if worker.activeProbeDueLocked(now) {
				eligible, due = true, worker.healthNextProbe
			}
			worker.poolAccess.Unlock()
			if !eligible || (candidate != nil && !due.Before(oldest)) {
				continue
			}
			candidate = worker
			oldest = due
		}
		if candidate == nil {
			break
		}
		candidate.poolAccess.Lock()
		if candidate.activeProbeDueLocked(now) {
			candidate.beginActiveProbeLocked(now)
			candidate.poolAccess.Unlock()
			available--
			continue
		}
		if candidate.poolState != workerProbeQueued {
			candidate.poolAccess.Unlock()
			continue
		}
		if !now.Before(candidate.poolIdleDeadlineLocked(p.config)) && p.withinPrewarmTargetLocked(candidate) {
			candidate.idleSince = now
		}
		if !now.Before(candidate.poolIdleDeadlineLocked(p.config)) {
			closeDone := candidate.markPoolClosedLocked("idle_ttl")
			candidate.poolAccess.Unlock()
			if closeDone {
				p.poolStats.workerClosedIdleTTL.Add(1)
				go func(worker *ClientWorker) {
					recordMuxWorkerEvent(p, worker, "READY", "CLOSED", "idle_ttl", 0)
					common.Must(worker.done.Close())
				}(candidate)
			}
			continue
		}
		starts = append(starts, candidate.beginProbeLocked(workerProbing))
		candidate.poolAccess.Unlock()
		available--
	}
	return starts
}

func (p *IncrementalWorkerPicker) noteWarmFailureLocked(now time.Time) {
	p.warmFailures++
	seconds := []time.Duration{1, 2, 4, 8, 16, 30}
	idx := p.warmFailures - 1
	if idx >= len(seconds) {
		idx = len(seconds) - 1
	}
	p.nextWarmAttempt = now.Add(p.jitterLocked(seconds[idx] * time.Second))
}

func (p *IncrementalWorkerPicker) scheduleWarmRetryLocked(delay time.Duration) {
	if p.warmTimer != nil {
		p.warmTimer.Stop()
	}
	p.warmTimer = p.afterFuncLocked(delay, p.requestEnsurePrewarm)
}

func (p *IncrementalWorkerPicker) requestEnsurePrewarm() {
	p.access.Lock()
	if p.config == nil || p.poolClosed || !p.poolUsed || p.config.PrewarmWorkers == 0 || p.warmRunning {
		p.access.Unlock()
		return
	}
	p.warmRunning = true
	p.access.Unlock()
	go p.ensurePrewarm()
}

func (p *IncrementalWorkerPicker) ensurePrewarm() {
	for {
		p.access.Lock()
		p.initializePoolLocked()
		if p.config == nil || p.poolClosed || !p.poolUsed {
			p.warmRunning = false
			p.access.Unlock()
			return
		}
		now := p.nowLocked()
		reserved, probing := p.poolCountsLocked(nil)
		if reserved >= p.config.PrewarmWorkers || p.poolWorkerCountLocked(nil) >= uint64(p.config.ReuseThreshold) || probing >= p.config.MaxProbingWorkers {
			p.warmRunning = false
			p.access.Unlock()
			return
		}
		if now.Before(p.nextWarmAttempt) {
			delay := p.nextWarmAttempt.Sub(now)
			p.scheduleWarmRetryLocked(delay)
			p.warmRunning = false
			p.access.Unlock()
			return
		}
		p.warmCreating = true
		p.access.Unlock()

		worker, err := p.Factory.Create()
		p.access.Lock()
		p.warmCreating = false
		if err != nil {
			p.poolStats.workerWarmFailed.Add(1)
			if p.poolClosed {
				p.warmRunning = false
				p.access.Unlock()
				return
			}
			p.noteWarmFailureLocked(p.nowLocked())
			p.warmRunning = false
			delay := p.nextWarmAttempt.Sub(p.nowLocked())
			p.scheduleWarmRetryLocked(delay)
			p.access.Unlock()
			errors.LogInfoInner(context.Background(), err, "failed to create warm mux worker")
			return
		}

		now = p.nowLocked()
		reserved, probing = p.poolCountsLocked(nil)
		if p.poolClosed || reserved >= p.config.PrewarmWorkers || p.poolWorkerCountLocked(nil) >= uint64(p.config.ReuseThreshold) || probing >= p.config.MaxProbingWorkers {
			p.access.Unlock()
			worker.closeForPool("capacity_reclaim")
			continue
		}
		if !worker.attachPool(p, workerWarmDialing, now) {
			p.poolStats.workerWarmFailed.Add(1)
			p.noteWarmFailureLocked(now)
			p.warmRunning = false
			delay := p.nextWarmAttempt.Sub(now)
			p.scheduleWarmRetryLocked(delay)
			p.access.Unlock()
			worker.closeForPool("transport_closed")
			return
		}
		p.workers = append(p.workers, worker)
		p.poolStats.workerCreatedWarm.Add(1)
		worker.poolAccess.Lock()
		started := worker.beginProbeLocked(workerWarmDialing)
		worker.poolAccess.Unlock()
		p.access.Unlock()
		recordMuxWorkerEvent(p, worker, "DIALING", "VALIDATING", "warm_created", 0)
		go worker.runProbe(started, p)
	}
}

func (p *IncrementalWorkerPicker) onWorkerClosed(worker *ClientWorker, affectedSessions uint32) {
	var starts []probeRun
	var unregister bool

	p.access.Lock()
	worker.poolAccess.Lock()
	previousState := worker.poolState
	reason := "transport_closed"
	if worker.sessionEndTimedOut.Load() {
		reason = "session_end_timeout"
	}
	worker.markPoolClosedLocked(reason)
	worker.poolAccess.Unlock()
	transportClosed := previousState != workerClosed
	if transportClosed {
		p.poolStats.workerClosedTransport.Add(1)
		if previousState == workerWarmDialing {
			p.poolStats.workerWarmFailed.Add(1)
		}
	}
	for i, candidate := range p.workers {
		if candidate == worker {
			copy(p.workers[i:], p.workers[i+1:])
			p.workers[len(p.workers)-1] = nil
			p.workers = p.workers[:len(p.workers)-1]
			break
		}
	}
	if p.config != nil && !p.poolClosed && isProbingState(previousState) {
		p.poolStats.workerClosedProbe.Add(1)
		p.noteWarmFailureLocked(p.nowLocked())
	}
	if p.config != nil && !p.poolClosed {
		starts = append(starts, p.promoteQueuedLocked(p.nowLocked())...)
	}
	shouldRefill := p.config != nil && !p.poolClosed
	unregister = p.poolClosed && len(p.workers) == 0
	p.access.Unlock()
	if transportClosed {
		recordMuxWorkerEvent(p, worker, observableWorkerState(previousState), "DEAD", reason, affectedSessions)
	}
	for _, next := range starts {
		go next.worker.runProbe(next, p)
	}
	if shouldRefill {
		p.requestEnsurePrewarm()
	}
	if unregister {
		p.disableObservation()
	}
}

func (p *IncrementalWorkerPicker) drainPool(force bool) error {
	p.access.Lock()
	// Pool configuration is normally materialized by the first PickAvailable.
	// Drain can win the race against that first pick when an outbound is removed,
	// so materialize it here as well before deciding this picker is unpooled.
	if p.config == nil {
		p.config = p.Pool
	}
	if p.config == nil && !force {
		p.access.Unlock()
		return nil
	}
	if p.poolClosed && !force {
		p.access.Unlock()
		return nil
	}
	p.poolClosed = true
	p.activeHealthGeneration++
	if p.activeHealthTimer != nil {
		p.activeHealthTimer.Stop()
		p.activeHealthTimer = nil
	}
	if p.warmTimer != nil {
		p.warmTimer.Stop()
		p.warmTimer = nil
	}
	if p.cleanupTask != nil {
		common.Close(p.cleanupTask)
	}
	workers := append([]*ClientWorker(nil), p.workers...)
	unregister := len(workers) == 0
	p.access.Unlock()
	for _, worker := range workers {
		previousState, nextState, active, changed := worker.drainForPool(force)
		if !changed {
			continue
		}
		affected := uint32(0)
		if nextState == workerClosed {
			affected = active
		}
		recordMuxWorkerEvent(p, worker, observableWorkerState(previousState), observableWorkerState(nextState), "planned_rotation", affected)
	}
	if unregister {
		p.disableObservation()
	}
	return nil
}

// Drain stops probes and replenishment while allowing already active sessions
// to finish. It is used when an outbound is removed from a live Xray process.
func (p *IncrementalWorkerPicker) Drain() error {
	return p.drainPool(false)
}

// Close stops the picker and all workers during full Xray shutdown.
func (p *IncrementalWorkerPicker) Close() error {
	return p.drainPool(true)
}
