package anytls

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
)

func TestStreamCloseIsIdempotentAndWakesReaders(t *testing.T) {
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()

	stream := newStream(1, endpoint.link)
	wantErr := errors.New("stream closed by test")
	var hookCalls atomic.Int32
	stream.dieHook = func() {
		hookCalls.Add(1)
	}

	const readerCount = 4
	ready := sync.WaitGroup{}
	ready.Add(readerCount)
	readErrs := make(chan error, readerCount)
	for range readerCount {
		go func() {
			ready.Done()
			mb, err := endpoint.output.ReadMultiBuffer()
			buf.ReleaseMulti(mb)
			readErrs <- err
		}()
	}
	ready.Wait()

	stream.close(wantErr)
	stream.close(errors.New("ignored second close"))
	for range readerCount {
		select {
		case err := <-readErrs:
			if !errors.Is(err, io.EOF) {
				t.Fatalf("blocked reader error = %v, want EOF", err)
			}
		case <-time.After(time.Second):
			t.Fatal("blocked reader was not woken by stream close")
		}
	}

	if got := hookCalls.Load(); got != 1 {
		t.Fatalf("dieHook calls = %d, want 1", got)
	}
	if got := stream.result(); !errors.Is(got, wantErr) {
		t.Fatalf("stream result = %v, want %v", got, wantErr)
	}
}

func TestSessionCloseInterruptsBlockedWrite(t *testing.T) {
	conn, peer := net.Pipe()
	defer peer.Close()
	s := newSessionForConn(conn, true)

	writeDone := make(chan error, 1)
	go func() {
		writeDone <- s.sendFrame(&frame{cmd: cmdPSH, sid: 1, data: make([]byte, maxFramePayload)})
	}()
	// net.Pipe has no reader, so the buffered writer must eventually park in Write.
	time.Sleep(20 * time.Millisecond)
	s.close(nil)

	select {
	case err := <-writeDone:
		if err == nil {
			t.Fatal("blocked session write unexpectedly succeeded")
		}
	case <-time.After(time.Second):
		t.Fatal("session close did not interrupt blocked write")
	}
}

func TestSessionPreservesBufferedDataBeforeFIN(t *testing.T) {
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()

	payload := make([]byte, 2*buf.Size+17)
	for i := range payload {
		payload[i] = byte((i*29 + 7) % 251)
	}
	s, _ := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdPSH, sid: 1, data: payload},
		testWireFrame{cmd: cmdFIN, sid: 1},
	), false)
	stream := newStream(1, endpoint.link)
	s.streams[1] = stream

	if err := s.readLoop(context.Background()); !errors.Is(err, io.EOF) {
		t.Fatalf("readLoop error = %v, want EOF", err)
	}
	got := readPipeExact(t, endpoint.output, len(payload))
	if !bytes.Equal(got, payload) {
		t.Fatal("payload was lost when FIN closed the stream")
	}
	if mb, err := endpoint.output.ReadMultiBuffer(); !errors.Is(err, io.EOF) || !mb.IsEmpty() {
		buf.ReleaseMulti(mb)
		t.Fatalf("read after buffered payload = (%v, %v), want EOF", mb, err)
	}
}

func TestSessionTruncatedFramesReturnReadError(t *testing.T) {
	tests := []struct {
		name string
		wire []byte
	}{
		{
			name: "header",
			wire: marshalTestFrames(testWireFrame{cmd: cmdWaste})[:6],
		},
		{
			name: "body",
			wire: marshalTestFrames(testWireFrame{cmd: cmdWaste, data: []byte("truncated")})[:7+3],
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, _ := newWireSession(tt.wire, false)
			err := s.readLoop(context.Background())
			if !errors.Is(err, io.ErrUnexpectedEOF) {
				t.Fatalf("readLoop error = %v, want unexpected EOF", err)
			}
		})
	}
}

func TestSessionTruncatedPSHBodyReturnsReadError(t *testing.T) {
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()
	defer endpoint.closeOutput()

	wire := marshalTestFrames(testWireFrame{cmd: cmdPSH, sid: 1, data: []byte("payload")})
	s, _ := newWireSession(wire[:7+2], false)
	s.streams[1] = newStream(1, endpoint.link)
	if err := s.readLoop(context.Background()); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("readLoop error = %v, want unexpected EOF", err)
	}
}

