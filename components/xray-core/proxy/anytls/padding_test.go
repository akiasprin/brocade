package anytls

import (
	"bytes"
	"fmt"
	"strings"
	"testing"

	"github.com/xtls/xray-core/common/buf"
)

func TestPaddingSchemeParsing(t *testing.T) {
	tests := []struct {
		name      string
		raw       string
		wantStop  uint32
		wantMD5   string
		wantError bool
	}{
		{
			name:     "default",
			raw:      string(defaultPaddingScheme),
			wantStop: 8,
		},
		{
			name:     "whitespace-and-comments-are-not-special",
			raw:      " stop = 2\n 0 = 30-30 \n1=9-9,c\n",
			wantStop: 2,
		},
		{name: "empty", wantError: true},
		{name: "missing-stop", raw: "0=30-30", wantError: true},
		{name: "malformed-line", raw: "stop=2\n0", wantError: true},
		{name: "empty-key", raw: "stop=2\n=30-30", wantError: true},
		{name: "empty-value", raw: "stop=2\n0=", wantError: true},
		{name: "duplicate-key", raw: "stop=2\n0=30-30\n0=40-40", wantError: true},
		{name: "duplicate-stop", raw: "stop=2\nstop=3", wantError: true},
		{name: "invalid-stop", raw: "stop=02", wantError: true},
		{name: "invalid-packet-key", raw: "stop=2\na=30-30", wantError: true},
		{name: "invalid-range", raw: "stop=2\n0=0-30", wantError: true},
		{name: "reversed-range", raw: "stop=2\n0=30-20", wantError: true},
		{name: "oversized-range", raw: fmt.Sprintf("stop=2\n1=1-%d", maxPaddingTargetSize+1), wantError: true},
		{name: "packet-zero-maximum", raw: fmt.Sprintf("stop=1\n0=%d-%d", maxFramePayload, maxFramePayload), wantStop: 1},
		{name: "packet-zero-over-uint16", raw: fmt.Sprintf("stop=1\n0=1-%d", maxFramePayload+1), wantError: true},
		{name: "later-packet-over-uint16", raw: fmt.Sprintf("stop=2\n1=%d-%d", maxFramePayload+1, maxFramePayload+1), wantStop: 2},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parsePaddingScheme(tt.raw)
			if tt.wantError {
				if err == nil {
					t.Fatal("expected parsing error")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if got.stop != tt.wantStop {
				t.Fatalf("stop = %d, want %d", got.stop, tt.wantStop)
			}
			if tt.wantMD5 != "" && got.md5 != tt.wantMD5 {
				t.Fatalf("md5 = %q, want %q", got.md5, tt.wantMD5)
			}
		})
	}
}

func TestPaddingSchemeRejectsOversizedText(t *testing.T) {
	raw := strings.Repeat("0=1-1\n", maxPaddingSchemeSize/4)
	if len(raw) <= maxPaddingSchemeSize {
		raw += strings.Repeat("x", maxPaddingSchemeSize-len(raw)+1)
	}
	if _, err := newPaddingScheme([]byte(raw)); err == nil {
		t.Fatal("expected oversized padding scheme error")
	}
}

func TestGenerateRecordPayloadSizes(t *testing.T) {
	scheme, err := parsePaddingScheme("stop=4\n0=30-30\n1=100-100,c,200-200\n2=7-7")
	if err != nil {
		t.Fatal(err)
	}

	if got := scheme.GenerateRecordPayloadSizes(0); len(got) != 1 || got[0] != 30 {
		t.Fatalf("packet 0 sizes = %v, want [30]", got)
	}
	if got := scheme.GenerateRecordPayloadSizes(1); len(got) != 3 || got[0] != 100 || got[1] != CheckMark || got[2] != 200 {
		t.Fatalf("packet 1 sizes = %v, want [100, CheckMark, 200]", got)
	}
	if got := scheme.GenerateRecordPayloadSizes(2); len(got) != 1 || got[0] != 7 {
		t.Fatalf("packet 2 sizes = %v, want [7]", got)
	}
	if got := scheme.GenerateRecordPayloadSizes(99); len(got) != 0 {
		t.Fatalf("unknown packet sizes = %v, want empty", got)
	}
	if got := (*paddingScheme)(nil).GenerateRecordPayloadSizes(0); got != nil {
		t.Fatalf("nil scheme sizes = %v, want nil", got)
	}
}

func TestSessionPacketPaddingStopsAtConfiguredLimit(t *testing.T) {
	scheme, err := parsePaddingScheme("stop=3\n1=64-64\n2=96-96")
	if err != nil {
		t.Fatal(err)
	}
	s := &session{paddingScheme: scheme}
	s.pktCounter.Store(1)

	if got, enabled := s.nextPacketIndex(); got != 1 || !enabled {
		t.Fatalf("first packet = (%d, %v), want (1, true)", got, enabled)
	}
	if got, enabled := s.nextPacketIndex(); got != 2 || !enabled {
		t.Fatalf("second packet = (%d, %v), want (2, true)", got, enabled)
	}
	if got, enabled := s.nextPacketIndex(); got != 0 || enabled {
		t.Fatalf("packet after stop = (%d, %v), want (0, false)", got, enabled)
	}
	if got, enabled := s.nextPacketIndex(); got != 0 || enabled {
		t.Fatalf("later packet = (%d, %v), want (0, false)", got, enabled)
	}
}

func TestSessionWithoutPaddingDoesNotUsePacketZeroRule(t *testing.T) {
	scheme, err := parsePaddingScheme("stop=1\n0=64-64")
	if err != nil {
		t.Fatal(err)
	}
	s := &session{paddingScheme: scheme}
	s.pktCounter.Store(1)

	if got, enabled := s.nextPacketIndex(); got != 0 || enabled {
		t.Fatalf("packet after stop = (%d, %v), want (0, false)", got, enabled)
	}
}

func TestPaddingSizeAndWasteFrameBoundaries(t *testing.T) {
	if got := getPadding0Size(nil); got != 30 {
		t.Fatalf("nil padding size = %d, want 30", got)
	}
	scheme, err := parsePaddingScheme("stop=1\n0=64-64")
	if err != nil {
		t.Fatal(err)
	}
	if got := getPadding0Size(scheme); got != 64 {
		t.Fatalf("padding size = %d, want 64", got)
	}
	scheme.scheme["0"] = "65536-65536"
	if got := getPadding0Size(scheme); got != 30 {
		t.Fatalf("defensive oversized packet 0 fallback = %d, want 30", got)
	}

	for _, total := range []int{0, 1, 6, maxPaddingTargetSize + 1} {
		var wire bytes.Buffer
		writer := buf.NewBufferedWriter(buf.NewWriter(&wire))
		if err := (&session{fw: newFrameWriter(writer)}).writeWasteFrames(total); err == nil {
			t.Fatalf("writeWasteFrames(%d) unexpectedly succeeded", total)
		}
	}
}
