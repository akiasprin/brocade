package mux

import (
	"bytes"
	"encoding/binary"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/crypto"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func marshalMetadataForTest(t *testing.T, meta FrameMetadata) []byte {
	t.Helper()
	b := buf.New()
	defer b.Release()
	if err := meta.WriteTo(b); err != nil {
		t.Fatalf("WriteTo() error = %v", err)
	}
	return append([]byte(nil), b.Bytes()...)
}

func TestProbeFrameRoundTrip(t *testing.T) {
	for _, ack := range []bool{false, true} {
		t.Run(map[bool]string{false: "ping", true: "pong"}[ack], func(t *testing.T) {
			meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive, ProbeID: 0x0102030405060708}
			meta.Option.Set(OptionProbe)
			if ack {
				meta.Option.Set(OptionAck)
			}
			wire := marshalMetadataForTest(t, meta)
			if got := binary.BigEndian.Uint16(wire[:2]); got != 12 {
				t.Fatalf("metadata length = %d, want 12", got)
			}
			var decoded FrameMetadata
			if err := decoded.Unmarshal(bytes.NewReader(wire), false); err != nil {
				t.Fatalf("Unmarshal() error = %v", err)
			}
			if decoded.SessionStatus != SessionStatusKeepAlive || decoded.SessionID != 0 || decoded.ProbeID != meta.ProbeID {
				t.Fatalf("decoded probe = %+v", decoded)
			}
			if !decoded.Option.Has(OptionProbe) || decoded.Option.Has(OptionAck) != ack {
				t.Fatalf("decoded options = %#x", decoded.Option)
			}
		})
	}
}

func TestProbeFrameIgnoresBufferUDPSidecar(t *testing.T) {
	destination := net.UDPDestination(net.DomainAddress("example.com"), 53)
	b := buf.New()
	b.UDP = &destination
	defer b.Release()
	meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe, ProbeID: 17}
	if err := meta.WriteTo(b); err != nil {
		t.Fatal(err)
	}
	if got := binary.BigEndian.Uint16(b.Bytes()[:2]); got != 12 {
		t.Fatalf("metadata length with UDP sidecar = %d, want 12", got)
	}
}

func TestProbeFrameRejectsInvalidShapes(t *testing.T) {
	tests := []FrameMetadata{
		{SessionStatus: SessionStatusKeepAlive, Option: OptionAck},
		{SessionStatus: SessionStatusKeep, Option: OptionProbe, ProbeID: 1},
		{SessionStatus: SessionStatusKeepAlive, SessionID: 1, Option: OptionProbe, ProbeID: 1},
		{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe | OptionData, ProbeID: 1},
		{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe | OptionError, ProbeID: 1},
		{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe | 0x80, ProbeID: 1},
	}
	for i, meta := range tests {
		b := buf.New()
		err := meta.WriteTo(b)
		b.Release()
		if err == nil {
			t.Fatalf("case %d: invalid probe was accepted", i)
		}
	}

	valid := marshalMetadataForTest(t, FrameMetadata{
		SessionStatus: SessionStatusKeepAlive,
		Option:        OptionProbe,
		ProbeID:       7,
	})
	valid[1] = 11
	var decoded FrameMetadata
	if err := decoded.Unmarshal(bytes.NewReader(valid[:len(valid)-1]), false); err == nil {
		t.Fatal("truncated probe was accepted")
	}

	for _, option := range []byte{byte(OptionProbe | OptionError), byte(OptionProbe) | 0x80} {
		invalid := append([]byte(nil), valid...)
		invalid[5] = option
		if err := decoded.Unmarshal(bytes.NewReader(invalid), false); err == nil {
			t.Fatalf("probe with invalid option %#x was accepted", option)
		}
	}
}

func TestFrameMetadataUnmarshalClearsProbeID(t *testing.T) {
	probe := marshalMetadataForTest(t, FrameMetadata{
		SessionStatus: SessionStatusKeepAlive,
		Option:        OptionProbe,
		ProbeID:       99,
	})
	ordinary := marshalMetadataForTest(t, FrameMetadata{SessionStatus: SessionStatusKeepAlive})
	var decoded FrameMetadata
	if err := decoded.Unmarshal(bytes.NewReader(probe), false); err != nil {
		t.Fatal(err)
	}
	if err := decoded.Unmarshal(bytes.NewReader(ordinary), false); err != nil {
		t.Fatal(err)
	}
	if decoded.ProbeID != 0 || decoded.Option != 0 {
		t.Fatalf("metadata retained fields from previous frame: %+v", decoded)
	}
}

