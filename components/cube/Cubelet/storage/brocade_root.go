// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package storage

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"syscall"

	"github.com/tencentcloud/CubeSandbox/Cubelet/pkg/utils"
	"github.com/tencentcloud/CubeSandbox/Cubelet/storage/cow"
	"k8s.io/apimachinery/pkg/api/resource"

	"github.com/tencentcloud/CubeSandbox/Cubelet/plugins/workflow"
	"github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
)

const brocadeProtectedRoot = "brocade.cvm.protected_root"
const brocadeExpectedRoot = "brocade.cvm.expected_root"
const brocadeDeleteRoot = "brocade.cvm.delete_root"

// LocalVolumeBackend is validated by the storage plugin before capabilities publish.
func LocalVolumeBackend() string { return localStorage.localBackend() }

// ProtectBrocadeRoot makes runtime cleanup independent of root-volume deletion.
// GC and failed cold boots retain the last writable volume until explicit deletion.

func ProtectBrocadeRoot(ctx context.Context, sandboxID string) error {
	info, err := localStorage.readBackendFileInfo(ctx, sandboxID)
	if err != nil {
		return err
	}
	if !cow.IsLocalBackend(info.Backend) {
		return fmt.Errorf("Brocade protected roots require local persistent volumes")
	}
	if len(info.Volumes) != 1 {
		return fmt.Errorf("Brocade requires exactly one root volume")
	}
	for _, v := range info.Volumes {
		if v == nil || v.Kind != "volume" || !strings.HasPrefix(v.VolumeName, "sb-"+sandboxID+"-") {
			return fmt.Errorf("root ownership mismatch")
		}
	}
	info.BrocadeProtectedRoot = true
	if err := localStorage.persistBrocadeRoot(info); err != nil {
		return err
	}
	return localStorage.writeBackendFileInfo(ctx, sandboxID, info)
}

func keepBrocadeRoot(info *StorageInfo, opts *workflow.DestroyContext) bool {
	return info != nil && info.BrocadeProtectedRoot && (opts == nil || opts.DestroyInfo.GetAnnotations()[brocadeDeleteRoot] != "true")
}

// Reuse only the exact retained object. A missing record or disk is an error,
// never permission to allocate an empty replacement with the same name.
func (l *local) reuseBrocadeRoot(ctx context.Context, opts *workflow.CreateContext) (bool, error) {
	expected := opts.ReqInfo.GetAnnotations()[brocadeExpectedRoot]
	if expected == "" {
		return false, nil
	}
	info, err := l.readBackendFileInfo(ctx, opts.SandboxID)
	if err != nil {
		if !errors.Is(err, utils.ErrorKeyNotFound) && !errors.Is(err, utils.ErrorBucketNotFound) {
			return true, fmt.Errorf("retained root unavailable: %w", err)
		}
		info, err = l.readBrocadeRoot(opts.SandboxID)
		if os.IsNotExist(err) {
			info, err = l.rebindBrocadeRoot(ctx, opts, expected)
		}
		if err != nil {
			return true, err
		}
		if err = l.refreshCowPaths(info); err != nil {
			return true, err
		}
		if err = l.writeBackendFileInfo(ctx, opts.SandboxID, info); err != nil {
			return true, err
		}
	}
	if !info.BrocadeProtectedRoot || len(info.Volumes) != 1 {
		return true, fmt.Errorf("retained root is not protected")
	}
	if storageBackendFromInfo(info) != createContextStorageBackend(opts) {
		return true, fmt.Errorf("retained root storage backend mismatch")
	}
	for name, v := range info.Volumes {
		if v == nil || v.VolumeName != expected || v.Kind != "volume" || !strings.HasPrefix(expected, "sb-"+opts.SandboxID+"-") {
			return true, fmt.Errorf("retained root identity mismatch")
		}
		matched := false
		for _, requested := range opts.ReqInfo.Volumes {
			if requested.Name == name && requested.GetVolumeSource().GetEmptyDir() != nil {
				matched = true
			}
		}
		if !matched {
			return true, fmt.Errorf("retained root mount mismatch")
		}
	}
	opts.StorageInfo = info
	if err := l.persistBrocadeRoot(info); err != nil {
		return true, err
	}
	return true, nil
}

func markBrocadeRoot(info *StorageInfo, request *cubebox.RunCubeSandboxRequest) {
	info.BrocadeProtectedRoot = request.GetAnnotations()[brocadeProtectedRoot] == "true"
}

var brocadeID = regexp.MustCompile(`^[a-zA-Z0-9_.-]{1,96}$`)

func (l *local) brocadeRootDir() string { return filepath.Join(l.config.DataPath, "brocade-roots") }

func validateBrocadeRoot(info *StorageInfo) error {
	if info == nil || !brocadeID.MatchString(info.SandboxID) || !info.BrocadeProtectedRoot || len(info.Volumes) != 1 || len(info.HostDirBackendInfos) > 0 || len(info.PluginVolumeBackendInfos) > 0 {
		return fmt.Errorf("invalid protected root manifest")
	}
	for name, v := range info.Volumes {
		if v == nil || !brocadeID.MatchString(name) || v.Name != name || v.Kind != "volume" || v.VolumeName != "sb-"+info.SandboxID+"-"+name {
			return fmt.Errorf("protected root ownership mismatch")
		}
	}
	return nil
}

