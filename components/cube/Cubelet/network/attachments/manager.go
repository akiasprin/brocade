// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"sync"
	"time"

	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	"github.com/tencentcloud/CubeSandbox/CubeNet/cubevs"
	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
)

// Config is owned by the node administrator. The caller may select only an
// advertised source; an RPC cannot supply a bridge, gateway, pool or host path.
type Config struct {
	StateDir      string `json:"state_dir"`
	VMMRoot       string `json:"vmm_root"`
	MaxInterfaces uint16 `json:"max_interfaces"`
	ShimVersion   string `json:"shim_version,omitempty"`
	ShimSHA256    string `json:"shim_sha256,omitempty"`
}
type config struct {
	VMMRoot      string
	ShimSHA256   string
	Capabilities object
}
type record struct {
	Instance          string                 `json:"instance"`
	Runtime           string                 `json:"runtime"`
	Generation        uint64                 `json:"generation"`
	SpecSHA256        string                 `json:"spec_sha256"`
	ContactID         string                 `json:"contact_id"`
	ContactMAC        string                 `json:"contact_mac"`
	Deleted           bool                   `json:"deleted"`
	Releasing         bool                   `json:"releasing,omitempty"`
	ReleaseForget     bool                   `json:"release_forget,omitempty"`
	ReleaseSpecSHA256 string                 `json:"release_spec_sha256,omitempty"`
	NICs              map[string]*attachment `json:"nics"`
}
type server struct {
	ctx     context.Context
	cfg     config
	root    string
	tasks   tasks.TasksClient
	mu      sync.Mutex
	records map[string]*record
}
type Manager struct {
	operation     sync.Mutex
	server        *server
	maxInterfaces uint16
	shimVersion   string
	shimSHA256    string
}

var nodeManager struct {
	sync.Mutex
	manager *Manager
}

// NodeRuntime loads the private administrator catalogue once. A failed load
// is retryable; a valid live catalogue stays frozen until process restart.
func NodeRuntime(tasks tasks.TasksClient) (*Manager, error) {
	nodeManager.Lock()
	defer nodeManager.Unlock()
	if nodeManager.manager != nil {
		return nodeManager.manager, nil
	}
	cfg := Config{StateDir: "/data/cubelet/network-attachments-v2", VMMRoot: "/run/vc/vm", MaxInterfaces: 3}
	path := os.Getenv("CUBE_NETWORK_ATTACHMENTS_CONFIG")
	if path != "" {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path {
			return nil, errors.New("Cube network configuration requires an absolute clean path")
		}
		data, err := checkedFile(path, maxMessage, true)
		if err != nil {
			return nil, err
		}
		if err := json.Unmarshal(data, &cfg); err != nil {
			return nil, errors.New("invalid Cube network attachment configuration")
		}
	}
	manager, err := New(cfg, tasks)
	if err != nil {
		return nil, err
	}
	nodeManager.manager = manager
	return manager, nil
}

func New(cfg Config, tasks tasks.TasksClient) (*Manager, error) {
	if cfg.MaxInterfaces < 1 || cfg.MaxInterfaces > 3 {
		return nil, errors.New("unsupported Cube attachment capacity")
	}
	if err := privateDir(cfg.StateDir); err != nil {
		return nil, err
	}
	if !filepath.IsAbs(cfg.VMMRoot) || filepath.Clean(cfg.VMMRoot) != cfg.VMMRoot {
		return nil, errors.New("invalid Cube VMM root")
	}
	s := &server{root: cfg.StateDir, tasks: tasks, ctx: context.Background(), cfg: config{VMMRoot: cfg.VMMRoot, ShimSHA256: cfg.ShimSHA256}, records: map[string]*record{}}
	entries, err := os.ReadDir(s.root)
	if err != nil {
		return nil, err
	}
	for _, entry := range entries {
		if filepath.Ext(entry.Name()) != ".json" {
			continue
		}
		if len(s.records) >= 8192 {
			return nil, errors.New("Cube attachment state capacity exceeded")
		}
		data, err := checkedFile(filepath.Join(s.root, entry.Name()), 32*1024, true)
		if err != nil {
			return nil, err
		}
		var r record
		if err := json.Unmarshal(data, &r); err != nil {
			return nil, errors.New("invalid Cube attachment state")
		}
		if !safeID.MatchString(r.Instance) || entry.Name() != r.Instance+".json" || !safeRuntime.MatchString(r.Runtime) {
			return nil, errors.New("invalid Cube attachment state identity")
		}
		if err := validateSavedNICs(&r); err != nil {
			return nil, err
		}
		s.records[r.Instance] = &r
	}
	if err := validateLeases(s.records); err != nil {
		return nil, err
	}
	m := &Manager{server: s, maxInterfaces: cfg.MaxInterfaces, shimVersion: cfg.ShimVersion, shimSHA256: cfg.ShimSHA256}
	if err := m.restoreQuota(); err != nil {
		return nil, err
	}
	return m, nil
}

