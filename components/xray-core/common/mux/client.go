package mux

import (
	"context"
	goerrors "errors"
	"io"
	"sync"
	"sync/atomic"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal/done"
	"github.com/xtls/xray-core/common/task"
	"github.com/xtls/xray-core/common/xudp"
	"github.com/xtls/xray-core/proxy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/pipe"
)

type ClientManager struct {
	Enabled bool // whether mux is enabled from user config
	Picker  WorkerPicker
}

func (m *ClientManager) Drain() error {
	if m == nil || m.Picker == nil {
		return nil
	}
	if drainer, ok := m.Picker.(interface{ Drain() error }); ok {
		return drainer.Drain()
	}
	return nil
}

func (m *ClientManager) Close() error {
	if m == nil || m.Picker == nil {
		return nil
	}
	return common.Close(m.Picker)
}

func (m *ClientManager) Dispatch(ctx context.Context, link *transport.Link) error {
	for i := 0; i < 16; i++ {
		if err := ctx.Err(); err != nil {
			return err
		}
		worker, err := m.Picker.PickAvailable()
		if err != nil {
			return err
		}
		if worker.Dispatch(ctx, link) {
			return nil
		}
	}

	return errors.New("unable to find an available mux client").AtWarning()
}

type WorkerPicker interface {
	PickAvailable() (*ClientWorker, error)
}

type IncrementalWorkerPicker struct {
	Factory ClientWorkerFactory
	Pool    *WorkerPoolConfig
	Tag     string

	// Observation identity is assigned only to runtime outbounds. Tests and other embedders that
	// construct a picker directly keep the data-plane behavior without entering the global
	// snapshot registry.
	observationID   atomic.Uint64
	observationKind string

	access                 sync.Mutex
	workers                []*ClientWorker
	cleanupTask            *task.Periodic
	config                 *WorkerPoolConfig
	poolClosed             bool
	poolUsed               bool
	warmRunning            bool
	warmCreating           bool // An out-of-lock factory call reserves one base connection.
	warmFailures           int
	nextWarmAttempt        time.Time
	warmTimer              poolTimer
	lastTimeoutLog         time.Time
	suppressedLogs         uint64
	clock                  poolClock
	jitter                 func(time.Duration) time.Duration
	poolStats              workerPoolStats
	activeHealthTimer      poolTimer
	activeHealthGeneration uint64
	healthDialAttempts     int
	nextHealthDial         time.Time
}

func (p *IncrementalWorkerPicker) cleanupFunc() error {
	p.access.Lock()
	defer p.access.Unlock()

	if len(p.workers) == 0 {
		return errors.New("no worker")
	}

	p.cleanup()
	return nil
}

func (p *IncrementalWorkerPicker) cleanup() {
	var activeWorkers []*ClientWorker
	for _, w := range p.workers {
		if !w.Closed() {
			activeWorkers = append(activeWorkers, w)
		}
	}
	p.workers = activeWorkers
}

func (p *IncrementalWorkerPicker) findAvailable(forceReuse ...bool) (int, clientWorkerState) {
	minIdx := -1
	var minLoad uint32
	if p.config == nil {
		for idx, worker := range p.workers {
			if worker.IsFull() {
				continue
			}
			load := worker.ActiveConnections()
			if minIdx == -1 || load < minLoad {
				minIdx = idx
				minLoad = load
			}
		}
		return minIdx, workerActive
	}

	// Use healthy idle capacity first, then grow to the base threshold before
	// sharing active workers. This is a soft threshold: unavailable/full workers
	// never block overflow creation. Include warm dials and dispatch reservations
	// in the accounting so concurrent requests cannot reuse the same budget.
	// A worker can change before reservation; exclude a lost candidate and retry
	// rather than dialing while other usable capacity still exists.
	excluded := make([]bool, len(p.workers))
	for {
		minIdx = -1
		var minState clientWorkerState
		atBase := p.poolWorkerCountLocked(nil) >= uint64(p.config.ReuseThreshold) || (len(forceReuse) > 0 && forceReuse[0])
		for idx, worker := range p.workers {
			if excluded[idx] {
				continue
			}
			state, load, available := worker.poolDispatchCandidate()
			if !available || (state == workerActive && !atBase) {
				continue
			}
			if minIdx == -1 ||
				(state == workerIdleReady && minState == workerActive) ||
				(state == minState && load < minLoad) {
				minIdx = idx
				minState = state
				minLoad = load
			}
		}
		if minIdx == -1 {
			return -1, workerClosed
		}
		if p.workers[minIdx].reserveForDispatch(minState, false) {
			return minIdx, minState
		}
		excluded[minIdx] = true
	}
}

