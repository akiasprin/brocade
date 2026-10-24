//go:build !unix

package filesystem

import (
	"errors"
	"os"
)

// OpenControlFile opens a private operational checkpoint without following links or waiting on Unix special files.
// The caller must check that the returned descriptor is a regular file and bound its reads.
func OpenControlFile(path string) (*os.File, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("control must be a regular file")
	}
	return os.Open(path)
}
