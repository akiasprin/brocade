package mux

import (
	"bytes"
	"io"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/common/signal/done"
	"github.com/xtls/xray-core/transport/pipe"
)

type metadataBlockingReader struct {
	entered chan struct{}
	release chan struct{}
}

func (r *metadataBlockingReader) Read([]byte) (int, error) {
	select {
	case <-r.entered:
	default:
		close(r.entered)
	}
	<-r.release
	return 0, io.EOF
}

func packetFrameForTest(t *testing.T, payload []byte) *buf.BufferedReader {
	t.Helper()
	wire := buf.New()
	if _, err := serial.WriteUint16(wire, uint16(len(payload))); err != nil {
		wire.Release()
		t.Fatal(err)
	}
	if _, err := wire.Write(payload); err != nil {
		wire.Release()
		t.Fatal(err)
	}
	encoded := append([]byte(nil), wire.Bytes()...)
	wire.Release()
	return &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(encoded))}
}

func TestClientPacketTargetSurvivesNextMetadataReset(t *testing.T) {
	outputReader, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	defer outputReader.Interrupt()
	defer outputWriter.Close()

	manager := NewSessionManager()
	s := &Session{
		ID:           7,
		parent:       manager,
		output:       outputWriter,
		transferType: protocol.TransferTypePacket,
		done:         done.New(),
	}
	if !manager.Add(s) {
		t.Fatal("failed to register packet session")
	}
	worker := &ClientWorker{sessionManager: manager}
	meta := FrameMetadata{
		SessionID:     s.ID,
		SessionStatus: SessionStatusKeep,
		Option:        OptionData,
		Target:        net.UDPDestination(net.IPAddress([]byte{1, 2, 3, 4}), 53),
	}
	if err := worker.handleStatusKeep(&meta, packetFrameForTest(t, []byte("ordinary-udp-response"))); err != nil {
		t.Fatal(err)
	}

	// fetchOutput immediately reuses meta for the next frame. Unmarshal clears it
	// before blocking on the next frame's length, while the packet above may still
	// be waiting in the asynchronous pipe.
	blocking := &metadataBlockingReader{entered: make(chan struct{}), release: make(chan struct{})}
	doneReading := make(chan error, 1)
	go func() { doneReading <- meta.Unmarshal(blocking, false) }()
	select {
	case <-blocking.entered:
	case <-time.After(time.Second):
		t.Fatal("next metadata read did not start")
	}

	packets, err := outputReader.ReadMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(packets)
	if len(packets) != 1 || string(packets[0].Bytes()) != "ordinary-udp-response" {
		t.Fatalf("packet payload = %q", packets[0].Bytes())
	}
	if packets[0].UDP == nil || packets[0].UDP.Address == nil || packets[0].UDP.String() != "udp:1.2.3.4:53" {
		t.Fatalf("packet destination changed after metadata reuse: %v", packets[0].UDP)
	}

	// EndpointOverrideWriter is allowed to rewrite the packet-owned sidecar. It
	// must no longer mutate the FrameMetadata object reused by fetchOutput.
	packets[0].UDP.Address = net.IPAddress([]byte{5, 6, 7, 8})
	if meta.Target.Address != nil {
		t.Fatalf("reused metadata was mutated through packet sidecar: %v", meta.Target)
	}

	close(blocking.release)
	if err := <-doneReading; err != io.EOF {
		t.Fatalf("next metadata read error = %v, want EOF", err)
	}
}

func TestFrameMetadataDoesNotRetainPreviousPacketTarget(t *testing.T) {
	first := marshalMetadataForTest(t, FrameMetadata{
		SessionID:     1,
		SessionStatus: SessionStatusNew,
		Option:        OptionData,
		Target:        net.UDPDestination(net.DomainAddress("dns.example"), 53),
	})
	second := marshalMetadataForTest(t, FrameMetadata{
		SessionID:     2,
		SessionStatus: SessionStatusKeepAlive,
	})

	var decoded FrameMetadata
	if err := decoded.Unmarshal(bytes.NewReader(first), false); err != nil {
		t.Fatal(err)
	}
	if decoded.Target.Address == nil {
		t.Fatal("first packet target was not decoded")
	}
	if err := decoded.Unmarshal(bytes.NewReader(second), false); err != nil {
		t.Fatal(err)
	}
	if decoded.Target.Address != nil || decoded.Target.Network != net.Network_Unknown || decoded.Target.Port != 0 {
		t.Fatalf("metadata retained the previous packet target: %v", decoded.Target)
	}
}
