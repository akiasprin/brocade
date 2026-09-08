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

// WorkerPoolConfig enables bounded idle workers with an application-level
// Ping/Pong or recent connection I/O before offering an idle worker.
type WorkerPoolConfig struct {
	MinIdleWorkers    uint32
	MaxIdleWorkers    uint32
	MaxProbingWorkers uint32
	ProbeInterval     time.Duration
	ProbeTimeout      time.Duration
	IdleTTL           time.Duration
}

func (c *WorkerPoolConfig) Validate(maxRequests uint32) error {
	if c == nil {
		return nil
	}
	if c.MaxIdleWorkers < 1 {
		return errors.New("max idle workers must be at least 1")
	}
	if c.MinIdleWorkers > c.MaxIdleWorkers {
		return errors.New("min idle workers must not exceed max idle workers")
	}
	if c.MaxProbingWorkers < 1 || c.MaxProbingWorkers > c.MaxIdleWorkers {
		return errors.New("max probing workers must be between 1 and max idle workers")
	}
	if c.ProbeInterval < 2*time.Second || c.ProbeInterval > 60*time.Second {
		return errors.New("probe interval must be between 2 and 60 seconds")
	}
	if c.ProbeTimeout < 200*time.Millisecond || c.ProbeTimeout > 10*time.Second || c.ProbeTimeout >= c.ProbeInterval {
		return errors.New("probe timeout must be between 200 milliseconds and 10 seconds and shorter than probe interval")
	}
	if c.IdleTTL < time.Second {
		return errors.New("idle TTL must be at least 1 second")
	}
	roundedTimeout := ((c.ProbeTimeout + time.Second - 1) / time.Second) * time.Second
	if c.IdleTTL < c.ProbeInterval+roundedTimeout {
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
		w.poolState = workerClosed
		return false
	}
	w.poolOwner = owner
	w.poolClock = owner.clock
	w.strategy.WorkerPool = owner.config
	w.poolState = state
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
	w.poolState = state
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
	}
}

