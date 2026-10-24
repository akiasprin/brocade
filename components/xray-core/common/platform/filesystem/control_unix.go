//go:build unix

package filesystem

import (
	"os"

	"golang.org/x/sys/unix"
)

// OpenControlFile opens a private operational checkpoint without following links or waiting on Unix special files.
// The caller must check that the returned descriptor is a regular file and bound its reads.
func OpenControlFile(path string) (*os.File, error) {
	return os.OpenFile(path, os.O_RDONLY|unix.O_NONBLOCK|unix.O_NOFOLLOW, 0)
}
