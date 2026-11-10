use crate::{files, Result};
use brocade_deployment::cvm::{InstanceDesired, InstanceReport, InterfaceObservation};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Record {
    pub(crate) instance: String,
    pub(crate) runtime: String,
    pub(crate) volume: String,
    pub(crate) launch_sha256: String,
    pub(crate) boot_sha256: String,
    pub(crate) spec_sha256: String,
    pub(crate) contact_id: String,
    pub(crate) native_shim_version: String,
    pub(crate) actual_vcpus: u64,
    pub(crate) actual_memory_bytes: u64,
    pub(crate) generation: u64,
    pub(crate) restart_epoch: u64,
    pub(crate) reinstall_epoch: u64,
    /// Old-root deletion is durable before boot completes, so recovery cannot erase the new disk.
    pub(crate) reinstall_reset_epoch: u64,
    pub(crate) transition: String,
    pub(crate) host_boot_id: String,
    pub(crate) attempted: bool,
    pub(crate) initialized: bool,
    pub(crate) deleted: bool,
    pub(crate) detached_interfaces: Vec<String>,
    pub(crate) objects: Vec<RootObject>,
    pub(crate) last_interfaces: Vec<InterfaceObservation>,
    pub(crate) report: Option<InstanceReport>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RootObject {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) role: String,
}

impl Record {
    pub(crate) fn save(&self, root: &Path) -> Result<()> {
        files::atomic_json(&root.join(format!("{}.json", self.instance)), self)
    }
}

pub(crate) fn load(root: &Path) -> Result<BTreeMap<String, Record>> {
    let mut records = BTreeMap::new();
    for entry in std::fs::read_dir(root).map_err(|_| "Cube execution records unavailable")? {
        let entry = entry.map_err(|_| "Cube execution record entry unavailable")?;
        if entry.path().extension().is_none_or(|e| e != "json") {
            continue;
        }
        if records.len() >= 8192 {
            return Err("Cube instance history limit reached".into());
        }
        let bytes = files::checked_file(&entry.path(), 32 * 1024, true)?;
        let mut record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid durable Cube record; refusing to recreate instances")?;
        if !brocade_deployment::cvm::valid_id(&record.instance)
            || entry.file_name() != std::ffi::OsString::from(format!("{}.json", record.instance))
        {
            return Err("Cube durable execution ownership is invalid".into());
        }
        // A saved receipt is historical evidence, never proof that a runtime survived restart.
        record.report = None;
        records.insert(record.instance.clone(), record);
    }
    Ok(records)
}

pub(crate) fn boot_digest(desired: &InstanceDesired) -> Result<String> {
    let mut value = serde_json::json!({"image":desired.spec.image,"resources":desired.spec.resources,"ssh_public_key":desired.spec.ssh_public_key});
    value.sort_all_objects();
    let json =
        serde_json::to_string(&value).map_err(|_| "Cube boot identity serialization failed")?;
    Ok(brocade_core::hash::sha256_hex(json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn boot_identity_preserves_network_edits_and_detects_changed_resources() {
        let mut desired: InstanceDesired = serde_json::from_value(serde_json::json!({
            "id":"cvm-current", "operation_id":"op-current", "generation":1,
            "spec_sha256":"a".repeat(64), "power":"running", "delete_data":false,
            "restart_epoch":0, "reinstall_epoch":0,
            "spec":{
                "name":"before", "ssh_public_key":"public-key",
                "image":{"id":"debian","name":"Debian","version":"13","architecture":"x86_64","sha256":"b".repeat(64),"boot_mode":"cube-oci"},
                "resources":{"vcpus":1,"memory_bytes":134217728,"root_disk_bytes":3221225472u64},
                "interfaces":[]
            }
        })).unwrap();
        let before = boot_digest(&desired).unwrap();
        desired.spec.name = "after".into();
        desired.spec.interfaces.push(
            serde_json::from_value(serde_json::json!({
                "id":"nic-business", "source_id":"private", "purpose":"business",
                "default_ipv4":true, "default_ipv6":false
            }))
            .unwrap(),
        );
        assert_eq!(boot_digest(&desired).unwrap(), before);
        desired.spec.resources.memory_bytes *= 2;
        assert_ne!(boot_digest(&desired).unwrap(), before);
    }

    #[test]
    fn current_checkpoint_preserves_disk_identity_without_trusting_a_saved_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let record = Record {
            instance: "cvm-current".into(),
            volume: "cube-original-root".into(),
            attempted: true,
            generation: 7,
            ..Default::default()
        };
        record.save(directory.path()).unwrap();
        let loaded = load(directory.path()).unwrap();
        let saved = &loaded["cvm-current"];
        assert_eq!(saved.volume, record.volume);
        assert!(saved.attempted);
        assert_eq!(saved.generation, 7);
        assert!(saved.report.is_none());
    }

    #[test]
    fn unsafe_or_misowned_records_are_never_silently_discarded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cvm-current.json");
        let record = Record {
            instance: "cvm-current".into(),
            ..Default::default()
        };
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(directory.path()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&path, directory.path().join("another-instance.json")).unwrap();
        assert!(load(directory.path()).is_err());
    }
}
