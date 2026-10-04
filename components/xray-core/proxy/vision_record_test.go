package proxy

import (
	"testing"

	"github.com/xtls/xray-core/common/buf"
)

func TestIsCompleteRecordAcrossBufferBoundaries(t *testing.T) {
	record := []byte{0x17, 0x03, 0x03, 0x00, 0x04, 1, 2, 3, 4}
	for split := 0; split <= len(record); split++ {
		buffers := buf.MultiBuffer{
			buf.FromBytes(record[:split]),
			buf.FromBytes(record[split:]),
		}
		if !IsCompleteRecord(buffers) {
			t.Fatalf("record split at %d was reported incomplete", split)
		}
	}
}

func TestIsCompleteRecordRejectsMalformedOrPartialRecords(t *testing.T) {
	tests := []struct {
		name string
		data []byte
	}{
		{name: "partial header", data: []byte{0x17, 0x03, 0x03, 0x00}},
		{name: "wrong content type", data: []byte{0x16, 0x03, 0x03, 0x00, 0x01, 1}},
		{name: "wrong version", data: []byte{0x17, 0x03, 0x02, 0x00, 0x01, 1}},
		{name: "partial payload", data: []byte{0x17, 0x03, 0x03, 0x00, 0x02, 1}},
		{name: "zero payload", data: []byte{0x17, 0x03, 0x03, 0x00, 0x00}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if IsCompleteRecord(buf.MultiBuffer{buf.FromBytes(test.data)}) {
				t.Fatal("malformed record was reported complete")
			}
		})
	}
}

func TestIsCompleteRecordAcceptsMultipleRecords(t *testing.T) {
	data := []byte{
		0x17, 0x03, 0x03, 0x00, 0x02, 1, 2,
		0x17, 0x03, 0x03, 0x00, 0x03, 3, 4, 5,
	}
	if !IsCompleteRecord(buf.MultiBuffer{buf.FromBytes(data[:6]), buf.FromBytes(data[6:10]), buf.FromBytes(data[10:])}) {
		t.Fatal("complete record sequence was reported incomplete")
	}
}

func TestIsCompleteRecordDoesNotAllocate(t *testing.T) {
	data := []byte{0x17, 0x03, 0x03, 0x00, 0x04, 1, 2, 3, 4}
	buffers := buf.MultiBuffer{buf.FromBytes(data[:3]), buf.FromBytes(data[3:])}
	if allocations := testing.AllocsPerRun(1000, func() {
		if !IsCompleteRecord(buffers) {
			t.Fatal("complete record was reported incomplete")
		}
	}); allocations != 0 {
		t.Fatalf("IsCompleteRecord allocations = %v, want 0", allocations)
	}
}
