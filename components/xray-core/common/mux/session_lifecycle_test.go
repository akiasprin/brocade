package mux

import (
	"bytes"
	"context"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal/done"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

type auditEndWriter struct {
	buf.Writer
	started     chan struct{}
	once        sync.Once
	keepStarted chan struct{}
	keepOnce    sync.Once
}

func (w *auditEndWriter) Interrupt() { common.Interrupt(w.Writer) }

func (w *auditEndWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	var meta FrameMetadata
	if !mb.IsEmpty() && meta.Unmarshal(bytes.NewReader(mb[0].Bytes()), false) == nil && meta.SessionStatus == SessionStatusEnd {
		w.once.Do(func() { close(w.started) })
	}
	if meta.SessionStatus == SessionStatusKeep && w.keepStarted != nil {
		w.keepOnce.Do(func() { close(w.keepStarted) })
	}
	return w.Writer.WriteMultiBuffer(mb)
}

func lifecycleTestContext(ctx context.Context) context.Context {
	return session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: net.TCPDestination(net.DomainAddress("lifecycle.test"), 443)}})
}

// A direct *pipe.Reader tells Dispatch that the caller transferred ownership of
// an asynchronous stream. Wrapping it exercises the synchronous-reader path,
// where the request context continues to own cancellation.
type synchronousLifecycleReader struct{ *pipe.Reader }

func TestCanceledOrRemoteEndedSessionEscapesBlockedDataWrite(t *testing.T) {
	for _, remote := range []bool{false, true} {
		t.Run(map[bool]string{false: "context-cancel", true: "remote-end"}[remote], func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			uplinkReader, uplinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
			downlinkReader, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
			wire := &auditEndWriter{Writer: uplinkWriter, started: make(chan struct{}), keepStarted: make(chan struct{})}
			worker, err := NewClientWorker(transport.Link{Reader: downlinkReader, Writer: wire}, ClientStrategy{MaxConcurrency: 2, MaxConnection: 128, WorkerPool: cfg})
			if err != nil {
				t.Fatal(err)
			}
			picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }})
			reader, input := pipe.New(pipe.WithoutSizeLimit())
			_, output := pipe.New(pipe.WithoutSizeLimit())
			t.Cleanup(func() { picker.Close(); input.Close(); uplinkReader.Interrupt(); downlinkWriter.Close() })
			ctx, cancel := context.WithCancel(lifecycleTestContext(context.Background()))
			defer cancel()
			input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("first"))})
			dispatched := make(chan error, 1)
			go func() {
				dispatched <- (&ClientManager{Picker: picker}).Dispatch(ctx, &transport.Link{
					Reader: synchronousLifecycleReader{reader}, Writer: output,
				})
			}()
			first, err := uplinkReader.ReadMultiBufferTimeout(time.Second)
			if err != nil {
				t.Fatal(err)
			}
			buf.ReleaseMulti(first)
			var fill buf.MultiBuffer
			for range 33 {
				fill = append(fill, buf.FromBytes(make([]byte, 2048)))
			}
			if err := uplinkWriter.WriteMultiBuffer(fill); err != nil {
				t.Fatal(err)
			}
			input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("blocked-business-data"))})
			select {
			case <-wire.keepStarted:
			case <-time.After(time.Second):
				t.Fatal("data write did not start")
			}
			if remote {
				frame := buf.New()
				meta := FrameMetadata{SessionID: 1, SessionStatus: SessionStatusEnd}
				if err := meta.WriteTo(frame); err != nil {
					t.Fatal(err)
				}
				if err := downlinkWriter.WriteMultiBuffer(buf.MultiBuffer{frame}); err != nil {
					t.Fatal(err)
				}
			} else {
				cancel()
			}
			select {
			case err := <-dispatched:
				if err != nil {
					t.Fatal(err)
				}
			case <-time.After(time.Second):
				t.Fatal("synchronous Dispatch did not finish")
			}
			waitForTest(t, "blocked data session removed", func() bool { return worker.ActiveConnections() == 0 })
			if worker.endingSessions.Load() != 1 || !worker.IsFull() {
				t.Fatal("blocked writer was offered for new business")
			}
			clock.Advance(sessionEndTimeout)
			waitForTest(t, "blocked data worker terminated", worker.Closed)
			waitForTest(t, "blocked data/End goroutine exits", func() bool { return worker.endingSessions.Load() == 0 })
		})
	}
}

