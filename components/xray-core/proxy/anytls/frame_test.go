package anytls

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"testing"

	"github.com/xtls/xray-core/common/buf"
)

func TestFrameSerializationBoundaries(t *testing.T) {
	tests := []struct {
		name    string
		dataLen int
		wantErr bool
	}{
		{name: "empty", dataLen: 0},
		{name: "one", dataLen: 1},
		{name: "maximum", dataLen: maxFramePayload},
		{name: "too-large", dataLen: maxFramePayload + 1, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := bytes.Repeat([]byte{0x5a}, tt.dataLen)
			mb, err := (&frame{cmd: cmdPSH, sid: 17, data: data}).toMultiBuffer()
			if tt.wantErr {
				if err == nil {
					t.Fatal("expected serialization error")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			wire := multiBufferBytes(t, mb)
			if len(wire) != 7+tt.dataLen {
				t.Fatalf("wire length = %d, want %d", len(wire), 7+tt.dataLen)
			}
			if wire[0] != cmdPSH || binary.BigEndian.Uint32(wire[1:5]) != 17 {
				t.Fatalf("unexpected header: %x", wire[:7])
			}
			if got := int(binary.BigEndian.Uint16(wire[5:7])); got != tt.dataLen {
				t.Fatalf("payload length = %d, want %d", got, tt.dataLen)
			}
		})
	}
}

func TestFrameSerializationWithBodyOwnership(t *testing.T) {
	for _, bodyLen := range []int{0, 1, buf.Size + 1, maxFramePayload} {
		t.Run(fmt.Sprintf("body-%d", bodyLen), func(t *testing.T) {
			body := buf.NewWithSize(int32(bodyLen))
			body.Extend(int32(bodyLen))
			want := make([]byte, bodyLen)
			for i := range body.Bytes() {
				body.Bytes()[i] = byte(i)
				want[i] = byte(i)
			}
			mb, err := (&frame{cmd: cmdPSH, sid: 3}).toMultiBufferWithBody(body)
			if err != nil {
				t.Fatal(err)
			}
			wire := multiBufferBytes(t, mb)
			if len(wire) != 7+bodyLen {
				t.Fatalf("wire length = %d, want %d", len(wire), 7+bodyLen)
			}
			if !bytes.Equal(wire[7:], want) {
				t.Fatal("body payload was not preserved")
			}
		})
	}
}

func TestFrameSerializationRejectsNilFrameAndOversizedBody(t *testing.T) {
	if mb, err := (*frame)(nil).toMultiBuffer(); err == nil || mb != nil {
		t.Fatalf("nil frame result = (%v, %v), want error and nil buffer", mb, err)
	}

	body := buf.NewWithSize(maxFramePayload + 1)
	body.Extend(maxFramePayload + 1)
	mb, err := (&frame{cmd: cmdPSH, sid: 1}).toMultiBufferWithBody(body)
	if err == nil || mb != nil {
		t.Fatalf("oversized body result = (%v, %v), want error and nil buffer", mb, err)
	}
}

func TestFrameWriterRejectsOversizedFrames(t *testing.T) {
	var wire bytes.Buffer
	writer := newFrameWriter(buf.NewBufferedWriter(buf.NewWriter(&wire)))
	if err := writer.writeFrame(&frame{cmd: cmdPSH, sid: 1, data: make([]byte, maxFramePayload+1)}); err == nil {
		t.Fatal("expected writeFrame to reject oversized payload")
	}
	body := buf.NewWithSize(maxFramePayload + 1)
	body.Extend(maxFramePayload + 1)
	if err := writer.writeMultiBuffer(cmdPSH, 1, buf.MultiBuffer{body}); err == nil {
		t.Fatal("expected writeMultiBuffer to reject oversized payload")
	}
}

func TestSendStreamDataSplitsLargePayload(t *testing.T) {
	var wire bytes.Buffer
	s := &session{
		bw:      buf.NewBufferedWriter(buf.NewWriter(&wire)),
		streams: make(map[uint32]*stream),
	}
	s.fw = newFrameWriter(s.bw)
	s.paddingScheme, _ = parsePaddingScheme("stop=0\n0=30-30")

	payload := make([]byte, 2*maxFramePayload+123)
	for i := range payload {
		payload[i] = byte(i)
	}
	if err := s.sendStreamData(9, buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		t.Fatal(err)
	}
	frames := parseTestFrames(t, wire.Bytes())
	if len(frames) != 3 {
		t.Fatalf("frame count = %d, want 3", len(frames))
	}
	var got bytes.Buffer
	for _, frame := range frames {
		if frame.cmd != cmdPSH || frame.sid != 9 {
			t.Fatalf("unexpected frame: cmd=%d sid=%d", frame.cmd, frame.sid)
		}
		got.Write(frame.data)
	}
	if !bytes.Equal(got.Bytes(), payload) {
		t.Fatal("large payload was not reconstructed exactly")
	}
}
