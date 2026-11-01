package anytls

import (
	"io"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
)

func TestSessionWorkerAdmissionStopsBeforeCloseWaits(t *testing.T) {
	s := &session{}
	started := make(chan struct{})
	release := make(chan struct{})
	var releaseOnce sync.Once
	s.startWorker(func() {
		close(started)
		<-release
	})
	<-started
	closing := make(chan struct{})
	s.cancel = func() { close(closing) }
	closed := make(chan struct{})
	go func() {
		s.close(io.ErrClosedPipe)
		close(closed)
	}()
	t.Cleanup(func() { releaseOnce.Do(func() { close(release) }) })
	select {
	case <-closing:
	case <-time.After(2 * time.Second):
		t.Fatal("session close did not begin")
	}
	var lateRuns atomic.Int32
	s.startWorker(func() { lateRuns.Add(1) })
	select {
	case <-closed:
		t.Fatal("close returned before its admitted worker stopped")
	default:
	}
	releaseOnce.Do(func() { close(release) })
	select {
	case <-closed:
	case <-time.After(2 * time.Second):
		t.Fatal("session close did not join its worker")
	}
	if lateRuns.Load() != 0 {
		t.Fatal("a worker was admitted after session close began")
	}
}

func TestSessionRejectedDeliveryDiscardsPendingBuffers(t *testing.T) {
	s := &session{}
	s.close(io.ErrClosedPipe)
	endpoint := newTestLinkEndpoint()
	st := newStream(1, endpoint.link)
	st.startWorker = s.startWorker
	t.Cleanup(func() {
		st.close(nil)
		s.workers.Wait()
		endpoint.closeInput()
		endpoint.closeOutput()
	})
	if err := st.enqueueDelivery(buf.MultiBuffer{buf.FromBytes([]byte("pending"))}); err != nil {
		t.Fatal(err)
	}
	deliveryDone := make(chan struct{})
	st.setDeliveryDoneHook(func() { close(deliveryDone) })
	var delivered atomic.Bool
	st.startDeliveryWorker(func(body buf.MultiBuffer) error {
		buf.ReleaseMulti(body)
		delivered.Store(true)
		return nil
	})
	select {
	case <-deliveryDone:
	default:
		t.Fatal("rejected delivery did not finish its queued buffers")
	}
	if delivered.Load() || len(st.deliveryCh) != 0 || st.result() != errSessionClosed {
		t.Fatalf("rejected delivery state: delivered=%v queued=%d error=%v", delivered.Load(), len(st.deliveryCh), st.result())
	}
}