// Cube's default storage DB lives in its tmpfs state directory. This small
// fsynced execution manifest is stored beside the actual persistent disks.
func (l *local) persistBrocadeRoot(info *StorageInfo) error {
	if !info.BrocadeProtectedRoot {
		return nil
	}
	if err := validateBrocadeRoot(info); err != nil {
		return err
	}
	dir := l.brocadeRootDir()
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	st, err := os.Lstat(dir)
	if err != nil {
		return err
	}
	owner, ok := st.Sys().(*syscall.Stat_t)
	if !ok || !st.IsDir() || owner.Uid != uint32(os.Geteuid()) || st.Mode().Perm()&0077 != 0 {
		return fmt.Errorf("unsafe root manifest directory")
	}
	b, err := json.Marshal(info)
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(dir, ".root-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err = f.Write(b); err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(f.Name(), filepath.Join(dir, info.SandboxID+".json")); err != nil {
		return err
	}
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer d.Close()
	return d.Sync()
}

func (l *local) readBrocadeRoot(id string) (*StorageInfo, error) {
	if !brocadeID.MatchString(id) {
		return nil, fmt.Errorf("invalid root identity")
	}
	path := filepath.Join(l.brocadeRootDir(), id+".json")
	st, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	owner, ok := st.Sys().(*syscall.Stat_t)
	if !ok || !st.Mode().IsRegular() || owner.Uid != uint32(os.Geteuid()) || st.Mode().Perm()&0077 != 0 || st.Size() > 128*1024 {
		return nil, fmt.Errorf("unsafe root manifest")
	}
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, 128*1024+1))
	if err != nil {
		return nil, err
	}
	var info StorageInfo
	if err = json.Unmarshal(b, &info); err != nil {
		return nil, err
	}
	if info.SandboxID != id {
		return nil, fmt.Errorf("root manifest identity mismatch")
	}
	return &info, validateBrocadeRoot(&info)
}

func (l *local) recoverBrocadeRoots(ctx context.Context) error {
	entries, err := os.ReadDir(l.brocadeRootDir())
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	if len(entries) > 8192 {
		return fmt.Errorf("protected root manifest limit exceeded")
	}
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		info, err := l.readBrocadeRoot(strings.TrimSuffix(entry.Name(), ".json"))
		if err != nil {
			return err
		}
		// Missing physical disks remain quarantined in their manifest. They
		// cannot be recreated, and must not stop unrelated healthy instances.
		if err = l.refreshCowPaths(info); err != nil {
			continue
		}
		if err = l.writeBackendFileInfo(ctx, info.SandboxID, info); err != nil {
			return err
		}
	}
	return nil
}

// Upgrade compatibility for an adapter that already froze the exact native
// volume identity before persistent manifests existed. Resolve only that
// existing object; this path never calls a volume-creation or format function.
func (l *local) rebindBrocadeRoot(ctx context.Context, opts *workflow.CreateContext, expected string) (*StorageInfo, error) {
	if len(opts.ReqInfo.Volumes) != 1 {
		return nil, fmt.Errorf("legacy root mount is ambiguous")
	}
	v := opts.ReqInfo.Volumes[0]
	empty := v.GetVolumeSource().GetEmptyDir()
	if empty == nil || expected != "sb-"+opts.SandboxID+"-"+v.Name {
		return nil, fmt.Errorf("legacy root ownership mismatch")
	}
	q, err := resource.ParseQuantity(empty.SizeLimit)
	if err != nil || q.Sign() <= 0 {
		return nil, fmt.Errorf("invalid retained capacity")
	}
	backend := createContextStorageBackend(opts)
	store, err := l.storeForBackend(backend)
	if err != nil {
		return nil, err
	}
	size, err := store.GetSizeBytes(ctx, expected)
	if err != nil || size != uint64(q.Value()) {
		return nil, fmt.Errorf("original physical root is missing or changed")
	}
	path, err := store.ResolveDevPath(ctx, expected, "volume")
	if err != nil {
		return nil, err
	}
	volume := newCowVolume(expected, "volume", 0, path)
	info := &StorageInfo{Namespace: "default", SandboxID: opts.SandboxID, Backend: backend, BrocadeProtectedRoot: true, Volumes: map[string]*BackendFileInfo{v.Name: newDefaultMediumBackendInfo(v.Name, path, q, normalizeRootfsSizes(q), volume)}}
	return info, l.persistBrocadeRoot(info)
}

func (l *local) removeBrocadeRoot(info *StorageInfo) error {
	if !info.BrocadeProtectedRoot {
		return nil
	}
	for _, v := range info.Volumes {
		if _, err := os.Stat(v.FilePath); !os.IsNotExist(err) {
			return fmt.Errorf("protected root removal is not confirmed")
		}
	}
	dir := l.brocadeRootDir()
	if err := os.Remove(filepath.Join(dir, info.SandboxID+".json")); err != nil && !os.IsNotExist(err) {
		return err
	}
	f, err := os.Open(dir)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	defer f.Close()
	return f.Sync()
}
