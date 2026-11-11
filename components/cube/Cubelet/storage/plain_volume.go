// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package storage

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"

	"golang.org/x/sys/unix"
)

// A committed receipt proves that formatting completed before any runtime was
// permitted to attach this volume. An existing unmarked disk is never formatted.
type plainInitialization struct {
	Format string `json:"format"`
	Name   string `json:"name"`
	Size   uint64 `json:"size"`
	Inode  uint64 `json:"inode"`
}

func plainFile(path, name string, size uint64) (*os.File, *syscall.Stat_t, error) {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || filepath.Base(path) != name || size == 0 || size > 1<<63-1 {
		return nil, nil, fmt.Errorf("invalid plain volume binding")
	}
	fd, err := unix.Open(path, unix.O_RDWR|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, nil, err
	}
	file := os.NewFile(uintptr(fd), path)
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, nil, err
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || !info.Mode().IsRegular() || stat.Uid != uint32(os.Geteuid()) || info.Mode().Perm()&0077 != 0 || uint64(info.Size()) != size || stat.Nlink != 1 {
		file.Close()
		return nil, nil, fmt.Errorf("plain volume ownership or physical identity mismatch")
	}
	return file, stat, nil
}

func commitPlainInitialization(path, name string, size uint64) error {
	file, stat, err := plainFile(path, name, size)
	if err != nil {
		return err
	}
	defer file.Close()
	// mkfs must not turn a promised physical allocation into sparse holes.
	if err := unix.Fallocate(int(file.Fd()), 0, 0, int64(size)); err != nil {
		return err
	}
	if err := file.Sync(); err != nil {
		return err
	}
	data, err := json.Marshal(plainInitialization{Format: "brocade-plain-initialized-v1", Name: name, Size: size, Inode: stat.Ino})
	if err != nil {
		return err
	}
	receipt := filepath.Join(filepath.Dir(path), ".initialized")
	temporary := receipt + ".pending"
	f, err := os.OpenFile(temporary, os.O_WRONLY|os.O_CREATE|os.O_EXCL|syscall.O_NOFOLLOW, 0600)
	if err != nil {
		return err
	}
	defer os.Remove(temporary)
	_, writeErr := f.Write(data)
	if writeErr == nil {
		writeErr = f.Sync()
	}
	closeErr := f.Close()
	if writeErr != nil || closeErr != nil {
		return errors.Join(writeErr, closeErr)
	}
	if err := unix.Renameat2(unix.AT_FDCWD, temporary, unix.AT_FDCWD, receipt, unix.RENAME_NOREPLACE); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

func checkPlainInitialization(path, name string, size uint64) error {
	file, stat, err := plainFile(path, name, size)
	if err != nil {
		return err
	}
	defer file.Close()
	if uint64(stat.Blocks)*512 < size {
		return fmt.Errorf("plain volume physical reservation is incomplete")
	}
	receipt := filepath.Join(filepath.Dir(path), ".initialized")
	f, err := os.OpenFile(receipt, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return fmt.Errorf("plain volume initialization is unconfirmed: %w", err)
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return err
	}
	owner, ok := info.Sys().(*syscall.Stat_t)
	if !ok || !info.Mode().IsRegular() || owner.Uid != uint32(os.Geteuid()) || info.Mode().Perm()&0077 != 0 || info.Size() > 1024 {
		return fmt.Errorf("unsafe plain volume initialization receipt")
	}
	data, err := io.ReadAll(io.LimitReader(f, 1025))
	if err != nil || len(data) > 1024 {
		return fmt.Errorf("invalid plain volume initialization receipt")
	}
	var r plainInitialization
	if json.Unmarshal(data, &r) != nil || r.Format != "brocade-plain-initialized-v1" || r.Name != name || r.Size != size || r.Inode != stat.Ino {
		return fmt.Errorf("plain volume initialization receipt does not match the original disk")
	}
	return nil
}
