// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"

	"github.com/tencentcloud/CubeSandbox/CubeNet/cubevs"
)

type quotaRequest struct {
	Assignments []cubevs.QuotaAssignment `json:"assignments"`
	// Primary TAPs come from the trusted Rust client's Cubelet inventory.
	Endpoints []cubevs.QuotaEndpoint `json:"endpoints"`
}

func (m *Manager) Quota(ctx context.Context, raw []byte, inventory bool) ([]byte, error) {
	if len(raw) == 0 || len(raw) > 128*1024 {
		return nil, errors.New("quota request exceeds node-local bound")
	}
	var request quotaRequest
	if json.Unmarshal(raw, &request) != nil {
		return nil, errors.New("invalid node-local quota request")
	}
	physical, err := cubevs.EffectiveQuotaMemory()
	if err != nil {
		return nil, err
	}
	capacity, err := cubevs.QuotaCapacityForMemory(physical, 0)
	if err != nil {
		return nil, err
	}
	if inventory {
		if err := cubevs.PrepareQuota(capacity.Total); err != nil {
			return nil, err
		}
		return json.Marshal(map[string]any{"contract": "cubevs-attachments-v2", "capacity": capacity})
	}
	if !m.operation.TryLock() {
		return nil, errors.New("Cube network quota convergence is busy; retry same desired version")
	}
	defer m.operation.Unlock()
	if len(request.Assignments) > 512 || len(request.Endpoints) > 256 {
		return nil, errors.New("quota ownership inventory exceeds bound")
	}
	owners := map[string]bool{}
	for _, a := range request.Assignments {
		for _, id := range a.Instances {
			owners[id] = true
		}
	}
	for _, ep := range request.Endpoints {
		if !owners[ep.Instance] || !safeID.MatchString(ep.Instance) || ep.Interface != "contact" {
			return nil, errors.New("primary quota endpoint ownership mismatch")
		}
		link, err := net.InterfaceByIndex(int(ep.Index))
		if err != nil || !interfaceName(link.Name) {
			return nil, errors.New("quota primary TAP unavailable")
		}
	}
	for id, r := range m.server.records {
		if r.Deleted || !owners[id] {
			continue
		}
		for _, nic := range r.NICs {
			if !nic.Removed && nic.Ifindex > 0 {
				// A reboot removes TAP devices before desired-state recovery recreates
				// them. An absent device has no traffic to protect; an existing
				// device must still prove ownership before it can receive a quota.
				if _, err := os.Stat(filepath.Join("/sys/class/net", nic.Tap)); os.IsNotExist(err) {
					continue
				} else if err != nil {
					return nil, err
				}
				if err := checkTap(r, nic); err != nil {
					return nil, err
				}
				request.Endpoints = append(request.Endpoints, cubevs.QuotaEndpoint{Index: uint32(nic.Ifindex), Instance: id, Interface: nic.ID})
			}
		}
	}
	// Freeze authority before applying it: restart can restore assignment IDs
	// without retaining stale physical ifindices from a previous boot.
	saved := request
	saved.Endpoints = nil
	dir := filepath.Join(m.server.root, "quota")
	if err := privateDir(dir); err != nil {
		return nil, err
	}
	path := filepath.Join(dir, "desired.json")
	wanted, err := json.Marshal(saved)
	if err != nil {
		return nil, err
	}
	old, readErr := checkedFile(path, 128*1024, true)
	if readErr != nil && !os.IsNotExist(readErr) {
		return nil, readErr
	}
	var compact bytes.Buffer
	if readErr == nil && json.Compact(&compact, old) != nil {
		return nil, errors.New("invalid persisted quota authority")
	}
	if readErr != nil || !bytes.Equal(compact.Bytes(), wanted) {
		if err := atomicJSON(path, saved); err != nil {
			return nil, err
		}
	}
	reports, err := cubevs.ApplyQuota(capacity, request.Assignments, request.Endpoints)
	if err != nil {
		return nil, err
	}
	return json.Marshal(map[string]any{"contract": "cubevs-attachments-v2", "reports": reports})
}

func (m *Manager) restoreQuota() error {
	raw, err := checkedFile(filepath.Join(m.server.root, "quota", "desired.json"), 128*1024, true)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	var saved quotaRequest
	if json.Unmarshal(raw, &saved) != nil {
		return errors.New("invalid persisted quota authority")
	}
	physical, err := cubevs.EffectiveQuotaMemory()
	if err != nil {
		return err
	}
	capacity, err := cubevs.QuotaCapacityForMemory(physical, 0)
	if err != nil {
		return err
	}
	_, err = cubevs.ApplyQuota(capacity, saved.Assignments, nil)
	return err
}