func (p *IncrementalWorkerPicker) pickInternal() (*ClientWorker, bool, error) {
	p.access.Lock()
	defer p.access.Unlock()
	if p.poolClosed {
		return nil, false, errors.New("mux worker picker is closed")
	}
	if p.config == nil {
		p.config = p.Pool
	}

	idx, reusedState := p.findAvailable()
	if idx >= 0 {
		return p.takeAvailableLocked(idx, reusedState), false, nil
	}

	p.cleanup()

	replacingUnhealthy := p.config != nil && p.hasUnhealthyWorkerLocked()
	if replacingUnhealthy && p.nowLocked().Before(p.nextHealthDial) {
		if idx, state := p.findAvailable(true); idx >= 0 {
			return p.takeAvailableLocked(idx, state), false, nil
		}
		p.poolStats.dispatchRejected.Add(1)
		p.poolStats.healthDialThrottled.Add(1)
		return nil, false, errors.New("mux health replacement is backing off")
	}
	worker, err := p.Factory.Create()
	if err != nil {
		if replacingUnhealthy {
			p.noteHealthDialLocked()
		}
		if p.config != nil {
			// Growing to the base is a preference, not a reason to reject a
			// request when creation fails but healthy active capacity remains.
			if idx, state := p.findAvailable(true); idx >= 0 {
				return p.takeAvailableLocked(idx, state), false, nil
			}
			p.poolStats.dispatchRejected.Add(1)
		}
		return nil, false, err
	}
	if p.config != nil && !worker.attachPool(p, workerActive, p.nowLocked()) {
		common.Close(worker)
		p.poolStats.dispatchRejected.Add(1)
		return nil, false, errors.New("new mux worker closed before use")
	}
	if replacingUnhealthy {
		worker.poolAccess.Lock()
		worker.healthReplacement = true
		worker.healthNextProbe = p.nowLocked()
		worker.poolAccess.Unlock()
		p.noteHealthDialLocked()
	}
	if p.config != nil && !worker.reserveForDispatch(workerActive, true) {
		common.Close(worker)
		p.poolStats.dispatchRejected.Add(1)
		return nil, false, errors.New("new mux worker could not reserve its first request")
	}
	p.workers = append(p.workers, worker)
	if p.config != nil {
		p.poolStats.dispatchTotal.Add(1)
		p.poolStats.dispatchDemandDial.Add(1)
		p.poolStats.workerCreatedDemand.Add(1)
		p.warmFailures = 0
		p.nextWarmAttempt = time.Time{}
		if p.warmTimer != nil {
			p.warmTimer.Stop()
			p.warmTimer = nil
		}
		recordMuxWorkerEvent(p, worker, "DIALING", "READY", "demand_created", 0)
	}

	if p.config == nil && p.cleanupTask == nil {
		p.cleanupTask = &task.Periodic{
			Interval: time.Second * 30,
			Execute:  p.cleanupFunc,
		}
	}

	return worker, true, nil
}

