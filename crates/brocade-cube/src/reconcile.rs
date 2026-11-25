use crate::{
    backend::{report, running, valid_runtime, Inner},
    records::{boot_digest, Record},
    Result,
};
use brocade_deployment::cvm::*;
use serde_json::json;

impl Inner {
    pub(crate) fn reconcile(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<InstanceReport> {
        if record.instance != desired.id {
            return Err("本机执行记录归属不一致，拒绝执行".into());
        }
        let contact = match self.validate_nics(desired) {
            Ok(nic) => nic,
            Err(error) => return Ok(report(desired, ApplyStatus::Failed, Some(&error))),
        };
        if !record.contact_id.is_empty() && record.contact_id != contact.id {
            return Err("原生联络设备身份发生变化".into());
        }
        if !self.cfg.capabilities.stable_storage
            && !record.boot_sha256.is_empty()
            && record.boot_sha256 != boot_digest(desired)?
        {
            return Err("运行中系统、容量或访问身份不可替换".into());
        }
        if desired.power == PowerState::Running && desired.spec.login_revision == 0 {
            return Ok(report(
                desired,
                ApplyStatus::Failed,
                Some("缺少容器登录凭据版本，尚未创建运行时"),
            ));
        }
        if desired.power == PowerState::Stopped && !self.cfg.capabilities.stable_storage {
            return Ok(report(
                desired,
                ApplyStatus::Failed,
                Some("稳定根盘停止与恢复尚未验证"),
            ));
        }
        if record.deleted {
            return Ok(if desired.power == PowerState::Deleted {
                deleted_report(record, desired)
            } else {
                report(
                    desired,
                    ApplyStatus::Failed,
                    Some("已删除的运行身份不可重新创建"),
                )
            });
        }
        let mut boxes = self.list(&record.instance)?;
        if boxes.len() > 1 {
            return Err("发现重复运行身份，需要隔离核查".into());
        }
        if let Some(runtime) = boxes.first() {
            if crate::text(&runtime["labels"], "brocade.cvm.spec_sha256")
                != record.launch_sha256.as_str()
            {
                return Err("运行时标签与冻结规格不一致，拒绝接管".into());
            }
            let id = crate::text(runtime, "id");
            if !record.runtime.is_empty() && record.runtime != id {
                return Err("运行时身份变化，拒绝接管未知系统盘".into());
            }
            record.runtime = id.into();
            record.save(&self.root)?;
        }
        if self.cfg.capabilities.stable_storage && desired.power != PowerState::Deleted {
            if let Some(report) = self.reconcile_lifecycle(record, desired, &boxes)? {
                return Ok(report);
            }
            boxes = self.list(&record.instance)?;
        }
        if desired.power == PowerState::Deleted {
            if !desired.delete_data {
                if !self.cfg.capabilities.stable_storage {
                    return Ok(report(
                        desired,
                        ApplyStatus::Failed,
                        Some("该 Cube 后端不能保盘删除"),
                    ));
                }
                self.stop_runtime(record, desired, &boxes, false)?;
            } else {
                if record.attempted && record.runtime.is_empty() {
                    return Err("创建结果不明，缺少清理对象身份，预约保留".into());
                }
                if !record.runtime.is_empty() {
                    self.runtime_owned(record)?;
                    if record.objects.is_empty() {
                        self.capture_volumes(record)?;
                        record.save(&self.root)?;
                    }
                    let response=self.cube("Destroy",json!({"requestID":format!("brocade-delete-{}",record.instance),"sandboxID":record.runtime,"annotations":{"brocade.cvm.delete_root":"true"}}))?;
                    crate::successful(&response)?;
                    if !self.list(&record.instance)?.is_empty() {
                        return Err("运行时移除尚未确认".into());
                    }
                    self.prove_absent(record)?;
                }
                self.remove_nics(record, desired)?;
            }
            record.deleted = true;
            record.save(&self.root)?;
            return Ok(deleted_report(record, desired));
        }
        if desired.power != PowerState::Running {
            return Ok(report(
                desired,
                ApplyStatus::Failed,
                Some("不支持的电源期望"),
            ));
        }
        if boxes.is_empty() {
            if record.attempted && record.transition != "boot" && record.transition != "fresh" {
                return Err("原运行时不存在或创建结果不明，拒绝自动重建空盘".into());
            }
            let mut request = match self.cfg.create_request(desired) {
                Ok(req) => req,
                Err(error) => return Ok(report(desired, ApplyStatus::Failed, Some(&error))),
            };
            if self.cfg.capabilities.stable_storage {
                request["annotations"]["brocade.cvm.protected_root"] = json!("true");
                if !record.volume.is_empty() {
                    if record.objects.len() != 1 {
                        return Err("原盘对象身份不完整".into());
                    }
                    request["annotations"]["brocade.cvm.expected_root"] =
                        json!(record.objects[0].name);
                }
            }
            record.launch_sha256 = desired.spec_sha256.clone();
            record.boot_sha256 = boot_digest(desired)?;
            record.contact_id = contact.id.clone();
            self.prepare_nics(record, desired)?;
            request["annotations"]["cube.master.components.cube-shim.version"] =
                json!(record.native_shim_version);
            crate::artifacts::record_sandbox(record, desired, &request)?;
            record.attempted = true;
            record.save(&self.root)?;
            let response = self.cube("Create", request)?;
            crate::successful(&response)?;
            let runtime = crate::text(&response, "sandboxID");
            if !valid_runtime(runtime) {
                return Err("Cube 创建结果未确认，后续仅核查原身份".into());
            }
            record.runtime = runtime.into();
            record.save(&self.root)?;
        }
        if !valid_runtime(&record.runtime) {
            return Err("运行时身份无效".into());
        }
        let sandbox = desired
            .artifacts
            .as_ref()
            .ok_or("Cube artifacts missing")?
            .file(CvmArtifactKind::Sandbox)?;
        if record.sandbox_sha256 != sandbox.sha256
            || !valid_digest(&record.sandbox_effective_sha256)
        {
            return Err("当前运行时缺少匹配的沙箱产物回执，拒绝接管".into());
        }
        if !record.initialized || record.login_revision != desired.spec.login_revision {
            let guest =
                self.artifact_value(desired, CvmArtifactKind::GuestInit, &Default::default())?;
            if guest["contract"] != "brocade-guest-init-v2"
                || guest["user"] != "cube"
                || guest["credential_revision"] != desired.spec.login_revision
            {
                return Err("unsupported Guest password initialization contract".into());
            }
            let path = self
                .root
                .join("credentials")
                .join(format!("{}.json", desired.id));
            let bytes = crate::files::checked_file(&path, 4096, true)?;
            let credential: GuestCredential =
                serde_json::from_slice(&bytes).map_err(|_| "Guest credential is invalid")?;
            credential.validate()?;
            if credential.instance_id != desired.id
                || credential.revision != desired.spec.login_revision
            {
                return Err("Guest credential does not match approved specification".into());
            }
            self.guest_exec_input(
                &record.runtime,
                false,
                &[
                    "/bin/sh",
                    "-c",
                    brocade_cvm_compile::PASSWORD_INIT_SCRIPT,
                    "brocade-password-init",
                ],
                format!("{}\n", credential.password).as_bytes(),
                std::time::Duration::from_secs(20),
            )?;
            record.initialized = true;
            record.login_revision = credential.revision;
            record.save(&self.root)?;
        }
        let (interfaces, network, detached) = self.ensure_nics(record, desired)?;
        let volume = self.volume(record, desired)?;
        if !record.volume.is_empty() && record.volume != volume {
            return Err("原系统盘身份发生变化，需要核查".into());
        }
        record.volume = volume.clone();
        if self.cfg.capabilities.stable_storage
            && (record.actual_vcpus != u64::from(desired.spec.resources.vcpus)
                || record.actual_memory_bytes != desired.spec.resources.memory_bytes)
        {
            return Err("实际 CPU 或内存尚未匹配配置".into());
        }
        boxes = self.list(&record.instance)?;
        if boxes.len() != 1 || !running(&boxes[0]) {
            return Err("实际运行状态尚未确认".into());
        }
        record.transition.clear();
        record.restart_epoch = desired.restart_epoch;
        record.reinstall_epoch = desired.reinstall_epoch;
        record.save(&self.root)?;
        let address = self.access_address(&record.runtime)?;
        let mut out = report(desired, ApplyStatus::Applied, None);
        out.power = Some(PowerState::Running);
        out.runtime_id = Some(record.runtime.clone());
        out.volume_id = Some(volume);
        out.access_address = Some(address);
        out.interfaces = interfaces;
        out.network = Some(network);
        out.detached_interfaces = detached;
        out.validate_for(desired)?;
        Ok(out)
    }
}

fn deleted_report(record: &Record, desired: &InstanceDesired) -> InstanceReport {
    let mut out = report(desired, ApplyStatus::Applied, None);
    out.power = Some(PowerState::Deleted);
    out.network_absent = true;
    out.volume_deleted = desired.delete_data;
    out.volume_retained = !desired.delete_data;
    if !desired.delete_data && !record.volume.is_empty() {
        out.volume_id = Some(record.volume.clone());
    }
    out
}