func TestCancelOneSessionDoesNotCloseHealthySharedWorker(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.ReuseThreshold = 1
	worker := newBlackholeWorker(t, cfg, 2)
	picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }})
	t.Cleanup(func() { picker.Close() })
	manager := &ClientManager{Picker: picker}
	ctx, cancel := context.WithCancel(lifecycleTestContext(context.Background()))
	defer cancel()
	firstDispatch := make(chan error, 1)
	for index, requestCtx := range []context.Context{ctx, lifecycleTestContext(context.Background())} {
		reader, input := pipe.New(pipe.WithoutSizeLimit())
		_, output := pipe.New(pipe.WithoutSizeLimit())
		t.Cleanup(func() { input.Close() })
		link := &transport.Link{Reader: reader, Writer: output}
		if index == 0 {
			link.Reader = synchronousLifecycleReader{reader}
			go func() { firstDispatch <- manager.Dispatch(requestCtx, link) }()
			waitForTest(t, "first synchronous stream allocated", func() bool { return worker.ActiveConnections() == 1 })
		} else if err := manager.Dispatch(requestCtx, link); err != nil {
			t.Fatal(err)
		}
	}
	if worker.ActiveConnections() != 2 {
		t.Fatal("business streams did not share the worker")
	}
	cancel()
	select {
	case err := <-firstDispatch:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("canceled synchronous Dispatch did not finish")
	}
	waitForTest(t, "only canceled stream released", func() bool { return worker.ActiveConnections() == 1 && worker.endingSessions.Load() == 0 })
	clock.Advance(cfg.ProbeTimeout + time.Second)
	if worker.Closed() || worker.IsFull() {
		t.Fatal("normal cancellation closed/quarantined another healthy business stream")
	}
	chosen, err := picker.PickAvailable()
	if err != nil || chosen != worker {
		t.Fatalf("healthy remaining capacity was lost: %v", err)
	}
}

func TestCanceledReservedDispatchReleasesReservation(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	worker := newBlackholeWorker(t, cfg, 2)
	picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }})
	t.Cleanup(func() { picker.Close() })
	chosen, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(lifecycleTestContext(context.Background()))
	cancel()
	if chosen.Dispatch(ctx, &transport.Link{}) {
		t.Fatal("canceled request was dispatched")
	}
	worker.poolAccess.Lock()
	reserved := worker.poolReservations
	worker.poolAccess.Unlock()
	if reserved != 0 || worker.ActiveConnections() != 0 || worker.TotalConnections() != 0 || workerStateForTest(worker) == workerActive {
		t.Fatal("canceled reservation left an empty active worker")
	}
}

func TestConsumedReservationFailureNeverLeavesEmptyActiveWorker(t *testing.T) {
	for _, test := range []struct {
		name                string
		breakBeforeDispatch func(*ClientWorker)
		want                clientWorkerState
	}{
		{
			name: "request limit reached",
			breakBeforeDispatch: func(worker *ClientWorker) {
				worker.sessionManager.Lock()
				worker.sessionManager.count = 1
				worker.sessionManager.Unlock()
			},
			want: workerClosed,
		},
		{
			name: "session manager rejects allocation",
			breakBeforeDispatch: func(worker *ClientWorker) {
				worker.sessionManager.Lock()
				worker.sessionManager.closed = true
				worker.sessionManager.Unlock()
			},
			want: workerProbing,
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			cfg.ReuseThreshold = 1
			picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{})
			worker := newBlackholeWorker(t, cfg, 1)
			worker.strategy.MaxConnection = 1
			addWorkerForTest(t, picker, worker, workerActive, clock.Now())
			t.Cleanup(func() { picker.Close() })

			if !worker.reserveForDispatch(workerActive, true) {
				t.Fatal("failed to reserve worker")
			}
			test.breakBeforeDispatch(worker)
			reader, input := pipe.New(pipe.WithoutSizeLimit())
			_, output := pipe.New(pipe.WithoutSizeLimit())
			t.Cleanup(func() { input.Close() })
			if worker.Dispatch(lifecycleTestContext(context.Background()), &transport.Link{Reader: reader, Writer: output}) {
				t.Fatal("Dispatch unexpectedly succeeded")
			}
			waitForTest(t, "failed reservation lifecycle", func() bool { return workerStateForTest(worker) == test.want })
			worker.poolAccess.Lock()
			reservations := worker.poolReservations
			worker.poolAccess.Unlock()
			if reservations != 0 || (test.want != workerClosed && worker.ActiveConnections() == 0 && workerStateForTest(worker) == workerActive) {
				t.Fatalf("failed Dispatch left zombie state: reservations=%d state=%v", reservations, workerStateForTest(worker))
			}
		})
	}
}

