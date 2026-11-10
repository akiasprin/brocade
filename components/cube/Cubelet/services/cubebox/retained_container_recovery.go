// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubebox

import (
	"context"
	"fmt"
	"strings"

	containerd "github.com/containerd/containerd/v2/client"
	"github.com/containerd/errdefs"
	cubeboxstore "github.com/tencentcloud/CubeSandbox/Cubelet/pkg/store/cubebox"
)

func (l *local) loadContainerForCleanup(ctx context.Context, id string) (containerd.Container, error) {
	if id == "" {
		return nil, fmt.Errorf("missing container cleanup identity")
	}
	if l.loadContainerFn != nil {
		return l.loadContainerFn(ctx, id)
	}
	return l.client.LoadContainer(ctx, id)
}

func (l *local) recoverRetainedContainer(ctx context.Context, sb *cubeboxstore.CubeBox, id string) (bool, error) {
	if sb == nil || sb.SandboxID == "" || sb.Labels["brocade.cvm.instance"] != sb.SandboxID ||
		sb.Annotations["brocade.cvm.protected_root"] != "true" ||
		!strings.HasPrefix(sb.Annotations["brocade.cvm.expected_root"], "sb-"+sb.SandboxID+"-") {
		return false, nil
	}
	container, err := l.loadContainerForCleanup(ctx, id)
	if errdefs.IsNotFound(err) {
		return true, nil
	}
	if err != nil {
		return true, fmt.Errorf("load retained container metadata: %w", err)
	}
	info, err := container.Info(ctx)
	if err != nil {
		return true, fmt.Errorf("inspect retained container metadata: %w", err)
	}
	if container.ID() != id || info.ID != id || info.Labels["brocade.cvm.instance"] != sb.SandboxID {
		return true, fmt.Errorf("retained container ownership mismatch")
	}
	if err := l.waitReplacedSandboxGone(ctx, sb); err != nil {
		return true, err
	}
	if err := l.cleanContainerdContainer(ctx, container); err != nil {
		return true, err
	}
	return true, nil
}