func TestSessionControlFrameErrors(t *testing.T) {
	tests := []struct {
		name     string
		isClient bool
		frame    testWireFrame
		wantText string
	}{
		{
			name:     "client-alert-with-body",
			isClient: true,
			frame:    testWireFrame{cmd: cmdAlert, data: []byte("bad credentials")},
			wantText: "server alert: bad credentials",
		},
		{
			name:     "client-empty-alert",
			isClient: true,
			frame:    testWireFrame{cmd: cmdAlert},
			wantText: "server alert",
		},
		{
			name:     "server-alert-from-client",
			isClient: false,
			frame:    testWireFrame{cmd: cmdAlert, data: []byte("unexpected")},
			wantText: "unexpected Alert from client",
		},
		{
			name:     "client-empty-padding-update",
			isClient: true,
			frame:    testWireFrame{cmd: cmdUpdatePaddingScheme},
			wantText: "empty padding update",
		},
		{
			name:     "client-invalid-padding-update",
			isClient: true,
			frame:    testWireFrame{cmd: cmdUpdatePaddingScheme, data: []byte("stop=1\n0=invalid")},
			wantText: "invalid padding update",
		},
		{
			name:     "server-padding-update-from-client",
			isClient: false,
			frame:    testWireFrame{cmd: cmdUpdatePaddingScheme, data: []byte("stop=1\n0=30-30")},
			wantText: "unexpected UpdatePaddingScheme from client",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, _ := newWireSession(marshalTestFrames(tt.frame), tt.isClient)
			err := s.readLoop(context.Background())
			if err == nil || !strings.Contains(err.Error(), tt.wantText) {
				t.Fatalf("readLoop error = %v, want text %q", err, tt.wantText)
			}
		})
	}
}

func TestSessionSYNACKSignalsSuccessAndRejection(t *testing.T) {
	tests := []struct {
		name       string
		data       []byte
		wantResult string
	}{
		{name: "success"},
		{name: "rejected", data: []byte("destination refused"), wantResult: "destination refused"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, output := newWireSession(marshalTestFrames(testWireFrame{cmd: cmdSYNACK, sid: 7, data: tt.data}), true)
			s.streams[7] = newStream(7, nil)
			resultCh := make(chan error, 1)
			s.synAckCh[7] = resultCh
			if err := s.readLoop(context.Background()); !errors.Is(err, io.EOF) {
				t.Fatalf("readLoop error = %v, want EOF", err)
			}
			result := <-resultCh
			if tt.wantResult == "" {
				if result != nil {
					t.Fatalf("SYNACK result = %v, want nil", result)
				}
				if frames := parseTestFrames(t, output.Bytes()); len(frames) != 0 {
					t.Fatalf("successful SYNACK emitted frames: %+v", frames)
				}
				return
			}
			if result == nil || !strings.Contains(result.Error(), tt.wantResult) {
				t.Fatalf("SYNACK result = %v, want text %q", result, tt.wantResult)
			}
			frames := parseTestFrames(t, output.Bytes())
			if len(frames) != 1 || frames[0].cmd != cmdFIN || frames[0].sid != 7 || len(frames[0].data) != 0 {
				t.Fatalf("rejected SYNACK did not emit FIN: %+v", frames)
			}
		})
	}
}

func TestSessionFinishStreamIsIdempotent(t *testing.T) {
	conn, peer := net.Pipe()
	defer peer.Close()

	s := newSessionForConn(conn, true)
	s.client = &Client{}
	stream := newStream(1, nil)
	s.streams[1] = stream
	s.activeStreams.Store(1)
	wantErr := errors.New("stream failed")

	s.finishStream(1, wantErr)
	s.finishStream(1, errors.New("ignored second finish"))

	if got := s.activeStreams.Load(); got != 0 {
		t.Fatalf("active streams = %d, want 0", got)
	}
	if got := stream.result(); !errors.Is(got, wantErr) {
		t.Fatalf("stream result = %v, want %v", got, wantErr)
	}
	select {
	case <-stream.done:
	default:
		t.Fatal("finishStream did not close stream")
	}
}

