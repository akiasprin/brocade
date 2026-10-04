//go:build linux

package anytls

import (
	"io"
	"net"
	"syscall"

	"github.com/xtls/xray-core/common/buf"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sys/unix"
)

const maxWritevVectors = 1024

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
	totalPayload := int(data.Len())
	bufferIndex := 0
	bufferOffset := 0
	for totalPayload > 0 {
		batchPayload := min(totalPayload, int(maxPSHBatchPayloadSize))
		frameCount := (batchPayload + maxFramePayload - 1) / maxFramePayload
		headers := make([][frameHeaderSize]byte, frameCount)
		vectors := make([][]byte, 0, frameCount+len(data)-bufferIndex)

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

		written, calls, writeErr := writevFull(raw, vectors)
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

func writevFull(destination syscall.RawConn, vectors [][]byte) (int, int, error) {
	writtenTotal := 0
	syscalls := 0
	for len(vectors) > 0 {
		batch := vectors
		if len(batch) > maxWritevVectors {
			batch = batch[:maxWritevVectors]
		}
		var (
			written int
			opErr   error
		)
		err := destination.Write(func(fd uintptr) bool {
			syscalls++
			written, opErr = unix.Writev(int(fd), batch)
			if opErr == unix.EAGAIN || opErr == unix.EWOULDBLOCK {
				opErr = nil
				return false
			}
			return true
		})
		if err != nil {
			return writtenTotal, syscalls, err
		}
		if opErr != nil {
			return writtenTotal, syscalls, opErr
		}
		if written <= 0 {
			return writtenTotal, syscalls, io.ErrUnexpectedEOF
		}
		writtenTotal += written
		vectors = advanceWritevVectors(vectors, written)
	}
	return writtenTotal, syscalls, nil
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
