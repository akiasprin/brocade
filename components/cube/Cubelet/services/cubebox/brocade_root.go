// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubebox

import (
	"context"
	"github.com/tencentcloud/CubeSandbox/Cubelet/storage"
	"github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
	"github.com/tencentcloud/CubeSandbox/pkgs/proto/services/errorcode/v1"
)

// This extension is node-local; Brocade still owns lifecycle intentions and
// retries. It changes storage retention, not Cube's snapshot/paused semantics.
func (s *service) brocadeRootUpdate(ctx context.Context, req *cubebox.UpdateCubeSandboxRequest) (*cubebox.UpdateCubeSandboxResponse, bool) {
	action := req.GetAnnotations()["brocade.cvm.action"]
	if action != "storage-contract-v1" && action != "protect-root-v1" {
		return nil, false
	}
	rsp := &cubebox.UpdateCubeSandboxResponse{RequestID: req.RequestID, Ret: &errorcode.Ret{RetCode: errorcode.ErrorCode_Success}, ExtInfo: map[string][]byte{"brocade_storage_contract": []byte("protected-root-v1")}}
	if action == "storage-contract-v1" {
		rsp.ExtInfo["brocade_storage_backend"] = []byte(storage.LocalVolumeBackend())
		return rsp, true
	}
	unlock := s.sandboxLifecycleLocks.Lock(req.SandboxID)
	defer unlock()
	sb, err := s.cubeboxMgr.cubeboxManger.Get(ctx, req.SandboxID)
	if err != nil || sb == nil || sb.Labels["brocade.cvm.instance"] != req.SandboxID {
		rsp.Ret.RetCode = errorcode.ErrorCode_PreConditionFailed
		rsp.Ret.RetMsg = "Brocade runtime ownership mismatch"
		return rsp, true
	}
	if err := storage.ProtectBrocadeRoot(ctx, req.SandboxID); err != nil {
		rsp.Ret.RetCode = errorcode.ErrorCode_PreConditionFailed
		rsp.Ret.RetMsg = "Brocade root protection failed"
	}
	return rsp, true
}