func (m *Manager) Catalogue(ctx context.Context) (wire.Catalogue, error) {
	result := wire.Catalogue{Contract: wire.Contract, MaxInterfaces: m.maxInterfaces, Sources: map[string]wire.Source{}, ManagedNICs: true, NetworkQuota: true}
	result.ShimVersion = m.shimVersion
	result.PrimaryIPDelivery = cubevs.PrimaryIPDeliveryReady()
	result.RouteServices = true
	if err := m.verifyShim(); err != nil {
		return result, err
	}
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	if _, err := exec.CommandContext(bounded, "nft", "-j", "list", "tables").Output(); err != nil {
		return result, errors.New("allocated NICs require usable nftables")
	}
	return result, nil
}

func (s *server) persist(r *record) error {
	return atomicJSON(filepath.Join(s.root, r.Instance+".json"), r)
}

// Reconcile is a desired-state operation. An interrupted operation retains its
// durable attachment leases. Cubelet checks runtime ownership before calling it.
func (m *Manager) Reconcile(ctx context.Context, req wire.Request) (wire.Result, error) {
	result := wire.Result{Contract: wire.Contract, Interfaces: []wire.Observation{}, Detached: []string{}}
	if req.Contract != wire.Contract || !safeID.MatchString(req.InstanceID) || !safeRuntime.MatchString(req.RuntimeID) || req.Generation == 0 || len(req.Interfaces) > 16 || (!req.Release && len(req.Interfaces) > int(m.maxInterfaces)) {
		return result, errors.New("invalid Cube network attachment request")
	}
	if !req.Release {
		if _, _, _, err := m.desired(req); err != nil {
			return result, err
		}
	}
	if !m.operation.TryLock() {
		return result, errors.New("Cube network reconciliation is busy; retry the same desired version")
	}
	defer m.operation.Unlock()
	s := m.server
	s.ctx = ctx
	r := s.records[req.InstanceID]
	if r != nil && req.Generation < r.Generation {
		return result, errors.New("stale Cube network attachment generation")
	}
	if r != nil && r.Runtime != req.RuntimeID && !req.Release {
		// Runtime identifiers may change only after a proven stopped release has
		// cleared all physical devices, while stable address allocations remain owned.
		for _, a := range r.NICs {
			if a.Ifindex != 0 {
				return result, errors.New("previous runtime network has not been released")
			}
		}
		r.Runtime = req.RuntimeID
		r.ContactMAC = ""
	}
	if r == nil {
		if len(s.records) >= 8192 {
			return result, errors.New("Cube attachment record capacity reached")
		}
		r = &record{Instance: req.InstanceID, Runtime: req.RuntimeID, NICs: map[string]*attachment{}}
		s.records[r.Instance] = r
	}
	if r.Deleted {
		if req.Release && req.Forget && req.Generation == r.Generation {
			result.NetworkAbsent = true
			return result, nil
		}
		return result, errors.New("deleted Cube attachment state cannot be replayed or retained again")
	}
	if r.Releasing && !req.Release {
		return result, errors.New("previous Cube network release is still pending")
	}
	if req.Release {
		// Pin the full stopped intent before any physical cleanup.
		wanted := map[string]bool{}
		for _, nic := range req.Interfaces {
			wanted[nic.ID] = true
		}
		detached := map[string]bool{}
		if len(req.Detach) > 16 {
			return result, errors.New("stopped detach assignment exceeds limit")
		}
		for _, id := range req.Detach {
			if !safeID.MatchString(id) || wanted[id] || detached[id] {
				return result, errors.New("invalid stopped detach assignment")
			}
			detached[id] = true
		}
		var desiredDigest string
		var releaseDesired object
		var releasePrimary string
		if !req.Forget {
			d, primary, digest, err := m.desired(req)
			if err != nil {
				return result, err
			}
			if r.ContactID != "" && r.ContactID != primary {
				return result, errors.New("stopped primary attachment identity changed")
			}
			releaseDesired = d
			releasePrimary = primary
			desiredDigest = digest
		}
		for id, nic := range r.NICs {
			if !wanted[id] && (!nic.Removed || nic.RemovedGeneration == req.Generation) {
				detached[id] = true
			}
		}
		if len(detached) > 16 {
			return result, errors.New("stopped detach evidence exceeds limit")
		}
		assigned := append([]string(nil), req.Detach...)
		sort.Strings(assigned)
		intent, err := json.Marshal(struct {
			Interfaces []wire.Interface
			Detach     []string
			Forget     bool
			SpecSHA256 string
		}{req.Interfaces, assigned, req.Forget, desiredDigest})
		if err != nil {
			return result, err
		}
		intentDigest := hash(intent)
		if r.Generation == req.Generation && r.ReleaseSpecSHA256 != "" && r.ReleaseSpecSHA256 != intentDigest {
			return result, errors.New("stopped network intent changed within one generation")
		}
		if r.Runtime != req.RuntimeID {
			return result, errors.New("Cube network release runtime identity mismatch")
		}
		if r.Releasing && r.Generation == req.Generation && r.ReleaseForget != req.Forget {
			return result, errors.New("Cube release intent changed within one generation")
		}
		r.Generation = req.Generation
		r.Releasing = true
		r.ReleaseForget = req.Forget
		r.ReleaseSpecSHA256 = intentDigest
		if r.ContactID == "" && releasePrimary != "" {
			r.ContactID = releasePrimary
		}
		if err := s.persist(r); err != nil {
			return result, err
		}
		if releaseDesired != nil {
			if err := s.prepareNICs(r, releaseDesired); err != nil {
				return result, err
			}
		}
		if err := s.removeAllNICs(r); err != nil {
			return result, err
		}
		for _, a := range r.NICs {
			a.Ifindex = 0
			if !wanted[a.ID] && !a.Removed {
				a.Removed = true
				a.RemovedGeneration = req.Generation
			}
		}
		if desiredDigest != "" {
			r.SpecSHA256 = desiredDigest
		}
		r.Deleted = req.Forget
		r.Releasing = false
		r.Generation = req.Generation
		if err := s.persist(r); err != nil {
			return result, err
		}
		result.NetworkAbsent = true
		for id := range detached {
			result.Detached = append(result.Detached, id)
		}
		sort.Strings(result.Detached)
		return result, nil
	}
	d, primaryID, digest, err := m.desired(req)
	if err != nil {
		return result, err
	}
	if r.ContactID != "" && r.ContactID != primaryID {
		return result, errors.New("primary attachment identity changed")
	}
	if r.Generation == req.Generation && r.SpecSHA256 != "" && r.SpecSHA256 != digest {
		return result, errors.New("Cube attachment desired state changed within one generation")
	}
	r.ContactID = primaryID
	if r.Generation != req.Generation {
		r.ReleaseSpecSHA256 = ""
	}
	r.Generation = req.Generation
	r.SpecSHA256 = digest
	if err := s.persist(r); err != nil {
		return result, err
	}
	observed, policy, detached, err := s.ensureNICs(r, d)
	if err != nil {
		return result, err
	}
	info, err := s.vmInfo(r)
	if err != nil {
		return result, err
	}
	out, _ := json.Marshal(object{"contract": wire.Contract, "interfaces": observed, "network": policy, "detached_interfaces": detached})
	if err := json.Unmarshal(out, &result); err != nil {
		return result, err
	}
	result.ActualVCPUs = info.Config.CPUs.Boot
	result.ActualMemoryBytes = info.Config.Memory.Size
	if err := s.persist(r); err != nil {
		return result, err
	}
	return result, nil
}

