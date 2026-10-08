//go:build linux

package anytls

import (
	"io"
	"net"
	"runtime"
	"sync"
	"syscall"
	"unsafe"

	"github.com/xtls/xray-core/common/buf"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sys/unix"
)

const (
	maxWritevVectors                     = 1024
	initialKernelTLSWritevVectorCapacity = 48
	maxPooledKernelTLSWritevVectors      = 256
)

type kernelTLSWritevScratch struct {
	headers     [maxPSHBatchFrameCount][frameHeaderSize]byte
	vectors     [][]byte
	iovecs      []unix.Iovec
	vectorsUsed int
	iovecsUsed  int
	pending     [][]byte
	written     int
	opErr       error
	syscalls    int
	callback    func(uintptr) bool
}

func newKernelTLSWritevScratch() any {
	scratch := &kernelTLSWritevScratch{
		vectors: make([][]byte, 0, initialKernelTLSWritevVectorCapacity),
		iovecs:  make([]unix.Iovec, 0, initialKernelTLSWritevVectorCapacity),
	}
	scratch.callback = scratch.writeReady
	return scratch
}

var kernelTLSWritevScratchPool = sync.Pool{New: newKernelTLSWritevScratch}

func (s *kernelTLSWritevScratch) reset() {
	if cap(s.vectors) > maxPooledKernelTLSWritevVectors {
		s.vectors = make([][]byte, 0, initialKernelTLSWritevVectorCapacity)
	} else {
		clear(s.vectors[:s.vectorsUsed])
		s.vectors = s.vectors[:0]
	}
	if cap(s.iovecs) > maxPooledKernelTLSWritevVectors {
		s.iovecs = make([]unix.Iovec, 0, initialKernelTLSWritevVectorCapacity)
	} else {
		clear(s.iovecs[:s.iovecsUsed])
		s.iovecs = s.iovecs[:0]
	}
	s.vectorsUsed = 0
	s.iovecsUsed = 0
	s.pending = nil
	s.written = 0
	s.opErr = nil
	s.syscalls = 0
}

func (s *kernelTLSWritevScratch) writeReady(fd uintptr) bool {
	var (
		written uintptr
		errno   syscall.Errno
	)
	for {
		s.syscalls++
		written, _, errno = unix.Syscall(
			unix.SYS_WRITEV,
			fd,
			uintptr(unsafe.Pointer(unsafe.SliceData(s.iovecs))),
			uintptr(len(s.iovecs)),
		)
		if errno != unix.EINTR {
			break
		}
	}
	runtime.KeepAlive(s.pending)
	s.written = int(written)
	if errno == 0 {
		s.opErr = nil
	} else {
		s.opErr = errno
	}
	if errno == unix.EAGAIN || errno == unix.EWOULDBLOCK {
		s.opErr = nil
		return false
	}
	return true
}

func (s *kernelTLSWritevScratch) prepare(vectors [][]byte) {
	s.pending = vectors
	s.iovecs = s.iovecs[:0]
	for _, vector := range vectors {
		if len(vector) == 0 {
			continue
		}
		iovec := unix.Iovec{Base: unsafe.SliceData(vector)}
		iovec.SetLen(len(vector))
		s.iovecs = append(s.iovecs, iovec)
	}
	s.iovecsUsed = max(s.iovecsUsed, len(s.iovecs))
}

type kernelTLSWritevResult struct {
	handled      bool
	payloadBytes int64
	wireBytes    int64
	batches      int64
	syscalls     int64
	err          error
}

// writePSHBatchKernelTLS preserves the AnyTLS wire format while avoiding the
// contiguous staging copy used by writePSHBatch. The payload already resides
// in userspace buffers, so writev can submit headers and bodies to software
// kTLS with one syscall per normal 128 KiB batch.
func writePSHBatchKernelTLS(conn net.Conn, sid uint32, data buf.MultiBuffer) kernelTLSWritevResult {
	raw, ok := kernelTLSWritevRawConn(conn)
	if !ok {
		return kernelTLSWritevResult{}
	}
	return writePSHBatchVectored(raw, sid, data)
}

