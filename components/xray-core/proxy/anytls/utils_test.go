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
