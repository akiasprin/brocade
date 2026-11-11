//! 容器访问入口：任意机器的地址按 L3（整 IP）或 L4（协议 + 端口）投递到容器主网卡。
//!
//! 入口独立于业务网卡，不占用容器网卡槽位。入口机器与计算宿主不同时，两端经受管的 WireGuard
//! 机器对通道传递原始 IP 包：入口机器把目的地址换成容器的宿主内地址、把来源换成本端通道地址，按通道
//! 槽位的包标记送入通道；计算宿主再把来源换成 Cube 网关地址，经 CubeVS `primary_delivery` 交给容器
//! 主网卡，回包沿原路返回。通道地址由槽位派生（见 [`tunnel_addresses`]），两端的连接跟踪元组因此
//! 互不冲突，即使不同宿主上的容器地址相同。Guest 看到的来源是宿主网关，与旧交付一致，不声明保留
//! 访问者原始 IP。Cube 主网卡只有 IPv4，所以入口地址与投递目标都是 IPv4。
//!
//! 入口属于容器：删除容器即释放；同一用户的容器之间可以改派，地址与端口不变。
use super::{
    valid_id, L4Protocol, NetworkOwner, TransportKind, TransportListenSide, TransportOffer,
    MAX_CATALOG_ITEMS,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::Ipv4Addr};

/// Agent 与 Console 都支持入口投递的协议版本。
pub const ENTRY_PROTOCOL_VERSION: u32 = 67;
/// Agent 上报这个契约（[`super::NicNodeCapabilities::entries`]）且计算宿主的 Cube 上报
/// `primary_ip_delivery` 时，才向该节点下发入口。
pub const ENTRY_CONTRACT: &str = "cvm-entry-delivery-v1";
pub const MAX_ENTRY_ADDRESSES: usize = 256;
pub const MAX_ENTRY_PUBLICATIONS: usize = 256;
pub const MAX_ENTRIES_PER_NODE: usize = 4096;
pub const MAX_ENTRIES_PER_INSTANCE: usize = 64;
pub const MAX_ENTRY_TUNNELS: usize = 256;
/// 全局通道槽位数。槽位在所有机器上唯一，节点据此派生通道地址、包标记与路由表。
pub const ENTRY_TUNNEL_SLOTS: u16 = 32768;
/// 通道地址取自不会出现在公网的 240.0.0.0/15（E 类保留段，Linux 作单播使用）；网卡隔离已占用
/// 198.18.0.0/15。地址只存在于通道两端，不会离开通道。
pub const ENTRY_TUNNEL_NETWORK: Ipv4Addr = Ipv4Addr::new(240, 0, 0, 0);
/// 入口机器为每条通道使用的包标记与路由表基数：`ENTRY_TUNNEL_MARK + slot`。
pub const ENTRY_TUNNEL_MARK: u32 = 0x00b6_0000;

