package mux

import (
	"context"
	"sync"
	"time"

	"github.com/xtls/xray-core/common/buf"
)

// Only the ending phase has a deadline. A legitimate long-running business
// stream is not timed out merely because it is quiet or backpressured.
const sessionEndTimeout = 10 * time.Second

type sessionLifecycle struct {
	mu        sync.Mutex
	clock     poolClock
	timeout   time.Duration
	timer     poolTimer
	ending    bool
	ended     bool
	timedOut  bool
	expired   chan struct{}
	begin     func() // Must not take SessionManager / picker locks.
	finish    func() // Invoked outside lifecycle and SessionManager locks.
	fail      func()
	onTimeout func() // Invoked outside lifecycle and SessionManager locks.
}

func (l *sessionLifecycle) startEnding() {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.ending || l.ended {
		return
	}
	l.ending = true
	l.expired = make(chan struct{})
	if l.begin != nil {
		l.begin()
	}
	l.timer = l.clock.AfterFunc(l.timeout, func() {
		l.mu.Lock()
		if l.ended {
			l.mu.Unlock()
			return
		}
		l.timedOut = true
		l.mu.Unlock()
		// Quarantine before releasing the final ending reference. Other sessions
		// retain the carrier; only this session's writer stops waiting.
		if l.onTimeout != nil {
			l.onTimeout()
		} else if l.fail != nil {
			l.fail()
		}
		close(l.expired)
	})
}

func (l *sessionLifecycle) complete() {
	l.mu.Lock()
	if l.ended {
		l.mu.Unlock()
		return
	}
	l.ended = true
	if l.timer != nil {
		l.timer.Stop()
	}
	finish := l.finish
	ending, timedOut, expired := l.ending, l.timedOut, l.expired
	l.mu.Unlock()
	if timedOut {
		<-expired
	}
	if finish != nil && ending {
		finish()
	}
}

func (l *sessionLifecycle) timeoutOccurred() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.timedOut
}

type sessionFrameWriter struct {
	writer  *healthWriter
	session *Session
}

func (w *sessionFrameWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	return w.writer.writeCancelable(mb, w.session.done.Wait())
}

func (w *sessionFrameWriter) WriteEndFrame(meta FrameMetadata) error {
	b := buf.New()
	if err := meta.WriteTo(b); err != nil {
		b.Release()
		return err
	}
	l := w.session.lifecycle
	l.mu.Lock()
	expired := l.expired
	l.mu.Unlock()
	return w.writer.writeCancelable(buf.MultiBuffer{b}, expired)
}

func (s *Session) frameWriter(output buf.Writer) buf.Writer {
	if writer, ok := output.(*healthWriter); ok && s.lifecycle != nil {
		return &sessionFrameWriter{writer: writer, session: s}
	}
	return output
}

func (s *Session) watchCancellation(ctx context.Context) func() bool {
	if s.lifecycle == nil {
		return func() bool { return true }
	}
	return context.AfterFunc(ctx, func() { s.Close(false) })
}

func (s *Session) finishInput(writer *Writer) {
	if s.lifecycle == nil {
		// Reverse and XUDP retain their own ownership paths without a lifecycle.
		writer.Close()
		s.Close(false)
		return
	}
	defer s.lifecycle.complete()
	// startEnding runs before removal and quarantines client dispatch. This
	// releases the local slot without depending on a successful End write.
	s.Close(false)
	if err := writer.Close(); err != nil && !s.lifecycle.timeoutOccurred() {
		s.lifecycle.fail()
	}
}
