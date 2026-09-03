package anytls

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"testing"

	"github.com/xtls/xray-core/common/buf"
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

func TestWritePacketWithPaddingSplitsOversizedRecord(t *testing.T) {
	var wire bytes.Buffer
	writer := buf.NewBufferedWriter(buf.NewWriter(&wire))
	s := &session{
		fw: newFrameWriter(writer),
	}
	s.paddingScheme, _ = parsePaddingScheme("stop=1\n0=70000-70000")

	frames, err := (&frame{cmd: cmdHeartRequest, sid: 0}).toMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	if err := s.writePacketWithPadding(0, frames); err != nil {
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