func (p *IncrementalWorkerPicker) takeAvailableLocked(idx int, state clientWorkerState) *ClientWorker {
	worker := p.workers[idx]
	n := len(p.workers)
	if n > 1 && idx != n-1 {
		p.workers[n-1], p.workers[idx] = p.workers[idx], p.workers[n-1]
	}
	if p.config != nil {
		p.poolStats.dispatchTotal.Add(1)
		if state == workerIdleReady {
			p.poolStats.dispatchIdleReuse.Add(1)
		} else {
			p.poolStats.dispatchActiveReuse.Add(1)
		}
	}
	return worker
}

func (p *IncrementalWorkerPicker) PickAvailable() (*ClientWorker, error) {
	worker, start, err := p.pickInternal()
	if start && p.cleanupTask != nil {
		common.Must(p.cleanupTask.Start())
	}

	if p.Pool != nil {
		p.runActiveHealth()
	}
	return worker, err
}

type ClientWorkerFactory interface {
	Create() (*ClientWorker, error)
}

type DialingWorkerFactory struct {
	Proxy    proxy.Outbound
	Dialer   internet.Dialer
	Strategy ClientStrategy
}

func (f *DialingWorkerFactory) Create() (*ClientWorker, error) {
	opts := []pipe.Option{pipe.WithSizeLimit(64 * 1024)}
	uplinkReader, upLinkWriter := pipe.New(opts...)
	downlinkReader, downlinkWriter := pipe.New(opts...)

	c, err := NewClientWorker(transport.Link{
		Reader: downlinkReader,
		Writer: upLinkWriter,
	}, f.Strategy)
	if err != nil {
		return nil, err
	}

	go func(p proxy.Outbound, d internet.Dialer, c *done.Instance) {
		outbounds := []*session.Outbound{{
			Target: net.TCPDestination(muxCoolAddress, muxCoolPort),
		}}
		ctx := session.ContextWithOutbounds(context.Background(), outbounds)
		ctx, cancel := context.WithCancel(ctx)
		go func() {
			select {
			case <-c.Wait():
				cancel()
			case <-ctx.Done():
			}
		}()

		if errP := p.Process(ctx, &transport.Link{Reader: uplinkReader, Writer: downlinkWriter}, d); errP != nil {
			errC := errors.Cause(errP)
			if !(goerrors.Is(errC, io.EOF) || goerrors.Is(errC, io.ErrClosedPipe) || goerrors.Is(errC, context.Canceled)) {
				errors.LogInfoInner(ctx, errP, "failed to handler mux client connection")
			}
		}
		cancel()
		common.Must(c.Close())
	}(f.Proxy, f.Dialer, c.done)

	return c, nil
}

type ClientStrategy struct {
	MaxConcurrency uint32
	MaxConnection  uint32
	WorkerPool     *WorkerPoolConfig
}

type ClientWorker struct {
	health             *ReverseHealth
	sessionManager     *SessionManager
	link               transport.Link
	done               *done.Instance
	timer              *time.Ticker
	strategy           ClientStrategy
	workerID           uint64
	endingSessions     atomic.Int32 // Local slots released, End/control writes still pending.
	sessionEndTimedOut atomic.Bool

	poolAccess          sync.Mutex
	poolOwner           *IncrementalWorkerPicker
	poolClock           poolClock
	poolState           clientWorkerState
	poolReason          string // Lifecycle reason for poolState; health reasons are kept separately.
	idleSince           time.Time
	lastRead            time.Time // Remote activity eligible for the idle reuse window.
	nextProbeAt         time.Time
	poolTimer           poolTimer
	poolTimerGeneration uint64
	probeGeneration     uint64
	nextProbeID         uint64
	pendingProbeID      uint64
	pendingProbe        chan struct{}
	pendingProbeAcked   bool
	pendingProbeSentAt  time.Time
	poolReservations    uint32
	poolProbes          uint64
	poolAcks            uint64
	poolTimeouts        uint64
	lastProbeAck        time.Time
	lastProbeRTT        time.Duration
	poolControl         *healthWriter
	healthState         poolHealthState
	healthReason        string
	healthLeaseUntil    time.Time
	healthNextProbe     time.Time
	healthConfirmUntil  time.Time
	healthProbeID       uint64
	healthProbeDeadline time.Time
	healthProbeSentAt   time.Time
	healthRecoveryAcks  uint32
	healthReplacement   bool
}

