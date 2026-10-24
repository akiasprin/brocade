//go:build linux

package proxy

import (
	"context"
	"github.com/xtls/xray-core/common/machinelimit"
	"io"
	"net"

	"golang.org/x/sys/unix"
)

// copyAccountedSplice follows the socket -> nonblocking pipe -> socket polling pattern used by
// Go's net splice implementation, but reports each completed pump immediately. TCPConn.ReadFrom
// only returns a total at EOF or a size limit; keeping that total on a goroutine's stack loses
// unreported traffic when a managed process exits, even if its counters themselves are durable.
// handled=false is only possible before source bytes have been consumed, so buffered fallback is safe.
func copyAccountedSplice(ctx context.Context, reader net.Conn, writer *net.TCPConn, account func(int64)) (handled bool, err error) {
	source, ok := reader.(*net.TCPConn)
	if !ok {
		return false, nil
	}
	src, err := source.SyscallConn()
	if err != nil {
		return false, err
	}
	dst, err := writer.SyscallConn()
	if err != nil {
		return false, err
	}
	var pipe [2]int
	if err := unix.Pipe2(pipe[:], unix.O_NONBLOCK|unix.O_CLOEXEC); err != nil {
		return false, err
	}
	defer unix.Close(pipe[0])
	defer unix.Close(pipe[1])
	// Match Go's preferred pipe capacity; permissions/quotas may leave a smaller usable pipe.
	_, _ = unix.FcntlInt(uintptr(pipe[1]), unix.F_SETPIPE_SZ, 1<<20)
	controller := machinelimit.FromContext(ctx)
	for {
		chunk, limitErr := controller.ChunkSize()
		if limitErr != nil {
			return handled, limitErr
		}
		var available int64
		var readErr error
		err := src.Read(func(fd uintptr) bool {
			for {
				available, readErr = unix.Splice(int(fd), nil, pipe[1], nil, chunk, unix.SPLICE_F_NONBLOCK)
				if readErr == unix.EINTR {
					continue
				}
				return readErr != unix.EAGAIN
			}
		})
		if err != nil {
			return true, err
		}
		if readErr != nil {
			if !handled && readErr == unix.EINVAL {
				return false, nil
			}
			return true, readErr
		}
		handled = true
		if available == 0 {
			return true, nil
		}
		if err := controller.Wait(ctx, int(available)); err != nil {
			return true, err
		}
		for available > 0 {
			var writeErr error
			err := dst.Write(func(fd uintptr) bool {
				for {
					written, opErr := unix.Splice(pipe[0], nil, int(fd), nil, int(available), unix.SPLICE_F_NONBLOCK)
					if opErr == unix.EINTR {
						continue
					}
					if written > 0 {
						// Commit inside the poll callback, before waiting for more socket data.
						account(written)
						available -= written
					}
					writeErr = opErr
					if written == 0 && opErr == nil {
						writeErr = io.ErrNoProgress
					}
					return opErr != unix.EAGAIN
				}
			})
			if err != nil {
				return true, err
			}
			if writeErr != nil {
				return true, writeErr
			}
		}
	}
}
