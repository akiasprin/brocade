// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"syscall"
)

const maxMessage = 128 * 1024

var safeID = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9_.-]{0,95}$`)
var safeRuntime = regexp.MustCompile(`^[A-Za-z0-9_-]{1,150}$`)

func str(o object, k string) string { s, _ := o[k].(string); return s }

type object = map[string]any

// Capabilities and desired specifications stay JSON documents defined by
// brocade-deployment::cvm; there is no second copy of their domain types.
func obj(o object, k string) object  { m, _ := o[k].(map[string]any); return m }
func array(o object, k string) []any { a, _ := o[k].([]any); return a }
func number(o object, k string) uint64 {
	switch value := o[k].(type) {
	case json.Number:
		n, err := strconv.ParseUint(string(value), 10, 64)
		if err == nil {
			return n
		}
	case uint64:
		return value
	case uint32:
		return uint64(value)
	case int:
		if value >= 0 {
			return uint64(value)
		}
	case float64:
		if value >= 0 && value < 18446744073709551616.0 && value == float64(uint64(value)) {
			return uint64(value)
		}
	}
	return 0
}
func hash(b []byte) string { v := sha256.Sum256(b); return hex.EncodeToString(v[:]) }
func checkedFile(path string, limit int64, private bool) ([]byte, error) {
	st, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	sys, ok := st.Sys().(*syscall.Stat_t)
	if !st.Mode().IsRegular() || !ok || sys.Uid != uint32(os.Geteuid()) || st.Mode().Perm()&0022 != 0 || (private && st.Mode().Perm()&0077 != 0) || st.Size() > limit {
		return nil, errors.New("file ownership, mode or size is unsafe")
	}
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil || int64(len(b)) > limit {
		return nil, errors.New("file read exceeds limit")
	}
	return b, nil
}
func privateDir(path string) error {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return errors.New("state directory must be an absolute clean path")
	}
	if err := os.MkdirAll(path, 0700); err != nil {
		return err
	}
	st, err := os.Lstat(path)
	if err != nil {
		return err
	}
	sys, ok := st.Sys().(*syscall.Stat_t)
	if !st.IsDir() || st.Mode()&os.ModeSymlink != 0 || !ok || sys.Uid != uint32(os.Geteuid()) || st.Mode().Perm()&0077 != 0 {
		return errors.New("state directory must be private and owned by the service user")
	}
	return nil
}
func atomicJSON(path string, value any) error {
	b, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if len(b) > 32*1024 {
		return errors.New("execution record exceeds limit")
	}
	f, err := os.CreateTemp(filepath.Dir(path), ".cvm-*")
	if err != nil {
		return err
	}
	name := f.Name()
	defer os.Remove(name)
	if err = f.Chmod(0600); err == nil {
		_, err = f.Write(b)
	}
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(name, path); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