func TestServerRepliesToProbeExactlyOnce(t *testing.T) {
	reader, writer := pipe.New(pipe.WithoutSizeLimit())
	worker := &ServerWorker{link: &transport.Link{Writer: writer}, sessionManager: NewSessionManager()}
	meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionProbe, ProbeID: 42}
	if err := worker.handleStatusKeepAlive(&meta, nil); err != nil {
		t.Fatalf("handleStatusKeepAlive() error = %v", err)
	}
	var pong FrameMetadata
	if err := pong.Unmarshal(&buf.BufferedReader{Reader: reader}, false); err != nil {
		t.Fatalf("reading pong: %v", err)
	}
	if pong.ProbeID != 42 || !pong.Option.Has(OptionProbe) || !pong.Option.Has(OptionAck) {
		t.Fatalf("pong = %+v", pong)
	}
	if worker.sessionManager.Size() != 0 || worker.sessionManager.Count() != 0 {
		t.Fatal("probe created a business session and could enter user accounting")
	}
	if err := worker.handleStatusKeepAlive(&pong, nil); err != nil {
		t.Fatalf("handling pong: %v", err)
	}
}

func TestOrdinaryKeepAliveDataDoesNotCompleteProbe(t *testing.T) {
	var wire bytes.Buffer
	chunkWriter := crypto.NewChunkStreamWriter(crypto.PlainChunkSizeParser{}, &wire)
	payload := buf.New()
	payload.Write([]byte("keepalive payload"))
	if err := chunkWriter.WriteMultiBuffer(buf.MultiBuffer{payload}); err != nil {
		t.Fatal(err)
	}
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(wire.Bytes()))}
	pending := make(chan struct{}, 1)
	worker := &ClientWorker{
		poolState:      workerProbing,
		pendingProbeID: 73,
		pendingProbe:   pending,
	}
	meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive, Option: OptionData}
	if err := worker.handleStatueKeepAlive(&meta, reader); err != nil {
		t.Fatal(err)
	}
	select {
	case <-pending:
		t.Fatal("ordinary KeepAlive completed a probe")
	default:
	}
	if reader.BufferedBytes() != 0 {
		t.Fatalf("ordinary KeepAlive left %d buffered bytes", reader.BufferedBytes())
	}
}

func TestOrdinaryKeepAliveDoesNotReplyOrChangeProbe(t *testing.T) {
	pending := make(chan struct{}, 1)
	worker := &ClientWorker{
		poolState:      workerProbing,
		pendingProbeID: 91,
		pendingProbe:   pending,
	}
	meta := FrameMetadata{SessionStatus: SessionStatusKeepAlive}
	if err := worker.handleStatueKeepAlive(&meta, nil); err != nil {
		t.Fatal(err)
	}
	if state := workerStateForTest(worker); state != workerProbing {
		t.Fatalf("ordinary KeepAlive changed worker state to %v", state)
	}
	select {
	case <-pending:
		t.Fatal("ordinary KeepAlive completed a probe")
	default:
	}
}

func FuzzFrameMetadataUnmarshal(f *testing.F) {
	f.Add([]byte{})
	f.Add([]byte{0, 4, 0, 0, byte(SessionStatusKeepAlive), 0})
	f.Add(marshalProbeSeed(0xfeedbeef, false))
	f.Add(marshalProbeSeed(0xfeedbeef, true))
	f.Fuzz(func(t *testing.T, wire []byte) {
		if len(wire) > 1024 {
			return
		}
		var meta FrameMetadata
		_ = meta.Unmarshal(bytes.NewReader(wire), false)
	})
}

func FuzzProbeFrameRoundTrip(f *testing.F) {
	f.Add(uint64(0), false)
	f.Add(^uint64(0), true)
	f.Add(uint64(0x0102030405060708), true)
	f.Fuzz(func(t *testing.T, probeID uint64, ack bool) {
		wire := marshalProbeSeed(probeID, ack)
		var meta FrameMetadata
		if err := meta.Unmarshal(bytes.NewReader(wire), false); err != nil {
			t.Fatalf("Unmarshal() error = %v", err)
		}
		if meta.ProbeID != probeID || meta.Option.Has(OptionAck) != ack || !meta.Option.Has(OptionProbe) {
			t.Fatalf("round trip mismatch: %+v", meta)
		}
	})
}

func marshalProbeSeed(probeID uint64, ack bool) []byte {
	wire := make([]byte, 14)
	binary.BigEndian.PutUint16(wire[:2], 12)
	wire[4] = byte(SessionStatusKeepAlive)
	wire[5] = byte(OptionProbe)
	if ack {
		wire[5] |= byte(OptionAck)
	}
	binary.BigEndian.PutUint64(wire[6:], probeID)
	return wire
}
