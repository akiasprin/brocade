//go:build linux

package anytls

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"syscall"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/transport/internet/stat"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sys/unix"
)

type framedDownlinkSplicer struct {
	session *session
	sid     uint32
	ready   chan struct{}
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
			n, spliceErr := unix.Splice(pipeReadFD, nil, int(fd), nil, remaining, unix.SPLICE_F_MOVE|unix.SPLICE_F_MORE)
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
