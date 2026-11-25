//! Reusable IP network services, their allocated container devices and independent routes.
use super::{valid_id, ExecutionTarget, NetworkDiagnostics, RouteTarget};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

/// Current node network execution contract.
pub const NIC_CONTRACT_VERSION: u32 = 1;
/// Agent 与 Cubelet 都具备时才下发带协议端口的出口规则（组合台 L4 出口）。
pub const ROUTE_SERVICE_CONTRACT: &str = "cvm-route-service-v1";
pub const MAX_NETWORK_ROUTES: usize = 256;
pub const MAX_NETWORK_PUBLICATIONS: usize = 256;
pub const MAX_NETWORK_BINDINGS: usize = 1024;

/// Canonical prefixes make overlapping routes and allocation domains comparable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IpPrefix {
    address: IpAddr,
    length: u8,
}
impl TryFrom<String> for IpPrefix {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<IpPrefix> for String {
    fn from(value: IpPrefix) -> Self {
        value.to_string()
    }
}
impl std::fmt::Display for IpPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.address, self.length)
    }
}
impl FromStr for IpPrefix {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (address, length) = match value.split_once('/') {
            Some((address, length)) => (
                address
                    .parse::<IpAddr>()
                    .map_err(|_| "IP 地址格式不正确，请填写有效的 IPv4 或 IPv6 地址")?,
                Some(
                    length
                        .parse::<u8>()
                        .map_err(|_| "网段前缀长度不正确，IPv4 须为 0–32，IPv6 须为 0–128")?,
                ),
            ),
            None => (
                value
                    .parse::<IpAddr>()
                    .map_err(|_| "目标须为 IP 地址或网段，例如 203.0.113.1 或 203.0.113.0/24")?,
                None,
            ),
        };
        if matches!(address, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()) {
            return Err("不支持映射到 IPv6 的 IPv4 地址，请直接填写 IPv4 地址".into());
        }
        let bits = if address.is_ipv4() { 32 } else { 128 };
        let length = length.unwrap_or(bits);
        if length > bits {
            return Err("网段前缀长度不正确，IPv4 须为 0–32，IPv6 须为 0–128".into());
        }
        let network = Self { address, length };
        if network.number() & !network.mask() != 0 {
            return Err("网段须填写网络地址，例如 10.0.0.0/24，不能填写 10.0.0.1/24".into());
        }
        Ok(network)
    }
}
impl IpPrefix {
    fn number(&self) -> u128 {
        ip_number(self.address)
    }
    fn mask(&self) -> u128 {
        let bits = if self.address.is_ipv4() { 32 } else { 128 };
        if self.length == 0 {
            0
        } else {
            u128::MAX << (bits - self.length)
        }
    }
    pub fn address(&self) -> IpAddr {
        self.address
    }
    pub fn length(&self) -> u8 {
        self.length
    }
    pub fn is_ipv4(&self) -> bool {
        self.address.is_ipv4()
    }
    pub fn contains(&self, address: IpAddr) -> bool {
        self.address.is_ipv4() == address.is_ipv4()
            && (ip_number(address) & self.mask()) == self.number()
    }
    pub fn contains_prefix(&self, other: &Self) -> bool {
        self.length <= other.length && self.contains(other.address)
    }
    /// Allocation searches are explicitly bounded by the caller, even for IPv6 pools.
    pub fn host(&self, offset: u64) -> Option<IpAddr> {
        let number = self.number().checked_add(u128::from(offset))?;
        let address = if self.address.is_ipv4() {
            IpAddr::V4(Ipv4Addr::from(u32::try_from(number).ok()?))
        } else {
            IpAddr::V6(Ipv6Addr::from(number))
        };
        self.contains(address).then_some(address)
    }
}
fn ip_number(address: IpAddr) -> u128 {
    match address {
        IpAddr::V4(ip) => u128::from(u32::from(ip)),
        IpAddr::V6(ip) => u128::from(ip),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Xray,
    Wireguard,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportListenSide {
    #[default]
    Auto,
    Provider,
    Consumer,
}
impl TransportListenSide {
    pub fn resolved(self) -> Result<BindingSide, String> {
        match self {
            Self::Provider => Ok(BindingSide::Provider),
            Self::Consumer => Ok(BindingSide::Consumer),
            Self::Auto => Err("unresolved NIC transport listener".into()),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NicAddressMode {
    SharedNat,
    DedicatedRouted,
    DedicatedNat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkOwner {
    pub tenant_id: String,
    pub user_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportOffer {
    #[serde(default)]
    pub listen_side: TransportListenSide,
    pub kind: TransportKind,
    pub endpoint: IpAddr,
    pub port_start: u16,
    pub port_end: u16,
    pub mtu: u16,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicLimits {
    pub max_attachments: u16,
    pub upload_bps: Option<u64>,
    pub download_bps: Option<u64>,
    pub monthly_bytes: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicPublicationSpec {
    pub name: String,
    pub provider_node: String,
    pub resource_domain: String,
    pub ipv4_pool: Option<IpPrefix>,
    pub ipv6_pool: Option<IpPrefix>,
    pub public_ipv4_pool: Option<IpPrefix>,
    pub public_ipv6_pool: Option<IpPrefix>,
    pub shared_ipv4: Option<IpAddr>,
    pub shared_ipv6: Option<IpAddr>,
    pub address_mode: NicAddressMode,
    /// 出口网卡可达的目标网段；入站由独立的访问入口承担（见 [`super::Entry`]）。
    pub destinations: Vec<IpPrefix>,
    pub route_presets: Vec<IpPrefix>,
    pub dns_suggestions: Vec<IpAddr>,
    pub transports: Vec<TransportOffer>,
    pub grants: Vec<NetworkOwner>,
    pub limits: NicLimits,
}
impl NicPublicationSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 80
            || self.name.chars().any(char::is_control)
            || !valid_id(&self.provider_node)
            || !valid_id(&self.resource_domain)
        {
            return Err("出口网络名称须为 1–80 字，提供机器和地址资源范围必须有效".into());
        }
        if self.ipv4_pool.is_none() && self.ipv6_pool.is_none() {
            return Err("请至少填写一个 IPv4 或 IPv6 地址池".into());
        }
        for (pool, v4) in [
            (&self.ipv4_pool, true),
            (&self.ipv6_pool, false),
            (&self.public_ipv4_pool, true),
            (&self.public_ipv6_pool, false),
        ] {
            if let Some(pool) = pool {
                if pool.is_ipv4() != v4
                    || pool.address().is_multicast()
                    || pool.address().is_unspecified()
                    || pool.address().is_loopback()
                {
                    return Err("地址池的 IP 类型或地址无效，不能使用回环、组播或未指定地址".into());
                }
                if v4 && pool.length() < 16 || !v4 && pool.length() < 64 {
                    return Err("地址池范围过大，IPv4 前缀长度至少为 16，IPv6 至少为 64".into());
                }
            }
        }
        for (address, v4) in [(&self.shared_ipv4, true), (&self.shared_ipv6, false)] {
            if address.is_some_and(|ip| {
                ip.is_ipv4() != v4 || ip.is_unspecified() || ip.is_multicast() || ip.is_loopback()
            }) {
                return Err("共享出口地址无效，请填写对应类型的可用 IPv4 或 IPv6 地址".into());
            }
        }
        if self.address_mode == NicAddressMode::DedicatedNat
            && ((self.ipv4_pool.is_some() != self.public_ipv4_pool.is_some())
                || (self.ipv6_pool.is_some() != self.public_ipv6_pool.is_some()))
        {
            return Err("一对一映射需要为每个已启用的地址类型填写对应的公网地址池".into());
        }
        for raw in [
            "198.18.0.0/15",
            "169.254.0.0/16",
            "fd42:6263:6e69::/48",
            "fe80::/10",
        ] {
            let reserved: IpPrefix = raw
                .parse()
                .expect("fixed network isolation prefix is canonical");
            for pool in [&self.ipv4_pool, &self.ipv6_pool].into_iter().flatten() {
                if reserved.contains_prefix(pool) || pool.contains_prefix(&reserved) {
                    return Err("地址池与系统连接网段或链路本地地址重叠，请更换地址池".into());
                }
            }
        }
        if self.destinations.is_empty() {
            return Err("请填写此出口允许访问的目标 IP／网段".into());
        }
        if self.destinations.len() > MAX_NETWORK_ROUTES
            || self.route_presets.len() > MAX_NETWORK_ROUTES
            || self.dns_suggestions.len() > 4
            || self.grants.len() > 256
        {
            return Err(
                "目标网段和初始路由最多各 256 条，DNS 建议最多 4 个，授权用户最多 256 个".into(),
            );
        }
        if self.limits.max_attachments == 0
            || usize::from(self.limits.max_attachments) > MAX_NETWORK_BINDINGS
            || [
                self.limits.upload_bps,
                self.limits.download_bps,
                self.limits.monthly_bytes,
            ]
            .into_iter()
            .flatten()
            .any(|n| n == 0)
        {
            return Err("网卡数量、限速或流量额度无效；填写限额时须大于 0".into());
        }
        if self.transports.is_empty() || self.transports.len() > 2 {
            return Err("请至少启用 Xray 或 WireGuard 中的一种连接方式".into());
        }
        let mut kinds = BTreeSet::new();
        for offer in &self.transports {
            if !kinds.insert(offer.kind)
                || offer.endpoint.is_unspecified()
                || offer.endpoint.is_multicast()
                || offer.endpoint.is_loopback()
                || match offer.endpoint {
                    IpAddr::V4(ip) => ip.is_link_local(),
                    IpAddr::V6(ip) => ip.is_unicast_link_local(),
                }
                || offer.port_start == 0
                || offer.port_end < offer.port_start
                || u32::from(offer.port_end) - u32::from(offer.port_start) > 4095
                || offer.mtu < 1280
                || offer.mtu > 1500
            {
                return Err("连接配置无效，请核对端点 IP、端口范围和 MTU（1280–1500）".into());
            }
        }
        let mut owners = BTreeSet::new();
        for owner in &self.grants {
            if owner.tenant_id.is_empty()
                || owner.tenant_id.len() > 96
                || !valid_id(&owner.user_id)
                || !owners.insert((&owner.tenant_id, &owner.user_id))
            {
                return Err("授权用户无效或重复，请核对用户并移除重复项".into());
            }
        }
        for route in &self.route_presets {
            if !self
                .destinations
                .iter()
                .any(|network| network.contains_prefix(route))
            {
                return Err(
                    "初始路由超出此出口允许访问的目标范围，请调整初始路由或目标网段".into(),
                );
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicPublication {
    pub id: String,
    pub revision: u64,
    pub admitting: bool,
    pub retired: bool,
    pub spec: NicPublicationSpec,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationWrite {
    pub request_id: String,
    pub expected_revision: u64,
    pub spec: NicPublicationSpec,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationAdmission {
    pub request_id: String,
    pub expected_revision: u64,
    pub admitting: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicMountRequest {
    #[serde(default)]
    pub use_dns_suggestions: bool,
    pub request_id: String,
    pub expected_generation: u64,
    pub publication_id: String,
    pub transport: TransportKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicTransportChange {
    #[serde(default)]
    pub rebuild: bool,
    pub request_id: String,
    pub expected_generation: u64,
    pub transport: TransportKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicVersionChange {
    pub request_id: String,
    pub expected_generation: u64,
    pub publication_revision: u64,
    pub reallocate: bool,
    #[serde(default)]
    pub transport: Option<TransportKind>,
}

/// Allocated by Store. Neither Cubelet nor a tunnel adapter may choose replacements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicVersionUpdate {
    pub interface_id: String,
    pub publication_revision: u64,
    pub reallocate: bool,
    #[serde(default)]
    pub transport: Option<TransportKind>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicVersionsChange {
    pub request_id: String,
    pub expected_generation: u64,
    pub updates: Vec<NicVersionUpdate>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocatedNic {
    pub slot: u16,
    pub publication_id: String,
    pub publication_revision: u64,
    pub provider_node: String,
    pub resource_domain: String,
    pub allocation_id: String,
    pub mac: String,
    pub ipv4: Option<IpAddr>,
    pub ipv6: Option<IpAddr>,
    pub public_ipv4: Option<IpAddr>,
    pub public_ipv6: Option<IpAddr>,
    pub address_mode: NicAddressMode,
    pub destinations: Vec<IpPrefix>,
    pub transport: TransportKind,
    pub transport_generation: u64,
    pub mtu: u16,
    pub blocked: bool,
    pub quota_exceeded: bool,
    pub limits: NicLimits,
}
impl AllocatedNic {
    pub fn is_blocked(&self) -> bool {
        self.blocked || self.quota_exceeded
    }
    pub fn supports(&self, prefix: &IpPrefix) -> bool {
        (if prefix.is_ipv4() {
            self.ipv4.is_some()
        } else {
            self.ipv6.is_some()
        }) && self
            .destinations
            .iter()
            .any(|allowed| allowed.contains_prefix(prefix))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfacePriority {
    #[serde(flatten)]
    pub target: RouteTarget,
    pub ipv4: Option<u16>,
    pub ipv6: Option<u16>,
}
/// L4 出口匹配：协议加目的端口段。路由项不带它时只按目的网段匹配（L3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteService {
    pub protocol: L4Protocol,
    pub port_start: u16,
    pub port_end: u16,
}
impl RouteService {
    pub fn valid(&self) -> bool {
        self.port_start > 0 && self.port_start <= self.port_end
    }
    pub fn matches(&self, protocol: L4Protocol, port: u16) -> bool {
        self.protocol == protocol && (self.port_start..=self.port_end).contains(&port)
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.protocol == other.protocol
            && self.port_start <= other.port_end
            && other.port_start <= self.port_end
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContainerRoute {
    pub destination: IpPrefix,
    #[serde(flatten)]
    pub target: RouteTarget,
    /// 只有 Agent 与 Cubelet 都上报 [`ROUTE_SERVICE_CONTRACT`] 时才下发带端口的规则：
    /// 旧执行端会忽略这个字段，把它当成整段路由执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<RouteService>,
}
impl ContainerRoute {
    /// Precedence shared by flow matching and the Linux route rule emitters.
    pub fn precedence(&self, priority: u16) -> impl Ord + use<> {
        (
            std::cmp::Reverse(self.destination.length()),
            self.service.is_none(),
            priority,
            self.service
                .map(|s| s.port_end.saturating_sub(s.port_start)),
        )
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRoutes {
    pub revision: u64,
    pub priorities: Vec<InterfacePriority>,
    pub routes: Vec<ContainerRoute>,
    pub dns: Vec<IpAddr>,
}
impl ContainerRoutes {
    pub fn normalize(&mut self) {
        self.routes.sort();
        self.routes.dedup();
        self.priorities.sort_by(|a, b| a.target.cmp(&b.target));
        self.dns.dedup();
    }
    pub fn priority(&self, id: &str, ipv4: bool) -> Option<u16> {
        self.exit_priority(&RouteTarget::interface(id), ipv4)
    }
    pub fn exit_priority(&self, target: &RouteTarget, ipv4: bool) -> Option<u16> {
        self.priorities
            .iter()
            .find(|p| &p.target == target)
            .and_then(|p| if ipv4 { p.ipv4 } else { p.ipv6 })
    }
    pub fn chain_ids(&self) -> BTreeSet<&str> {
        self.routes
            .iter()
            .filter_map(|r| r.target.chain_id())
            .collect()
    }
    pub fn validate(&self, devices: &BTreeMap<String, AllocatedNic>) -> Result<(), String> {
        if self.routes.len() > MAX_NETWORK_ROUTES
            || self.priorities.len() > MAX_NETWORK_ROUTES
            || self.dns.len() > 4
        {
            return Err("路由和出口优先级最多各 256 条，DNS 服务器最多 4 个，请减少后重试".into());
        }
        let mut ids = BTreeSet::new();
        for priority in &self.priorities {
            if !valid_id(priority.target.id()) || !ids.insert(&priority.target) {
                return Err("出口优先级的目标无效或重复，请为每个出口只保留一项优先级".into());
            }
            if let Some(id) = priority.target.interface_id() {
                let nic = devices
                    .get(id)
                    .ok_or("出口优先级引用了未挂载的网卡，请重新选择出口")?;
                if priority.ipv4.is_some() && nic.ipv4.is_none()
                    || priority.ipv6.is_some() && nic.ipv6.is_none()
                {
                    return Err(
                        "出口优先级的地址类型不可用，请核对该网卡是否分配了 IPv4 或 IPv6 地址"
                            .into(),
                    );
                }
            }
        }
        let mut selections = BTreeMap::new();
        let mut services: Vec<(&IpPrefix, u16, RouteService, &RouteTarget)> = Vec::new();
        for route in &self.routes {
            if !valid_id(route.target.id()) {
                return Err("出口引用无效，请重新选择网卡或线路".into());
            }
            if let Some(id) = route.target.interface_id() {
                let nic = devices
                    .get(id)
                    .ok_or("路由引用了未挂载的网卡，请重新选择出口")?;
                if !nic.supports(&route.destination) {
                    return Err(
                        "目标网段超出该出口允许访问的范围，或该网卡没有对应的 IPv4／IPv6 地址"
                            .into(),
                    );
                }
            }
            let priority = self
                .exit_priority(&route.target, route.destination.is_ipv4())
                .ok_or("请先为所选出口填写目标地址类型对应的 IPv4 或 IPv6 优先级")?;
            if let Some(service) = route.service {
                if !service.valid() {
                    return Err("路由协议或端口范围不正确，请填写 1–65535 的端口，且起始端口不能大于结束端口".into());
                }
                if services.iter().any(|(prefix, p, other, target)| {
                    *prefix == &route.destination
                        && *p == priority
                        && other.overlaps(&service)
                        && *target != &route.target
                }) {
                    return Err(
                        "相同目标网段和优先级下，重叠的协议端口不能指向不同出口，请调整优先级或端口范围".into(),
                    );
                }
                services.push((&route.destination, priority, service, &route.target));
            } else if let Some(previous) =
                selections.insert((&route.destination, priority), &route.target)
            {
                if previous != &route.target {
                    return Err("相同目标网段和优先级不能指向不同出口，请调整出口优先级".into());
                }
            }
        }
        if self
            .dns
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast() || ip.is_loopback())
        {
            return Err(
                "容器 DNS 不能使用回环、组播或未指定地址，请填写容器可访问的 DNS 服务器 IP".into(),
            );
        }
        Ok(())
    }
    /// Match a new flow against the configured table, not runtime health or management exceptions.
    /// Cube selects a whole-prefix chain even for other protocols (its classifier then drops
    /// the packet); bare metal only selects a chain for TCP/UDP in the host OUTPUT policy.
    pub fn select_route_flow(
        &self,
        address: IpAddr,
        flow: Option<(L4Protocol, u16)>,
        execution: &ExecutionTarget,
    ) -> Option<(usize, &ContainerRoute, u16)> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, r)| r.destination.contains(address))
            .filter(|(_, r)| match (r.service, flow) {
                (None, None) => execution.is_cube() || r.target.chain_id().is_none(),
                (None, Some(_)) => true,
                (Some(s), Some((protocol, port))) => s.matches(protocol, port),
                (Some(_), None) => false,
            })
            .filter_map(|(index, r)| {
                self.exit_priority(&r.target, address.is_ipv4())
                    .map(|p| (index, r, p))
            })
            // Equal-ranked overlapping rules can only select the same target. Use the
            // canonical rule ordering for a deterministic row, including in unsaved edits.
            .min_by_key(|(_, r, p)| (r.precedence(*p), *r))
    }
    pub fn select_target_flow(
        &self,
        address: IpAddr,
        flow: Option<(L4Protocol, u16)>,
    ) -> Option<&RouteTarget> {
        self.select_route_flow(address, flow, &ExecutionTarget::Cube)
            .map(|(_, r, _)| &r.target)
    }
    pub fn select(&self, address: IpAddr) -> Option<&str> {
        self.select_flow(address, None)
    }
    pub fn select_flow(&self, address: IpAddr, flow: Option<(L4Protocol, u16)>) -> Option<&str> {
        self.select_target_flow(address, flow)
            .and_then(RouteTarget::interface_id)
    }
    pub fn references(&self, id: &str) -> bool {
        self.routes
            .iter()
            .any(|r| r.target.interface_id() == Some(id))
            || self
                .priorities
                .iter()
                .any(|p| p.target.interface_id() == Some(id))
    }
    pub fn default_interface(&self, ipv4: bool) -> Option<&str> {
        let address: IpAddr = if ipv4 { "0.0.0.0" } else { "::" }
            .parse()
            .expect("constant address");
        self.routes
            .iter()
            .filter(|r| {
                r.destination.length() == 0
                    && r.destination.is_ipv4() == ipv4
                    && r.service.is_none()
            })
            .filter_map(|r| {
                self.exit_priority(&r.target, address.is_ipv4())
                    .map(|p| (r, p))
            })
            .min_by_key(|(_, p)| *p)
            .and_then(|(r, _)| r.target.interface_id())
    }
    pub fn remove(&mut self, id: &str) {
        self.routes.retain(|r| r.target.interface_id() != Some(id));
        self.priorities
            .retain(|p| p.target.interface_id() != Some(id));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutesWrite {
    pub request_id: String,
    pub expected_generation: u64,
    pub expected_revision: u64,
    pub routes: ContainerRoutes,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicNodeCapabilities {
    #[serde(default)]
    pub entry_protocol_rules: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_ports: Vec<u16>,
    pub contract: u32,
    pub transports: Vec<TransportKind>,
    pub consume: bool,
    pub provide: bool,
    pub max_bindings: u16,
    pub addresses: Vec<IpAddr>,
    pub host_ports: Vec<NicHostPort>,
    pub xray_certificate_pem: Option<String>,
    pub error: Option<String>,
    /// Agent 能执行入口规则、宿主投递与机器对通道（[`super::ENTRY_CONTRACT`]）。
    pub entries: bool,
    /// Agent 能在宿主侧按协议端口执行出口规则（[`ROUTE_SERVICE_CONTRACT`]）。
    pub route_service: bool,
    /// L3 forwarding of a host address preserves host replies and transport sockets.
    pub entry_host_l3: bool,
    /// Entry IP transport, independently gated from the existing egress NIC transport.
    pub entry_xray: bool,
    pub entry_phantun: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPhase {
    Preparing,
    Active,
    Standby,
    Withdrawing,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSide {
    Consumer,
    Provider,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicBinding {
    pub side: BindingSide,
    pub id: String,
    pub interface_id: String,
    pub instance_id: String,
    pub consumer_node: String,
    pub provider_node: String,
    pub generation: u64,
    pub phase: BindingPhase,
    pub nic: AllocatedNic,
    pub offer: TransportOffer,
    pub port: u16,
    /// WireGuard keys or Xray authentication are delivered only to authenticated participants.
    pub credential: String,
    pub peer_credential: String,
    pub routing: ContainerRoutes,
    /// 出入对称：容器在提供机器上有整 IP 入口时，提供侧 IPv4 出网改用这个入口地址作为源地址。
    /// 只对 NAT 地址模式生效；路由独立地址模式下 Guest 本身就使用公网地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_source_ipv4: Option<Ipv4Addr>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum L4Protocol {
    Tcp,
    Udp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicHostPort {
    pub protocol: L4Protocol,
    pub port: u16,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicDesired {
    pub node_id: String,
    pub budgets: BTreeMap<String, NicTrafficBudget>,
    pub generation: u64,
    pub bindings: Vec<NicBinding>,
    pub fences: Vec<NicResourceFence>,
    pub management_networks: Vec<IpPrefix>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicResourceFence {
    pub interface_id: String,
    pub address: IpAddr,
    pub protocol: Option<L4Protocol>,
    pub port_start: u16,
    pub port_end: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicTrafficBudget {
    pub period_start: u64,
    pub used_bytes: u64,
    pub sample: Option<NicTrafficSample>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicTrafficSample {
    pub epoch: String,
    pub observed_at: u64,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicBindingReport {
    #[serde(default)]
    pub local_block: Option<String>,
    #[serde(default)]
    pub traffic: Option<NicTrafficSample>,
    pub id: String,
    pub side: BindingSide,
    pub generation: u64,
    pub phase: BindingPhase,
    pub configured: bool,
    pub reachable: Option<bool>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicNodeReport {
    pub generation: u64,
    pub bindings: Vec<NicBindingReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<NetworkDiagnostics>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefixes_reject_aliases_and_bound_address_arithmetic() {
        assert!("10.0.0.1/24".parse::<IpPrefix>().is_err());
        assert!("::ffff:192.0.2.1/128".parse::<IpPrefix>().is_err());
        let network: IpPrefix = "192.0.2.0/30".parse().unwrap();
        assert_eq!(network.host(1).unwrap().to_string(), "192.0.2.1");
        assert_eq!(network.host(4), None);
        assert!("::/0"
            .parse::<IpPrefix>()
            .unwrap()
            .contains("2001:db8::1".parse().unwrap()));
        assert_eq!(
            serde_json::from_str::<IpPrefix>("\"192.0.2.4\"")
                .unwrap()
                .to_string(),
            "192.0.2.4/32"
        );
    }
    #[test]
    fn routes_choose_prefix_then_per_family_priority_without_health_fallback() {
        let mut routes = ContainerRoutes {
            revision: 1,
            priorities: vec![
                InterfacePriority {
                    target: RouteTarget::interface("nic-a"),
                    ipv4: Some(20),
                    ipv6: Some(1),
                },
                InterfacePriority {
                    target: RouteTarget::interface("nic-b"),
                    ipv4: Some(10),
                    ipv6: Some(2),
                },
            ],
            routes: vec![
                ContainerRoute {
                    destination: "0.0.0.0/0".parse().unwrap(),
                    target: RouteTarget::interface("nic-a"),
                    service: None,
                },
                ContainerRoute {
                    destination: "10.0.0.0/8".parse().unwrap(),
                    target: RouteTarget::interface("nic-b"),
                    service: None,
                },
                ContainerRoute {
                    destination: "10.1.0.0/16".parse().unwrap(),
                    target: RouteTarget::interface("nic-a"),
                    service: None,
                },
                ContainerRoute {
                    destination: "::/0".parse().unwrap(),
                    target: RouteTarget::interface("nic-b"),
                    service: None,
                },
            ],
            dns: vec![],
        };
        assert_eq!(routes.select("10.1.2.3".parse().unwrap()), Some("nic-a"));
        assert_eq!(routes.select("10.2.2.3".parse().unwrap()), Some("nic-b"));
        assert_eq!(routes.select("2001:db8::1".parse().unwrap()), Some("nic-b"));
        routes.remove("nic-a");
        assert_eq!(routes.select("10.1.2.3".parse().unwrap()), Some("nic-b"));
        assert_eq!(routes.select("8.8.8.8".parse().unwrap()), None);
    }
    #[test]
    fn port_rules_refine_the_same_prefix_but_never_beat_a_more_specific_prefix() {
        let route = |destination: &str, id: &str, service: Option<RouteService>| ContainerRoute {
            destination: destination.parse().unwrap(),
            target: RouteTarget::interface(id),
            service,
        };
        let ssh = RouteService {
            protocol: L4Protocol::Tcp,
            port_start: 22,
            port_end: 22,
        };
        let routes = ContainerRoutes {
            revision: 1,
            priorities: ["home", "ntt", "office"]
                .iter()
                .map(|id| InterfacePriority {
                    target: RouteTarget::interface(*id),
                    ipv4: Some(10),
                    ipv6: None,
                })
                .collect(),
            routes: vec![
                route("0.0.0.0/0", "home", None),
                route("0.0.0.0/0", "ntt", Some(ssh)),
                route("192.168.10.0/24", "office", None),
            ],
            dns: vec![],
        };
        let public = "203.0.113.9".parse().unwrap();
        let office = "192.168.10.5".parse().unwrap();
        assert_eq!(
            routes.select_flow(public, Some((L4Protocol::Tcp, 22))),
            Some("ntt")
        );
        assert_eq!(
            routes.select_flow(public, Some((L4Protocol::Udp, 22))),
            Some("home")
        );
        assert_eq!(routes.select_flow(public, None), Some("home"));
        assert_eq!(
            routes.select_flow(office, Some((L4Protocol::Tcp, 22))),
            Some("office")
        );
        assert_eq!(routes.default_interface(true), Some("home"));
        assert!(!RouteService {
            protocol: L4Protocol::Tcp,
            port_start: 0,
            port_end: 1
        }
        .valid());
        assert!(ssh.overlaps(&RouteService {
            protocol: L4Protocol::Tcp,
            port_start: 20,
            port_end: 30
        }));
        assert!(!ssh.overlaps(&RouteService {
            protocol: L4Protocol::Udp,
            port_start: 22,
            port_end: 22
        }));
    }
}