func TestDrainingWorkerWaitsForOtherBusinessAndEndWrites(t *testing.T) {
	clock := newFakePoolClock()
	cfg := testPoolConfig()
	cfg.ReuseThreshold = 1
	worker := newBlackholeWorker(t, cfg, 2)
	picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }})
	t.Cleanup(func() { picker.Close() })
	manager := &ClientManager{Picker: picker}
	var inputs []*pipe.Writer
	for range 2 {
		reader, input := pipe.New(pipe.WithoutSizeLimit())
		inputs = append(inputs, input)
		_, output := pipe.New(pipe.WithoutSizeLimit())
		t.Cleanup(func() { input.Close() })
		if err := manager.Dispatch(lifecycleTestContext(context.Background()), &transport.Link{Reader: reader, Writer: output}); err != nil {
			t.Fatal(err)
		}
	}
	picker.Drain()
	inputs[0].Close()
	waitForTest(t, "first draining session fully ended", func() bool { return worker.ActiveConnections() == 1 && worker.endingSessions.Load() == 0 })
	clock.Advance(cfg.ProbeTimeout + time.Second)
	if worker.Closed() {
		t.Fatal("finishing one session killed another during planned drain")
	}
	inputs[1].Close()
	waitForTest(t, "last draining session closes carrier", worker.Closed)
	waitForTest(t, "all ending writers exited", func() bool { return worker.endingSessions.Load() == 0 })
}

func TestServerSessionReleasesBeforeBlockedEnd(t *testing.T) {
	clock := newFakePoolClock()
	reader, input := pipe.New(pipe.WithoutSizeLimit())
	carrierReader, carrierInput := pipe.New(pipe.WithoutSizeLimit())
	blocked := newBlockingProbeWriter()
	server := &ServerWorker{sessionManager: NewSessionManager(), done: done.New(), link: &transport.Link{Reader: carrierReader, Writer: blocked}}
	s := &Session{ID: 1, parent: server.sessionManager, input: reader, output: buf.Discard,
		lifecycle: &sessionLifecycle{clock: clock, timeout: sessionEndTimeout, fail: func() { server.Close() }}}
	if !server.sessionManager.Add(s) {
		t.Fatal("add server session")
	}
	finished := make(chan struct{})
	go server.monitor()
	go func() { defer close(finished); handle(context.Background(), s, blocked) }()
	t.Cleanup(func() { server.Close(); input.Close(); carrierInput.Close(); blocked.Interrupt() })
	input.Close()
	select {
	case <-blocked.started:
	case <-time.After(time.Second):
		t.Fatal("server End was not attempted")
	}
	if server.ActiveConnections() != 0 {
		t.Fatal("server held slot behind blocked End")
	}
	clock.Advance(sessionEndTimeout)
	select {
	case <-finished:
	case <-time.After(time.Second):
		t.Fatal("server End remained blocked after deadline")
	}
}

