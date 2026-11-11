// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"
)

var ErrRuntimeUpgradeRequired = errors.New("running Cube shim requires a retained-root restart before NIC hotplug")

func (s *server) verifyRunningShim(r *record) error {
	if s.cfg.ShimSHA256 == "" {
		return nil
	}
	ctx, cancel := context.WithTimeout(s.ctx, 5*time.Second)
	defer cancel()
	path := filepath.Join(s.cfg.VMMRoot, r.Runtime, "chapi")
	conn, pid, err := trustedVMMPeer(ctx, path)
	if err != nil {
		return err
	}
	defer conn.Close()
	file, err := os.Open(fmt.Sprintf("/proc/%d/exe", pid))
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Size() > 256*1024*1024 {
		return errors.New("invalid running Cube shim executable")
	}
	h := sha256.New()
	if _, err := io.Copy(h, io.LimitReader(file, 256*1024*1024+1)); err != nil {
		return err
	}
	if hex.EncodeToString(h.Sum(nil)) != s.cfg.ShimSHA256 {
		return ErrRuntimeUpgradeRequired
	}
	return nil
}

// The host shim is an execution-stack component, independent of the Guest
// image and its CPU/memory choices. Only the private administrator catalogue
// selects its immutable version, never an attachment request.
func (m *Manager) verifyShim() error {
	if m.shimVersion == "" {
		return nil
	}
	if !safeID.MatchString(m.shimVersion) || len(m.shimSHA256) != 64 {
		return errors.New("invalid native Cube shim artifact identity")
	}
	path := filepath.Join("/data/cubelet/root/component_versions/cube-shim", m.shimVersion, "bin/containerd-shim-cube-rs")
	data, err := checkedFile(path, 256*1024*1024, false)
	if err != nil {
		return err
	}
	if hash(data) != m.shimSHA256 {
		return errors.New("native Cube shim artifact does not match its pinned digest")
	}
	return nil
}