var (
	muxCoolAddress = net.DomainAddress("v1.mux.cool")
	muxCoolPort    = net.Port(9527)
)

// NewClientWorker creates a new mux.Client.
func NewClientWorker(stream transport.Link, s ClientStrategy) (*ClientWorker, error) {
	return newClientWorker(stream, s, nil)
}

// NewReverseClientWorker creates a reverse mux client whose capacity and
// lifecycle are owned entirely by ReverseHealthConfig. Keeping this separate
// from ClientStrategy prevents ordinary mux pool policy from being combined
// with reverse-worker policy at a call site.
func NewReverseClientWorker(stream transport.Link, config ReverseHealthConfig) (*ClientWorker, error) {
	return newClientWorker(stream, ClientStrategy{
		MaxConcurrency: config.MaxSessionsPerWorker,
		// Session IDs are uint16 and are never reused. This is a protocol ceiling,
		// not a user-configurable reverse-worker lifetime policy.
		MaxConnection: uint32(^uint16(0)),
	}, &config)
}

func newClientWorker(stream transport.Link, s ClientStrategy, reverseHealth *ReverseHealthConfig) (*ClientWorker, error) {
	c := &ClientWorker{
		sessionManager: NewSessionManager(),
		link:           stream,
		done:           done.New(),
		strategy:       s,
		poolState:      workerActive,
		poolReason:     "serving",
		workerID:       newProbeSeed(),
	}
	if reverseHealth != nil {
		c.link.Writer = newHealthWriter(stream.Writer, c.done)
		hc := *reverseHealth
		hc.ActiveSessions = c.ActiveConnections
		hc.DrainIdle = func() bool { return c.sessionManager.Size() == 0 }
		c.health = newReverseHealth(hc, c.link.Writer.(*healthWriter), c.done)
		c.sessionManager.SetOnEmpty(func() {
			if c.IsClosing() {
				c.Close()
			}
		})
	} else if s.WorkerPool == nil {
		c.timer = time.NewTicker(time.Second * 16)
	} else {
		c.nextProbeID = newProbeSeed()
		c.sessionManager.SetOnEmpty(c.onSessionEmpty)
		c.link.Reader = &poolActivityReader{Reader: c.link.Reader, worker: c}
		c.poolControl = newHealthWriter(c.link.Writer, c.done)
		c.link.Writer = c.poolControl
	}

	go c.fetchOutput()
	go c.monitor()

	return c, nil
}

func (m *ClientWorker) TotalConnections() uint32 {
	return uint32(m.sessionManager.Count())
}

func (m *ClientWorker) ActiveConnections() uint32 {
	return uint32(m.sessionManager.Size())
}

// Closed returns true if this Client is closed.
func (m *ClientWorker) Closed() bool {
	return m.done.Done()
}

func (m *ClientWorker) WaitClosed() <-chan struct{} {
	return m.done.Wait()
}

func (m *ClientWorker) Close() error {
	if m.health != nil {
		m.health.stop("closed")
	}
	return m.done.Close()
}

func (m *ClientWorker) monitor() {
	if m.timer == nil {
		<-m.done.Wait()
		if m.health != nil {
			m.health.stop("transport_closed")
		}
		affectedSessions := uint32(m.sessionManager.Size())
		m.sessionManager.Close()
		common.Interrupt(m.link.Writer)
		common.Interrupt(m.link.Reader)
		m.poolAccess.Lock()
		owner := m.poolOwner
		if owner == nil {
			m.setPoolStateLocked(workerClosed, "transport_closed")
			m.stopPoolTimerLocked()
		}
		m.poolAccess.Unlock()
		if owner != nil {
			owner.onWorkerClosed(m, affectedSessions)
		}
		return
	}
	defer m.timer.Stop()

	for {
		checkSize := m.sessionManager.Size()
		checkCount := m.sessionManager.Count()
		select {
		case <-m.done.Wait():
			if m.health != nil {
				m.health.stop("transport_closed")
			}
			m.sessionManager.Close()
			common.Interrupt(m.link.Writer)
			common.Interrupt(m.link.Reader)
			return
		case <-m.timer.C:
			if m.sessionManager.CloseIfNoSessionAndIdle(checkSize, checkCount) {
				common.Must(m.done.Close())
			}
		}
	}
}

