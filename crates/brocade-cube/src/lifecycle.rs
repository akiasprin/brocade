use crate::{
    backend::{report, running, Inner},
    records::{boot_digest, Record},
    Result,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use brocade_deployment::cvm::*;
use serde_json::{json, Value};

impl Inner {
    pub(crate) fn storage_contract(&self) -> Result<()> {
        let response=self.cube_timeout("Update",json!({"requestID":"brocade-storage-contract","annotations":{"brocade.cvm.action":"storage-contract-v1"}}),std::time::Duration::from_secs(10))?;
        crate::successful(&response)?;
        let decode = |key: &str| {
            STANDARD
                .decode(crate::text(&response["ext_info"], key))
                .map_err(|_| "Cube storage contract encoding invalid")
        };
        if decode("brocade_storage_contract")? != b"protected-root-v1" {
            return Err("protected-root-v1 unavailable".into());
        }
        let backend = decode("brocade_storage_backend")?;
        if backend != self.cfg.storage_backend.as_bytes() {
            return Err("Cubelet storage backend does not match the Agent configuration".into());
        }
        Ok(())
    }

    fn protect_root(&self, record: &Record) -> Result<()> {
        let response=self.cube("Update",json!({"requestID":format!("brocade-protect-{}",record.instance),"sandboxID":record.runtime,"annotations":{"brocade.cvm.action":"protect-root-v1"}}))?;
        crate::successful(&response)
    }

    pub(crate) fn stop_runtime(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
        boxes: &[Value],
        reset: bool,
    ) -> Result<()> {
        if !boxes.is_empty() {
            self.runtime_owned(record)?;
            self.protect_root(record)?;
            if record.volume.is_empty() {
                record.volume = self.volume(record, desired)?;
            }
            let boot = host_boot_id()?;
            if running(&boxes[0]) && (record.host_boot_id.is_empty() || record.host_boot_id == boot)
            {
                self.guest_exec(
                    &record.runtime,
                    false,
                    &["/bin/sync"],
                    std::time::Duration::from_secs(20),
                )?;
            }
        }
        if record.volume.is_empty() || record.objects.len() != 1 {
            return Err("original Cube root identity unavailable".into());
        }
        if record.transition != "stop" && record.transition != "reset" {
            record.transition = if reset { "reset" } else { "stop" }.into();
            record.save(&self.root)?;
        }
        let annotations = if reset {
            json!({"brocade.cvm.delete_root":"true"})
        } else {
            json!({})
        };
        let response=self.cube("Destroy",json!({"requestID":format!("brocade-lifecycle-{}",record.instance),"sandboxID":record.runtime,"annotations":annotations}))?;
        crate::successful(&response)?;
        if !self.list(&record.instance)?.is_empty() {
            return Err("Cube runtime shutdown incomplete".into());
        }
        self.remove_nics(record, desired)?;
        if reset {
            self.prove_absent(record)?;
            record.volume.clear();
            record.objects.clear();
            record.reinstall_reset_epoch = desired.reinstall_epoch;
            record.transition = "fresh".into();
        } else {
            let volume = self.volume(record, desired)?;
            if volume != record.volume {
                return Err("retained Cube root identity changed".into());
            }
            record.transition = "boot".into();
        }
        record.initialized = false;
        record.save(&self.root)
    }

    pub(crate) fn reconcile_lifecycle(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
        boxes: &[Value],
    ) -> Result<Option<InstanceReport>> {
        self.storage_contract()?;
        let boot = host_boot_id()?;
        if !record.host_boot_id.is_empty()
            && record.host_boot_id != boot
            && record.transition.is_empty()
        {
            record.transition = "stop".into();
        }
        if boxes.len() == 1 {
            self.protect_root(record)?;
        }
        let reinstall = desired.reinstall_epoch > record.reinstall_epoch;
        let restart = desired.restart_epoch > record.restart_epoch;
        // "boot" also follows an ordinary shutdown and does not prove a reinstall cleared data.
        if reinstall && desired.reinstall_epoch > record.reinstall_reset_epoch {
            self.stop_runtime(record, desired, boxes, true)?;
        } else if record.attempted
            && (desired.power == PowerState::Stopped
                || record.transition == "stop"
                || record.transition == "reset"
                || (restart && record.transition.is_empty())
                || (boxes.len() == 1 && !running(&boxes[0]))
                || (boxes.is_empty() && record.transition.is_empty()))
        {
            self.stop_runtime(record, desired, boxes, record.transition == "reset")?;
        }
        record.host_boot_id = boot;
        record.save(&self.root)?;
        if desired.power == PowerState::Stopped {
            if !self.list(&record.instance)?.is_empty() || record.volume.is_empty() {
                return Err("Cube shutdown evidence incomplete".into());
            }
            // Persist prospective leases even when a NIC is attached while off.
            // Later detach can then release its precise instance-scoped lease.
            self.prepare_nics(record, desired)?;
            record.boot_sha256 = boot_digest(desired)?;
            record.restart_epoch = desired.restart_epoch;
            record.reinstall_epoch = desired.reinstall_epoch;
            record.save(&self.root)?;
            let mut out = report(desired, ApplyStatus::Applied, None);
            out.power = Some(PowerState::Stopped);
            out.volume_id = Some(record.volume.clone());
            out.volume_retained = true;
            out.network_absent = true;
            out.detached_interfaces = record.detached_interfaces.clone();
            return Ok(Some(out));
        }
        Ok(None)
    }
}

fn host_boot_id() -> Result<String> {
    let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "host boot identity unavailable")?;
    let id = value.trim();
    if id.len() != 36 {
        return Err("host boot identity invalid".into());
    }
    Ok(id.into())
}
