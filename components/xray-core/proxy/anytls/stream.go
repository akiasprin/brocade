package anytls

import (
	"io"
	"sync"
	"sync/atomic"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
)

const streamDeliveryQueueDepth = 8

var errStreamDeliveryQueueFull = errors.New("anytls: stream delivery queue is full")

type stream struct {
	sid  uint32
	link *transport.Link

	done         chan struct{}
	errMu        sync.Mutex
	err          error
	hookMu       sync.Mutex
	dieHook      func()
	closed       bool
	completed    bool
	forceClosing bool

	deliveryCh        chan buf.MultiBuffer
	deliveryStart     sync.Once
	deliveryStarted   atomic.Bool
	deliveryStop      chan struct{}
	deliveryStopOnce  sync.Once
	deliveryDrain     chan struct{}
	deliveryDrainOnce sync.Once
	deliveryDoneMu    sync.Mutex
	deliveryDoneHook  func()
	deliveryFinished  bool

	isUDP      bool
	udpTarget  *xnet.Destination
	uotConnect bool
	uotBuffer  []byte
}

func newStream(sid uint32, link *transport.Link) *stream {
	return &stream{
		sid:           sid,
		link:          link,
		done:          make(chan struct{}),
		deliveryCh:    make(chan buf.MultiBuffer, streamDeliveryQueueDepth),
		deliveryStop:  make(chan struct{}),
		deliveryDrain: make(chan struct{}),
	}
}

func (st *stream) close(err error) {
	st.closeWithDelivery(err, false)
}

func (st *stream) closeAfterDelivery(err error) {
	st.closeWithDelivery(err, true)
}

func (st *stream) closeWithDelivery(err error, drain bool) {
	st.hookMu.Lock()
	if st.completed {
		st.hookMu.Unlock()
		return
	}
	firstClose := !st.closed
	if firstClose {
		st.closed = true
		st.setCloseError(err, true)
	}
	if drain && st.deliveryStarted.Load() && !st.forceClosing {
		st.hookMu.Unlock()
		if firstClose {
			if st.link != nil {
				common.Close(st.link.Reader)
			}
		}
		st.deliveryDrainOnce.Do(func() { close(st.deliveryDrain) })
		return
	}
	if st.forceClosing {
		st.hookMu.Unlock()
		return
	}
	st.forceClosing = true
	st.hookMu.Unlock()

	if !firstClose {
		st.setCloseError(err, false)
	}
	if st.link != nil {
		common.Close(st.link.Reader)
	}
	st.stopDelivery()
	if st.link != nil {
		common.Close(st.link.Writer)
	}
	if !st.deliveryStarted.Load() {
		st.finishDelivery()
	}
	st.completeClose()
}

func (st *stream) setCloseError(err error, replace bool) {
	st.errMu.Lock()
	if replace || st.err == nil {
		st.err = err
	}
	st.errMu.Unlock()
}

func (st *stream) completeClose() {
	st.hookMu.Lock()
	if st.completed {
		st.hookMu.Unlock()
		return
	}
	st.completed = true
	hook := st.dieHook
	st.dieHook = nil
	if st.done != nil {
		close(st.done)
	}
	st.hookMu.Unlock()
	if hook != nil {
		hook()
	}
}

func (st *stream) setDieHook(hook func()) {
	if hook == nil {
		return
	}
	st.hookMu.Lock()
	if st.completed {
		st.hookMu.Unlock()
		hook()
		return
	}
	st.dieHook = hook
	st.hookMu.Unlock()
}

func (st *stream) startDeliveryWorker(deliver func(buf.MultiBuffer) error) {
	st.deliveryStart.Do(func() {
		st.hookMu.Lock()
		closed := st.closed
		unavailable := st.link == nil || st.link.Writer == nil
		if !closed && !unavailable {
			st.deliveryStarted.Store(true)
		}
		st.hookMu.Unlock()
		if closed {
			st.stopDelivery()
			st.finishDelivery()
			return
		}
		if unavailable {
			st.close(errors.New("anytls: stream delivery is unavailable"))
			return
		}
		go st.deliveryLoop(deliver)
	})
}

func (st *stream) setDeliveryDoneHook(hook func()) {
	if hook == nil {
		return
	}
	st.deliveryDoneMu.Lock()
	if st.deliveryFinished {
		st.deliveryDoneMu.Unlock()
		hook()
		return
	}
	st.deliveryDoneHook = hook
	st.deliveryDoneMu.Unlock()
}

func (st *stream) finishDelivery() {
	st.deliveryDoneMu.Lock()
	if st.deliveryFinished {
		st.deliveryDoneMu.Unlock()
		return
	}
	st.deliveryFinished = true
	hook := st.deliveryDoneHook
	st.deliveryDoneHook = nil
	st.deliveryDoneMu.Unlock()
	if hook != nil {
		hook()
	}
	st.hookMu.Lock()
	closed := st.closed
	st.hookMu.Unlock()
	if closed {
		st.completeClose()
	}
}

func (st *stream) enqueueDelivery(body buf.MultiBuffer) error {
	if body.IsEmpty() {
		buf.ReleaseMulti(body)
		return nil
	}
	if st.deliveryCh == nil {
		buf.ReleaseMulti(body)
		return errors.New("anytls: stream delivery is unavailable")
	}
	st.hookMu.Lock()
	if st.closed {
		st.hookMu.Unlock()
		buf.ReleaseMulti(body)
		return io.ErrClosedPipe
	}
	select {
	case st.deliveryCh <- body:
		st.hookMu.Unlock()
		return nil
	default:
		st.hookMu.Unlock()
		buf.ReleaseMulti(body)
		return errStreamDeliveryQueueFull
	}
}

func (st *stream) stopDelivery() {
	st.deliveryStopOnce.Do(func() { close(st.deliveryStop) })
}

func (st *stream) deliveryLoop(deliver func(buf.MultiBuffer) error) {
	defer func() {
		for {
			select {
			case body := <-st.deliveryCh:
				buf.ReleaseMulti(body)
			default:
				st.finishDelivery()
				return
			}
		}
	}()
	for {
		select {
		case body := <-st.deliveryCh:
			if err := deliver(body); err != nil {
				st.close(err)
				return
			}
		case <-st.deliveryDrain:
			for {
				select {
				case body := <-st.deliveryCh:
					if err := deliver(body); err != nil {
						st.close(err)
						return
					}
				default:
					if st.link != nil {
						common.Close(st.link.Writer)
					}
					return
				}
			}
		case <-st.deliveryStop:
			return
		}
	}
}

func (st *stream) result() error {
	st.errMu.Lock()
	defer st.errMu.Unlock()
	return st.err
}