func TestBlockedEndTimeoutPreservesOtherSharedBusiness(t *testing.T) {
	clock, cfg := newFakePoolClock(), testPoolConfig()
	cfg.ReuseThreshold = 1
	cfg.ProbeTimeout = 200 * time.Millisecond
	uplink, uplinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
	downlink, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
	wire := &auditEndWriter{Writer: uplinkWriter, started: make(chan struct{})}
	w, err := NewClientWorker(transport.Link{Reader: downlink, Writer: wire}, ClientStrategy{MaxConcurrency: 2, MaxConnection: 128, WorkerPool: cfg})
	if err != nil {
		t.Fatal(err)
	}
	p := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return w, nil }})
	t.Cleanup(func() { p.Close(); uplink.Interrupt(); downlinkWriter.Close() })
	var inputs []*pipe.Writer
	for range 2 {
		chosen, err := p.PickAvailable()
		if err != nil || chosen != w {
			t.Fatal(err)
		}
		input := dispatchHealthTestSession(t, w)
		inputs = append(inputs, input)
		input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("start"))})
		mb, err := uplink.ReadMultiBufferTimeout(time.Second)
		if err != nil {
			t.Fatal(err)
		}
		buf.ReleaseMulti(mb)
	}
	var fill buf.MultiBuffer
	for range 33 {
		fill = append(fill, buf.FromBytes(make([]byte, 2048)))
	}
	if err := uplinkWriter.WriteMultiBuffer(fill); err != nil {
		t.Fatal(err)
	}
	inputs[0].Close()
	select {
	case <-wire.started:
	case <-time.After(time.Second):
		t.Fatal("End not blocked")
	}
	clock.Advance(cfg.ProbeTimeout)
	if w.sessionEndTimedOut.Load() || w.Closed() {
		t.Fatal("probe timeout used as End deadline")
	}
	clock.Advance(sessionEndTimeout - cfg.ProbeTimeout)
	waitForTest(t, "bounded End waiter exits", func() bool { return w.endingSessions.Load() == 0 })
	if w.Closed() || w.ActiveConnections() != 1 || workerStateForTest(w) != workerDraining {
		t.Fatal("End timeout killed shared business or reopened admission")
	}
	mb, err := uplink.ReadMultiBufferTimeout(time.Second)
	if err != nil {
		t.Fatal(err)
	}
	buf.ReleaseMulti(mb)
	marker := []byte("other-session-still-transfers-after-timeout")
	inputs[1].WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(marker)})
	var received []byte
	for !bytes.Contains(received, marker) {
		mb, err := uplink.ReadMultiBufferTimeout(time.Second)
		if err != nil {
			t.Fatal("remaining flow did not progress", err)
		}
		for _, b := range mb {
			received = append(received, b.Bytes()...)
		}
		buf.ReleaseMulti(mb)
	}
	if w.Closed() {
		t.Fatal("remaining business was interrupted")
	}
	inputs[1].Close()
	waitForTest(t, "drain after last business", w.Closed)
}

func TestServerEndTimeoutDrainsWithoutKillingOtherBusiness(t *testing.T) {
	clock := newFakePoolClock()
	carrier, input := pipe.New()
	blocked := newBlockingProbeWriter()
	w := &ServerWorker{done: done.New(), sessionManager: NewSessionManager()}
	writer := newHealthWriter(blocked, w.done)
	w.link = &transport.Link{Reader: carrier, Writer: writer}
	var inputs []*pipe.Writer
	for id := uint16(1); id <= 2; id++ {
		r, input := pipe.New()
		inputs = append(inputs, input)
		s := &Session{ID: id, parent: w.sessionManager, input: r, output: buf.Discard, lifecycle: w.newSessionLifecycle(clock)}
		if !w.sessionManager.Add(s) {
			t.Fatal("session add failed")
		}
		go handle(context.Background(), s, writer)
	}
	go w.monitor()
	t.Cleanup(func() {
		w.Close()
		input.Close()
		for _, in := range inputs {
			in.Close()
		}
	})
	inputs[0].Close()
	select {
	case <-blocked.started:
	case <-time.After(time.Second):
		t.Fatal("missing End")
	}
	clock.Advance(sessionEndTimeout)
	waitForTest(t, "server End waiter exits", func() bool { return w.endingSessions.Load() == 0 })
	if w.Closed() || w.ActiveConnections() != 1 || !w.endDraining.Load() {
		t.Fatal("server killed other business")
	}
	inputs[1].Close()
	waitForTest(t, "last server session ends and timer is armed", func() bool { return w.endingSessions.Load() == 1 && w.ActiveConnections() == 0 })
	clock.Advance(sessionEndTimeout)
	waitForTest(t, "empty draining server reclaimed", w.Closed)
}