func writeFirstPayload(reader buf.Reader, writer *Writer) error {
	err := buf.CopyOnceTimeout(reader, writer, time.Millisecond*100)
	if err == buf.ErrNotTimeoutReader || err == buf.ErrReadTimeout {
		return writer.WriteMultiBuffer(buf.MultiBuffer{})
	}

	if err != nil {
		return err
	}

	return nil
}

func fetchInput(ctx context.Context, s *Session, output buf.Writer) {
	outbounds := session.OutboundsFromContext(ctx)
	ob := outbounds[len(outbounds)-1]
	transferType := protocol.TransferTypeStream
	if ob.Target.Network == net.Network_UDP {
		transferType = protocol.TransferTypePacket
	}
	s.transferType = transferType
	var inbound *session.Inbound
	if session.IsReverseMuxFromContext(ctx) {
		inbound = session.InboundFromContext(ctx)
	}
	writer := NewWriter(s.ID, ob.Target, s.frameWriter(output), transferType, xudp.GetGlobalID(ctx), inbound)
	defer s.finishInput(writer)

	errors.LogInfo(ctx, "dispatching request to ", ob.Target)
	if err := writeFirstPayload(s.input, writer); err != nil {
		errors.LogInfoInner(ctx, err, "failed to write first payload")
		writer.hasError = true
		return
	}

	if err := buf.Copy(s.input, writer); err != nil {
		errors.LogInfoInner(ctx, err, "failed to fetch all input")
		writer.hasError = true
		return
	}
}

func (m *ClientWorker) IsClosing() bool {
	sm := m.sessionManager
	if m.strategy.MaxConnection > 0 && sm.Count() >= int(m.strategy.MaxConnection) {
		return true
	}
	return false
}

// IsFull returns true if this ClientWorker is unable to accept more connections.
// it might be because it is closing, or the number of connections has reached the limit.
func (m *ClientWorker) IsFull() bool {
	if m.strategy.WorkerPool == nil {
		if m.IsClosing() || m.Closed() {
			return true
		}
		sm := m.sessionManager
		return m.strategy.MaxConcurrency > 0 && sm.Size() >= int(m.strategy.MaxConcurrency)
	}
	m.poolAccess.Lock()
	defer m.poolAccess.Unlock()
	if m.endingSessions.Load() != 0 {
		return true
	}
	if m.poolState != workerActive && m.poolState != workerIdleReady {
		return true
	}
	if m.poolState == workerActive && m.poolClock != nil && !m.activeHealthUsableLocked(m.poolClock.Now()) {
		return true
	}
	if m.poolState == workerIdleReady && (m.poolClock == nil || !m.idleReadyFreshLocked(m.poolClock.Now())) {
		return true
	}
	if m.IsClosing() || m.Closed() {
		return true
	}

	sm := m.sessionManager
	active := uint32(sm.Size())
	if m.poolState == workerActive && active == 0 && m.poolReservations == 0 && m.poolOwner != nil {
		return true
	}
	if m.strategy.MaxConcurrency > 0 && active+m.poolReservations >= m.strategy.MaxConcurrency {
		return true
	}
	if m.strategy.MaxConnection > 0 && uint32(sm.Count())+m.poolReservations >= m.strategy.MaxConnection {
		return true
	}
	return false
}