func kernelTLSWritevAvailable(conn net.Conn) bool {
	_, ok := kernelTLSWritevRawConn(conn)
	return ok
}

func kernelTLSWritevRawConn(conn net.Conn) (syscall.RawConn, bool) {
	destination, ok := v2tls.KernelTLSRawConn(conn)
	if !ok {
		return nil, false
	}
	syscallConn, ok := destination.(syscall.Conn)
	if !ok {
		return nil, false
	}
	raw, err := syscallConn.SyscallConn()
	if err != nil {
		return nil, false
	}
	return raw, true
}

func writePSHBatchVectored(raw syscall.RawConn, sid uint32, data buf.MultiBuffer) kernelTLSWritevResult {
	result := kernelTLSWritevResult{handled: true}
	scratch := kernelTLSWritevScratchPool.Get().(*kernelTLSWritevScratch)
	defer func() {
		scratch.reset()
		kernelTLSWritevScratchPool.Put(scratch)
	}()
	totalPayload := int(data.Len())
	bufferIndex := 0
	bufferOffset := 0
	for totalPayload > 0 {
		batchPayload := min(totalPayload, int(maxPSHBatchPayloadSize))
		frameCount := (batchPayload + maxFramePayload - 1) / maxFramePayload
		headers := scratch.headers[:frameCount]
		vectors := scratch.vectors[:0]

		batchRemaining := batchPayload
		for frameIndex := 0; batchRemaining > 0; frameIndex++ {
			framePayload := min(batchRemaining, maxFramePayload)
			putFrameHeader(headers[frameIndex][:], cmdPSH, sid, framePayload)
			vectors = append(vectors, headers[frameIndex][:])

			frameRemaining := framePayload
			for frameRemaining > 0 {
				if bufferIndex >= len(data) {
					result.err = io.ErrUnexpectedEOF
					return result
				}
				payload := data[bufferIndex].Bytes()
				if bufferOffset >= len(payload) {
					bufferIndex++
					bufferOffset = 0
					continue
				}
				length := min(frameRemaining, len(payload)-bufferOffset)
				vectors = append(vectors, payload[bufferOffset:bufferOffset+length])
				bufferOffset += length
				frameRemaining -= length
			}
			batchRemaining -= framePayload
		}
		scratch.vectors = vectors
		scratch.vectorsUsed = max(scratch.vectorsUsed, len(vectors))

		written, calls, writeErr := writevFull(raw, vectors, scratch)
		result.wireBytes += int64(written)
		result.syscalls += int64(calls)
		if writeErr != nil {
			result.err = writeErr
			return result
		}
		result.payloadBytes += int64(batchPayload)
		result.batches++
		totalPayload -= batchPayload
	}
	return result
}

func writevFull(destination syscall.RawConn, vectors [][]byte, scratch *kernelTLSWritevScratch) (int, int, error) {
	writtenTotal := 0
	scratch.syscalls = 0
	for len(vectors) > 0 {
		batch := vectors
		if len(batch) > maxWritevVectors {
			batch = batch[:maxWritevVectors]
		}
		scratch.prepare(batch)
		scratch.written = 0
		scratch.opErr = nil
		err := destination.Write(scratch.callback)
		if err != nil {
			return writtenTotal, scratch.syscalls, err
		}
		if scratch.opErr != nil {
			return writtenTotal, scratch.syscalls, scratch.opErr
		}
		if scratch.written <= 0 {
			return writtenTotal, scratch.syscalls, io.ErrUnexpectedEOF
		}
		writtenTotal += scratch.written
		vectors = advanceWritevVectors(vectors, scratch.written)
	}
	scratch.pending = nil
	return writtenTotal, scratch.syscalls, nil
}

func advanceWritevVectors(vectors [][]byte, written int) [][]byte {
	for len(vectors) > 0 && written >= len(vectors[0]) {
		written -= len(vectors[0])
		vectors = vectors[1:]
	}
	if len(vectors) > 0 && written > 0 {
		vectors[0] = vectors[0][written:]
	}
	return vectors
}