/// 入口按协议分配端口；`TcpUdp` 同时占用两种协议的同一端口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryProtocol {
    Tcp,
    Udp,
    TcpUdp,
}
impl EntryProtocol {
    pub fn covers(self, protocol: L4Protocol) -> bool {
        matches!(
            (self, protocol),
            (Self::TcpUdp, _) | (Self::Tcp, L4Protocol::Tcp) | (Self::Udp, L4Protocol::Udp)
        )
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.protocols()
            .into_iter()
            .any(|protocol| other.covers(protocol))
    }
    /// 实际占用的传输层协议，按固定顺序返回。
    pub fn protocols(self) -> Vec<L4Protocol> {
        [L4Protocol::Tcp, L4Protocol::Udp]
            .into_iter()
            .filter(|protocol| self.covers(*protocol))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryMode {
    /// 整个地址一对一交给容器，包含全部协议与 ICMP。
    L3,
    /// 只转发所选协议的一个入口端口到容器端口。
    L4 {
        protocol: EntryProtocol,
        listen_port: u16,
        target_port: u16,
    },
}
impl EntryMode {
    pub fn valid(&self) -> bool {
        !matches!(
            self,
            Self::L4 { listen_port: 0, .. } | Self::L4 { target_port: 0, .. }
        )
    }
}

/// 机器发布的一个入口地址。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryAddressKind {
    /// 机器自己的地址：按协议端口分给多个容器；宿主正在监听的端口不能分配。
    Shared { port_start: u16, port_end: u16 },
    /// 独立地址：整 IP 交给一个容器。
    Dedicated,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryAddressOffer {
    /// 数据包到达这台机器时的目的地址；共享地址必须是机器网卡上的实际地址。
    pub address: Ipv4Addr,
    /// 云厂商一对一 NAT 时访问者使用的公网地址，只用于展示；为空时与 `address` 相同。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_address: Option<Ipv4Addr>,
    #[serde(flatten)]
    pub kind: EntryAddressKind,
}
impl EntryAddressOffer {
    /// 这个地址能否承接一条入口；端口与宿主监听、其他入口的冲突由 Store 统一检查。
    pub fn permits(&self, mode: &EntryMode) -> bool {
        match (self.kind, mode) {
            (EntryAddressKind::Dedicated, EntryMode::L3) => true,
            (
                EntryAddressKind::Shared {
                    port_start,
                    port_end,
                },
                EntryMode::L4 { listen_port, .. },
            ) => (port_start..=port_end).contains(listen_port),
            _ => false,
        }
    }
    pub fn visible_address(&self) -> Ipv4Addr {
        self.public_address.unwrap_or(self.address)
    }
}

/// 管理员按机器发布的入口地址，授权方式与出口网络相同。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPublicationSpec {
    pub name: String,
    pub provider_node: String,
    pub addresses: Vec<EntryAddressOffer>,
    /// 计算宿主不是这台机器时，机器对通道使用的承载报价。
    pub transports: Vec<TransportOffer>,
    pub grants: Vec<NetworkOwner>,
    /// 这份发布最多同时承接的入口数。
    pub max_entries: u16,
}
impl EntryPublicationSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 80
            || self.name.chars().any(char::is_control)
            || !valid_id(&self.provider_node)
            || self.addresses.is_empty()
            || self.addresses.len() > MAX_ENTRY_ADDRESSES
            || self.transports.len() > 2
            || self.grants.len() > MAX_CATALOG_ITEMS
            || self.max_entries == 0
            || usize::from(self.max_entries) > MAX_ENTRIES_PER_NODE
        {
            return Err("CVM entry publication exceeds its limits".into());
        }
        let mut addresses = BTreeSet::new();
        for offer in &self.addresses {
            if !entry_address(offer.address)
                || offer.public_address.is_some_and(|ip| !entry_address(ip))
                || !addresses.insert(offer.address)
            {
                return Err("CVM entry address is invalid or repeated".into());
            }
            if let EntryAddressKind::Shared {
                port_start,
                port_end,
            } = offer.kind
            {
                if port_start == 0 || port_end < port_start {
                    return Err("CVM shared entry port range is invalid".into());
                }
            }
        }
        let mut kinds = BTreeSet::new();
        for offer in &self.transports {
            // 机器对通道首版只用 WireGuard：内核设备可以按标记路由，不需要用户态转发。
            if offer.kind != TransportKind::Wireguard {
                return Err("CVM entry tunnels currently use WireGuard only".into());
            }
            if !kinds.insert(offer.kind)
                || offer.port_start == 0
                || offer.port_end < offer.port_start
                || !(1280..=9000).contains(&offer.mtu)
                || offer.endpoint.is_unspecified()
                || offer.endpoint.is_multicast()
                || offer.endpoint.is_loopback()
            {
                return Err("CVM entry transport offer is invalid".into());
            }
        }
        let mut owners = BTreeSet::new();
        if self.grants.iter().any(|owner| {
            !valid_id(&owner.tenant_id)
                || !valid_id(&owner.user_id)
                || !owners.insert((owner.tenant_id.as_str(), owner.user_id.as_str()))
        }) {
            return Err("CVM entry grant is invalid or repeated".into());
        }
        Ok(())
    }
    pub fn offer(&self, address: Ipv4Addr) -> Option<&EntryAddressOffer> {
        self.addresses.iter().find(|offer| offer.address == address)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPublication {
    pub id: String,
    pub revision: u64,
    pub admitting: bool,
    pub retired: bool,
    pub spec: EntryPublicationSpec,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPublicationWrite {
    pub request_id: String,
    pub expected_revision: u64,
    pub spec: EntryPublicationSpec,
}

/// 一个容器的访问入口。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub publication_id: String,
    pub provider_node: String,
    pub instance_id: String,
    pub address: Ipv4Addr,
    pub mode: EntryMode,
}
impl Entry {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || !valid_id(&self.publication_id)
            || !valid_id(&self.provider_node)
            || !valid_id(&self.instance_id)
            || !entry_address(self.address)
            || !self.mode.valid()
        {
            return Err("CVM entry identity or ports are invalid".into());
        }
        Ok(())
    }
    /// 同一机器同一地址上：整 IP 与任何入口冲突；端口入口只在协议重叠且入口端口相同时冲突。
    pub fn conflicts(&self, other: &Self) -> bool {
        self.provider_node == other.provider_node
            && self.address == other.address
            && modes_conflict(self.mode, other.mode)
    }
}
pub fn modes_conflict(a: EntryMode, b: EntryMode) -> bool {
    match (a, b) {
        (
            EntryMode::L4 {
                protocol: a,
                listen_port: x,
                ..
            },
            EntryMode::L4 {
                protocol: b,
                listen_port: y,
                ..
            },
        ) => x == y && a.overlaps(b),
        _ => true,
    }
}