func (w *ClientWorker) acceptPong(probeID uint64) {
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	if (w.poolState != workerProbing && w.poolState != workerWarmDialing) || w.pendingProbeID != probeID || w.pendingProbe == nil || w.pendingProbeAcked {
		return
	}
	result := w.pendingProbe
	select {
	case result <- struct{}{}:
		w.pendingProbeAcked = true
		if w.poolOwner != nil {
			w.poolOwner.poolStats.probeAck.Add(1)
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
		r.worker.recordIO()
	}
	return mb, err
}

func (r *poolActivityReader) Interrupt()   { common.Interrupt(r.Reader) }
func (r *poolActivityReader) Close() error { return common.Close(r.Reader) }

type poolActivityWriter struct {
	buf.Writer
	worker *ClientWorker
}

func (w *poolActivityWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	hasData := !mb.IsEmpty()
	err := w.Writer.WriteMultiBuffer(mb)
	if err == nil && hasData {
		w.worker.recordIO()
	}
	return err
}

func (w *poolActivityWriter) Interrupt()   { common.Interrupt(w.Writer) }
func (w *poolActivityWriter) Close() error { return common.Close(w.Writer) }

// Track all completed non-empty Mux link reads and writes, including control
// frames. Activity never acknowledges a probe or changes its timeout deadline.
func (w *ClientWorker) recordIO() {
	w.poolAccess.Lock()
	owner := w.poolOwner
	if w.poolClock == nil || w.Closed() || w.poolState == workerClosed {
		w.poolAccess.Unlock()
		return
	}
	w.lastIO = w.poolClock.Now()
	refreshIdle := w.poolState == workerIdleReady || w.poolState == workerProbeQueued
	if refreshIdle {
		// Publish freshness under the same lock used by timers and reservations.
		// A timer that is already due must see the new deadline before it probes.
		w.poolState = workerIdleReady
		w.nextProbeAt = w.lastIO.Add(w.strategy.WorkerPool.ProbeInterval)
	}
	w.poolAccess.Unlock()
	if refreshIdle && owner != nil {
		owner.onIdleIO(w)
	}
}

func (p *IncrementalWorkerPicker) onIdleIO(w *ClientWorker) {
	p.access.Lock()
	defer p.access.Unlock()
	w.poolAccess.Lock()
	defer w.poolAccess.Unlock()
	if p.poolClosed || p.config == nil || w.Closed() || (w.poolState != workerIdleReady && w.poolState != workerProbeQueued) {
		return
	}
	now := p.nowLocked()
	due := w.lastIO.Add(p.config.ProbeInterval)
	if !now.Before(due) || !now.Before(w.poolIdleDeadlineLocked(p.config)) {
		return
	}
	w.poolState = workerIdleReady
	w.nextProbeAt = due
	w.schedulePoolTimerLocked(p, minTime(due, w.poolIdleDeadlineLocked(p.config)))
}

func (w *ClientWorker) idleReadyFreshLocked(now time.Time) bool {
	return now.Before(w.nextProbeAt) && now.Before(w.poolIdleDeadlineLocked(w.strategy.WorkerPool))
}

func (w *ClientWorker) markPoolClosedLocked() bool {
	if w.poolState == workerClosed {
		return false
	}
	w.stopPoolTimerLocked()
	w.probeGeneration++
	w.pendingProbe = nil
	w.pendingProbeAcked = false
	w.poolReservations = 0
	w.poolState = workerClosed
	return true
}

func (w *ClientWorker) closeForPool() {
	w.poolAccess.Lock()
	closeDone := w.markPoolClosedLocked()
	w.poolAccess.Unlock()
	if closeDone {
		common.Must(w.done.Close())
	}
}

func (w *ClientWorker) drainForPool(force bool) {
	w.poolAccess.Lock()
	if w.poolState == workerClosed {
		w.poolAccess.Unlock()
		return
	}
	w.stopPoolTimerLocked()
	w.probeGeneration++
	w.pendingProbe = nil
	w.pendingProbeAcked = false
	w.poolReservations = 0
	if !force && w.sessionManager.Size() > 0 {
		w.poolState = workerDraining
		w.poolAccess.Unlock()
		return
	}
	w.poolState = workerClosed
	w.poolAccess.Unlock()
	common.Must(w.done.Close())
}

func (w *ClientWorker) runProbe(run probeRun, owner *IncrementalWorkerPicker) {
	now := owner.now()
	w.poolAccess.Lock()
	wait := owner.config.ProbeTimeout
	outcomeOnTimer := probeTimedOut
	if !w.idleSince.IsZero() {
		if untilTTL := w.poolIdleDeadlineLocked(owner.config).Sub(now); untilTTL <= wait {
			wait = untilTTL
			outcomeOnTimer = probeIdleTTLExpired
		}
	}
	w.poolAccess.Unlock()
	if wait < 0 {
		wait = 0
	}
	// Start the deadline before writing the Ping. A size-limited outbound pipe
	// can itself block when the underlying transport is half-open; the timeout
	// callback closes the worker, whose monitor interrupts that blocked writer.
	timer := owner.afterFunc(wait, func() {
		owner.onProbeFinished(run, outcomeOnTimer)
	})
	defer timer.Stop()
	if err := writeProbeFrame(w.link.Writer, run.id, false); err != nil {
		owner.onProbeFinished(run, probeTransportFailed)
		return
	}
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
		state, _ := worker.poolStateSnapshot()
		// done may close just before monitor reports the terminal state. Keep the
		// reservation until that report is processed so warm failure backoff wins
		// the race with the replenishment loop.
		if state == workerClosed {
			continue
		}
		if isReservedIdleState(state) {
			reserved++
		}
		if isProbingState(state) {
			probing++
		}
	}
	return reserved, probing
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
	closeDone := oldest.markPoolClosedLocked()
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
	p.requestEnsureMinIdle()
}