func TestClientIdleSessionCleanupHonorsMinimumAndRemovesStale(t *testing.T) {
	now := time.Now()
	client := &Client{
		idleSessionTimeout: time.Second,
		minIdleSession:     1,
		sessions:           make(map[uint64]*session),
	}
	peers := make([]net.Conn, 0, 2)
	defer func() {
		for _, peer := range peers {
			_ = peer.Close()
		}
	}()

	for _, seq := range []uint64{1, 2} {
		sessionSeq := seq
		conn, peer := net.Pipe()
		peers = append(peers, peer)
		stream := newSessionForConn(conn, true)
		stream.seq = sessionSeq
		stream.dieHook = func() {
			client.sessionsMu.Lock()
			delete(client.sessions, sessionSeq)
			client.sessionsMu.Unlock()
		}
		client.sessions[sessionSeq] = stream
		client.markSessionIdle(stream)
		client.markSessionIdle(stream)
		stream.idleSinceNano.Store(now.Add(-2 * time.Second).UnixNano())
	}

	if got := len(client.idleSessions); got != 2 {
		t.Fatalf("idle session count after duplicate marking = %d, want 2", got)
	}
	client.cleanupIdleSessionsAt(now)

	if _, ok := client.sessions[1]; ok {
		t.Fatal("oldest stale session was not removed")
	}
	kept, ok := client.sessions[2]
	if !ok || kept.isClosed() {
		t.Fatal("minimum idle session was not retained")
	}
	if got := len(client.idleSessions); got != 1 || client.idleSessions[0] != 2 {
		t.Fatalf("idle session pool after minimum retention = %v, want [2]", client.idleSessions)
	}

	client.minIdleSession = 0
	client.cleanupIdleSessionsAt(now)
	if len(client.idleSessions) != 0 || len(client.sessions) != 0 {
		t.Fatalf("stale idle session remained: pool=%v sessions=%v", client.idleSessions, client.sessions)
	}
}

func TestClientIdleSessionCleanupPrunesMissingAndClosedSessions(t *testing.T) {
	client := &Client{
		idleSessionTimeout: time.Second,
		sessions:           make(map[uint64]*session),
		idleSessions:       []uint64{99},
	}
	conn, peer := net.Pipe()
	defer peer.Close()
	closed := newSessionForConn(conn, true)
	closed.seq = 1
	closed.inIdlePool.Store(true)
	closed.close(nil)
	client.sessions[1] = closed
	client.idleSessions = append(client.idleSessions, 1)

	client.cleanupIdleSessionsAt(time.Now())
	if len(client.idleSessions) != 0 {
		t.Fatalf("invalid idle session entries remained: %v", client.idleSessions)
	}
}

func TestClientCloseStopsCleanupAndClosesSessions(t *testing.T) {
	conn, peer := net.Pipe()
	defer peer.Close()

	sess := newSessionForConn(conn, true)
	sess.seq = 1
	sess.inIdlePool.Store(true)
	client := &Client{
		cleanupDone:              make(chan struct{}),
		idleSessionCheckInterval: time.Hour,
		idleSessions:             []uint64{sess.seq},
		sessions:                 map[uint64]*session{sess.seq: sess},
	}

	cleanupExited := make(chan struct{})
	go func() {
		client.cleanupIdleSessions()
		close(cleanupExited)
	}()

	if err := client.Close(); err != nil {
		t.Fatalf("client.Close() error = %v", err)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("second client.Close() error = %v", err)
	}

	select {
	case <-cleanupExited:
	case <-time.After(time.Second):
		t.Fatal("idle session cleanup did not stop after client close")
	}
	if !client.isClosed() {
		t.Fatal("client is not marked closed")
	}
	if len(client.idleSessions) != 0 || len(client.sessions) != 0 {
		t.Fatalf("client state after close: idle=%v sessions=%v", client.idleSessions, client.sessions)
	}
	if !sess.isClosed() {
		t.Fatal("client close did not close the session")
	}

	clientEndpoint := newTestLinkEndpoint()
	defer clientEndpoint.closeInput()
	defer clientEndpoint.closeOutput()
	if err := client.Process(context.Background(), clientEndpoint.link, nil); err == nil || !strings.Contains(err.Error(), "client closed") {
		t.Fatalf("Process after client close = %v, want client closed", err)
	}
}
