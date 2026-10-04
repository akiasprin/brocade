//go:build linux

package anytls

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"syscall"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/transport/internet/stat"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sys/unix"
)

type framedDownlinkSplicer struct {
	session *session
	sid     uint32
	ready   chan struct{}
	// useSplice selects the zero-copy socket -> pipe -> kTLS socket path.
	// Otherwise the hook bypasses the transport pipe but reads the source into
	// pooled buffers and submits each complete AnyTLS batch with writev.
	useSplice bool
}

func (s *framedDownlinkSplicer) SpliceDownlink(ctx context.Context, source net.Conn, onBytes func(int64)) (bool, error) {
	if s == nil || s.session == nil || source == nil {
		return false, nil
	}
	if s.ready != nil {
		select {
		case <-s.ready:
		case <-ctx.Done():
			return true, ctx.Err()
		}
	}
	if !s.session.unpaddedFastPathAvailable() {
		return false, nil
	}
	if !s.useSplice {
		return s.writevDownlink(ctx, source, onBytes)
	}
	destination, ok := v2tls.KernelTLSRawConn(s.session.conn)
	if !ok {
		return false, nil
	}
	sourceSyscall, ok := source.(syscall.Conn)
	if !ok {
		return false, nil
	}
	destinationSyscall, ok := destination.(syscall.Conn)
	if !ok {
		return false, nil
	}
	sourceRaw, err := sourceSyscall.SyscallConn()
	if err != nil {
		return false, nil
	}
	destinationRaw, err := destinationSyscall.SyscallConn()
	if err != nil {
		return false, nil
	}

	pipeFDs := [2]int{-1, -1}
	if err := unix.Pipe2(pipeFDs[:], unix.O_CLOEXEC|unix.O_NONBLOCK); err != nil {
		return false, nil
	}
	defer unix.Close(pipeFDs[0])
	defer unix.Close(pipeFDs[1])
	_, _ = unix.FcntlInt(uintptr(pipeFDs[1]), unix.F_SETPIPE_SZ, maxFramePayload+1)
	var performance *performanceStats
	if s.session.server != nil {
		performance = s.session.server.performance
	}
	performance.recordSpliceConnection()

	for {
		length, readErr := spliceSocketToPipe(sourceRaw, pipeFDs[1], maxFramePayload)
		if length == 0 {
			if readErr == nil || errors.Cause(readErr) == io.EOF {
				return true, nil
			}
			performance.recordSpliceError()
			return true, readErr
		}

		if err := s.writeSplicedFrame(ctx, destinationRaw, pipeFDs[0], length); err != nil {
			performance.recordSpliceError()
			s.session.close(err)
			return true, err
		}
		if counterConn, ok := s.session.conn.(*stat.CounterConnection); ok && counterConn.WriteCounter != nil {
			counterConn.WriteCounter.Add(int64(length))
		}
		if onBytes != nil {
			onBytes(int64(length))
		}
		performance.recordSpliceBytes(int64(length))
		if readErr != nil {
			if errors.Cause(readErr) == io.EOF {
				return true, nil
			}
			performance.recordSpliceError()
			return true, readErr
		}
	}
}

func (s *framedDownlinkSplicer) writevDownlink(ctx context.Context, source net.Conn, onBytes func(int64)) (bool, error) {
	// The framed fast-path contract only permits a fallback before consuming
	// source bytes. Validate kTLS and syscall.Conn before the first read.
	if !kernelTLSWritevAvailable(s.session.conn) {
		return false, nil
	}

	for {
		select {
		case <-ctx.Done():
			return true, ctx.Err()
		default:
		}

		payload := buf.NewWithSize(maxPSHBatchPayloadSize)
		readBuffer := payload.ExtendUninitialized(maxPSHBatchPayloadSize)
		length, readErr := source.Read(readBuffer)
		payload.Resize(0, int32(length))

		if length > 0 {
			s.session.writeMu.Lock()
			if s.session.isClosed() {
				s.session.writeMu.Unlock()
				payload.Release()
				return true, errSessionClosed
			}
			flushErr := s.session.fw.flush()
			var handled bool
			var writeErr error
			if flushErr == nil {
				handled, writeErr = s.session.writeKernelTLSVectoredLocked(s.sid, buf.MultiBuffer{payload})
			}
			s.session.writeMu.Unlock()
			payload.Release()

			if flushErr != nil {
				s.session.close(flushErr)
				return true, flushErr
			}
			if !handled {
				writeErr = errors.New("anytls: kTLS writev became unavailable after consuming source data")
			}
			if writeErr != nil {
				s.session.close(writeErr)
				return true, writeErr
			}
			if onBytes != nil {
				onBytes(int64(length))
			}
		} else {
			payload.Release()
		}

		if readErr != nil {
			if errors.Cause(readErr) == io.EOF {
				return true, nil
			}
			return true, readErr
		}
		if length == 0 {
			return true, io.ErrNoProgress
		}
	}
}

func (s *framedDownlinkSplicer) writeSplicedFrame(ctx context.Context, destination syscall.RawConn, pipeReadFD, length int) error {
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
	}

	var header [frameHeaderSize]byte
	header[0] = cmdPSH
	binary.BigEndian.PutUint32(header[1:5], s.sid)
	binary.BigEndian.PutUint16(header[5:7], uint16(length))

	s.session.writeMu.Lock()
	defer s.session.writeMu.Unlock()
	if s.session.isClosed() {
		return errSessionClosed
	}
	if err := s.session.fw.flush(); err != nil {
		return err
	}
	if err := writeFull(s.session.conn, header[:]); err != nil {
		return err
	}
	if err := splicePipeToSocket(destination, pipeReadFD, length); err != nil {
		return fmt.Errorf("anytls: splice PSH body: %w", err)
	}
	return nil
}

func spliceSocketToPipe(source syscall.RawConn, pipeWriteFD, limit int) (int, error) {
	var (
		length int
		opErr  error
	)
	err := source.Read(func(fd uintptr) bool {
		n, spliceErr := unix.Splice(int(fd), nil, pipeWriteFD, nil, limit, unix.SPLICE_F_MOVE|unix.SPLICE_F_MORE)
		if spliceErr == unix.EAGAIN || spliceErr == unix.EWOULDBLOCK {
			return false
		}
		length = int(n)
		opErr = spliceErr
		return true
	})
	if err != nil {
		return length, err
	}
	return length, opErr
}

func splicePipeToSocket(destination syscall.RawConn, pipeReadFD, length int) error {
	remaining := length
	for remaining > 0 {
		var (
			written int
			opErr   error
		)
		err := destination.Write(func(fd uintptr) bool {
			// This call completes the AnyTLS frame body. SPLICE_F_MORE maps to
			// MSG_MORE on the socket side and may leave a short kTLS record
			// corked while the origin waits for the peer's next request. That is
			// especially visible for a TLS ServerHello: both sides then wait for
			// each other until the application handshake deadline. A partial
			// splice is retried below, so every successful call may terminate the
			// current TLS record and must not advertise an unknown future write.
			n, spliceErr := unix.Splice(pipeReadFD, nil, int(fd), nil, remaining, unix.SPLICE_F_MOVE)
			if spliceErr == unix.EAGAIN || spliceErr == unix.EWOULDBLOCK {
				return false
			}
			written = int(n)
			opErr = spliceErr
			return true
		})
		if err != nil {
			return err
		}
		if opErr != nil {
			return opErr
		}
		if written == 0 {
			return io.ErrUnexpectedEOF
		}
		remaining -= written
	}
	return nil
}
