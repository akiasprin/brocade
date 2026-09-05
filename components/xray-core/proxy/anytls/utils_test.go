package anytls

import (
	"bytes"
	"errors"
	"io"
	"testing"

	"github.com/xtls/xray-core/common/buf"
)

type shortWriter struct {
	max int
	buf bytes.Buffer
}

func (w *shortWriter) Write(p []byte) (int, error) {
	if len(p) > w.max {
		p = p[:w.max]
	}
	return w.buf.Write(p)
}

type zeroWriter struct{}

func (zeroWriter) Write([]byte) (int, error) { return 0, nil }

type failingWriter struct{ err error }

func (w failingWriter) Write([]byte) (int, error) { return 0, w.err }

func TestWriteFullHandlesShortAndBrokenWriters(t *testing.T) {
	payload := []byte("anytls-write-full")
	writer := &shortWriter{max: 2}
	if err := writeFull(writer, payload); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(writer.buf.Bytes(), payload) {
		t.Fatalf("written = %q, want %q", writer.buf.Bytes(), payload)
	}
	if err := writeFull(zeroWriter{}, payload); !errors.Is(err, io.ErrShortWrite) {
		t.Fatalf("zero writer error = %v, want io.ErrShortWrite", err)
	}
	wantErr := errors.New("write failed")
	if err := writeFull(failingWriter{err: wantErr}, payload); !errors.Is(err, wantErr) {
		t.Fatalf("failing writer error = %v, want %v", err, wantErr)
	}
}

func TestReadMultiBufferExact(t *testing.T) {
	payload := bytes.Repeat([]byte{0x2a}, 2*buf.Size+11)
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(payload))}
	mb, err := readMultiBufferExact(reader, len(payload))
	if err != nil {
		t.Fatal(err)
	}
	if got := multiBufferBytes(t, mb); !bytes.Equal(got, payload) {
		t.Fatal("readMultiBufferExact changed payload")
	}

	reader = &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(payload[:len(payload)-1]))}
	mb, err = readMultiBufferExact(reader, len(payload))
	if err == nil || mb != nil {
		t.Fatalf("truncated read result = (%v, %v), want error and nil buffer", mb, err)
	}
}

func TestReadMultiBufferExactReusesCompleteBuffers(t *testing.T) {
	first := buf.New()
	firstPayload := first.Extend(101)
	for i := range firstPayload {
		firstPayload[i] = byte(i)
	}
	second := buf.New()
	secondPayload := second.Extend(203)
	for i := range secondPayload {
		secondPayload[i] = byte(i + len(firstPayload))
	}
	trailing := buf.New()
	trailing.Extend(17)

	reader := &buf.BufferedReader{
		Reader: buf.NewReader(bytes.NewReader(nil)),
		Buffer: buf.MultiBuffer{first, second, trailing},
	}
	mb, err := readMultiBufferExact(reader, len(firstPayload)+len(secondPayload))
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(mb)
	defer buf.ReleaseMulti(reader.Buffer)

	if len(mb) != 2 || mb[0] != first || mb[1] != second {
		t.Fatal("complete input buffers were copied instead of transferred")
	}
	if len(reader.Buffer) != 1 || reader.Buffer[0] != trailing {
		t.Fatal("reader did not retain the bytes after the exact body")
	}
}

func TestDiscardBytesAndReadText(t *testing.T) {
	payload := []byte("discard-mehello-world")
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(payload))}
	if err := discardBytes(reader, len("discard-me")); err != nil {
		t.Fatal(err)
	}
	text, err := readText(reader, len("hello-world"))
	if err != nil || text != "hello-world" {
		t.Fatalf("readText = (%q, %v), want hello-world", text, err)
	}

	reader = &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader([]byte("short")))}
	if err := discardBytes(reader, 6); err == nil {
		t.Fatal("discardBytes unexpectedly accepted truncated input")
	}
	reader = &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader([]byte("short")))}
	if _, err := readText(reader, 6); err == nil {
		t.Fatal("readText unexpectedly accepted truncated input")
	}
}

func TestReadTextLargerThanDefaultBuffer(t *testing.T) {
	payload := bytes.Repeat([]byte("a"), 2*buf.Size+17)
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(payload))}
	text, err := readText(reader, len(payload))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal([]byte(text), payload) {
		t.Fatal("readText changed large payload")
	}

	reader = &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(payload[:len(payload)-1]))}
	if _, err := readText(reader, len(payload)); err == nil {
		t.Fatal("readText unexpectedly accepted truncated large input")
	}
}