// Snapshot reports the actual frozen attachment bindings for local diagnostics.
func (m *Manager) Snapshot() ([]byte, error) {
	if !m.operation.TryLock() {
		return nil, errors.New("Cube network state is busy")
	}
	defer m.operation.Unlock()
	ids := make([]string, 0, len(m.server.records))
	for id := range m.server.records {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	rows := make([]*record, 0, len(ids))
	for _, id := range ids {
		rows = append(rows, m.server.records[id])
	}
	data, err := json.Marshal(rows)
	if err != nil {
		return nil, err
	}
	if len(data) > 2*1024*1024 {
		return nil, fmt.Errorf("Cube network observation exceeds bound")
	}
	return data, nil
}

// desired validates the complete interface list without allocating anything.
func (m *Manager) desired(req wire.Request) (object, string, string, error) {
	if req.Contract != wire.Contract || !safeID.MatchString(req.InstanceID) || !safeRuntime.MatchString(req.RuntimeID) || req.Generation == 0 || len(req.Interfaces) > int(m.maxInterfaces) {
		return nil, "", "", errors.New("invalid Cube attachment request")
	}
	raw, err := json.Marshal(req.Interfaces)
	if err != nil {
		return nil, "", "", err
	}
	var interfaces []any
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&interfaces); err != nil {
		return nil, "", "", err
	}
	primarySource, primaryID := "", ""
	defaults4, defaults6 := 0, 0
	for _, nic := range req.Interfaces {
		if nic.Purpose == "contact" {
			primarySource = nic.SourceID
			primaryID = nic.ID
		}
		if nic.DefaultIPv4 {
			defaults4++
		}
		if nic.DefaultIPv6 {
			defaults6++
		}
	}
	if defaults4 > 1 || defaults6 > 1 {
		return nil, "", "", errors.New("duplicate default attachment")
	}
	var routing object
	if len(req.Routing) > 0 {
		if err := json.Unmarshal(req.Routing, &routing); err != nil {
			return nil, "", "", errors.New("invalid container routing configuration")
		}
	}
	if routing == nil {
		routing = object{}
	}
	d := object{"generation": req.Generation, "spec": object{"interfaces": interfaces, "routing": routing}}
	temp := &server{cfg: config{Capabilities: object{"max_interfaces": uint64(m.maxInterfaces), "network_sources": []any{object{"id": primarySource, "kind": "public"}}}}}
	if _, err := temp.validateNICs(d); err != nil {
		return nil, "", "", err
	}
	identity, err := json.Marshal(object{"interfaces": req.Interfaces, "routing": routing})
	if err != nil {
		return nil, "", "", err
	}
	return d, primaryID, hash(identity), nil
}

