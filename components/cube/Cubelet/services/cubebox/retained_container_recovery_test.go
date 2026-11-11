// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubebox

import (
	"context"
	"errors"
	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	containerd "github.com/containerd/containerd/v2/client"
	"github.com/containerd/containerd/v2/core/containers"
	"github.com/containerd/containerd/v2/pkg/cio"
	"github.com/containerd/errdefs"
	"github.com/stretchr/testify/require"
	cubeboxstore "github.com/tencentcloud/CubeSandbox/Cubelet/pkg/store/cubebox"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/types/known/emptypb"
	"testing"
	"time"
)

type retainedTestContainer struct {
	containerd.Container
	owner     string
	deleted   bool
	deleteErr error
}

func (c *retainedTestContainer) ID() string { return "retained-recovery-test" }
func (c *retainedTestContainer) Info(context.Context, ...containerd.InfoOpts) (containers.Container, error) {
	return containers.Container{ID: c.ID(), Labels: map[string]string{"brocade.cvm.instance": c.owner}}, nil
}
func (c *retainedTestContainer) Task(context.Context, cio.Attach) (containerd.Task, error) {
	return nil, errdefs.ErrNotFound
}
func (c *retainedTestContainer) Image(context.Context) (containerd.Image, error) {
	return nil, errdefs.ErrNotFound
}
func (c *retainedTestContainer) Delete(context.Context, ...containerd.DeleteOpts) error {
	if c.deleteErr != nil {
		return c.deleteErr
	}
	c.deleted = true
	return nil
}

type retainedTestTasks struct{ tasks.TasksClient }

func (retainedTestTasks) Kill(context.Context, *tasks.KillRequest, ...grpc.CallOption) (*emptypb.Empty, error) {
	return nil, errdefs.ErrNotFound
}
func (retainedTestTasks) Delete(context.Context, *tasks.DeleteTaskRequest, ...grpc.CallOption) (*tasks.DeleteResponse, error) {
	return nil, errdefs.ErrNotFound
}

func TestRetainedContainerRecoveryRequiresOwnershipAbsenceAndMetadataDeletion(t *testing.T) {
	for _, scenario := range []string{"recover", "foreign", "live", "delete-failed", "unprotected", "metadata-missing"} {
		t.Run(scenario, func(t *testing.T) {
			c := &retainedTestContainer{owner: "retained-recovery-test"}
			sb := newCubeboxWithStatusForTest(c.ID(), cubeboxstore.Status{})
			sb.SandboxID = c.ID()
			sb.Labels = map[string]string{"brocade.cvm.instance": sb.SandboxID}
			sb.Annotations = map[string]string{"brocade.cvm.protected_root": "true", "brocade.cvm.expected_root": "sb-" + sb.SandboxID + "-root"}
			loadCalls := 0
			l := &local{localTask: retainedTestTasks{}, loadContainerFn: func(context.Context, string) (containerd.Container, error) {
				loadCalls++
				if scenario == "metadata-missing" {
					return nil, errdefs.ErrNotFound
				}
				return c, nil
			}}
			if scenario == "foreign" {
				c.owner = "another-instance"
			}
			if scenario == "unprotected" {
				delete(sb.Annotations, "brocade.cvm.protected_root")
			}
			if scenario == "delete-failed" {
				c.deleteErr = errors.New("metadata deletion unconfirmed")
			}
			if scenario == "live" {
				identity := startSleeper(t)
				sb.Endpoint.Pid = uint32(identity.Pid)
				sb.Endpoint.PidStartTime = identity.StartTime
			}
			ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
			defer cancel()
			recovered, err := l.recoverRetainedContainer(ctx, sb, c.ID())
			switch scenario {
			case "recover":
				require.NoError(t, err)
				require.True(t, recovered)
				require.True(t, c.deleted)
			case "metadata-missing":
				require.NoError(t, err)
				require.True(t, recovered)
				require.False(t, c.deleted)
			case "unprotected":
				require.NoError(t, err)
				require.False(t, recovered)
				require.Zero(t, loadCalls)
			default:
				require.Error(t, err)
				require.True(t, recovered)
				require.False(t, c.deleted)
			}
		})
	}
}
