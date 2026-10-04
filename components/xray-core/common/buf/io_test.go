package buf_test

import (
	"crypto/tls"
	"io"
	"sync/atomic"
	"testing"
	"time"

	. "github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/testing/servers/tcp"
)

type timeoutReadResult struct {
	buffer MultiBuffer
	err    error
}

type controlledTimeoutReader struct {
	calls   atomic.Int64
	results chan timeoutReadResult
}

func (r *controlledTimeoutReader) ReadMultiBuffer() (MultiBuffer, error) {
	r.calls.Add(1)
	result := <-r.results
	return result.buffer, result.err
}

type timeoutTestCounter struct {
	value atomic.Int64
}

func (c *timeoutTestCounter) Value() int64          { return c.value.Load() }
func (c *timeoutTestCounter) Set(value int64) int64 { return c.value.Swap(value) }
func (c *timeoutTestCounter) Add(value int64) int64 { return c.value.Add(value) }

func TestTimeoutWrapperReaderReportsTimeoutAndPreservesPendingRead(t *testing.T) {
	underlying := &controlledTimeoutReader{results: make(chan timeoutReadResult, 1)}
	counter := new(timeoutTestCounter)
	reader := &TimeoutWrapperReader{Reader: underlying, Counter: counter}

	buffer, err := reader.ReadMultiBufferTimeout(10 * time.Millisecond)
	if err != ErrReadTimeout {
		t.Fatalf("ReadMultiBufferTimeout() error = %v, want %v", err, ErrReadTimeout)
	}
	if buffer != nil {
		t.Fatalf("ReadMultiBufferTimeout() buffer = %v, want nil", buffer)
	}
	if calls := underlying.calls.Load(); calls != 1 {
		t.Fatalf("underlying reads = %d, want 1", calls)
	}

	underlying.results <- timeoutReadResult{buffer: MultiBuffer{FromBytes([]byte("pending"))}}
	buffer, err = reader.ReadMultiBuffer()
	if err != nil {
		t.Fatalf("ReadMultiBuffer() error = %v", err)
	}
	defer ReleaseMulti(buffer)
	if got := buffer.String(); got != "pending" {
		t.Fatalf("ReadMultiBuffer() = %q, want pending", got)
	}
	if got := counter.Value(); got != int64(len("pending")) {
		t.Fatalf("counter = %d, want %d", got, len("pending"))
	}
}

func TestTimeoutWrapperReaderReusesPendingReadAcrossTimeouts(t *testing.T) {
	underlying := &controlledTimeoutReader{results: make(chan timeoutReadResult, 1)}
	reader := &TimeoutWrapperReader{Reader: underlying}

	for attempt := 0; attempt < 2; attempt++ {
		buffer, err := reader.ReadMultiBufferTimeout(time.Millisecond)
		if err != ErrReadTimeout {
			t.Fatalf("attempt %d error = %v, want %v", attempt, err, ErrReadTimeout)
		}
		if buffer != nil {
			t.Fatalf("attempt %d buffer = %v, want nil", attempt, buffer)
		}
	}
	if calls := underlying.calls.Load(); calls != 1 {
		t.Fatalf("underlying reads = %d, want one pending read", calls)
	}

	underlying.results <- timeoutReadResult{err: io.EOF}
	buffer, err := reader.ReadMultiBuffer()
	if buffer != nil || err != io.EOF {
		t.Fatalf("ReadMultiBuffer() = (%v, %v), want (nil, EOF)", buffer, err)
	}
}

type immediateTimeoutReader struct{}

func (immediateTimeoutReader) ReadMultiBuffer() (MultiBuffer, error) {
	return nil, io.EOF
}

func BenchmarkTimeoutWrapperReaderImmediateRead(b *testing.B) {
	b.Run("current", func(b *testing.B) {
		for b.Loop() {
			reader := &TimeoutWrapperReader{Reader: immediateTimeoutReader{}}
			_, _ = reader.ReadMultiBufferTimeout(time.Millisecond)
		}
	})
	b.Run("legacy-timer-goroutine", func(b *testing.B) {
		for b.Loop() {
			_, _ = legacyReadMultiBufferTimeout(immediateTimeoutReader{}, time.Millisecond)
		}
		b.StopTimer()
		time.Sleep(2 * time.Millisecond)
	})
}

func legacyReadMultiBufferTimeout(reader Reader, duration time.Duration) (MultiBuffer, error) {
	var mb MultiBuffer
	var err error
	done := make(chan struct{})
	go func() {
		mb, err = reader.ReadMultiBuffer()
		close(done)
	}()
	timeout := make(chan struct{})
	go func() {
		time.Sleep(duration)
		close(timeout)
	}()
	select {
	case <-done:
		return mb, err
	case <-timeout:
		return nil, nil
	}
}

func TestWriterCreation(t *testing.T) {
	tcpServer := tcp.Server{}
	dest, err := tcpServer.Start()
	if err != nil {
		t.Fatal("failed to start tcp server: ", err)
	}
	defer tcpServer.Close()

	conn, err := net.Dial("tcp", dest.NetAddr())
	if err != nil {
		t.Fatal("failed to dial a TCP connection: ", err)
	}
	defer conn.Close()

	{
		writer := NewWriter(conn)
		if _, ok := writer.(*BufferToBytesWriter); !ok {
			t.Fatal("writer is not a BufferToBytesWriter")
		}

		writer2 := NewWriter(writer.(io.Writer))
		if writer2 != writer {
			t.Fatal("writer is not reused")
		}
	}

	tlsConn := tls.Client(conn, &tls.Config{
		InsecureSkipVerify: true,
	})
	defer tlsConn.Close()

	{
		writer := NewWriter(tlsConn)
		if _, ok := writer.(*SequentialWriter); !ok {
			t.Fatal("writer is not a SequentialWriter")
		}
	}
}