/// 新增入口；`instance_id` 是目标容器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryCreate {
    pub request_id: String,
    pub instance_id: String,
    pub publication_id: String,
    pub address: Ipv4Addr,
    pub mode: EntryMode,
}
/// 改派入口到同一用户的另一个容器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRepoint {
    pub request_id: String,
    pub expected_revision: u64,
    pub instance_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRelease {
    pub request_id: String,
    pub expected_revision: u64,
}

/// 网卡隔离使用的 198.18.0.0/15 和 E 类保留段不能作为入口或投递目标。
pub fn entry_address(ip: Ipv4Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && ip.octets()[0] < 240
        && u32::from(ip) & 0xfffe_0000 != u32::from(Ipv4Addr::new(198, 18, 0, 0))
}
/// 槽位对应的 /30 通道地址：入口机器一端、计算宿主一端。
pub fn tunnel_addresses(slot: u16) -> Option<(Ipv4Addr, Ipv4Addr)> {
    if slot >= ENTRY_TUNNEL_SLOTS {
        return None;
    }
    let base = u32::from(ENTRY_TUNNEL_NETWORK) + u32::from(slot) * 4;
    Some((Ipv4Addr::from(base + 1), Ipv4Addr::from(base + 2)))
}

/// 入口机器上的一条转发规则：入口地址（端口）→ 容器的宿主内地址（端口）。
/// `tunnel` 为空表示容器就在这台机器上，直接交给本机 CubeVS；`target` 为空表示容器当前没有
/// 已确认的地址（例如已关机），入口仍占用地址与端口并丢弃流量，不能落到宿主服务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRule {
    pub id: String,
    pub address: Ipv4Addr,
    pub mode: EntryMode,
    pub target: Option<Ipv4Addr>,
    pub tunnel: Option<String>,
}
/// 计算宿主上的一条投递：从机器对通道收到的包交给容器主网卡，回包沿通道返回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDelivery {
    pub id: String,
    pub instance_id: String,
    pub mode: EntryMode,
    pub target: Ipv4Addr,
    pub tunnel: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryTunnelSide {
    /// 入口机器一端（承载的提供侧）。
    Entry,
    /// 计算宿主一端（承载的消费侧）。
    Target,
}
/// 节点私有的机器对通道。承载字段与网卡承载相同，只经节点认证端点下发，
/// 不出现在管理端或容器接口中。`offer.listen_side` 已解析：`Provider` 表示入口机器监听。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryTunnel {
    pub id: String,
    pub side: EntryTunnelSide,
    pub entry_node: String,
    pub target_node: String,
    pub offer: TransportOffer,
    pub port: u16,
    /// 全局唯一的槽位，见 [`ENTRY_TUNNEL_SLOTS`]。
    pub slot: u16,
    pub credential: String,
    pub peer_credential: String,
}
impl std::fmt::Debug for EntryTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryTunnel")
            .field("id", &self.id)
            .field("side", &self.side)
            .field("entry_node", &self.entry_node)
            .field("target_node", &self.target_node)
            .field("transport", &self.offer.kind)
            .finish_non_exhaustive()
    }
}
impl EntryTunnel {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || !valid_id(&self.entry_node)
            || !valid_id(&self.target_node)
            || self.entry_node == self.target_node
            || self.offer.listen_side == TransportListenSide::Auto
            || self.offer.kind != TransportKind::Wireguard
            || self.port == 0
            || !(self.offer.port_start..=self.offer.port_end).contains(&self.port)
            || !(1280..=9000).contains(&self.offer.mtu)
            || self.offer.endpoint.is_unspecified()
            || self.slot >= ENTRY_TUNNEL_SLOTS
            || self.credential.is_empty()
            || self.peer_credential.is_empty()
            || self.credential.len() > 16 * 1024
            || self.peer_credential.len() > 16 * 1024
        {
            return Err("invalid CVM entry tunnel assignment".into());
        }
        Ok(())
    }
    /// 这一端是否负责监听承载端口。
    pub fn listens(&self) -> bool {
        matches!(
            (self.side, self.offer.listen_side),
            (EntryTunnelSide::Entry, TransportListenSide::Provider)
                | (EntryTunnelSide::Target, TransportListenSide::Consumer)
        )
    }
    pub fn peer_node(&self) -> &str {
        match self.side {
            EntryTunnelSide::Entry => &self.target_node,
            EntryTunnelSide::Target => &self.entry_node,
        }
    }
}