func (m *ClientWorker) poolDispatchLoadLocked(desired clientWorkerState, allowEmptyActive bool) (uint32, bool) {
	if (desired != workerActive && desired != workerIdleReady) || m.poolState != desired || m.IsClosing() || m.Closed() || m.endingSessions.Load() != 0 {
		return 0, false
	}
	if desired == workerActive && m.poolClock != nil && !m.activeHealthUsableLocked(m.poolClock.Now()) {
		return 0, false
	}
	// A due timer may still be waiting to run. Never let scheduling latency extend
	// the idle reuse window or the business idle TTL.
	if desired == workerIdleReady && (m.poolClock == nil || !m.idleReadyFreshLocked(m.poolClock.Now())) {
		return 0, false
	}
	active := uint32(m.sessionManager.Size())
	if desired == workerActive && active == 0 && m.poolReservations == 0 && !allowEmptyActive {
		return 0, false
	}
	if m.strategy.MaxConcurrency > 0 && active+m.poolReservations >= m.strategy.MaxConcurrency {
		return 0, false
	}
	if m.strategy.MaxConnection > 0 && uint32(m.sessionManager.Count())+m.poolReservations >= m.strategy.MaxConnection {
		return 0, false
	}
	// Reservations are requests already assigned by the picker but not yet
	// materialized in the session map. Count them as load so concurrent callers
	// spread out instead of reserving the same apparently-idle worker in a burst.
	return active + m.poolReservations, true
}

func (m *ClientWorker) poolDispatchCandidate() (clientWorkerState, uint32, bool) {
	m.poolAccess.Lock()
	defer m.poolAccess.Unlock()
	state := m.poolState
	load, available := m.poolDispatchLoadLocked(state, false)
	return state, load, available
}

// reserveForDispatch makes Picker selection and capacity accounting one atomic
// operation. In particular, an active worker whose last session has already
// left the map cannot be reused in the short interval before its onEmpty
// callback completes the idle transition.
func (m *ClientWorker) reserveForDispatch(desired clientWorkerState, allowEmptyActive bool) bool {
	m.poolAccess.Lock()
	defer m.poolAccess.Unlock()
	if _, available := m.poolDispatchLoadLocked(desired, allowEmptyActive); !available {
		return false
	}
	m.poolReservations++
	m.stopPoolTimerLocked()
	m.setPoolStateLocked(workerActive, "serving")
	m.idleSince = time.Time{}
	m.nextProbeAt = time.Time{}
	return true
}

