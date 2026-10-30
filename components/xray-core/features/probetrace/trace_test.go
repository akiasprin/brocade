package probetrace

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"io"
	stdnet "net"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/transport"
)

func contextFor(tag, email string) context.Context {
	return session.ContextWithInbound(context.Background(), &session.Inbound{Tag: tag, User: &protocol.MemoryUser{Email: email}})
}
func origin(t *testing.T, node string) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "node-id")
	if err := os.WriteFile(path, []byte(node), 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(NodeFileEnv, path)
}
func evidenceFile(t *testing.T) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "evidence.jsonl")
	if err := os.WriteFile(p, nil, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(FileEnv, p)
	t.Setenv(ProfileEnv, `{"fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","strategies":{"out:forward":"UseIP"}}`)
	exportMu.Lock()
	exported = 0
	exportMu.Unlock()
	return p
}
func readAll(t *testing.T, r buf.Reader) []byte {
	t.Helper()
	var output bytes.Buffer
	if err := buf.Copy(r, buf.NewWriter(&output)); err != nil {
		t.Fatal(err)
	}
	return output.Bytes()
}
func terminalLookup() []dns.LookupTrace {
	return []dns.LookupTrace{{Version: 1, Domain: "www.youtube.com", Complete: true, Steps: []dns.LookupTraceStep{{Tag: "dns:egress:shared", Scoped: true, Outcome: "empty", Other: true}, {Tag: "xray.system.opaque", Scoped: false, Outcome: "answer"}}}}
}

func TestActualConnectionEvidenceTraversesRelaysWithoutChangingTLSBytes(t *testing.T) {
	path := evidenceFile(t)
	origin(t, "exit-mo")
	destination := net.TCPDestination(net.DomainAddress("www.youtube.com"), 443)
	payload, reply := []byte("actual TLS client hello"), []byte("actual TLS response")
	var localOutput, hopOutput, exitOutput bytes.Buffer
	local := &transport.Link{Reader: buf.NewReader(bytes.NewReader(payload)), Writer: buf.NewWriter(&localOutput)}
	ctx, err := Prepare(contextFor(LocalTag+"youtube_premium.webpage", ""), destination, local)
	if err != nil {
		t.Fatal(err)
	}
	state := FromContext(ctx)
	forward := Relay(state, local)
	hop := &transport.Link{Reader: forward.Reader, Writer: buf.NewWriter(&hopOutput)}
	hopCtx, err := Prepare(contextFor("public", "probe#v4:ingress"), destination, hop)
	if err != nil {
		t.Fatal(err)
	}
	hopState := FromContext(hopCtx)
	next := Relay(hopState, hop)
	exit := &transport.Link{Reader: next.Reader, Writer: buf.NewWriter(&exitOutput)}
	exitCtx, err := Prepare(contextFor("relay", "probe#v4:listener"), destination, exit)
	if err != nil {
		t.Fatal(err)
	}
	if got := readAll(t, exit.Reader); !bytes.Equal(got, payload) {
		t.Fatalf("payload changed: %q", got)
	}
	FromContext(exitCtx).Finish("out:forward:probe-v4", "v4", true, "", terminalLookup())
	if err := exit.Writer.WriteMultiBuffer(buf.MergeBytes(nil, reply)); err != nil {
		t.Fatal(err)
	}
	// Both frame headers and bodies can be arbitrarily fragmented by a transport.
	for _, b := range exitOutput.Bytes() {
		if err := next.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte{b})}); err != nil {
			t.Fatal(err)
		}
	}
	for _, b := range hopOutput.Bytes() {
		if err := forward.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes([]byte{b})}); err != nil {
			t.Fatal(err)
		}
	}
	if !bytes.Equal(localOutput.Bytes(), reply) {
		t.Fatalf("response changed: %q", localOutput.Bytes())
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var record Record
	lines := bytes.Split(bytes.TrimSpace(data), []byte{'\n'})
	if len(lines) != 2 {
		t.Fatal("missing connection start/completion")
	}
	if err := json.Unmarshal(lines[len(lines)-1], &record); err != nil {
		t.Fatal(err)
	}
	if record.Exit != "exit-mo" || record.Group != "youtube_premium.webpage" || record.ID != state.ID || !record.Connected || !record.Complete || record.Family != "v4" || len(record.Lookups) != 1 {
		t.Fatalf("wrong evidence: %+v", record)
	}
}

