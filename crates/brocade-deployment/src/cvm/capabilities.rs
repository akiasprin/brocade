use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSystemInventory {
    pub os: String,
    pub primary_device: String,
    /// Present on the unified Cube + host executor. Older coexistence reports only carried
    /// presentation fields and remain readable during a rolling Agent upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<HostIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    /// Physical-host terminal readiness is independent from Cube Guest terminals.
    #[serde(default)]
    pub terminal_ready: bool,
}

impl HostSystemInventory {
    pub fn validate(&self) -> Result<(), String> {
        if self.os.trim().is_empty()
            || self.os.len() > 160
            || self.os.chars().any(char::is_control)
            || self.primary_device.is_empty()
            || self.primary_device.len() > 15
            || !self
                .primary_device
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte))
        {
            return Err("invalid host management information".into());
        }
        match (&self.identity, &self.terminal_user, &self.resources) {
            (None, None, None) if !self.terminal_ready => {}
            (None, None, None) => {
                return Err("legacy host information cannot advertise terminal readiness".into());
            }
            (Some(identity), Some(terminal_user), Some(resources)) => {
                identity.validate()?;
                if terminal_user.is_empty()
                    || terminal_user.len() > 32
                    || !terminal_user
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                    || resources.vcpus == 0
                    || resources.vcpus > 1024
                    || resources.memory_bytes == 0
                    || resources.memory_bytes > i64::MAX as u64
                    || resources.root_disk_bytes == 0
                    || resources.root_disk_bytes > i64::MAX as u64
                {
                    return Err("invalid dynamic host execution information".into());
                }
            }
            _ => return Err("incomplete dynamic host execution information".into()),
        }
        Ok(())
    }

    pub fn adoptable(&self) -> Option<HostInventory> {
        Some(HostInventory {
            identity: self.identity.clone()?,
            os: self.os.clone(),
            terminal_user: self.terminal_user.clone()?,
            primary_device: self.primary_device.clone(),
        })
    }
}

#[cfg(test)]
mod host_system_tests {
    use super::*;

    #[test]
    fn cube_can_report_host_system_without_advertising_exclusive_adoption() {
        let mut capabilities = Capabilities {
            backend: "cube-kvm-v1".into(),
            host_system: Some(HostSystemInventory {
                os: "Linux".into(),
                primary_device: "eth0".into(),
                identity: Some(HostIdentity {
                    machine: "a".repeat(64),
                    root_filesystem: "b".repeat(64),
                }),
                terminal_user: Some("root".into()),
                resources: Some(Resources {
                    vcpus: 4,
                    memory_bytes: 8 * 1024 * 1024 * 1024,
                    root_disk_bytes: 64 * 1024 * 1024 * 1024,
                }),
                terminal_ready: true,
            }),
            ..Default::default()
        };
        assert_eq!(capabilities.validate(), Ok(()));
        assert!(capabilities.baremetal.is_none());
        let value = serde_json::to_value(&capabilities).unwrap();
        assert!(value.get("baremetal").is_none());
        assert_eq!(value["host_system"]["primary_device"], "eth0");

        capabilities.backend = "baremetal-v1".into();
        assert!(capabilities.validate().is_err());
    }