func (m *ClientWorker) Dispatch(ctx context.Context, link *transport.Link) bool {
	sm := m.sessionManager
	var s *Session
	var owner *IncrementalWorkerPicker
	startedDraining := false
	if m.strategy.WorkerPool == nil {
		if m.IsClosing() || m.Closed() || (m.strategy.MaxConcurrency > 0 && sm.Size() >= int(m.strategy.MaxConcurrency)) {
			return false
		}
		if m.health != nil {
			m.health.mu.Lock()
			if m.health.usableLocked(time.Now()) {
				s = sm.allocateLink(&m.strategy, link)
			} else {
				m.health.snapshot.RejectedDispatches++
				m.health.counters.rejected.Add(1)
			}
			m.health.mu.Unlock()
		} else {
			s = sm.allocateLink(&m.strategy, link)
		}
		if s == nil {
			return false
		}
	} else {
		m.poolAccess.Lock()
		managed := m.poolOwner != nil
		if managed {
			if m.poolReservations == 0 {
				m.poolAccess.Unlock()
				return false
			}
			m.poolReservations--
		}
		if ctx.Err() != nil || m.endingSessions.Load() != 0 {
			owner = m.poolOwner
			m.poolAccess.Unlock()
			if owner != nil {
				owner.onWorkerIdle(m)
			}
			return false
		}
		if m.poolState != workerActive && m.poolState != workerIdleReady {
			m.poolAccess.Unlock()
			m.onSessionEmpty()
			return false
		}
		if m.poolClock != nil && !m.activeHealthUsableLocked(m.poolClock.Now()) {
			m.poolAccess.Unlock()
			m.onSessionEmpty()
			return false
		}
		if m.IsClosing() || m.Closed() || (m.strategy.MaxConcurrency > 0 && sm.Size() >= int(m.strategy.MaxConcurrency)) {
			m.poolAccess.Unlock()
			m.onSessionEmpty()
			return false
		}
		clock := m.poolClock
		if clock == nil {
			clock = realPoolClock{}
		}
		lifecycle := &sessionLifecycle{
			clock: clock, timeout: sessionEndTimeout,
			begin: func() { m.endingSessions.Add(1) },
			finish: func() {
				if m.endingSessions.Add(-1) == 0 {
					m.onSessionEmpty()
				}
			},
			fail: func() { m.Close() },
			onTimeout: func() {
				m.drainSessionEndTimeout()
			},
		}
		s = sm.allocateLinkWithLifecycle(&m.strategy, link, lifecycle)
		if s == nil {
			m.poolAccess.Unlock()
			m.onSessionEmpty()
			return false
		}
		m.stopPoolTimerLocked()
		if m.strategy.MaxConnection > 0 && sm.Count() >= int(m.strategy.MaxConnection) {
			if m.poolState != workerDraining && m.poolOwner != nil {
				m.poolOwner.poolStats.workerClosedRequests.Add(1)
				startedDraining = true
			}
			m.setPoolStateLocked(workerDraining, "request_limit")
			m.healthProbeID = 0
		} else {
			m.setPoolStateLocked(workerActive, "serving")
		}
		m.idleSince = time.Time{}
		m.nextProbeAt = time.Time{}
		owner = m.poolOwner
		m.poolAccess.Unlock()
	}
	if startedDraining {
		recordMuxWorkerEvent(owner, m, "READY", "DRAINING", "request_limit", 0)
	}
	if m.health != nil && m.IsClosing() {
		m.health.Drain()
	}
	_, asynchronousPipe := link.Reader.(*pipe.Reader)
	var stopCancellation func() bool
	if !asynchronousPipe {
		stopCancellation = s.watchCancellation(ctx)
	}
	go func() {
		if stopCancellation != nil {
			defer stopCancellation()
		}
		fetchInput(ctx, s, m.link.Writer)
	}()
	if owner != nil {
		// Some ingress readers keep Dispatch blocked for the entire session.
		// Prewarming is triggered by allocation, not by that session's EOF.
		owner.onWorkerUsed(m)
	}
	if !asynchronousPipe {
		select {
		case <-ctx.Done():
		case <-s.done.Wait():
		}
	}
	return true
}

func (m *ClientWorker) onSessionEmpty() {
	// The map-empty callback runs before the final End completes. Conversely,
	// completing one End usually leaves other business in the map. Neither needs
	// the global picker lock until both conditions are actually empty.
	if m.endingSessions.Load() != 0 || m.sessionManager.Size() != 0 {
		return
	}
	m.poolAccess.Lock()
	owner := m.poolOwner
	eligible := m.poolReservations == 0 && (m.poolState == workerActive || m.poolState == workerDraining)
	m.poolAccess.Unlock()
	if owner != nil && eligible {
		owner.onWorkerIdle(m)
	}
}

func (m *ClientWorker) drainSessionEndTimeout() {
	m.poolAccess.Lock()
	defer m.poolAccess.Unlock()
	if m.Closed() || m.poolState == workerClosed {
		return
	}
	m.sessionEndTimedOut.Store(true)
	previous := observableWorkerState(m.poolState)
	if m.healthState == poolHealthSuspect {
		previous = "SUSPECT"
	}
	m.setPoolStateLocked(workerDraining, "session_end_timeout")
	m.healthState = poolHealthDraining
	m.healthReason = ""
	m.healthProbeID = 0
	m.recordHealthEventLocked(m.poolOwner, previous, "DRAINING", m.poolReason)
}

