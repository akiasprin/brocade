// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"errors"
	"net/netip"
)

func validateLeases(records map[string]*record) error {
	type addressKey struct {
		domain  string
		address netip.Addr
	}
	used := map[addressKey]string{}
	for owner, r := range records {
		if r.Deleted {
			continue
		}
		for _, a := range r.NICs {
			if a.Removed {
				continue
			}
			for _, raw := range []string{a.IPv4, a.IPv6} {
				if raw == "" {
					continue
				}
				prefix, err := netip.ParsePrefix(raw)
				if err != nil {
					return err
				}
				domain := "local"
				if a.Allocated != nil {
					domain = a.Allocated.ResourceDomain
				}
				key := addressKey{domain, prefix.Addr()}
				if previous := used[key]; previous != "" && previous != owner+":"+a.ID {
					return errors.New("Cube attachment address reservations conflict")
				}
				used[key] = owner + ":" + a.ID
			}
		}
	}
	return nil
}