/// 一个节点的入口期望状态：作为入口机器的规则、作为计算宿主的投递，以及两类共用的机器对通道。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDesired {
    pub node_id: String,
    pub generation: u64,
    pub rules: Vec<EntryRule>,
    pub deliveries: Vec<EntryDelivery>,
    pub tunnels: Vec<EntryTunnel>,
}
impl EntryDesired {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.node_id)
            || self.rules.len() > MAX_ENTRIES_PER_NODE
            || self.deliveries.len() > MAX_ENTRIES_PER_NODE
            || self.tunnels.len() > MAX_ENTRY_TUNNELS
        {
            return Err("CVM entry desired state exceeds its limits".into());
        }
        let mut tunnels = BTreeSet::new();
        let mut slots = BTreeSet::new();
        for tunnel in &self.tunnels {
            tunnel.validate()?;
            let local_side = match tunnel.side {
                EntryTunnelSide::Entry => &tunnel.entry_node,
                EntryTunnelSide::Target => &tunnel.target_node,
            };
            if local_side != &self.node_id
                || !tunnels.insert(tunnel.id.as_str())
                || !slots.insert(tunnel.slot)
            {
                return Err("repeated or foreign CVM entry tunnel".into());
            }
        }
        let tunnel_side = |id: &str, side: EntryTunnelSide| {
            self.tunnels
                .iter()
                .any(|tunnel| tunnel.id == id && tunnel.side == side)
        };
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if !valid_id(&rule.id)
                || !ids.insert(("rule", rule.id.as_str()))
                || !entry_address(rule.address)
                || !rule.mode.valid()
                || rule.target.is_some_and(|ip| !entry_address(ip))
                || rule
                    .tunnel
                    .as_deref()
                    .is_some_and(|id| !tunnel_side(id, EntryTunnelSide::Entry))
            {
                return Err("invalid CVM entry rule".into());
            }
        }
        for delivery in &self.deliveries {
            if !valid_id(&delivery.id)
                || !valid_id(&delivery.instance_id)
                || !ids.insert(("delivery", delivery.id.as_str()))
                || !delivery.mode.valid()
                || !entry_address(delivery.target)
                || !tunnel_side(&delivery.tunnel, EntryTunnelSide::Target)
            {
                return Err("invalid CVM entry delivery".into());
            }
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if self.rules[index + 1..]
                .iter()
                .any(|other| rule.address == other.address && modes_conflict(rule.mode, other.mode))
            {
                return Err("conflicting CVM entry rules".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryItemReport {
    pub id: String,
    pub configured: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryTunnelReport {
    pub id: String,
    pub connected: bool,
    pub error: Option<String>,
}
/// `error` 为空表示整份期望状态已收敛：被移除的规则、投递与通道已经不存在。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryNodeReport {
    pub generation: u64,
    pub rules: Vec<EntryItemReport>,
    pub deliveries: Vec<EntryItemReport>,
    pub tunnels: Vec<EntryTunnelReport>,
    pub error: Option<String>,
}
impl EntryNodeReport {
    pub fn validate(&self) -> Result<(), String> {
        let text = |value: &Option<String>| value.as_ref().is_some_and(|e| e.len() > 500);
        if self.rules.len() > MAX_ENTRIES_PER_NODE
            || self.deliveries.len() > MAX_ENTRIES_PER_NODE
            || self.tunnels.len() > MAX_ENTRY_TUNNELS
            || text(&self.error)
            || self
                .rules
                .iter()
                .chain(&self.deliveries)
                .any(|item| !valid_id(&item.id) || text(&item.error))
            || self
                .tunnels
                .iter()
                .any(|item| !valid_id(&item.id) || text(&item.error))
        {
            return Err("invalid CVM entry report".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(address: &str, mode: EntryMode) -> Entry {
        Entry {
            id: "e1".into(),
            publication_id: "hk".into(),
            provider_node: "hk-bgp".into(),
            instance_id: "web".into(),
            address: address.parse().unwrap(),
            mode,
        }
    }
    fn l4(protocol: EntryProtocol, listen_port: u16) -> EntryMode {
        EntryMode::L4 {
            protocol,
            listen_port,
            target_port: 443,
        }
    }
    #[test]
    fn whole_ip_owns_the_address_and_ports_share_it_by_protocol() {
        let tcp = entry("203.0.113.12", l4(EntryProtocol::Tcp, 30443));
        let udp = entry("203.0.113.12", l4(EntryProtocol::Udp, 30443));
        let both = entry("203.0.113.12", l4(EntryProtocol::TcpUdp, 30443));
        assert!(!tcp.conflicts(&udp));
        assert!(both.conflicts(&tcp) && both.conflicts(&udp));
        assert!(!tcp.conflicts(&entry("203.0.113.12", l4(EntryProtocol::Tcp, 30022))));
        assert!(entry("203.0.113.12", EntryMode::L3).conflicts(&tcp));
        assert!(!entry("203.0.113.13", EntryMode::L3).conflicts(&tcp));
        assert!(entry("203.0.113.12", l4(EntryProtocol::Tcp, 0))
            .validate()
            .is_err());
        assert!(entry("198.18.0.9", EntryMode::L3).validate().is_err());
        assert_eq!(
            EntryProtocol::TcpUdp.protocols(),
            vec![L4Protocol::Tcp, L4Protocol::Udp]
        );
    }
    #[test]
    fn shared_addresses_take_ports_and_dedicated_addresses_take_whole_ips() {
        let shared = EntryAddressOffer {
            address: "10.0.0.12".parse().unwrap(),
            public_address: Some("203.0.113.12".parse().unwrap()),
            kind: EntryAddressKind::Shared {
                port_start: 30000,
                port_end: 30999,
            },
        };
        let dedicated = EntryAddressOffer {
            address: "198.51.100.210".parse().unwrap(),
            public_address: None,
            kind: EntryAddressKind::Dedicated,
        };
        assert!(shared.permits(&l4(EntryProtocol::Tcp, 30022)));
        assert!(!shared.permits(&l4(EntryProtocol::Tcp, 22)));
        assert!(!shared.permits(&EntryMode::L3));
        assert!(dedicated.permits(&EntryMode::L3));
        assert!(!dedicated.permits(&l4(EntryProtocol::Tcp, 30022)));
        assert_eq!(shared.visible_address().to_string(), "203.0.113.12");
        assert_eq!(dedicated.visible_address(), dedicated.address);
        let json = serde_json::to_value(shared).unwrap();
        assert_eq!(json["kind"], "shared");
        assert_eq!(json["port_start"], 30000);
        assert!(serde_json::to_value(dedicated)
            .unwrap()
            .get("public_address")
            .is_none());
    }
    #[test]
    fn tunnel_slots_map_to_disjoint_class_e_pairs() {
        assert_eq!(
            tunnel_addresses(0).unwrap(),
            ("240.0.0.1".parse().unwrap(), "240.0.0.2".parse().unwrap())
        );
        let (entry, target) = tunnel_addresses(ENTRY_TUNNEL_SLOTS - 1).unwrap();
        assert_eq!(entry.to_string(), "240.1.255.253");
        assert_eq!(target.to_string(), "240.1.255.254");
        assert!(tunnel_addresses(ENTRY_TUNNEL_SLOTS).is_none());
        assert!(!entry_address(entry) && !entry_address(target));
    }
    #[test]
    fn desired_state_rejects_conflicts_and_unknown_tunnels_and_hides_secrets() {
        let tunnel = EntryTunnel {
            id: "t1".into(),
            side: EntryTunnelSide::Entry,
            entry_node: "hk-bgp".into(),
            target_node: "jp-iij".into(),
            offer: TransportOffer {
                listen_side: TransportListenSide::Provider,
                kind: TransportKind::Wireguard,
                endpoint: "203.0.113.12".parse().unwrap(),
                port_start: 51820,
                port_end: 51829,
                mtu: 1420,
            },
            port: 51820,
            slot: 7,
            credential: "a".repeat(44),
            peer_credential: "b".repeat(44),
        };
        assert!(!format!("{tunnel:?}").contains(&"a".repeat(44)));
        assert!(tunnel.listens());
        assert_eq!(tunnel.peer_node(), "jp-iij");
        let rule = |id: &str, mode: EntryMode, tunnel: Option<&str>| EntryRule {
            id: id.into(),
            address: "203.0.113.12".parse().unwrap(),
            mode,
            target: Some("192.168.100.56".parse().unwrap()),
            tunnel: tunnel.map(Into::into),
        };
        let mut desired = EntryDesired {
            node_id: "hk-bgp".into(),
            generation: 1,
            rules: vec![
                rule("a", l4(EntryProtocol::Tcp, 30022), Some("t1")),
                rule("b", l4(EntryProtocol::Udp, 30022), None),
            ],
            deliveries: vec![],
            tunnels: vec![tunnel.clone()],
        };
        assert!(desired.validate().is_ok());
        desired
            .rules
            .push(rule("c", l4(EntryProtocol::TcpUdp, 30022), None));
        assert!(desired.validate().is_err());
        desired.rules.pop();
        desired
            .rules
            .push(rule("c", l4(EntryProtocol::Tcp, 30023), Some("missing")));
        assert!(desired.validate().is_err());
        desired.rules.pop();
        // 计算宿主一端的通道不能承接入口规则，也不能出现在别的节点的期望状态里。
        desired.deliveries.push(EntryDelivery {
            id: "d".into(),
            instance_id: "web".into(),
            mode: EntryMode::L3,
            target: "192.168.100.56".parse().unwrap(),
            tunnel: "t1".into(),
        });
        assert!(desired.validate().is_err());
        desired.deliveries.clear();
        desired.node_id = "jp-iij".into();
        assert!(desired.validate().is_err());
    }
}