func FuzzProbeResponseEnvelope(f *testing.F) {
	f.Add([]byte{})
	f.Add([]byte("HTTP/1.1 200 OK\r\n\r\n"))
	f.Add(append(append([]byte(nil), responseMagic...), make([]byte, 18)...))
	f.Fuzz(func(t *testing.T, data []byte) {
		if len(data) > 16384 {
			return
		}
		state := &State{ID: "00000000000000000000000000000001", Domain: "www.youtube.com"}
		var payload bytes.Buffer
		writer := &collectorWriter{Writer: buf.NewWriter(&payload), state: state}
		_ = writer.WriteMultiBuffer(buf.MergeBytes(nil, data))
		_ = writer.Close()
		if result := state.Result(); result != nil && !result.Valid() {
			t.Fatal("malformed input produced invalid evidence")
		}
	})
}

func TestOrdinaryUsersAndUninstrumentedCapabilityChecksKeepTheirBytes(t *testing.T) {
	t.Setenv(FileEnv, "")
	destination := net.TCPDestination(net.DomainAddress("www.youtube.com"), 443)
	for _, email := range []string{"person@tenant#ingress", "probe#v4:capability"} {
		data := []byte("GET / HTTP/1.1\r\nHost: www.youtube.com\r\n\r\n")
		link := &transport.Link{Reader: buf.NewReader(bytes.NewReader(data)), Writer: buf.Discard}
		ctx, err := Prepare(contextFor("public", email), destination, link)
		if err != nil {
			t.Fatal(err)
		}
		if FromContext(ctx) != nil || !bytes.Equal(readAll(t, link.Reader), data) {
			t.Fatalf("ordinary stream changed for %s", email)
		}
	}
}

func TestCorruptTruncatedAndOverLimitEvidenceDoesNotBecomeAResult(t *testing.T) {
	evidenceFile(t)
	for _, kind := range []string{"identity", "size", "truncated"} {
		state, err := newLocal("www.youtube.com", "youtube_premium.cdn")
		if err != nil {
			t.Fatal(err)
		}
		var payload bytes.Buffer
		writer := &collectorWriter{Writer: buf.NewWriter(&payload), state: state}
		frame := append([]byte(nil), responseMagic...)
		frame = append(frame, make([]byte, 16)...)
		if kind != "identity" {
			id, _ := hexDecode(state.ID)
			copy(frame[len(responseMagic):], id)
		}
		size := uint16(1)
		if kind == "size" {
			size = MaxFrame + 1
		}
		frame = binary.BigEndian.AppendUint16(frame, size)
		if kind != "truncated" {
			frame = append(frame, '{')
		}
		err = writer.WriteMultiBuffer(buf.MergeBytes(nil, frame))
		if kind == "size" && err == nil {
			t.Fatalf("accepted %s envelope", kind)
		}
		writer.Close()
		if got := state.Result(); got == nil || got.Complete || got.Connected || (got.Error != "missing_evidence" && got.Error != "invalid_evidence") {
			t.Fatalf("corrupt evidence became success: %+v", got)
		}
		if kind == "identity" {
			if err != nil {
				t.Fatal("optional malformed metadata blocked response payload")
			}
			if err := writer.WriteMultiBuffer(buf.MergeBytes(nil, []byte("TLS response"))); err != nil {
				t.Fatal(err)
			}
			if payload.String() != "TLS response" {
				t.Fatal("response payload changed")
			}
		}
	}
}
func hexDecode(id string) ([]byte, error) { return hex.DecodeString(id) }