func (p *IncrementalWorkerPicker) onWorkerIdle(worker *ClientWorker) {
	var closeWorkers []*ClientWorker
	var run *probeRun

	p.access.Lock()
	p.initializePoolLocked()
	if p.poolClosed || p.config == nil {
		p.access.Unlock()
		worker.closeForPool()
		return
	}
	now := p.nowLocked()
	worker.poolAccess.Lock()
	if worker.poolState == workerDraining && worker.sessionManager.Size() == 0 {
		if worker.markPoolClosedLocked() {
			closeWorkers = append(closeWorkers, worker)
		}
		worker.poolAccess.Unlock()
		p.access.Unlock()
		for _, item := range closeWorkers {
			common.Must(item.done.Close())
		}
		return
	}
	if worker.poolState != workerActive || worker.poolReservations != 0 || worker.sessionManager.Size() != 0 {
		worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	if worker.strategy.MaxConnection > 0 && worker.sessionManager.Count() >= int(worker.strategy.MaxConnection) {
		if worker.markPoolClosedLocked() {
			closeWorkers = append(closeWorkers, worker)
		}
		worker.poolAccess.Unlock()
		p.access.Unlock()
		for _, item := range closeWorkers {
			common.Must(item.done.Close())
		}
		p.requestEnsureMinIdle()
		return
	}

	reserved, probing := p.poolCountsLocked(worker)
	if reserved >= p.config.MaxIdleWorkers {
		if victim := p.closeOldestIdleReadyLocked(worker); victim != nil {
			closeWorkers = append(closeWorkers, victim)
			p.poolStats.workerClosedMaxIdle.Add(1)
			reserved--
		} else {
			if worker.markPoolClosedLocked() {
				closeWorkers = append(closeWorkers, worker)
				p.poolStats.workerClosedMaxIdle.Add(1)
			}
			worker.poolAccess.Unlock()
			p.access.Unlock()
			for _, item := range closeWorkers {
				common.Must(item.done.Close())
			}
			return
		}
	}
	worker.idleSince = now
	worker.nextProbeAt = now
	if !worker.lastIO.IsZero() && now.Before(worker.lastIO.Add(p.config.ProbeInterval)) {
		worker.poolState = workerIdleReady
		worker.nextProbeAt = worker.lastIO.Add(p.config.ProbeInterval)
		worker.schedulePoolTimerLocked(p, minTime(worker.nextProbeAt, worker.poolIdleDeadlineLocked(p.config)))
	} else if probing < p.config.MaxProbingWorkers {
		started := worker.beginProbeLocked(workerProbing)
		run = &started
	} else {
		worker.poolState = workerProbeQueued
		worker.schedulePoolTimerLocked(p, worker.poolIdleDeadlineLocked(p.config))
	}
	worker.poolAccess.Unlock()
	p.access.Unlock()

	for _, item := range closeWorkers {
		common.Must(item.done.Close())
	}
	if run != nil {
		go worker.runProbe(*run, p)
	}
	p.requestEnsureMinIdle()
}

func (p *IncrementalWorkerPicker) onWorkerTimer(worker *ClientWorker, token uint64) {
	var closeWorker bool
	var run *probeRun

	p.access.Lock()
	if p.poolClosed || p.config == nil {
		p.access.Unlock()
		worker.closeForPool()
		return
	}
	now := p.nowLocked()
	worker.poolAccess.Lock()
	if token != worker.poolTimerGeneration || (worker.poolState != workerIdleReady && worker.poolState != workerProbeQueued) {
		worker.poolAccess.Unlock()
		p.access.Unlock()
		return
	}
	if !now.Before(worker.poolIdleDeadlineLocked(p.config)) {
		closeWorker = worker.markPoolClosedLocked()
		if closeWorker {
			p.poolStats.workerClosedIdleTTL.Add(1)
		}
	} else if worker.poolState == workerIdleReady && now.Before(worker.nextProbeAt) {
		worker.schedulePoolTimerLocked(p, minTime(worker.nextProbeAt, worker.poolIdleDeadlineLocked(p.config)))
	} else {
		_, probing := p.poolCountsLocked(worker)
		if probing < p.config.MaxProbingWorkers {
			started := worker.beginProbeLocked(workerProbing)
			run = &started
		} else {
			worker.poolState = workerProbeQueued
			worker.schedulePoolTimerLocked(p, worker.poolIdleDeadlineLocked(p.config))
		}
	}
	worker.poolAccess.Unlock()
	p.access.Unlock()

	if closeWorker {
		common.Must(worker.done.Close())
		p.requestEnsureMinIdle()
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
	if outcome == probeSucceeded && !now.Before(run.worker.poolIdleDeadlineLocked(p.config)) {
		outcome = probeIdleTTLExpired
	}
	if p.poolClosed || outcome != probeSucceeded {
		closeWorker = run.worker.markPoolClosedLocked()
		switch outcome {
		case probeTimedOut:
			p.poolStats.probeTimeout.Add(1)
			p.poolStats.workerClosedProbe.Add(1)
			p.noteWarmFailureLocked(now)
			logTimeout, suppressed = p.probeTimeoutLogLocked(now)
		case probeTransportFailed:
			p.poolStats.workerClosedProbe.Add(1)
			p.noteWarmFailureLocked(now)
		case probeIdleTTLExpired:
			p.poolStats.workerClosedIdleTTL.Add(1)
		}
	} else {
		run.worker.poolState = workerIdleReady
		run.worker.nextProbeAt = now.Add(p.jitterLocked(p.config.ProbeInterval))
		run.worker.schedulePoolTimerLocked(p, minTime(run.worker.nextProbeAt, run.worker.poolIdleDeadlineLocked(p.config)))
		if wasWarm {
			p.warmFailures = 0
			p.nextWarmAttempt = time.Time{}
		}
	}
	run.worker.poolAccess.Unlock()
	if !p.poolClosed {
		starts = append(starts, p.promoteQueuedLocked(now)...)
	}
	p.access.Unlock()

	if closeWorker {
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
	p.requestEnsureMinIdle()
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
			state, since := worker.poolStateSnapshot()
			if state != workerProbeQueued || (candidate != nil && !since.Before(oldest)) {
				continue
			}
			candidate = worker
			oldest = since
		}
		if candidate == nil {
			break
		}
		candidate.poolAccess.Lock()
		if candidate.poolState != workerProbeQueued {
			candidate.poolAccess.Unlock()
			continue
		}
		if !now.Before(candidate.poolIdleDeadlineLocked(p.config)) {
			closeDone := candidate.markPoolClosedLocked()
			candidate.poolAccess.Unlock()
			if closeDone {
				p.poolStats.workerClosedIdleTTL.Add(1)
				go common.Must(candidate.done.Close())
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

func (p *IncrementalWorkerPicker) requestEnsureMinIdle() {
	p.access.Lock()
	if p.config == nil || p.poolClosed || !p.poolUsed || p.config.MinIdleWorkers == 0 || p.warmRunning {
		p.access.Unlock()
		return
	}
	p.warmRunning = true
	p.access.Unlock()
	go p.ensureMinIdle()
}

func (p *IncrementalWorkerPicker) ensureMinIdle() {
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
		if reserved >= p.config.MinIdleWorkers || reserved >= p.config.MaxIdleWorkers || probing >= p.config.MaxProbingWorkers {
			p.warmRunning = false
			p.access.Unlock()
			return
		}
		if now.Before(p.nextWarmAttempt) {
			delay := p.nextWarmAttempt.Sub(now)
			if p.warmTimer != nil {
				p.warmTimer.Stop()
			}
			p.warmTimer = p.afterFuncLocked(delay, p.requestEnsureMinIdle)
			p.warmRunning = false
			p.access.Unlock()
			return
		}
		p.access.Unlock()

		worker, err := p.Factory.Create()
		if err != nil {
			p.access.Lock()
			p.noteWarmFailureLocked(p.nowLocked())
			p.warmRunning = false
			delay := p.nextWarmAttempt.Sub(p.nowLocked())
			p.warmTimer = p.afterFuncLocked(delay, p.requestEnsureMinIdle)
			p.access.Unlock()
			errors.LogInfoInner(context.Background(), err, "failed to create warm mux worker")
			return
		}

		p.access.Lock()
		now = p.nowLocked()
		reserved, probing = p.poolCountsLocked(nil)
		if p.poolClosed || reserved >= p.config.MinIdleWorkers || reserved >= p.config.MaxIdleWorkers || probing >= p.config.MaxProbingWorkers {
			p.access.Unlock()
			worker.closeForPool()
			continue
		}
		if !worker.attachPool(p, workerWarmDialing, now) {
			p.noteWarmFailureLocked(now)
			p.warmRunning = false
			delay := p.nextWarmAttempt.Sub(now)
			p.warmTimer = p.afterFuncLocked(delay, p.requestEnsureMinIdle)
			p.access.Unlock()
			worker.closeForPool()
			return
		}
		p.workers = append(p.workers, worker)
		p.poolStats.workerCreatedWarm.Add(1)
		worker.poolAccess.Lock()
		started := worker.beginProbeLocked(workerWarmDialing)
		worker.poolAccess.Unlock()
		p.access.Unlock()
		go worker.runProbe(started, p)
	}
}

func (p *IncrementalWorkerPicker) onWorkerClosed(worker *ClientWorker) {
	var starts []probeRun

	p.access.Lock()
	worker.poolAccess.Lock()
	previousState := worker.poolState
	worker.markPoolClosedLocked()
	worker.poolAccess.Unlock()
	for i, candidate := range p.workers {
		if candidate == worker {
			copy(p.workers[i:], p.workers[i+1:])
			p.workers[len(p.workers)-1] = nil
			p.workers = p.workers[:len(p.workers)-1]
			break
		}
	}
	if p.config != nil && !p.poolClosed && isReservedIdleState(previousState) {
		if isProbingState(previousState) {
			p.poolStats.workerClosedProbe.Add(1)
		}
		p.noteWarmFailureLocked(p.nowLocked())
	}
	if p.config != nil && !p.poolClosed {
		starts = append(starts, p.promoteQueuedLocked(p.nowLocked())...)
	}
	shouldRefill := p.config != nil && !p.poolClosed
	p.access.Unlock()
	for _, next := range starts {
		go next.worker.runProbe(next, p)
	}
	if shouldRefill {
		p.requestEnsureMinIdle()
	}
}

func (p *IncrementalWorkerPicker) drainPool(force bool) error {
	p.access.Lock()
	// Pool configuration is normally materialized by the first PickAvailable.
	// Drain can win the race against that first pick when an outbound is removed,
	// so materialize it here as well before deciding this is a legacy picker.
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
	if p.warmTimer != nil {
		p.warmTimer.Stop()
		p.warmTimer = nil
	}
	if p.cleanupTask != nil {
		common.Close(p.cleanupTask)
	}
	workers := append([]*ClientWorker(nil), p.workers...)
	p.access.Unlock()
	for _, worker := range workers {
		worker.drainForPool(force)
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
