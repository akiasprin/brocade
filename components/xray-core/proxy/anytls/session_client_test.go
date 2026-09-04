package anytls

import (
	"bytes"
	"context"
	"encoding/binary"
	"fmt"
	"math"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
)

func TestWriteWasteFramesKeepsWireLengthAndFrameHeaders(t *testing.T) {
	for _, total := range []int{7, 8, 30, maxFramePayload + 7, maxFramePayload + 8, 4 * 1024 * 1024} {
		t.Run(fmt.Sprintf("total-%d", total), func(t *testing.T) {
			wire := new(bytes.Buffer)
			writer := buf.NewBufferedWriter(buf.NewWriter(wire))
			s := &session{fw: newFrameWriter(writer)}

			if err := s.writeWasteFrames(total); err != nil {
				t.Fatal(err)
			}
			if wire.Len() != total {
				t.Fatalf("wire length = %d, want %d", wire.Len(), total)
			}

			for remaining := wire.Bytes(); len(remaining) > 0; {
				if len(remaining) < 7 {
					t.Fatalf("truncated waste frame header: %d bytes remain", len(remaining))
				}
				if remaining[0] != cmdWaste {
					t.Fatalf("command = %d, want waste (%d)", remaining[0], cmdWaste)
				}
				if sid := binary.BigEndian.Uint32(remaining[1:5]); sid != 0 {
					t.Fatalf("stream id = %d, want 0", sid)
				}
				bodyLength := int(binary.BigEndian.Uint16(remaining[5:7]))
				if bodyLength > maxFramePayload {
					t.Fatalf("body length = %d, exceeds %d", bodyLength, maxFramePayload)
				}
				frameLength := 7 + bodyLength
				if len(remaining) < frameLength {
					t.Fatalf("truncated waste frame body: have %d, want %d", len(remaining), frameLength)
				}
				remaining = remaining[frameLength:]
			}
		})
	}
}

func TestSendStreamDataReleasesWriteLockAfterPaddingError(t *testing.T) {
	var wire bytes.Buffer
	s := &session{bw: buf.NewBufferedWriter(buf.NewWriter(&wire))}
	s.fw = newFrameWriter(s.bw)
	s.paddingScheme, _ = parsePaddingScheme("stop=2\n1=1-1")
	s.pktCounter.Store(1)

	if err := s.sendStreamData(1, buf.MultiBuffer{buf.FromBytes([]byte("first"))}); err == nil {
		t.Fatal("invalid padding scheme unexpectedly succeeded")
	}

	secondWrite := make(chan error, 1)
	go func() {
		secondWrite <- s.sendStreamData(1, buf.MultiBuffer{buf.FromBytes([]byte("second"))})
	}()
	select {
	case err := <-secondWrite:
		if err != nil {
			t.Fatalf("second write error = %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("writeMu remained locked after padding error")
	}
}

func TestWritePacketWithPaddingSplitsOversizedRecord(t *testing.T) {
	var wire bytes.Buffer
	writer := buf.NewBufferedWriter(buf.NewWriter(&wire))
	s := &session{
		fw: newFrameWriter(writer),
	}
	s.paddingScheme, _ = parsePaddingScheme("stop=2\n1=70000-70000")

	frames, err := (&frame{cmd: cmdHeartRequest, sid: 0}).toMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	if err := s.writePacketWithPadding(1, frames); err != nil {
		t.Fatal(err)
	}
	if wire.Len() != 70000 {
		t.Fatalf("wire length = %d, want 70000", wire.Len())
	}
	parsed := parseTestFrames(t, wire.Bytes())
	if len(parsed) < 2 || parsed[0].cmd != cmdHeartRequest || parsed[0].sid != 0 {
		t.Fatalf("padding record frames = %+v, want heartbeat followed by waste frames", parsed)
	}
	for _, frame := range parsed[1:] {
		if frame.cmd != cmdWaste || frame.sid != 0 {
			t.Fatalf("padding frame = %+v, want waste stream 0", frame)
		}
	}
}

func TestOpenStreamSerializesAssignedIDsOnWire(t *testing.T) {
	s, output := newWireSession(nil, true)
	const streamCount = 32
	endpoints := make([]*testLinkEndpoint, streamCount)
	results := make(chan *stream, streamCount)
	errs := make(chan error, streamCount)
	start := make(chan struct{})
	var wg sync.WaitGroup
	for i := range streamCount {
		endpoint := newTestLinkEndpoint()
		endpoints[i] = endpoint
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			stream, err := s.openStream(
				context.Background(),
				xnet.TCPDestination(xnet.DomainAddress("example.com"), 443),
				endpoint.link,
			)
			if err != nil {
				errs <- err
				return
			}
			results <- stream
		}()
	}
	close(start)
	wg.Wait()
	close(errs)
	close(results)
	for err := range errs {
		t.Fatalf("openStream error = %v", err)
	}

	var synIDs []uint32
	for _, frame := range parseTestFrames(t, output.Bytes()) {
		if frame.cmd == cmdSYN {
			synIDs = append(synIDs, frame.sid)
		}
	}
	if len(synIDs) != streamCount {
		t.Fatalf("SYN count = %d, want %d", len(synIDs), streamCount)
	}
	for i, sid := range synIDs {
		if want := uint32(i + 1); sid != want {
			t.Fatalf("SYN IDs = %v, want strictly increasing sequence at %d", synIDs, want)
		}
	}
	for stream := range results {
		s.finishStream(stream.sid, nil)
	}
	for _, endpoint := range endpoints {
		endpoint.closeInput()
		endpoint.closeOutput()
	}
}

func TestOpenStreamRetiresSessionInsteadOfWrappingToZero(t *testing.T) {
	s, output := newWireSession(nil, true)
	s.nextSID.Store(math.MaxUint32)
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()
	defer endpoint.closeOutput()

	stream, err := s.openStream(
		context.Background(),
		xnet.TCPDestination(xnet.DomainAddress("example.com"), 443),
		endpoint.link,
	)
	if err != nil {
		t.Fatal(err)
	}
	if stream.sid != math.MaxUint32 || s.nextSID.Load() != 0 {
		t.Fatalf("stream ID state = current:%d next:%d", stream.sid, s.nextSID.Load())
	}
	s.finishStream(stream.sid, nil)

	if _, err := s.openStream(context.Background(), xnet.TCPDestination(xnet.DomainAddress("example.com"), 443), endpoint.link); err != errStreamIDExhausted {
		t.Fatalf("open after maximum ID = %v, want %v", err, errStreamIDExhausted)
	}
	for _, frame := range parseTestFrames(t, output.Bytes()) {
		if frame.cmd == cmdSYN && frame.sid == 0 {
			t.Fatal("client emitted a SYN with stream ID 0")
		}
	}

	client := &Client{sessions: map[uint64]*session{1: s}}
	s.seq = 1
	s.setDieHook(func() {
		client.sessionsMu.Lock()
		delete(client.sessions, s.seq)
		client.sessionsMu.Unlock()
	})
	client.markSessionIdle(s)
	if !s.isClosed() || len(client.idleSessions) != 0 || len(client.sessions) != 0 {
		t.Fatalf("exhausted session was retained: closed=%v idle=%v sessions=%v", s.isClosed(), client.idleSessions, client.sessions)
	}
}
