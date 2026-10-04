package proxy

import (
	"bytes"
	"context"
	"io"
	stdnet "net"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/app/dispatcher"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal"
)

type spliceTestCounter struct {
	value atomic.Int64
}

type spliceTestMetrics struct {
	direct  atomic.Int64
	directB atomic.Int64
	spliced atomic.Int64
	bytes   atomic.Int64
}

func (m *spliceTestMetrics) MarkDirect() {
	m.direct.Add(1)
}

func (m *spliceTestMetrics) AddDirectBytes(bytes int64) {
	m.directB.Add(bytes)
}

func (m *spliceTestMetrics) MarkSplice() {
	m.spliced.Add(1)
}

func (m *spliceTestMetrics) AddSpliceBytes(bytes int64) {
	m.bytes.Add(bytes)
}

func (*spliceTestMetrics) SetNotSplicedReason(session.SpliceNotUsedReason) {}

func (*spliceTestMetrics) Finish() {}

func (c *spliceTestCounter) Value() int64 {
	return c.value.Load()
}

func (c *spliceTestCounter) Add(value int64) int64 {
	return c.value.Add(value)
}

func (c *spliceTestCounter) Set(value int64) int64 {
	c.value.Store(value)
	return value
}

func tcpConnPair(t *testing.T) (*stdnet.TCPConn, *stdnet.TCPConn) {
	t.Helper()
	listener, err := stdnet.ListenTCP("tcp4", &stdnet.TCPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	accepted := make(chan *stdnet.TCPConn, 1)
	acceptErr := make(chan error, 1)
	go func() {
		conn, err := listener.AcceptTCP()
		if err != nil {
			acceptErr <- err
			return
		}
		accepted <- conn
	}()

	client, err := stdnet.DialTCP("tcp4", nil, listener.Addr().(*stdnet.TCPAddr))
	if err != nil {
		t.Fatal(err)
	}
	select {
	case server := <-accepted:
		return server, client
	case err := <-acceptErr:
		_ = client.Close()
		t.Fatal(err)
	case <-time.After(5 * time.Second):
		_ = client.Close()
		t.Fatal("timed out accepting TCP test connection")
	}
	return nil, nil
}

func TestVisionWriterPublishesDirectOnlyAfterWrite(t *testing.T) {
	writerConn, output := tcpConnPair(t)
	defer writerConn.Close()
	defer output.Close()

	payload := []byte("direct payload")
	readResult := make(chan error, 1)
	go func() {
		got := make([]byte, len(payload))
		_, err := io.ReadFull(output, got)
		if err == nil && !bytes.Equal(got, payload) {
			err = io.ErrUnexpectedEOF
		}
		readResult <- err
	}()

	metrics := new(spliceTestMetrics)
	inbound := &session.Inbound{Conn: writerConn, SpliceMetrics: metrics}
	inbound.CanSpliceCopy.Store(session.SpliceCopyWaiting)
	ctx := session.ContextWithInbound(context.Background(), inbound)
	trafficState := NewTrafficState(nil)
	trafficState.NumberOfPacketToFilter = 0
	trafficState.Inbound.IsPadding = false
	trafficState.Inbound.DownlinkWriterDirectCopy = true

	writer := NewVisionWriter(buf.NewWriter(writerConn), trafficState, false, ctx, writerConn, nil, nil)
	if err := writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		t.Fatal(err)
	}
	if err := <-readResult; err != nil {
		t.Fatal(err)
	}
	if got := inbound.CanSpliceCopy.Load(); got != session.SpliceCopyDirect {
		t.Fatalf("splice state = %d, want %d", got, session.SpliceCopyDirect)
	}
	if got := metrics.direct.Load(); got != 1 {
		t.Fatalf("direct transitions = %d, want 1", got)
	}
	if got := metrics.directB.Load(); got != int64(len(payload)) {
		t.Fatalf("observed direct bytes = %d, want %d", got, len(payload))
	}
}

func TestCopyRawConnIfExistCountsSplicedTraffic(t *testing.T) {
	readerConn, input := tcpConnPair(t)
	writerConn, output := tcpConnPair(t)
	defer readerConn.Close()
	defer input.Close()
	defer writerConn.Close()
	defer output.Close()

	payload := make([]byte, 2*1024*1024)
	for index := range payload {
		payload[index] = byte(index)
	}

	readResult := make(chan error, 1)
	go func() {
		got := make([]byte, len(payload))
		_, err := io.ReadFull(output, got)
		if err == nil && !bytes.Equal(got, payload) {
			err = io.ErrUnexpectedEOF
		}
		readResult <- err
	}()
	writeResult := make(chan error, 1)
	go func() {
		_, err := input.Write(payload)
		if closeErr := input.CloseWrite(); err == nil {
			err = closeErr
		}
		writeResult <- err
	}()

	metrics := new(spliceTestMetrics)
	inbound := &session.Inbound{Conn: writerConn, SpliceMetrics: metrics}
	inbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
	outbound := &session.Outbound{}
	outbound.CanSpliceCopy.Store(session.SpliceCopyDirect)
	ctx := session.ContextWithInbound(context.Background(), inbound)
	ctx = session.ContextWithOutbounds(ctx, []*session.Outbound{outbound})
	counter := new(spliceTestCounter)
	writer := &dispatcher.SizeStatWriter{
		Counter: counter,
		Writer:  buf.NewWriter(writerConn),
	}
	if err := CopyRawConnIfExist(
		ctx,
		readerConn,
		writerConn,
		writer,
		signal.NewNoopActivityTimer(),
		nil,
	); err != nil {
		t.Fatal(err)
	}
	if err := <-writeResult; err != nil {
		t.Fatal(err)
	}
	if err := <-readResult; err != nil {
		t.Fatal(err)
	}
	if got := counter.Value(); got != int64(len(payload)) {
		t.Fatalf("spliced user traffic counter = %d, want %d", got, len(payload))
	}
	if got := metrics.spliced.Load(); got != 1 {
		t.Fatalf("splice transitions = %d, want 1", got)
	}
	if got := metrics.bytes.Load(); got != int64(len(payload)) {
		t.Fatalf("observed splice bytes = %d, want %d", got, len(payload))
	}
	if got := inbound.CanSpliceCopy.Load(); got != session.SpliceCopySplicing {
		t.Fatalf("splice state = %d, want %d", got, session.SpliceCopySplicing)
	}
}
