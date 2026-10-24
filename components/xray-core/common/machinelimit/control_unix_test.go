//go:build unix

package machinelimit

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func TestMachineLimitRejectsFIFOWithoutBlockingControlReload(t *testing.T) {
	path := filepath.Join(t.TempDir(), "control.json")
	if err := unix.Mkfifo(path, 0600); err != nil {
		t.Fatal(err)
	}
	defer func() {
		// Unblock the old implementation on a failed regression run, without leaving a worker.
		fd, err := unix.Open(path, unix.O_RDWR|unix.O_NONBLOCK, 0)
		if err == nil {
			_ = unix.Close(fd)
		}
	}()
	done := make(chan error, 1)
	go func() { done <- New(path).Wait(context.Background(), 1) }()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("a FIFO is not a valid private control file")
		}
	case <-time.After(time.Second):
		t.Fatal("FIFO blocked control reload and all data-plane controllers")
	}
}