// Prepare persists the Store assignment before Cubelet creates a runtime. It does not
// expose a device or run any Guest/VMM action. Failed creation keeps its leases
// until an explicit release proves runtime absence.
func (m *Manager) Prepare(ctx context.Context, req wire.Request) (wire.Result, error) {
	result := wire.Result{Contract: wire.Contract, ShimVersion: m.shimVersion}
	if err := m.verifyShim(); err != nil {
		return result, err
	}
	d, primary, digest, err := m.desired(req)
	if err != nil {
		return result, err
	}
	if !m.operation.TryLock() {
		return result, errors.New("Cube network reconciliation is busy")
	}
	defer m.operation.Unlock()
	s := m.server
	s.ctx = ctx
	r := s.records[req.InstanceID]
	if r == nil {
		if len(s.records) >= 8192 {
			return result, errors.New("Cube attachment record capacity reached")
		}
		r = &record{Instance: req.InstanceID, Runtime: req.RuntimeID, ContactID: primary, NICs: map[string]*attachment{}}
		s.records[r.Instance] = r
	}
	if r.Deleted || r.Releasing || r.Generation > req.Generation || (r.Generation == req.Generation && r.SpecSHA256 != "" && r.SpecSHA256 != digest) {
		return result, errors.New("Cube allocation execution identity conflict")
	}
	if r.ContactID != primary {
		return result, errors.New("Cube primary attachment identity conflict")
	}
	if r.Generation != req.Generation {
		r.ReleaseSpecSHA256 = ""
	}
	r.Generation = req.Generation
	r.SpecSHA256 = digest
	if err := s.prepareNICs(r, d); err != nil {
		return result, err
	}
	return result, s.persist(r)
}
