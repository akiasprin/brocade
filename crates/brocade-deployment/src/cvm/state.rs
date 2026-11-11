use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    Running,
    Stopped,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    Pending,
    Applied,
    Failed,
    Unknown,
    RestartRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDesired {
    /// Instance-scoped cleanup assignments; not part of the immutable specification.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detached_interfaces: Vec<String>,
    pub id: String,
    pub operation_id: String,
    pub generation: u64,
    pub spec_sha256: String,
    pub spec: InstanceSpec,
    pub power: PowerState,
    /// Deleting a running instance never implicitly authorizes destroying its data.
    pub delete_data: bool,
    /// Monotonic boot/reset intentions survive polling, retries and agent restarts.
    pub restart_epoch: u64,
    pub reinstall_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredState {
    pub instances: Vec<InstanceDesired>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceObservation {
    pub id: String,
    pub device: String,
    pub mac: String,
    pub addresses: Vec<String>,
    pub attached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkObservation {
    #[serde(default)]
    pub routing_revision: u64,
    pub contact_route_preserved: bool,
    pub business_fallback_blocked: bool,
    pub default_ipv4_interface: Option<String>,
    pub default_ipv6_interface: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceReport {
    pub restart_epoch: u64,
    pub reinstall_epoch: u64,
    pub id: String,
    pub operation_id: String,
    pub generation: u64,
    pub spec_sha256: String,
    pub status: ApplyStatus,
    pub power: Option<PowerState>,
    pub runtime_id: Option<String>,
    pub volume_id: Option<String>,
    /// Machine-scoped delivery address; separate from the addresses inside the Guest.
    pub access_address: Option<String>,
    pub interfaces: Vec<InterfaceObservation>,
    pub network: Option<NetworkObservation>,
    pub network_absent: bool,
    pub detached_interfaces: Vec<String>,
    pub volume_retained: bool,
    pub volume_deleted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryReport {
    pub capabilities: Capabilities,
    pub error: Option<String>,
}

impl InstanceReport {
    /// Applied is evidence, not an arbitrary success flag supplied by a runtime wrapper.
    pub fn validate_for(&self, desired: &InstanceDesired) -> Result<(), String> {
        if self.id != desired.id
            || self.operation_id != desired.operation_id
            || self.generation != desired.generation
            || self.spec_sha256 != desired.spec_sha256
        {
            return Err("CVM report does not match the frozen desired version".into());
        }
        if self.detached_interfaces.len() > MAX_INTERFACES
            || self.detached_interfaces.iter().any(|x| !valid_id(x))
        {
            return Err("CVM detached interface evidence is invalid".into());
        }
        if self.interfaces.len() > MAX_INTERFACES
            || self.error.as_ref().is_some_and(|e| e.len() > 500)
            || self.runtime_id.as_ref().is_some_and(|x| x.len() > 150)
            || self.volume_id.as_ref().is_some_and(|x| x.len() > 150)
        {
            return Err("CVM report exceeds its limits".into());
        }
        if self
            .access_address
            .as_ref()
            .is_some_and(|value| value.parse::<std::net::IpAddr>().is_err())
        {
            return Err("CVM delivery address is invalid".into());
        }
        let mut ids = BTreeSet::new();
        if self.interfaces.iter().any(|x| {
            !valid_id(&x.id)
                || !ids.insert(&x.id)
                || x.device.len() > 32
                || x.mac.len() > 32
                || x.addresses.len() > 16
                || x.addresses.iter().any(|a| !valid_interface_address(a))
                || (x.attached && (!valid_mac(&x.mac) || x.device.is_empty()))
        }) {
            return Err("CVM interface evidence is invalid".into());
        }
        if self.status != ApplyStatus::Applied {
            return Ok(());
        }
        if self.restart_epoch != desired.restart_epoch
            || self.reinstall_epoch != desired.reinstall_epoch
        {
            return Err("CVM lifecycle completion does not match the desired epoch".into());
        }
        if self.error.is_some() || self.power != Some(desired.power) {
            return Err("Applied CVM report has an error or wrong power state".into());
        }
        if self.volume_deleted && self.volume_retained {
            return Err("CVM volume cannot be both retained and deleted".into());
        }
        if desired.power == PowerState::Deleted {
            if (!desired.delete_data && self.volume_id.as_ref().is_none_or(|x| x.is_empty()))
                || self.runtime_id.is_some()
                || self.access_address.is_some()
                || !self.network_absent
                || self.interfaces.iter().any(|x| x.attached)
                || (desired.delete_data && !self.volume_deleted)
                || (!desired.delete_data && !self.volume_retained)
            {
                return Err("CVM deletion lacks runtime, network or volume evidence".into());
            }
        } else {
            if self.volume_id.as_ref().is_none_or(|x| x.is_empty()) {
                return Err("CVM report has no volume identity".into());
            }
            if desired.power == PowerState::Stopped
                && (self.runtime_id.is_some()
                    || !self.volume_retained
                    || !self.network_absent
                    || self.interfaces.iter().any(|x| x.attached))
            {
                return Err("CVM stop lacks runtime, network or retained volume evidence".into());
            }
            if desired.power == PowerState::Running {
                if self.runtime_id.as_ref().is_none_or(|x| x.is_empty())
                    || self.interfaces.len() != desired.spec.interfaces.len()
                    || desired.spec.interfaces.iter().any(|x| {
                        !self.interfaces.iter().any(|a| {
                            a.id == x.id
                                && a.attached
                                && !a.device.is_empty()
                                && !a.addresses.is_empty()
                        })
                    })
                {
                    return Err("CVM running report lacks actual Guest interface evidence".into());
                }
                if desired
                    .spec
                    .interfaces
                    .iter()
                    .any(|nic| nic.connection.purpose == InterfacePurpose::Business)
                {
                    let network = self.network.as_ref().ok_or("业务网卡缺少路由隔离回执")?;
                    let default_v4 = desired.spec.routing.default_interface(true);
                    let default_v6 = desired.spec.routing.default_interface(false);
                    if !network.contact_route_preserved
                        || !network.business_fallback_blocked
                        || network.routing_revision != desired.spec.routing.revision
                        || network.default_ipv4_interface.as_deref() != default_v4
                        || network.default_ipv6_interface.as_deref() != default_v6
                    {
                        return Err("业务出口或联络路由隔离尚未确认".into());
                    }
                    for nic in &desired.spec.interfaces {
                        if let Some(assigned) = &nic.allocated {
                            let observed = self
                                .interfaces
                                .iter()
                                .find(|n| n.id == nic.id)
                                .ok_or("allocated NIC is missing")?;
                            if !observed.mac.eq_ignore_ascii_case(&assigned.mac)
                                || [assigned.ipv4, assigned.ipv6]
                                    .into_iter()
                                    .flatten()
                                    .any(|ip| {
                                        !observed.addresses.iter().any(|a| {
                                            a.split('/').next().is_some_and(|a| {
                                                a.parse::<std::net::IpAddr>().ok() == Some(ip)
                                            })
                                        })
                                    })
                            {
                                return Err("Guest NIC does not match Store allocation".into());
                            }
                            for address in &observed.addresses {
                                let ip = address
                                    .split('/')
                                    .next()
                                    .and_then(|value| value.parse::<std::net::IpAddr>().ok())
                                    .ok_or("invalid allocated NIC observation")?;
                                if matches!(ip,std::net::IpAddr::V6(ip) if ip.is_unicast_link_local())
                                {
                                    continue;
                                }
                                if ![assigned.ipv4, assigned.ipv6].contains(&Some(ip)) {
                                    return Err("Guest NIC retains an address outside its current allocation".into());
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_release_requires_lifecycle_intent_and_completion_fields() {
        let desired = serde_json::json!({
            "id":"cvm-current", "operation_id":"op-current", "generation":1,
            "spec_sha256":"a".repeat(64), "power":"running", "delete_data":false,
            "restart_epoch":0, "reinstall_epoch":0,
            "spec":{
                "name":"current", "ssh_public_key":"public-key", "interfaces":[],
                "image":{"id":"debian","name":"Debian","version":"13","architecture":"x86_64","sha256":"b".repeat(64),"boot_mode":"cube-oci"},
                "resources":{"vcpus":1,"memory_bytes":134217728,"root_disk_bytes":3221225472u64}
            }
        });
        let current: InstanceDesired = serde_json::from_value(desired.clone()).unwrap();
        assert!(current.detached_interfaces.is_empty());
        for required in ["delete_data", "restart_epoch", "reinstall_epoch"] {
            let mut missing = desired.clone();
            missing.as_object_mut().unwrap().remove(required);
            assert!(
                serde_json::from_value::<InstanceDesired>(missing).is_err(),
                "{required}"
            );
        }
        let report = serde_json::json!({
            "id":"cvm-current", "operation_id":"op-current", "generation":1,
            "spec_sha256":"a".repeat(64), "status":"pending", "power":null,
            "restart_epoch":0, "reinstall_epoch":0, "runtime_id":null, "volume_id":null,
            "interfaces":[], "detached_interfaces":[], "network_absent":false,
            "volume_retained":false, "volume_deleted":false, "error":null
        });
        let mut current = report.clone();
        current["future_extension"] = serde_json::json!(true);
        serde_json::from_value::<InstanceReport>(current).unwrap();
        for required in [
            "restart_epoch",
            "reinstall_epoch",
            "network_absent",
            "detached_interfaces",
            "volume_retained",
            "volume_deleted",
        ] {
            let mut missing = report.clone();
            missing.as_object_mut().unwrap().remove(required);
            assert!(
                serde_json::from_value::<InstanceReport>(missing).is_err(),
                "{required}"
            );
        }
    }
}