func TestParallelConnectionsHaveDistinctCorrelationAndBoundedPrivateOutput(t *testing.T) {
	path := evidenceFile(t)
	origin(t, "exit")
	var workers sync.WaitGroup
	ids := sync.Map{}
	for i := 0; i < 64; i++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			state, err := newLocal("www.youtube.com", "youtube_premium.premium")
			if err != nil {
				t.Error(err)
				return
			}
			if _, loaded := ids.LoadOrStore(state.ID, true); loaded {
				t.Error("duplicate ID")
			}
			state.Finish("out:forward:probe-v4", "v4", true, "", terminalLookup())
		}()
	}
	workers.Wait()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(bytes.Split(bytes.TrimSpace(data), []byte{'\n'})) != 128 {
		t.Fatal("parallel records were lost or interleaved")
	}
	for i := 0; i < MaxRecords+1; i++ {
		state, _ := newLocal("www.youtube.com", "youtube_premium.premium")
		state.Finish("out:forward:probe-v4", "v4", true, "", terminalLookup())
	}
	info, _ := os.Stat(path)
	data, _ = os.ReadFile(path)
	if info.Mode().Perm() != 0600 || len(data) > MaxFile || len(bytes.Split(bytes.TrimSpace(data), []byte{'\n'})) != MaxRecords {
		t.Fatal("private evidence limits violated")
	}
}

func TestCancellationDoesNotWaitForAnEvidenceEmitter(t *testing.T) {
	entered, release, done := make(chan struct{}), make(chan struct{}), make(chan struct{})
	state := &State{ID: "00000000000000000000000000000001", Domain: "www.youtube.com", emit: func(Result) error { close(entered); <-release; return nil }}
	go func() { state.Unknown(); close(done) }()
	<-entered
	aborted := make(chan struct{})
	go func() { state.Abort(); close(aborted) }()
	select {
	case <-aborted:
	case <-time.After(time.Second):
		close(release)
		t.Fatal("cancellation blocked on network evidence write")
	}
	close(release)
	<-done
}

type blockedProbeConn struct {
	stdnet.Conn
	entered chan struct{}
	once    sync.Once
}

func (c *blockedProbeConn) Write(data []byte) (int, error) {
	c.once.Do(func() { close(c.entered) })
	return c.Conn.Write(data)
}
func TestInterruptUnblocksAnEvidenceFrameUnderBackpressure(t *testing.T) {
	left, right := stdnet.Pipe()
	defer left.Close()
	defer right.Close()
	conn := &blockedProbeConn{Conn: left, entered: make(chan struct{})}
	state := &State{ID: "00000000000000000000000000000001", Domain: "www.youtube.com"}
	writer := &responseWriter{Writer: buf.NewWriter(conn), state: state, cancel: func() { conn.Close() }}
	state.emit = writer.emit
	sent := make(chan struct{})
	go func() { state.Unknown(); close(sent) }()
	<-conn.entered
	stopped := make(chan struct{})
	go func() { writer.Interrupt(); close(stopped) }()
	select {
	case <-stopped:
	case <-time.After(time.Second):
		t.Fatal("interrupt did not close the blocked probe stream")
	}
	<-sent
}

type interruptibleProbeWriter struct {
	entered chan struct{}
	closed  chan struct{}
	once    sync.Once
}

func (w *interruptibleProbeWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	defer buf.ReleaseMulti(mb)
	close(w.entered)
	<-w.closed
	return io.ErrClosedPipe
}
func (w *interruptibleProbeWriter) Interrupt() { w.once.Do(func() { close(w.closed) }) }
func TestInterruptKeepsTheMultiplexedParentSessionOpen(t *testing.T) {
	stream := &interruptibleProbeWriter{entered: make(chan struct{}), closed: make(chan struct{})}
	parentClosed := false
	state := &State{ID: "00000000000000000000000000000001", Domain: "www.youtube.com"}
	writer := &responseWriter{Writer: stream, state: state, cancel: func() { parentClosed = true; stream.Interrupt() }}
	state.emit = writer.emit
	done := make(chan struct{})
	go func() { state.Unknown(); close(done) }()
	<-stream.entered
	writer.Interrupt()
	<-done
	if parentClosed {
		t.Fatal("cancelling one logical probe stream closed its multiplexed parent session")
	}
}
