use crate::{
    backend::Inner,
    config::root_name,
    records::{Record, RootObject},
    Result,
};
use brocade_deployment::cvm::InstanceDesired;
use serde_json::{json, Value};
use std::{
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

const OWNERSHIP_UNKNOWN: &str = "Cube storage ownership is unknown";
const UNALLOCATED_CREATE_ABSENCE_SECS: u64 = 300;

impl Inner {
    fn storage(&self) -> Result<Value> {
        let response = self.cube(
            "InspectStorageVolumes",
            json!({"requestID":"brocade-inspect"}),
        )?;
        crate::successful(&response)?;
        Ok(response)
    }

    pub(crate) fn capture_volumes(&self, record: &mut Record) -> Result<()> {
        let response = self.storage()?;
        for item in crate::array(&response, "sandboxes") {
            if crate::text(item, "sandboxID") != record.runtime {
                continue;
            }
            let mut roots = vec![];
            for volume in crate::array(item, "volumes") {
                let name = crate::text(volume, "volume_name");
                let kind = crate::text(volume, "kind");
                if name.is_empty() || !matches!(kind, "volume" | "snapshot") {
                    return Err("Cube storage object identity incomplete".into());
                }
                roots.push(RootObject {
                    name: name.into(),
                    kind: kind.into(),
                    role: crate::text(volume, "name").into(),
                });
            }
            if roots.is_empty() {
                return Err("Cube storage objects are missing".into());
            }
            record.objects = roots;
            return Ok(());
        }
        Err(OWNERSHIP_UNKNOWN.into())
    }

    pub(crate) fn volume(&self, record: &mut Record, desired: &InstanceDesired) -> Result<String> {
        let captured = self.capture_volumes(record);
        let root = if desired.power != brocade_deployment::cvm::PowerState::Running {
            // Cleanup uses the owned native disk identities, never a creation
            // manifest that may have been retired or damaged after creation.
            if record.objects.len() != 1 || record.objects[0].role.is_empty() {
                return Err(OWNERSHIP_UNKNOWN.into());
            }
            record.objects[0].role.clone()
        } else {
            let bundle = desired.artifacts.as_ref().ok_or("Cube artifacts missing")?;
            let image = brocade_cvm_compile::normalize_image(&bundle.input.image_manifest)?;
            root_name(&image)?.to_owned()
        };
        if let Err(error) = captured {
            if error != OWNERSHIP_UNKNOWN
                || !self.cfg.capabilities.stable_storage
                || record.volume.is_empty()
                || record.objects.len() != 1
                || record.objects[0].name != format!("sb-{}-{root}", record.runtime)
                || record.objects[0].kind != "volume"
            {
                return Err(error);
            }
        }
        record.save(&self.root)?;
        let object = record
            .objects
            .iter()
            .find(|o| o.role == root)
            .ok_or("Cube root volume identity unavailable")?;
        let response=self.cube("ListSandboxSnapshots",json!({"requestID":"brocade-inspect-root","sandboxID":record.runtime,"backend":self.cfg.storage_backend,"objects":[object]}))?;
        crate::successful(&response)?;
        let objects = crate::array(&response, "objects");
        if objects.len() != 1
            || crate::text(&objects[0], "name") != object.name
            || crate::text(&objects[0], "kind") != object.kind
            || objects[0]["exists"] != true
            || !crate::text(&objects[0], "error_message").is_empty()
            || crate::number(&objects[0], "size_bytes") != desired.spec.resources.root_disk_bytes
        {
            return Err("actual Cube system disk identity or capacity does not match the frozen specification".into());
        }
        Ok(format!(
            "cube-{}",
            brocade_core::hash::sha256_hex(object.name.as_bytes())
        ))
    }

    /// Recover a create whose original RPC never returned an identity.
    ///
    /// Brocade's compiled request pins the Cube sandbox ID to the instance ID.  That lets a
    /// later reconciliation prove that the old attempt left no runtime, root object, storage
    /// record or TAP before it allocates anything again. Two complete proofs are separated by
    /// longer than Cube's absolute create deadline. Any surviving evidence clears the first
    /// proof and keeps the reservation quarantined instead of risking an empty replacement disk.
    pub(crate) fn recover_unallocated_create(
        &self,
        record: &mut Record,
        desired: &InstanceDesired,
    ) -> Result<bool> {
        if !record.attempted
            || desired.generation < record.generation
            || !record.volume.is_empty()
            || !record.objects.is_empty()
            || record.launch_sha256 != desired.spec_sha256
        {
            return Ok(false);
        }

        let request = self.cfg.create_request(desired)?;
        let runtime = crate::text(&request["annotations"], "cube.master.desired.sandbox.id");
        if runtime != desired.id
            || crate::text(&request["labels"], "brocade.cvm.instance") != desired.id
            || (!record.runtime.is_empty() && record.runtime != runtime)
        {
            return self.clear_unallocated_absence(record);
        }
        record.runtime = runtime.into();

        let root = root_name(&request)?;
        let object = RootObject {
            name: format!("sb-{runtime}-{root}"),
            kind: "volume".into(),
            role: root.into(),
        };
        let response = self.cube(
            "ListSandboxSnapshots",
            json!({"requestID":"brocade-recover-unallocated-root","sandboxID":runtime,"backend":self.cfg.storage_backend,"objects":[object]}),
        )?;
        crate::successful(&response)?;
        let objects = crate::array(&response, "objects");
        if objects.len() != 1
            || crate::text(&objects[0], "name") != object.name
            || crate::text(&objects[0], "kind") != object.kind
            || !crate::text(&objects[0], "error_message").is_empty()
            || objects[0]["exists"] != false
        {
            return self.clear_unallocated_absence(record);
        }

        let storage = self.storage()?;
        if crate::array(&storage, "sandboxes")
            .iter()
            .any(|item| crate::text(item, "sandboxID") == runtime)
        {
            return self.clear_unallocated_absence(record);
        }
        let network = self.network_inventory()?;
        if crate::array(&network, "taps").iter().any(|tap| {
            crate::text(tap, "sandboxID") == runtime
                || crate::text(tap, "ownerSandboxID") == runtime
        }) {
            return self.clear_unallocated_absence(record);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is before the Unix epoch")?
            .as_secs();
        let first = record.unallocated_absence_since_unix_secs;
        if first == 0 || now < first {
            record.unallocated_absence_since_unix_secs = now;
            record.save(&self.root)?;
            return Ok(false);
        }
        if now.saturating_sub(first) < UNALLOCATED_CREATE_ABSENCE_SECS {
            return Ok(false);
        }

        record.attempted = false;
        record.unallocated_absence_since_unix_secs = 0;
        record.initialized = false;
        record.transition.clear();
        record.save(&self.root)?;
        Ok(true)
    }

    fn clear_unallocated_absence(&self, record: &mut Record) -> Result<bool> {
        if record.unallocated_absence_since_unix_secs != 0 {
            record.unallocated_absence_since_unix_secs = 0;
            record.save(&self.root)?;
        }
        Ok(false)
    }

    pub(crate) fn prove_absent(&self, record: &Record) -> Result<()> {
        if record.objects.is_empty() {
            return Err("physical Cube volume identities unknown".into());
        }
        let response=self.cube("ListSandboxSnapshots",json!({"requestID":"brocade-confirm-delete","sandboxID":record.runtime,"backend":self.cfg.storage_backend,"objects":record.objects}))?;
        crate::successful(&response)?;
        let objects = crate::array(&response, "objects");
        if objects.len() != record.objects.len() {
            return Err("physical Cube volume deletion evidence incomplete".into());
        }
        for wanted in &record.objects {
            let matches = objects
                .iter()
                .filter(|o| {
                    crate::text(o, "name") == wanted.name && crate::text(o, "kind") == wanted.kind
                })
                .collect::<Vec<_>>();
            if matches.len() != 1
                || matches[0]["exists"] != false
                || !crate::text(matches[0], "error_message").is_empty()
            {
                return Err("physical Cube volume deletion pending".into());
            }
        }
        let inventory = self.storage()?;
        if crate::array(&inventory, "sandboxes").iter().any(|b| {
            crate::text(b, "sandboxID") == record.runtime && !crate::array(b, "volumes").is_empty()
        }) {
            return Err("Cube volume metadata cleanup pending".into());
        }
        let network = self.network_inventory()?;
        if crate::array(&network, "taps").iter().any(|t| {
            crate::text(t, "sandboxID") == record.runtime
                || crate::text(t, "ownerSandboxID") == record.runtime
        }) {
            return Err("Cube network cleanup pending".into());
        }
        Ok(())
    }

    pub(crate) fn network_inventory(&self) -> Result<Value> {
        self.network_inventory_with_timeout(std::time::Duration::from_secs(10))
    }

    fn network_inventory_with_timeout(&self, timeout: std::time::Duration) -> Result<Value> {
        let response = self
            .http
            .get(&self.cfg.network_inspection_url)
            .timeout(timeout)
            .send()
            .map_err(|_| "Cube network inspection unavailable")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("Cube network inspection failed".into());
        }
        let mut bytes = vec![];
        response
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cube network inspection read failed")?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("Cube network inspection exceeds limit".into());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Cube network inspection invalid")?;
        if !value["taps"].is_array() {
            return Err("Cube network inspection lacks TAP evidence".into());
        }
        Ok(value)
    }

    pub(crate) fn access_address(&self, runtime: &str) -> Result<String> {
        self.access_address_with_timeout(runtime, std::time::Duration::from_secs(10))
    }

    pub(crate) fn access_address_with_timeout(
        &self,
        runtime: &str,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let inventory = self.network_inventory_with_timeout(timeout)?;
        let mut address = None;
        for tap in crate::array(&inventory, "taps") {
            if crate::text(tap, "sandboxID") != runtime
                && crate::text(tap, "ownerSandboxID") != runtime
            {
                continue;
            }
            let ip: std::net::Ipv4Addr = crate::text(tap, "sandboxIP")
                .parse()
                .map_err(|_| "Cube delivery IP invalid")?;
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || address.is_some() {
                return Err("Cube delivery identity ambiguous or invalid".into());
            }
            address = Some(ip.to_string());
        }
        address.ok_or_else(|| "Cube delivery address missing".into())
    }
}
