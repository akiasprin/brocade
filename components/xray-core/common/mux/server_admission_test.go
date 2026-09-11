package mux

import (
	"context"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func TestReverseSuspectInFlightNewPreservesExistingSessions(t *testing.T) {
	for _, queueBlocked := range []bool{false, true} {
		t.Run(map[bool]string{false: "reject-end", true: "full-control-queue"}[queueBlocked], func(t *testing.T) {
			h := healthForTest(t)
			reader, input := pipe.New()
			responses, responseWriter := pipe.New()
			if queueBlocked {
				blocked := newBlockingProbeWriter()
				h.writer = newHealthWriter(blocked, h.done)
				h.writer.enqueue(FrameMetadata{SessionStatus: SessionStatusKeepAlive})
				select {
				case <-blocked.started:
				case <-time.After(time.Second):
					t.Fatal("writer not blocked")
				}
				for range cap(h.writer.control) {
					h.writer.enqueue(FrameMetadata{SessionStatus: SessionStatusKeepAlive})
				}
			} else {
				h.writer = newHealthWriter(responseWriter, h.done)
			}
			w := &ServerWorker{health: h, done: h.done, sessionManager: NewSessionManager(), link: &transport.Link{Reader: reader, Writer: h.writer}}
			h.config.ActiveSessions = w.ActiveConnections
			h.config.DrainIdle = func() bool { return w.ActiveConnections() == 0 }
			down, output := pipe.New()
			in, _ := pipe.New()
			s := &Session{ID: 1, parent: w.sessionManager, input: in, output: output}
			if !w.sessionManager.Add(s) {
				t.Fatal("missing existing session")
			}
			h.snapshot.State = "SUSPECT"
			h.hardDeadline = time.Now().Add(time.Minute)
			h.pendingID = 10
			h.pendingDeadline = h.hardDeadline
			if h.Usable() || h.done.Done() {
				t.Fatal("expected SUSPECT")
			}
			go w.monitor()
			go w.run(context.Background())
			t.Cleanup(func() { w.Close(); input.Close(); responses.Interrupt(); down.Interrupt() })
			meta := FrameMetadata{SessionID: 2, SessionStatus: SessionStatusNew, Option: OptionData, Target: net.TCPDestination(net.DomainAddress("rejected.test"), 443)}
			if err := writeMetaWithFrame(input, meta, buf.MultiBuffer{buf.FromBytes([]byte("discard-new-payload"))}); err != nil {
				t.Fatal(err)
			}
			keep := FrameMetadata{SessionID: 1, SessionStatus: SessionStatusKeep, Option: OptionData}
			if err := writeMetaWithFrame(input, keep, buf.MultiBuffer{buf.FromBytes([]byte("old-flow-progress"))}); err != nil {
				t.Fatal(err)
			}
			mb, err := down.ReadMultiBufferTimeout(time.Second)
			if err != nil {
				t.Fatal("old flow interrupted by in-flight New", err)
			}
			buf.ReleaseMulti(mb)
			if w.Closed() || w.ActiveConnections() != 1 {
				t.Fatal("in-flight New closed old session or allocated rejected one")
			}
			if !queueBlocked {
				mb, err := responses.ReadMultiBufferTimeout(time.Second)
				if err != nil {
					t.Fatal(err)
				}
				r := &buf.BufferedReader{Reader: &buf.MultiBufferContainer{MultiBuffer: mb}}
				var end FrameMetadata
				if err := end.Unmarshal(r, false); err != nil || end.SessionID != 2 || end.SessionStatus != SessionStatusEnd || !end.Option.Has(OptionError) {
					t.Fatalf("missing per-session rejection: %+v %v", end, err)
				}
				r.Close()
			}
		})
	}
}
