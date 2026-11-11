// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import "github.com/cilium/ebpf"

// PrimaryIPDeliveryReady verifies the loaded data-plane generation rather than
// treating a package version or static capability JSON as proof of support.
func PrimaryIPDeliveryReady() bool {
	m, err := loadPinnedMap("primary_delivery")
	if err != nil {
		return false
	}
	defer m.Close()
	info, err := m.Info()
	if err != nil || info.KeySize != 16 || info.ValueSize != 8 {
		return false
	}
	id, ok := info.ID()
	if !ok {
		return false
	}
	for _, name := range []string{programNameFromEnvoy, programNameFromCube} {
		p, err := ebpf.LoadPinnedProgram(pinPath(name), nil)
		if err != nil {
			return false
		}
		facts, err := p.Info()
		p.Close()
		if err != nil {
			return false
		}
		ids, ok := facts.MapIDs()
		if !ok {
			return false
		}
		found := false
		for _, used := range ids {
			if used == id {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}
