//! Reusable IP network services, their allocated container devices and independent routes.
use super::{valid_id, MAX_INTERFACES};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

/// 67：网卡只承担出口（去掉入站方向、入站来源与 `NicIngress`），入站改由访问入口承担；
/// 绑定可携带出入对称的 IPv4 源地址。旧 Agent 不领取新语义。
pub const NIC_PROTOCOL_VERSION: u32 = 67;
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
                    .map_err(|_| "invalid IP address")?,
                Some(
                    length
                        .parse::<u8>()
                        .map_err(|_| "invalid IP prefix length")?,
                ),
            ),
            None => (
                value
                    .parse::<IpAddr>()
                    .map_err(|_| "route must be an IP address or prefix")?,
                None,
            ),
        };
        if matches!(address, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()) {
            return Err("mapped IPv4 addresses are not supported".into());
        }
        let bits = if address.is_ipv4() { 32 } else { 128 };
        let length = length.unwrap_or(bits);
        if length > bits {
            return Err("invalid IP prefix length".into());
        }
        let network = Self { address, length };
        if network.number() & !network.mask() != 0 {
            return Err("IP prefix must use its network address".into());
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
            return Err("invalid network publication identity".into());
        }
        if self.ipv4_pool.is_none() && self.ipv6_pool.is_none() {
            return Err("at least one address family is required".into());
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
                    return Err("invalid network allocation pool".into());
                }
                if v4 && pool.length() < 16 || !v4 && pool.length() < 64 {
                    return Err("network allocation pool is too large".into());
                }
            }
        }
        for (address, v4) in [(&self.shared_ipv4, true), (&self.shared_ipv6, false)] {
            if address.is_some_and(|ip| {
                ip.is_ipv4() != v4 || ip.is_unspecified() || ip.is_multicast() || ip.is_loopback()
            }) {
                return Err("invalid public egress address".into());
            }
        }
        if self.address_mode == NicAddressMode::DedicatedNat
            && ((self.ipv4_pool.is_some() != self.public_ipv4_pool.is_some())
                || (self.ipv6_pool.is_some() != self.public_ipv6_pool.is_some()))
        {
            return Err(
                "each dedicated NAT address family requires its public allocation pool".into(),
            );
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
                    return Err(
                        "allocation pool overlaps managed transit or link-local addresses".into(),
                    );
                }
            }
        }
        if self.destinations.is_empty() {
            return Err("outbound networks must be explicitly declared".into());
        }
        if self.destinations.len() > MAX_NETWORK_ROUTES
            || self.route_presets.len() > MAX_NETWORK_ROUTES
            || self.dns_suggestions.len() > 4
            || self.grants.len() > 256
        {
            return Err("network publication exceeds its limits".into());
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
            return Err("invalid network capacity or limits".into());
        }
        if self.transports.is_empty() || self.transports.len() > 2 {
            return Err("a publication requires Xray or WireGuard transport".into());
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
                return Err("invalid transport offer".into());
            }
        }
        let mut owners = BTreeSet::new();
        for owner in &self.grants {
            if owner.tenant_id.is_empty()
                || owner.tenant_id.len() > 96
                || !valid_id(&owner.user_id)
                || !owners.insert((&owner.tenant_id, &owner.user_id))
            {
                return Err("invalid or duplicate network grant".into());
            }
        }
        for route in &self.route_presets {
            if !self
                .destinations
                .iter()
                .any(|network| network.contains_prefix(route))
            {
                return Err("initial route exceeds published network access".into());
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
    #[serde(default)]
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
    pub interface_id: String,
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
    pub interface_id: String,
    /// 只有 Agent 与 Cubelet 都上报 [`ROUTE_SERVICE_CONTRACT`] 时才下发带端口的规则：
    /// 旧执行端会忽略这个字段，把它当成整段路由执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<RouteService>,
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
        self.priorities
            .sort_by(|a, b| a.interface_id.cmp(&b.interface_id));
        self.dns.dedup();
    }
    pub fn priority(&self, id: &str, ipv4: bool) -> Option<u16> {
        self.priorities
            .iter()
            .find(|p| p.interface_id == id)
            .and_then(|p| if ipv4 { p.ipv4 } else { p.ipv6 })
    }
    pub fn validate(&self, devices: &BTreeMap<String, AllocatedNic>) -> Result<(), String> {
        if self.routes.len() > MAX_NETWORK_ROUTES
            || self.priorities.len() > MAX_INTERFACES
            || self.dns.len() > 4
        {
            return Err("container routing configuration exceeds its limits".into());
        }
        let mut ids = BTreeSet::new();
        for priority in &self.priorities {
            let nic = devices
                .get(&priority.interface_id)
                .ok_or("priority references an unmounted NIC")?;
            if !ids.insert(&priority.interface_id)
                || priority.ipv4.is_some() && nic.ipv4.is_none()
                || priority.ipv6.is_some() && nic.ipv6.is_none()
            {
                return Err("invalid NIC priority or address family".into());
            }
        }
        let mut selections = BTreeMap::new();
        let mut services: Vec<(&IpPrefix, u16, RouteService, &String)> = Vec::new();
        for route in &self.routes {
            let nic = devices
                .get(&route.interface_id)
                .ok_or("route references an unmounted NIC")?;
            if !nic.supports(&route.destination) {
                return Err("route exceeds NIC network access or address family".into());
            }
            let priority = self
                .priority(&route.interface_id, route.destination.is_ipv4())
                .ok_or("route requires a NIC priority for its address family")?;
            if let Some(service) = route.service {
                if !service.valid() {
                    return Err("invalid route protocol or port range".into());
                }
                if services.iter().any(|(prefix, other_priority, other, id)| {
                    *prefix == &route.destination
                        && *other_priority == priority
                        && other.overlaps(&service)
                        && *id != &route.interface_id
                }) {
                    return Err(
                        "the same prefix, priority and ports cannot select different NICs".into(),
                    );
                }
                services.push((&route.destination, priority, service, &route.interface_id));
            } else if let Some(previous) =
                selections.insert((&route.destination, priority), &route.interface_id)
            {
                if previous != &route.interface_id {
                    return Err("the same prefix and priority cannot select different NICs".into());
                }
            }
        }
        if self
            .dns
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast() || ip.is_loopback())
        {
            return Err("invalid container DNS server".into());
        }
        Ok(())
    }
    /// Health is intentionally absent: an unavailable selected NIC cannot cause fallback.
    pub fn select(&self, address: IpAddr) -> Option<&str> {
        self.select_flow(address, None)
    }
    /// 目的网段越具体越优先；同一网段下，匹配协议端口的规则先于不带端口的规则；再按网卡优先级，
    /// 最后取端口段更窄的规则。不知道协议端口的流量（`flow` 为 `None`）只看不带端口的规则。
    pub fn select_flow(&self, address: IpAddr, flow: Option<(L4Protocol, u16)>) -> Option<&str> {
        self.routes
            .iter()
            .filter(|r| r.destination.contains(address))
            .filter(|r| match (r.service, flow) {
                (None, _) => true,
                (Some(service), Some((protocol, port))) => service.matches(protocol, port),
                (Some(_), None) => false,
            })
            .filter_map(|r| {
                self.priority(&r.interface_id, address.is_ipv4())
                    .map(|p| (r, p))
            })
            .min_by_key(|(r, p)| {
                (
                    std::cmp::Reverse(r.destination.length()),
                    r.service.is_none(),
                    *p,
                    r.service.map(|s| s.port_end - s.port_start),
                )
            })
            .map(|(r, _)| r.interface_id.as_str())
    }
    pub fn references(&self, id: &str) -> bool {
        self.routes.iter().any(|r| r.interface_id == id)
            || self.priorities.iter().any(|p| p.interface_id == id)
    }
    pub fn default_interface(&self, ipv4: bool) -> Option<&str> {
        self.routes
            .iter()
            .filter(|r| {
                r.service.is_none()
                    && r.destination.length() == 0
                    && r.destination.is_ipv4() == ipv4
            })
            .filter_map(|r| self.priority(&r.interface_id, ipv4).map(|p| (r, p)))
            .min_by_key(|(_, p)| *p)
            .map(|(r, _)| r.interface_id.as_str())
    }
    pub fn remove(&mut self, id: &str) {
        self.routes.retain(|r| r.interface_id != id);
        self.priorities.retain(|p| p.interface_id != id);
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
    #[serde(default)]
    pub entries: bool,
    /// Agent 能在宿主侧按协议端口执行出口规则（[`ROUTE_SERVICE_CONTRACT`]）。
    #[serde(default)]
    pub route_service: bool,
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
    #[serde(default)]
    pub budgets: BTreeMap<String, NicTrafficBudget>,
    pub generation: u64,
    pub bindings: Vec<NicBinding>,
    #[serde(default)]
    pub fences: Vec<NicResourceFence>,
    #[serde(default)]
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
                    interface_id: "nic-a".into(),
                    ipv4: Some(20),
                    ipv6: Some(1),
                },
                InterfacePriority {
                    interface_id: "nic-b".into(),
                    ipv4: Some(10),
                    ipv6: Some(2),
                },
            ],
            routes: vec![
                ContainerRoute {
                    destination: "0.0.0.0/0".parse().unwrap(),
                    interface_id: "nic-a".into(),
                    service: None,
                },
                ContainerRoute {
                    destination: "10.0.0.0/8".parse().unwrap(),
                    interface_id: "nic-b".into(),
                    service: None,
                },
                ContainerRoute {
                    destination: "10.1.0.0/16".parse().unwrap(),
                    interface_id: "nic-a".into(),
                    service: None,
                },
                ContainerRoute {
                    destination: "::/0".parse().unwrap(),
                    interface_id: "nic-b".into(),
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
            interface_id: id.into(),
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
                    interface_id: (*id).into(),
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