func TestSessionEndReleasesSlotBeforeBlockedWrite(t *testing.T) {
	for _, name := range []string{"normal-end", "blocked-end", "recovered-end", "planned-drain-blocked-end"} {
		t.Run(name, func(t *testing.T) {
			blocked := name != "normal-end"
			clock := newFakePoolClock()
			cfg := testPoolConfig()
			uplinkReader, uplinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
			downlinkReader, downlinkWriter := pipe.New(pipe.WithSizeLimit(64 * 1024))
			endWriter := &auditEndWriter{Writer: uplinkWriter, started: make(chan struct{})}
			worker, err := NewClientWorker(transport.Link{Reader: downlinkReader, Writer: endWriter}, ClientStrategy{
				MaxConcurrency: 2, MaxConnection: 128, WorkerPool: cfg,
			})
			if err != nil {
				t.Fatal(err)
			}
			picker := newPoolPickerForTest(clock, cfg, &testWorkerFactory{create: func() (*ClientWorker, error) { return worker, nil }})
			picker.EnableObservation("tcp")
			t.Cleanup(func() {
				picker.Close()
				picker.disableObservation()
				uplinkReader.Interrupt()
				downlinkWriter.Close()
			})
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{{Target: net.TCPDestination(net.DomainAddress("audit.test"), 443)}})
			inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
			_, outputWriter := pipe.New(pipe.WithoutSizeLimit())
			t.Cleanup(func() { inputWriter.Close() })
			if err := inputWriter.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte("business-payload"))}); err != nil {
				t.Fatal(err)
			}
			if err := (&ClientManager{Picker: picker}).Dispatch(ctx, &transport.Link{Reader: inputReader, Writer: outputWriter}); err != nil {
				t.Fatal(err)
			}
			first, err := uplinkReader.ReadMultiBufferTimeout(time.Second)
			if err != nil {
				t.Fatal(err)
			}
			buf.ReleaseMulti(first) // The initial New + business payload left the pipe.
			if blocked {
				// Fill the shared uplink after the business has started. The real
				// pipe checks its limit before append; above 64KB, End must wait.
				var fill buf.MultiBuffer
				for range 33 {
					fill = append(fill, buf.FromBytes(make([]byte, 2048)))
				}
				if err := uplinkWriter.WriteMultiBuffer(fill); err != nil {
					t.Fatal(err)
				}
			}
			if name == "recovered-end" {
				worker.poolAccess.Lock()
				worker.lastRead = clock.Now()
				worker.poolAccess.Unlock()
			}
			inputWriter.Close()
			select {
			case <-endWriter.started:
			case <-time.After(time.Second):
				t.Fatal("End write was not attempted")
			}
			if blocked {
				cancel()
				waitForTest(t, "slot released before End write completes", func() bool { return worker.ActiveConnections() == 0 })
				worker.poolAccess.Lock()
				reserved := worker.poolReservations
				worker.poolAccess.Unlock()
				if reserved != 0 || worker.endingSessions.Load() != 1 || !worker.IsFull() {
					t.Fatalf("ending worker must not accept new business: reservations=%d ending=%d full=%v", reserved, worker.endingSessions.Load(), worker.IsFull())
				}
				if picker.WorkerPoolStats().ProbeSentTotal != 0 {
					t.Fatal("ending worker started an idle probe before End completed")
				}
				snapshot := worker.muxSnapshot(picker, clock.Now())
				if snapshot.ActiveSessions != 0 || snapshot.AvailableSlots != 0 || snapshot.Phase != "ending" || snapshot.Reason != "session_ending" {
					t.Fatalf("blocked ending snapshot = %+v", snapshot)
				}
				if name == "recovered-end" {
					fill, err := uplinkReader.ReadMultiBufferTimeout(time.Second)
					if err != nil {
						t.Fatal(err)
					}
					buf.ReleaseMulti(fill)
					waitForTest(t, "End recovered and worker reusable", func() bool { return worker.endingSessions.Load() == 0 && !worker.IsFull() })
					clock.Advance(cfg.ProbeTimeout)
					if worker.Closed() || worker.sessionEndTimedOut.Load() {
						t.Fatal("completed End left an armed timeout")
					}
					return
				}
				if name == "planned-drain-blocked-end" {
					picker.Drain()
					if worker.Closed() {
						t.Fatal("planned drain did not wait for pending End")
					}
				}
				clock.Advance(sessionEndTimeout - time.Nanosecond)
				if worker.Closed() {
					t.Fatal("ending worker closed before its grace deadline")
				}
				clock.Advance(time.Nanosecond)
				waitForTest(t, "blocked worker closes at ending deadline", worker.Closed)
				waitForTest(t, "blocked End goroutine exits", func() bool { return worker.endingSessions.Load() == 0 })
				waitForTest(t, "ending timeout reason published", func() bool {
					for _, event := range GetMuxSnapshot().Events {
						if event.WorkerID == worker.workerID && event.Reason == "session_end_timeout" {
							return true
						}
					}
					return false
				})
				t.Log("blocked End: slot released first, dispatch quarantined, worker and End writer terminated at grace deadline")
			}
			waitForTest(t, "local session release", func() bool { return worker.ActiveConnections() == 0 })
			t.Logf("End can write: active=%d lifetime=%d; slot released, lifetime counter intentionally retained", worker.ActiveConnections(), worker.TotalConnections())
		})
	}
}
