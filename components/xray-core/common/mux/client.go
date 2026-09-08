package mux

import (
	"context"
	goerrors "errors"
	"io"
	"sync"
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

	access          sync.Mutex
	workers         []*ClientWorker
	cleanupTask     *task.Periodic
	config          *WorkerPoolConfig
	poolClosed      bool
	poolUsed        bool
	warmRunning     bool
	warmFailures    int
	nextWarmAttempt time.Time
	warmTimer       poolTimer
	lastTimeoutLog  time.Time
	suppressedLogs  uint64
	clock           poolClock
	jitter          func(time.Duration) time.Duration
	poolStats       workerPoolStats
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

func (p *IncrementalWorkerPicker) findAvailable() int {
	if p.config != nil {
		for _, desired := range []clientWorkerState{workerActive, workerIdleReady} {
			for idx, worker := range p.workers {
				if worker.reserveForDispatch(desired, false) {
					return idx
				}
			}
		}
		return -1
	}
	for idx, w := range p.workers {
		if !w.IsFull() {
			return idx
		}
	}

	return -1
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

	idx := p.findAvailable()
	if idx >= 0 {
		worker := p.workers[idx]
		n := len(p.workers)
		if n > 1 && idx != n-1 {
			p.workers[n-1], p.workers[idx] = p.workers[idx], p.workers[n-1]
		}
		return worker, false, nil
	}

	p.cleanup()

	worker, err := p.Factory.Create()
	if err != nil {
		return nil, false, err
	}
	if p.config != nil && !worker.attachPool(p, workerActive, p.nowLocked()) {
		common.Close(worker)
		return nil, false, errors.New("new mux worker closed before use")
	}
	if p.config != nil && !worker.reserveForDispatch(workerActive, true) {
		common.Close(worker)
		return nil, false, errors.New("new mux worker could not reserve its first request")
	}
	p.workers = append(p.workers, worker)
	if p.config != nil {
		p.poolStats.workerCreatedDemand.Add(1)
		p.warmFailures = 0
		p.nextWarmAttempt = time.Time{}
		if p.warmTimer != nil {
			p.warmTimer.Stop()
			p.warmTimer = nil
		}
	}

	if p.config == nil && p.cleanupTask == nil {
		p.cleanupTask = &task.Periodic{
			Interval: time.Second * 30,
			Execute:  p.cleanupFunc,
		}
	}

	return worker, true, nil
}

func (p *IncrementalWorkerPicker) PickAvailable() (*ClientWorker, error) {
	worker, start, err := p.pickInternal()
	if start && p.cleanupTask != nil {
		common.Must(p.cleanupTask.Start())
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
	sessionManager *SessionManager
	link           transport.Link
	done           *done.Instance
	timer          *time.Ticker
	strategy       ClientStrategy

	poolAccess          sync.Mutex
	poolOwner           *IncrementalWorkerPicker
	poolClock           poolClock
	poolState           clientWorkerState
	idleSince           time.Time
	lastIO              time.Time
	nextProbeAt         time.Time
	poolTimer           poolTimer
	poolTimerGeneration uint64
	probeGeneration     uint64
	nextProbeID         uint64
	pendingProbeID      uint64
	pendingProbe        chan struct{}
	pendingProbeAcked   bool
	poolReservations    uint32
}

var (
	muxCoolAddress = net.DomainAddress("v1.mux.cool")
	muxCoolPort    = net.Port(9527)
)

// NewClientWorker creates a new mux.Client.
func NewClientWorker(stream transport.Link, s ClientStrategy) (*ClientWorker, error) {
	c := &ClientWorker{
		sessionManager: NewSessionManager(),
		link:           stream,
		done:           done.New(),
		strategy:       s,
		poolState:      workerActive,
	}
	if s.WorkerPool == nil {
		c.timer = time.NewTicker(time.Second * 16)
	} else {
		c.nextProbeID = newProbeSeed()
		c.sessionManager.SetOnEmpty(c.onSessionEmpty)
		c.link.Reader = &poolActivityReader{Reader: c.link.Reader, worker: c}
		c.link.Writer = &poolActivityWriter{Writer: c.link.Writer, worker: c}
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
	return m.done.Close()
}

func (m *ClientWorker) monitor() {
	if m.timer == nil {
		<-m.done.Wait()
		m.sessionManager.Close()
		common.Interrupt(m.link.Writer)
		common.Interrupt(m.link.Reader)
		m.poolAccess.Lock()
		owner := m.poolOwner
		if owner == nil {
			m.poolState = workerClosed
			m.stopPoolTimerLocked()
		}
		m.poolAccess.Unlock()
		if owner != nil {
			owner.onWorkerClosed(m)
		}
		return
	}
	defer m.timer.Stop()

	for {
		checkSize := m.sessionManager.Size()
		checkCount := m.sessionManager.Count()
		select {
		case <-m.done.Wait():
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
	writer := NewWriter(s.ID, ob.Target, output, transferType, xudp.GetGlobalID(ctx), inbound)
	defer s.Close(false)
	defer writer.Close()

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
	if m.poolState != workerActive && m.poolState != workerIdleReady {
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

// reserveForDispatch makes Picker selection and capacity accounting one atomic
// operation. In particular, an active worker whose last session has already
// left the map cannot be reused in the short interval before its onEmpty
// callback completes the idle transition.
func (m *ClientWorker) reserveForDispatch(desired clientWorkerState, allowEmptyActive bool) bool {
	m.poolAccess.Lock()
	defer m.poolAccess.Unlock()
	if m.poolState != desired || m.IsClosing() || m.Closed() {
		return false
	}
	// A due timer may still be waiting to run. Never let scheduling latency extend
	// the idle reuse window or the business idle TTL.
	if desired == workerIdleReady && (m.poolClock == nil || !m.idleReadyFreshLocked(m.poolClock.Now())) {
		return false
	}
	active := uint32(m.sessionManager.Size())
	if desired == workerActive && active == 0 && m.poolReservations == 0 && !allowEmptyActive {
		return false
	}
	if m.strategy.MaxConcurrency > 0 && active+m.poolReservations >= m.strategy.MaxConcurrency {
		return false
	}
	if m.strategy.MaxConnection > 0 && uint32(m.sessionManager.Count())+m.poolReservations >= m.strategy.MaxConnection {
		return false
	}
	m.poolReservations++
	m.stopPoolTimerLocked()
	m.poolState = workerActive
	m.idleSince = time.Time{}
	m.nextProbeAt = time.Time{}
	return true
}

func (m *ClientWorker) Dispatch(ctx context.Context, link *transport.Link) bool {
	sm := m.sessionManager
	var s *Session
	var owner *IncrementalWorkerPicker
	if m.strategy.WorkerPool == nil {
		if m.IsClosing() || m.Closed() || (m.strategy.MaxConcurrency > 0 && sm.Size() >= int(m.strategy.MaxConcurrency)) {
			return false
		}
		s = sm.Allocate(&m.strategy)
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
		if m.poolState != workerActive && m.poolState != workerIdleReady {
			m.poolAccess.Unlock()
			return false
		}
		if m.IsClosing() || m.Closed() || (m.strategy.MaxConcurrency > 0 && sm.Size() >= int(m.strategy.MaxConcurrency)) {
			m.poolAccess.Unlock()
			return false
		}
		s = sm.Allocate(&m.strategy)
		if s == nil {
			m.poolAccess.Unlock()
			return false
		}
		m.stopPoolTimerLocked()
		if m.strategy.MaxConnection > 0 && sm.Count() >= int(m.strategy.MaxConnection) {
			m.poolState = workerDraining
		} else {
			m.poolState = workerActive
		}
		m.idleSince = time.Time{}
		m.nextProbeAt = time.Time{}
		owner = m.poolOwner
		m.poolAccess.Unlock()
	}
	s.input = link.Reader
	s.output = link.Writer
	go fetchInput(ctx, s, m.link.Writer)
	if _, ok := link.Reader.(*pipe.Reader); !ok {
		select {
		case <-ctx.Done():
		case <-s.done.Wait():
		}
	}
	if owner != nil {
		owner.onWorkerUsed(m)
	}
	return true
}

func (m *ClientWorker) onSessionEmpty() {
	m.poolAccess.Lock()
	owner := m.poolOwner
	m.poolAccess.Unlock()
	if owner != nil {
		owner.onWorkerIdle(m)
	}
}

func (m *ClientWorker) handleStatueKeepAlive(meta *FrameMetadata, reader *buf.BufferedReader) error {
	if meta.Option.Has(OptionProbe) {
		if meta.Option.Has(OptionAck) {
			m.acceptPong(meta.ProbeID)
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