    #[test]
    fn legacy_host_system_cannot_advertise_terminal_readiness() {
        let inventory = HostSystemInventory {
            os: "Linux".into(),
            primary_device: "eth0".into(),
            identity: None,
            terminal_user: None,
            resources: None,
            terminal_ready: true,
        };
        assert!(inventory.validate().is_err());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_compiler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cube_storage_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baremetal: Option<HostInventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_system: Option<HostSystemInventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_system_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_quota: Option<NetworkQuotaCapacity>,
    pub managed_networks: bool,
    /// Executor support; actual Guest PTY and resize are verified when opening a session.
    pub guest_terminal: bool,
    /// Persistent sessions require a matching instance-bound CubeShim endpoint.
    pub guest_terminal_persistent: bool,
    /// Native primary NIC forwards host-delivered IP replies, not TCP alone.
    pub primary_ip_delivery: bool,
    pub backend: String,
    pub create: bool,
    pub stop: bool,
    pub start: bool,
    pub delete: bool,
    pub stable_storage: bool,
    /// Separate from shutdown retention: requires a retained-volume workflow.
    pub retain_on_delete: bool,
    pub restart: bool,
    pub reinstall: bool,
    pub configure: bool,
    /// Update default routes without replacing NIC identities or address reservations.
    pub configure_network: bool,
    /// Cubelet 能在 Guest 中按协议端口写策略路由（出口 L4 规则）。
    pub route_services: bool,
    /// Primary-NIC transparent chain egress with terminal failure semantics.
    #[serde(default)]
    pub chain_egress: bool,
    pub hot_attach: bool,
    pub hot_detach: bool,
    pub max_interfaces: u16,
    pub allocatable_vcpus: u32,
    pub allocatable_memory_bytes: u64,
    pub allocatable_disk_bytes: u64,
    pub images: Vec<Image>,
    pub resource_limits: Option<ResourceLimits>,
    pub network_sources: Vec<NetworkSource>,
}

impl Capabilities {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .host_system_error
            .as_ref()
            .is_some_and(|error| error.len() > 500)
            || (self.host_system_error.is_some() && self.host_system.is_some())
            || ((self.host_system.is_some() || self.host_system_error.is_some())
                && (self.baremetal.is_some() || !self.backend.starts_with("cube-")))
        {
            return Err("invalid coexisting host management inventory".into());
        }
        if let Some(host) = &self.host_system {
            host.validate()?;
        }
        if let Some(host) = &self.baremetal {
            host.validate()?;
            if self.backend != "baremetal-v1"
                || self.create
                || self.start
                || self.stop
                || self.delete
                || self.reinstall
                || self.configure
                || self.restart
                || self.stable_storage
                || self.retain_on_delete
                || !self.images.is_empty()
                || self.network_quota.is_some()
            {
                return Err("bare-metal inventory cannot advertise VM lifecycle or storage".into());
            }
        }
        if let Some(capacity) = &self.network_quota {
            capacity.validate()?;
        }
        if (self.restart || self.reinstall || self.configure)
            && (!self.stable_storage || !self.start || !self.stop)
        {
            return Err("CVM lifecycle requires protected storage and power support".into());
        }
        if self.configure && self.resource_limits.is_none() {
            return Err("CVM configuration requires independent resource limits".into());
        }
        if let Some(limits) = &self.resource_limits {
            limits.validate()?;
        }
        if self.backend.len() > 80
            || self.max_interfaces as usize > MAX_INTERFACES
            || self.images.len() > MAX_CATALOG_ITEMS
            || self.network_sources.len() > MAX_CATALOG_ITEMS
        {
            return Err("CVM capability catalogue exceeds its limits".into());
        }
        for ids in [
            self.images
                .iter()
                .map(|x| x.id.as_str())
                .collect::<Vec<_>>(),
            self.network_sources.iter().map(|x| x.id.as_str()).collect(),
        ] {
            let mut seen = BTreeSet::new();
            if ids.iter().any(|id| !valid_id(id) || !seen.insert(*id)) {
                return Err("CVM catalogue identifiers are invalid or duplicated".into());
            }
        }
        for image in &self.images {
            if image.name.trim().is_empty()
                || image.name.len() > 100
                || image.version.len() > 100
                || !matches!(image.architecture.as_str(), "x86_64" | "aarch64")
                || !valid_digest(&image.sha256)
                || image.boot_mode.len() > 40
            {
                return Err("CVM image metadata is invalid".into());
            }
        }
        if self.network_sources.iter().any(|x| {
            x.name.trim().is_empty()
                || x.name.len() > 100
                || x.description.len() > 400
                || x.max_attachments
                    .is_some_and(|limit| limit == 0 || usize::from(limit) > MAX_INTERFACES)
        }) {
            return Err("CVM network source metadata is invalid".into());
        }
        if self.allocatable_memory_bytes > i64::MAX as u64
            || self.allocatable_disk_bytes > i64::MAX as u64
        {
            return Err("CVM capacity exceeds supported accounting range".into());
        }
        if self.create
            && (self.backend.is_empty()
                || self.max_interfaces == 0
                || self.images.is_empty()
                || self.resource_limits.is_none()
                || self.network_sources.is_empty())
        {
            return Err("CVM create capability has no verified catalogue".into());
        }
        Ok(())
    }
    pub fn validate_interfaces(&self, interfaces: &[InterfaceRequest]) -> Result<(), String> {
        if interfaces.is_empty() || interfaces.len() > usize::from(self.max_interfaces) {
            return Err("挂载网卡数量超出该机器的执行能力".into());
        }
        if interfaces.len() != 1 || interfaces[0].purpose != InterfacePurpose::Contact {
            return Err("配置来源仅用于一张主网卡；业务网卡必须由控制面分配".into());
        }
        let interface = &interfaces[0];
        if !self
            .network_sources
            .iter()
            .any(|source| source.id == interface.source_id)
        {
            return Err("网卡接入来源不受该机器支持".into());
        }
        if interface.default_ipv4 || interface.default_ipv6 {
            return Err("默认业务出口应配置在业务网卡，联络路由独立保留".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_requires_explicit_terminal_support_and_ignores_unknown_extensions() {
        let mut value = serde_json::to_value(Capabilities::default()).unwrap();
        value["future_extension"] = serde_json::json!({"version": 1});
        let caps: Capabilities = serde_json::from_value(value.clone()).unwrap();
        assert!(!caps.guest_terminal);
        value.as_object_mut().unwrap().remove("guest_terminal");
        assert!(serde_json::from_value::<Capabilities>(value).is_err());
    }

    #[test]
    fn persisted_inventory_may_omit_false_chain_egress_support() {
        let mut value = serde_json::to_value(Capabilities::default()).unwrap();
        value.as_object_mut().unwrap().remove("chain_egress");
        let caps: Capabilities = serde_json::from_value(value.clone()).unwrap();
        assert!(!caps.chain_egress);
        value["chain_egress"] = true.into();
        assert!(
            serde_json::from_value::<Capabilities>(value)
                .unwrap()
                .chain_egress
        );
    }
}
