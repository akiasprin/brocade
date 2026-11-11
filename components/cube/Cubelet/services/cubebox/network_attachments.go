// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubebox

import (
	"context"
	"encoding/json"
	"errors"

	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/errdefs"
	"github.com/tencentcloud/CubeSandbox/Cubelet/network/attachments"
	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
	cb "github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
	ec "github.com/tencentcloud/CubeSandbox/pkgs/proto/services/errorcode/v1"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// networkAttachmentsUpdate is a versioned node-local desired-state contract.
// It uses the Cubelet Update envelope and requires the matching network contract.
// The payload is typed by pkgs/proto/networkattachments and has a strict bound.
func (s *service) networkAttachmentsUpdate(ctx context.Context, req *cb.UpdateCubeSandboxRequest) (*cb.UpdateCubeSandboxResponse, bool) {
	action := req.GetAnnotations()[wire.ActionKey]
	if action == "" {
		return nil, false
	}
	rsp := &cb.UpdateCubeSandboxResponse{RequestID: req.RequestID, Ret: &ec.Ret{RetCode: ec.ErrorCode_Success}, ExtInfo: map[string][]byte{}}
	fail := func(err error) (*cb.UpdateCubeSandboxResponse, bool) {
		rsp.Ret.RetCode = ec.ErrorCode_PreConditionFailed
		rsp.Ret.RetMsg = err.Error()
		return rsp, true
	}
	manager, err := attachments.NodeRuntime(s.cubeboxMgr.client.TaskService())
	if err != nil {
		return fail(err)
	}
	if action == "inventory" {
		catalogue, err := manager.Catalogue(ctx)
		data, marshalErr := json.Marshal(catalogue)
		if marshalErr != nil {
			return fail(marshalErr)
		}
		rsp.ExtInfo[wire.ResultKey] = data
		if err != nil {
			return fail(err)
		}
		return rsp, true
	}
	if action == "snapshot" {
		data, err := manager.Snapshot()
		if err != nil {
			return fail(err)
		}
		rsp.ExtInfo[wire.ResultKey] = data
		return rsp, true
	}
	if action == "quota_inventory" || action == "quota" {
		data, err := manager.Quota(ctx, []byte(req.GetAnnotations()[wire.PayloadKey]), action == "quota_inventory")
		if err != nil {
			return fail(err)
		}
		rsp.ExtInfo[wire.ResultKey] = data
		return rsp, true
	}
	if action != "reconcile" && action != "prepare" {
		return fail(errors.New("unsupported Cube network attachment action"))
	}
	payload := req.GetAnnotations()[wire.PayloadKey]
	if len(payload) == 0 || len(payload) > wire.MaxMessage {
		return fail(errors.New("Cube network attachment request exceeds bound"))
	}
	var desired wire.Request
	if err := json.Unmarshal([]byte(payload), &desired); err != nil {
		return fail(errors.New("invalid Cube network attachment request"))
	}
	if desired.RuntimeID != req.SandboxID {
		return fail(errors.New("Cube network runtime envelope does not match"))
	}
	unlock := s.sandboxLifecycleLocks.Lock(req.SandboxID)
	defer unlock()
	if action == "prepare" {
		sb, err := s.cubeboxMgr.cubeboxManger.Get(ctx, req.SandboxID)
		if err == nil && sb != nil && sb.Labels["brocade.cvm.instance"] != desired.InstanceID {
			return fail(errors.New("Cube reservation owner conflicts with existing runtime"))
		}
		evidence, err := manager.Prepare(ctx, desired)
		if err != nil {
			return fail(err)
		}
		data, err := json.Marshal(evidence)
		if err != nil {
			return fail(err)
		}
		rsp.ExtInfo[wire.ResultKey] = data
		return rsp, true
	}
	if desired.Release {
		// A release is physical cleanup after runtime termination, including retry
		// after Cubelet forgot its sandbox row. A live task forbids releasing leases.
		_, err := s.cubeboxMgr.client.TaskService().Get(namespaces.WithNamespace(ctx, "default"), &tasks.GetRequest{ContainerID: desired.RuntimeID})
		if !errdefs.IsNotFound(err) && status.Code(err) != codes.NotFound {
			return fail(errors.New("runtime absence has not been confirmed for network release"))
		}
	} else {
		sb, err := s.cubeboxMgr.cubeboxManger.Get(ctx, req.SandboxID)
		if err != nil || sb == nil || sb.Labels["brocade.cvm.instance"] != desired.InstanceID {
			return fail(errors.New("Cube network runtime ownership proof does not match"))
		}
	}
	evidence, err := manager.Reconcile(ctx, desired)
	if err != nil {
		if errors.Is(err, attachments.ErrRuntimeUpgradeRequired) {
			data, _ := json.Marshal(wire.Result{Contract: wire.Contract, RestartRequired: true})
			rsp.ExtInfo[wire.ResultKey] = data
		}
		return fail(err)
	}
	data, err := json.Marshal(evidence)
	if err != nil {
		return fail(err)
	}
	rsp.ExtInfo[wire.ResultKey] = data
	return rsp, true
}
