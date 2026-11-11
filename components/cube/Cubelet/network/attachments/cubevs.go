// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import "errors"

func (s *server) ensureGuard(r *record, a *attachment, d object) error {
	if a.Allocated == nil {
		return errors.New("NIC guard requires a Store allocation")
	}
	if !a.GuardIntent {
		a.GuardIntent = true
		if err := s.persist(r); err != nil {
			return err
		}
	}
	return s.ensureAssignedGuard(a)
}
func (s *server) removeGuard(r *record, a *attachment) error {
	if a.Allocated == nil {
		return errors.New("NIC cleanup requires Store ownership evidence")
	}
	if !a.GuardIntent && a.Ifindex == 0 {
		return nil
	}
	return s.removeAssignedGuard(a)
}
