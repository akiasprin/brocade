use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_quota: Option<NetworkQuotaCapacity>,
    #[serde(default)]
    pub managed_networks: bool,
    /// Executor support; actual Guest PTY and resize are verified when opening a session.
    pub guest_terminal: bool,
    /// Persistent sessions require a matching instance-bound CubeShim endpoint.
    #[serde(default)]
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
    /// Cubelet 能在 Guest 中按协议端口写策略路由（出口 L4 规则）。旧运行栈不上报即为不支持。
    #[serde(default)]
    pub route_services: bool,
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
            if limits.max_vcpus > self.allocatable_vcpus
                || limits.max_memory_bytes > self.allocatable_memory_bytes
                || limits.max_root_disk_bytes > self.allocatable_disk_bytes
            {
                return Err("CVM per-instance limits exceed machine capacity".into());
            }
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
        if !interfaces
            .iter()
            .any(|x| x.purpose == InterfacePurpose::Contact)
        {
            return Err("至少保留一张主网卡".into());
        }
        if interfaces.iter().filter(|x| x.default_ipv4).count() > 1
            || interfaces.iter().filter(|x| x.default_ipv6).count() > 1
        {
            return Err("同一地址族只能有一个默认业务出口".into());
        }
        for interface in interfaces {
            let source = self
                .network_sources
                .iter()
                .find(|x| x.id == interface.source_id)
                .ok_or("网卡接入来源不受该机器支持")?;
            if interface.default_ipv6 && source.supports_ipv6 == Some(false) {
                return Err("该接入来源尚未提供 IPv6 业务出口".into());
            }
            if source.max_attachments.is_some_and(|limit| {
                interfaces
                    .iter()
                    .filter(|nic| nic.source_id == source.id)
                    .count()
                    > usize::from(limit)
            }) {
                return Err("同一接入来源的挂载数量超出执行能力".into());
            }
            if interface.purpose == InterfacePurpose::Contact && source.kind != NetworkKind::Public
            {
                return Err("主网卡需要明确的公网连接来源".into());
            }
            if interface.purpose == InterfacePurpose::Contact
                && (interface.default_ipv4 || interface.default_ipv6)
            {
                return Err("默认业务出口应配置在业务网卡，联络路由独立保留".into());
            }
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
}
