// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"fmt"
	"github.com/tencentcloud/CubeSandbox/pkgs/nodeexec"
)

func (s *server) guest(runtime string, readJSON bool, args ...string) ([]byte, error) {
	out, err := (&nodeexec.Client{Tasks: s.tasks, StateDir: s.root}).Exec(s.ctx, runtime, readJSON, args...)
	if err != nil {
		stage := "observation"
		if len(args) > 0 && args[0] == "/bin/sh" {
			stage = "route batch"
		}
		return out, fmt.Errorf("Guest %s: %w", stage, err)
	}
	return out, nil
}
