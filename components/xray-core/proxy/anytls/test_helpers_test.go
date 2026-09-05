package anytls

import (
	"bytes"
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport"
)

type testWireFrame struct {
	cmd  byte
	sid  uint32
	data []byte
}

func marshalTestFrame(cmd byte, sid uint32, data []byte) []byte {
	if len(data) > maxFramePayload {
		panic("test frame payload exceeds protocol limit")
	}
	frame := make([]byte, 7+len(data))
	frame[0] = cmd
	binary.BigEndian.PutUint32(frame[1:5], sid)
	binary.BigEndian.PutUint16(frame[5:7], uint16(len(data)))
	copy(frame[7:], data)
	return frame
}

func marshalTestFrames(frames ...testWireFrame) []byte {
	var wire bytes.Buffer
	for _, frame := range frames {
		wire.Write(marshalTestFrame(frame.cmd, frame.sid, frame.data))
	}
	return wire.Bytes()
}

func parseTestFrames(t *testing.T, wire []byte) []testWireFrame {
	t.Helper()
	var frames []testWireFrame
	for len(wire) > 0 {
		if len(wire) < 7 {
			t.Fatalf("truncated frame header: %d bytes remain", len(wire))
		}
		length := int(binary.BigEndian.Uint16(wire[5:7]))
		if len(wire) < 7+length {
			t.Fatalf("truncated frame body: have %d bytes, want %d", len(wire), 7+length)
		}
		frames = append(frames, testWireFrame{
			cmd:  wire[0],
			sid:  binary.BigEndian.Uint32(wire[1:5]),
			data: bytes.Clone(wire[7 : 7+length]),
		})
		wire = wire[7+length:]
	}
	return frames
}

func newWireSession(wire []byte, isClient bool) (*session, *bytes.Buffer) {
	var output bytes.Buffer
	s := &session{
		isClient:        isClient,
		br:              &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(wire))},
		bw:              buf.NewBufferedWriter(buf.NewWriter(&output)),
		streams:         make(map[uint32]*stream),
		drainingStreams: make(map[uint32]*stream),
		peerVersion:     1,
	}
	s.fw = newFrameWriter(s.bw)
	s.paddingScheme = getDefaultPaddingScheme()
	s.nextSID.Store(1)
	s.pktCounter.Store(1)
	return s, &output
}

type testDispatcher struct {
	dispatch func(context.Context, xnet.Destination) (*transport.Link, error)
}

func (d *testDispatcher) Dispatch(ctx context.Context, destination xnet.Destination) (*transport.Link, error) {
	if d.dispatch == nil {
		return nil, fmt.Errorf("test dispatcher has no dispatch function")
	}
	return d.dispatch(ctx, destination)
}

func (*testDispatcher) DispatchLink(context.Context, xnet.Destination, *transport.Link) error {
	return nil
}

func (*testDispatcher) Start() error { return nil }

func (*testDispatcher) Close() error { return nil }

func (*testDispatcher) Type() interface{} { return routing.DispatcherType() }

func multiBufferBytes(t *testing.T, mb buf.MultiBuffer) []byte {
	t.Helper()
	defer buf.ReleaseMulti(mb)
	result := make([]byte, mb.Len())
	mb.Copy(result)
	return result
}

func readOnePipeBuffer(t *testing.T, reader buf.Reader) []byte {
	t.Helper()
	mb, err := reader.ReadMultiBuffer()
	if err != nil {
		t.Fatalf("read pipe buffer: %v", err)
	}
	return multiBufferBytes(t, mb)
}

func readPipeExact(t *testing.T, reader buf.Reader, length int) []byte {
	t.Helper()
	result := make([]byte, 0, length)
	for len(result) < length {
		mb, err := reader.ReadMultiBuffer()
		if err != nil {
			t.Fatalf("read pipe buffer at %d/%d bytes: %v", len(result), length, err)
		}
		result = append(result, multiBufferBytes(t, mb)...)
	}
	if len(result) != length {
		t.Fatalf("read %d bytes, want %d", len(result), length)
	}
	return result
}

func readUntilEOF(t *testing.T, reader *buf.BufferedReader) []byte {
	t.Helper()
	var result bytes.Buffer
	for {
		mb, err := reader.ReadMultiBuffer()
		if !mb.IsEmpty() {
			result.Write(multiBufferBytes(t, mb))
		}
		if err != nil {
			if err != io.EOF {
				t.Fatalf("read until EOF: %v", err)
			}
			return result.Bytes()
		}
	}
}
