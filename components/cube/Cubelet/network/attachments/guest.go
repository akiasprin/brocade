// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import "github.com/tencentcloud/CubeSandbox/pkgs/nodeexec"

func (s *server) guest(runtime string, readJSON bool, args ...string) ([]byte, error) {
	return (&nodeexec.Client{Tasks: s.tasks, StateDir: s.root}).Exec(s.ctx, runtime, readJSON, args...)
}
