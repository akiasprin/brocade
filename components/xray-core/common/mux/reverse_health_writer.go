package mux

import (
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/signal/done"
	"io"
	"sync"
	"sync/atomic"
	"time"
)

// One owner writes complete mux frames. Control has bounded priority between
// business frames (stream frames are already split at 8 KiB). No caller holds a
// health/state lock while waiting for transport I/O.
type healthWrite struct {
	enqueued time.Time
	mb       buf.MultiBuffer
	result   chan error
}
type healthWriter struct {
	queueDelayMS atomic.Int64
	mu           sync.Mutex
	stopped      bool
	output       buf.Writer
	closed       *done.Instance
	done         <-chan struct{}
	control      chan healthWrite
	data         chan healthWrite
}

func newHealthWriter(output buf.Writer, closed *done.Instance) *healthWriter {
	w := &healthWriter{output: output, done: closed.Wait(), closed: closed, control: make(chan healthWrite, 8), data: make(chan healthWrite)}
	go w.run()
	return w
}
func (w *healthWriter) Interrupt() { common.Interrupt(w.output) }
func (w *healthWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	return w.writeCancelable(mb, nil)
}

// Once accepted, a complete frame belongs to the single writer even if its
// session stops waiting. Never interrupt a shared frame/transport on cancellation.
func (w *healthWriter) writeCancelable(mb buf.MultiBuffer, cancel <-chan struct{}) error {
	r := healthWrite{mb: mb, result: make(chan error, 1)}
	select {
	case w.data <- r:
	case <-cancel:
		buf.ReleaseMulti(mb)
		return io.ErrClosedPipe
	case <-w.done:
		buf.ReleaseMulti(mb)
		return io.ErrClosedPipe
	}
	select {
	case err := <-r.result:
		return err
	case <-cancel:
		return io.ErrClosedPipe
	case <-w.done:
		return io.ErrClosedPipe
	}
}
func (w *healthWriter) enqueue(meta FrameMetadata) error {
	b := buf.New()
	if err := meta.WriteTo(b); err != nil {
		b.Release()
		return err
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.stopped {
		b.Release()
		return io.ErrClosedPipe
	}
	select {
	case <-w.done:
		b.Release()
		return io.ErrClosedPipe
	default:
	}
	select {
	case w.control <- healthWrite{enqueued: time.Now(), mb: buf.MultiBuffer{b}}:
		return nil
	default:
		b.Release()
		return io.ErrShortBuffer
	}
}
func (w *healthWriter) run() {
	// Closing the worker interrupts an in-progress transport write as well as reads.
	go func() { <-w.done; w.Interrupt() }()
	defer func() {
		w.mu.Lock()
		defer w.mu.Unlock()
		w.stopped = true
		for {
			select {
			case r := <-w.control:
				buf.ReleaseMulti(r.mb)
			default:
				return
			}
		}
	}()
	for {
		var r healthWrite
		select {
		case <-w.done:
			return
		default:
		}
		select {
		case r = <-w.control:
		default:
			select {
			case <-w.done:
				return
			case r = <-w.control:
			case r = <-w.data:
			}
		}
		if !r.enqueued.IsZero() {
			w.queueDelayMS.Store(time.Since(r.enqueued).Milliseconds())
		}
		err := w.output.WriteMultiBuffer(r.mb)
		if r.result != nil {
			r.result <- err
		}
		if err != nil {
			w.closed.Close()
			w.Interrupt()
			return
		}
	}
}