func (m *ClientWorker) handleStatueKeepAlive(meta *FrameMetadata, reader *buf.BufferedReader) error {
	if meta.Option.Has(OptionProbe) {
		if m.health != nil {
			return m.health.receive(meta)
		}
		if meta.Option.Has(OptionAck) {
			m.acceptPong(meta.ProbeID)
			return nil
		}
		if m.poolControl != nil {
			// A peer Ping must not park the only receive loop behind a full shared
			// writer. Losing a bounded-queue reply is preferable: the peer's lease
			// will quarantine admission without stalling unrelated downlink data.
			if err := m.poolControl.enqueue(FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe | OptionAck, ProbeID: meta.ProbeID}); err != nil {
				m.poolAccess.Lock()
				owner := m.poolOwner
				m.poolAccess.Unlock()
				if owner != nil {
					owner.poolStats.healthQueueFailures.Add(1)
				}
			}
			return nil
		}
		return writeProbeFrame(m.link.Writer, meta.ProbeID, true)
	}
	if meta.Option.Has(OptionData) {
		return buf.Copy(NewStreamReader(reader), buf.Discard)
	}
	return nil
}

func (m *ClientWorker) handleStatusNew(meta *FrameMetadata, reader *buf.BufferedReader) error {
	if meta.Option.Has(OptionData) {
		return buf.Copy(NewStreamReader(reader), buf.Discard)
	}
	return nil
}

func (m *ClientWorker) handleStatusKeep(meta *FrameMetadata, reader *buf.BufferedReader) error {
	if !meta.Option.Has(OptionData) {
		return nil
	}

	s, found := m.sessionManager.Get(meta.SessionID)
	if !found {
		// Notify remote peer to close this session.
		closingWriter := NewResponseWriter(meta.SessionID, m.link.Writer, protocol.TransferTypeStream)
		closingWriter.Close()

		return buf.Copy(NewStreamReader(reader), buf.Discard)
	}

	// PacketReader attaches the destination pointer to the buffer that it emits.
	// That buffer may remain queued in the downstream pipe after this handler
	// returns, while fetchOutput reuses and resets its FrameMetadata for the next
	// frame. Give every emitted packet an independently owned destination so a
	// later metadata reset (or endpoint override) cannot mutate an in-flight one.
	target := meta.Target
	rr := s.NewReader(reader, &target)
	err := buf.Copy(rr, s.output)
	if err != nil && buf.IsWriteError(err) {
		errors.LogInfoInner(context.Background(), err, "failed to write to downstream. closing session ", s.ID)
		s.Close(false)
		return buf.Copy(rr, buf.Discard)
	}

	return err
}

func (m *ClientWorker) handleStatusEnd(meta *FrameMetadata, reader *buf.BufferedReader) error {
	if s, found := m.sessionManager.Get(meta.SessionID); found {
		s.Close(false)
	}
	if meta.Option.Has(OptionData) {
		return buf.Copy(NewStreamReader(reader), buf.Discard)
	}
	return nil
}

func (m *ClientWorker) fetchOutput() {
	defer func() {
		common.Must(m.done.Close())
	}()

	reader := &buf.BufferedReader{Reader: m.link.Reader}

	var meta FrameMetadata
	for {
		err := meta.Unmarshal(reader, false)
		if err != nil {
			if errors.Cause(err) != io.EOF {
				errors.LogInfoInner(context.Background(), err, "failed to read metadata")
			}
			break
		}

		switch meta.SessionStatus {
		case SessionStatusKeepAlive:
			err = m.handleStatueKeepAlive(&meta, reader)
		case SessionStatusEnd:
			err = m.handleStatusEnd(&meta, reader)
		case SessionStatusNew:
			err = m.handleStatusNew(&meta, reader)
		case SessionStatusKeep:
			err = m.handleStatusKeep(&meta, reader)
		default:
			status := meta.SessionStatus
			errors.LogError(context.Background(), "unknown status: ", status)
			return
		}

		if err != nil {
			errors.LogInfoInner(context.Background(), err, "failed to process data")
			return
		}
	}
}
