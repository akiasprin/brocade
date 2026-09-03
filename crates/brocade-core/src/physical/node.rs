use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use sha2::{Digest, Sha256};

use crate::{
    hash::hex_lower,
    ir::{
        hops::{HopDialWire, HopPath},
        routing::{egress_tag as routing_egress_tag, AppIr, DestMatch, Ingress, Rule},
        system::{Dial, Link, LinkWrap, SystemIr, SystemNode},
    },
    model::{
        Action, AnyTls, Dns, DomainStrategy, EgressDnsAddressStrategy, EgressDnsFallback,
        EgressDnsResolution, EgressDnsTransport, ExternalOutboundProtocol,
        ExternalOutboundSecurity, GeodataSettings, HopPool, HopWire, IngressGuard, Network,
        RealityClientPolicy, RealityFallbackLimits, RealityFallbackRateLimit, RealitySettings,
        Transport, Xhttp,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePlan {
    pub node_id: String,
    pub wireguard: Option<WireGuardPlan>,
    /// Present only where fake TCP is used: declaring fake TCP means hosting a server,
    /// and dialing another node's fake-TCP port means starting a client.
    pub phantun: Option<PhantunPlan>,
    pub xray: Option<XrayPlan>,
    pub grant_sync: GrantSyncPlan,
    /// The UDP ranges this machine redirects onto a single listening port, one per hopping
    /// Hysteria 2 ingress. Empty on a machine with no hopping ingress.
    ///
    /// Machine-level state rather than part of the xray artifact: xray binds one port and has no
    /// knowledge of the range, so its config does not change when a range does. Keeping it
    /// separate also allows the redirect to be removed without restarting xray.
    pub hy2_port_hops: Vec<Hy2PortHopPlan>,
}

/// One `start..=end` UDP range redirected to `to` on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hy2PortHopPlan {
    pub start: u16,
    pub end: u16,
    /// The listening port. Inside the range by construction, because validation rejects a
    /// range that excludes it (`ingress.hy2-hop-listener`).
    pub to: u16,
}

/// The phantun instances on this machine.
///
/// phantun only adds a synthetic TCP header to pass filters and performs neither
/// retransmission nor congestion control, so it avoids the TCP-over-TCP degradation in
/// which the two layers amplify each other under loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunPlan {
    pub node_id: String,
    /// The servers to host here: accept TCP, forward to the local wg UDP port.
    ///
    /// A list, because one public machine may serve as the endpoint for several peers
    /// behind NAT, each on the port that peer declared (rule 3 of `LinkWrap`).
    /// Deduplicated by TCP port: one instance per port is sufficient, since a phantun
    /// server accepts many clients.
    pub servers: Vec<PhantunServerPlan>,
    /// One client per peer dialed over fake TCP.
    pub clients: Vec<PhantunClientPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunServerPlan {
    pub tcp_port: u16,
    /// The local wg UDP port.
    pub forward_to_udp_port: u16,
    /// The peers arriving through this port. Their packets emerge only from the tun, so
    /// at runtime these peers' endpoints have to be loopback addresses. A public address
    /// means roaming has moved the endpoint, and the agent clears it so it is relearned.
    pub peers: Vec<String>,
    pub tun: PhantunTun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunClientPlan {
    pub peer_node_id: String,
    /// The UDP port on local loopback that `wg0.conf`'s `Endpoint` points at.
    pub listen_udp_port: u16,
    /// The peer's fake-TCP endpoint, as `host:port`.
    pub remote_tcp_endpoint: String,
    pub tun: PhantunTun,
}

/// One phantun instance's TUN device and the addresses at both its ends.
///
/// Each instance needs its own pair. phantun defaults every instance to
/// `192.168.200.1/2`, so two on one machine collide, and the symptom is either the second
/// failing to start or packets crossing to the first. Each instance index therefore gets a
/// /30: `192.168.200.{4i+1}` is the kernel end and `{4i+2}` the phantun end.
///
/// Device names also have to differ, or `ip link add` fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunTun {
    pub name: String,
    pub local: Ipv4Addr,
    pub peer: Ipv4Addr,
}

/// The TUN address range. 192.168.200.0/24 matches phantun's own default, so the
/// addresses an operator finds in its documentation are the ones on the machine.
const PHANTUN_TUN_NET: [u8; 3] = [192, 168, 200];

/// The TUN for instance `index`. The index is assigned as servers from 0, then clients in
/// peer-id order. Like the local ports, this is a pure function, so the same model yields
/// the same result on every build.
fn phantun_tun(prefix: &str, index: usize) -> PhantunTun {
    let base = 4 * index as u8;
    PhantunTun {
        name: format!("bt{prefix}{index}"),
        local: Ipv4Addr::new(
            PHANTUN_TUN_NET[0],
            PHANTUN_TUN_NET[1],
            PHANTUN_TUN_NET[2],
            base + 1,
        ),
        peer: Ipv4Addr::new(
            PHANTUN_TUN_NET[0],
            PHANTUN_TUN_NET[1],
            PHANTUN_TUN_NET[2],
            base + 2,
        ),
    }
}

/// The base for phantun clients' local ports.
///
/// It is computed by a pure function, because compiling one model twice has to produce
/// byte-identical output, which rules out random values and runtime port probing. Ports
/// are assigned in peer-id order, so adding a machine shifts the local ports of every peer
/// after it. Under a full mesh, adding a machine already rewrites every peer table, so
/// this adds no further churn.
pub const PHANTUN_LOCAL_PORT_BASE: u16 = 29000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardPlan {
    pub private_key: String,
    pub address: Ipv4Addr,
    pub listen_port: Option<u16>,
    pub mtu: Option<u16>,
    pub peers: Vec<WireGuardPeerPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardPeerPlan {
    pub node_id: String,
    pub public_key: String,
    pub allowed_ip: Ipv4Addr,
    pub endpoint: Option<String>,
    pub persistent_keepalive: Option<u16>,
}

/// A machine's connection policy after the fallbacks are applied.
///
/// `buffer_size_kb` stays optional through every layer: absent means the artifact writes no
/// `bufferSize` key, and xray then selects by CPU architecture. Substituting a number here
/// would apply one value to both arm64's 4 KB case and x86_64's 512 KB case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedConnection {
    pub conn_idle_secs: u32,
    pub uplink_only_secs: u32,
    pub downlink_only_secs: u32,
    pub buffer_size_kb: Option<u32>,
    pub handshake_secs: u32,
    /// Fleet-wide, so it comes from the settings with no node override applied.
    pub stats_user_online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayPlan {
    pub node_id: String,
    pub api_port: Option<u16>,
    pub reality_client: RealityClientPolicy,
    pub dns: Dns,
    /// See `model::DomainStrategy`. Every freedom outbound this machine emits carries it,
    /// including the internal direct one, because it is a property of the machine's egress
    /// and `out:internal` is also egress.
    pub domain_strategy: DomainStrategy,
    /// Already merged: this machine's overrides applied over the global defaults. Merging
    /// here rather than in the artifact keeps the precedence rule in one place and out of
    /// the renderer, which then only writes values.
    pub connection: ResolvedConnection,
    pub dns_route: Option<String>,
    /// Machine-owned domain-scoped resolvers. They are independent of the machine default above:
    /// each query is tagged and routed through a dedicated direct Freedom outbound. They do not
    /// inherit any chain rule's source address or outbound context.
    pub egress_dns: Vec<XrayEgressDnsPlan>,
    pub inbounds: Vec<XrayIngressPlan>,
    /// The relay inbounds on this machine, one per chain. A relay serving two chains
    /// has two entries here, with independent ports and wire formats.
    pub hop_inbounds: Vec<XrayHopInboundPlan>,
    pub forward_outbounds: Vec<XrayForwardOutboundPlan>,
    pub egress_outbounds: Vec<XrayEgressOutboundPlan>,
    /// External proxy resources referenced by this machine's rules. Only referenced resources
    /// are emitted, so credentials do not fan out to unrelated fleet nodes.
    pub external_outbounds: Vec<XrayExternalOutboundPlan>,
    pub block_outbound: bool,
    pub routing_rules: Vec<XrayRoutingRulePlan>,
    /// Reverse access's upstream half: wait for the downstream to connect, then send
    /// traffic back along that connection.
    pub reverse_portals: Vec<XrayReversePortalPlan>,
    /// Reverse access's downstream half: dial the upstream, attach the connection, and
    /// wait for traffic.
    pub reverse_bridges: Vec<XrayReverseBridgePlan>,
    /// Automatic `geoip.dat` / `geosite.dat` updates, verbatim from the global
    /// settings.
    ///
    /// Every machine carries it, regardless of `egress_allowed`. That switch controls
    /// whether user traffic may exit here, which is policy, while reaching an external
    /// URL is a capability a relay with a public IP already has. The two are independent,
    /// so a relay updates its own .dat files as well, and relays are where `geosite:`
    /// rules are most numerous, because branching happens on relays and exits happen on
    /// egress nodes.
    pub geodata: GeodataSettings,
}

// Reverse access's two ends. They were previously paired by an agreed `domain`, a token
// both sides had to write identically for xray to recognize the control connection. That
// pairing has been removed: xray now negotiates the tunnel inside VLESS, so the two ends
// are joined by the credential instead. The portal's tag is attached to the downstream's
// client entry, and the bridge's tag to the dialling outbound. This removed one failure
// mode, where a mismatched token left both sides' artifacts correct-looking while no
// tunnel was established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayReversePortalPlan {
    pub tag: String,
    /// The identity the downstream connects with (xray's client email). The tag is
    /// attached to this credential, so holding it is what authorizes attaching the
    /// tunnel, and no other client on this port can take it over.
    pub peer_label: String,
    /// The inbound the downstream connects to, which is this machine's relay port on
    /// this chain.
    pub inbound_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayReverseBridgePlan {
    pub tag: String,
    /// The outbound used to dial the upstream. The reverse tunnel's TCP connection
    /// originates here.
    pub dial_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayIngressPlan {
    pub id: String,
    pub tag: String,
    pub listen: IpAddr,
    pub port: u16,
    pub sniff: bool,
    pub protocol: IngressProtocol,
    pub security: IngressSecurity,
    /// Certificate name managed for this node. REALITY reads it only when its fallback mode is
    /// local; TLS needs only the files, because the name is already inside the certificate.
    pub certificate_name: Option<String>,
    /// The HTTP layer this ingress is carried inside, or `None` for one carried over plain TCP.
    /// Flattened out of the model's [`Transport`](crate::model::Transport) here because from the
    /// artifact layer down, the shapes differ only by whether this is present.
    pub xhttp: Option<Xhttp>,
    /// Present only for REALITY + XHTTP with an independent download projection.
    /// The public upload and TLS download listeners terminate their security layers and forward
    /// the clear HTTP stream into one loopback XHTTP inbound. Keeping the original tag on that
    /// core means grants, routing and usage attribution remain attached to the logical ingress.
    pub split: Option<XraySplitIngressPlan>,
    /// Compiler-owned loopback TLS listener used as REALITY's target. Ordinary HTTPS reaches only
    /// this listener and is routed to a fixed 403 response, never to the shared XHTTP core.
    pub cover_port: Option<u16>,
    /// Compiler-owned loopback listener placed between REALITY and the impersonated site,
    /// present when [`RealitySettings::guards_fallback`] holds. It receives the fallback's raw
    /// TLS stream, reads the name the client requested, and lets routing decide whether that
    /// name is one this ingress impersonates. The impersonated site's address also answers for
    /// its CDN neighbours, and REALITY itself does not distinguish them.
    ///
    /// Mutually exclusive with `cover_port` by construction: a local cover is already a listener
    /// on this machine, with no external name to confine.
    pub guard_port: Option<u16>,
}

// Not boxed, for the same reason as `IngressSecurity` in this file: a plan holds one of these
// per ingress, so boxing saves a negligible amount, and the indirection would need explaining
// to every later reader.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressProtocol {
    Vless,
    AnyTls(AnyTls),
    Hysteria2(crate::model::Hysteria2),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XraySplitIngressPlan {
    pub core_port: u16,
    pub download_ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrayFallbackRateLimitPlan {
    pub after_bytes: u64,
    pub bytes_per_sec: u64,
    pub burst_bytes_per_sec: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrayFallbackLimitsPlan {
    pub upload: XrayFallbackRateLimitPlan,
    pub download: XrayFallbackRateLimitPlan,
}

/// Which certificate the inbound presents, and what it needs to present it.
///
/// The two branches carry different fields because their requirements differ: REALITY needs a
/// keypair and an external name, while TLS needs only a file on disk, because the name is inside
/// the certificate and an inbound is never told its own name.
// Not boxed, for the same reason as `xray::Config`: the variants differ by about 200 bytes and a
// plan holds one of these per ingress, so a machine's whole plan saves single-digit kilobytes at
// most. Against that, a Box would require every reader of this type to work out what the
// indirection is for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressSecurity {
    Reality {
        params: RealitySettings,
        /// The ingress's own secret, used as REALITY's private key.
        private_key: String,
        short_ids: Vec<String>,
    },
    Tls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayHopInboundPlan {
    /// The chain this inbound belongs to, for display and troubleshooting. The identifier
    /// written into xray is `tag`.
    pub chain: String,
    /// This chain's relay inbound tag on this machine. Namespaced by app, so identically
    /// named chains from different apps do not collide when both reach one xray.
    pub tag: String,
    pub listen: IpAddr,
    pub port: u16,
    /// This machine's relay-port material for this chain, including the private key.
    pub security: HopWire,
    pub clients: Vec<XrayClientPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayClientPlan {
    pub uuid: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayForwardOutboundPlan {
    pub tag: String,
    pub address: String,
    pub port: u16,
    pub uuid: String,
    pub security: HopDialWire,
    pub pool: HopPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayEgressOutboundPlan {
    pub tag: String,
    pub send_through: Option<IpAddr>,
    /// `None` inherits the machine strategy. A custom resolver needs an outbound which always
    /// resolves, even when the machine default is `AsIs`, so it carries its own concrete value.
    pub domain_strategy: Option<DomainStrategy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayEgressDnsPlan {
    pub tag: String,
    pub outbound_tag: String,
    pub address: String,
    pub port: u16,
    pub transport: EgressDnsTransport,
    pub address_strategy: EgressDnsAddressStrategy,
    pub fallback: EgressDnsFallback,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayExternalOutboundPlan {
    pub tag: String,
    pub address: String,
    pub port: u16,
    pub protocol: ExternalOutboundProtocol,
    pub security: ExternalOutboundSecurity,
    /// Only meaningful for a WireGuard protocol after a managed WARP target is lowered. Zero
    /// leaves wireguard-go's automatic worker selection in control.
    pub wireguard_workers: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayRoutingRulePlan {
    pub selector: XrayRuleSelector,
    pub dest_match: DestMatch,
    pub outbound_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayRuleSelector {
    InboundTags(Vec<String>),
    Users(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSyncPlan {
    pub node_id: String,
    pub updates: Vec<GrantInboundUpdatePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInboundUpdatePlan {
    pub inbound_tag: String,
    pub clients: Vec<GrantClientPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantClientPlan {
    pub uuid: String,
    pub label: String,
    pub flow: Option<String>,
}

pub fn build_node_plan(sys: &SystemIr, node_id: &str) -> NodePlan {
    project_node(sys, &[], node_id)
}

pub fn project_node(sys: &SystemIr, apps: &[AppIr], node_id: &str) -> NodePlan {
    NodePlan {
        node_id: node_id.to_owned(),
        wireguard: wireguard_plan(sys, node_id),
        phantun: phantun_plan(sys, node_id),
        xray: xray_plan(sys, apps, node_id),
        grant_sync: grant_sync_plan(apps, node_id),
        hy2_port_hops: hy2_port_hops(apps, node_id),
    }
}

/// The redirects this machine needs, in a fixed order so that repeated builds are byte-identical.
///
/// Deduplicated: two ingresses cannot legitimately claim the same range, which validation
/// reports as `node.port-clash`, but a snapshot that reaches this point despite that must not
/// compile into two identical nft rules, because a partial teardown would leave one behind.
fn hy2_port_hops(apps: &[AppIr], node_id: &str) -> Vec<Hy2PortHopPlan> {
    let mut hops = apps
        .iter()
        .flat_map(|app| app.ingresses.iter())
        .filter(|ingress| ingress.node == node_id)
        .filter_map(|ingress| {
            let hysteria2 = ingress.wires.hysteria2()?;
            let hop = hysteria2.hop.as_ref()?;
            Some(Hy2PortHopPlan {
                start: hop.start,
                end: hop.end,
                to: hysteria2.port,
            })
        })
        .collect::<Vec<_>>();
    hops.sort_by_key(|hop| (hop.start, hop.end, hop.to));
    hops.dedup();
    hops
}

/// The phantun instances to run on this machine. Either side may be present: a link that
/// assigns this node the server role produces a server, and dialing a peer through phantun
/// produces a client. With neither, this returns None and the artifact takes Disabled.
///
/// Both are read from `Link.wrap` rather than from the node. The previous implementation
/// tested whether the peer's `fake_tcp_endpoint` had a value, and that field is always empty
/// for a machine behind NAT, so its declared fake TCP was dropped without any report.
fn phantun_plan(sys: &SystemIr, node_id: &str) -> Option<PhantunPlan> {
    let me = sys.nodes.iter().find(|node| node.id == node_id)?;
    // phantun wraps wg, and a relay off the backbone runs no wg.
    let my_wg = me.wireguard.as_ref()?;
    let touching = sys
        .links
        .iter()
        .filter(|link| link.a == node_id || link.b == node_id)
        .collect::<Vec<_>>();

    // The servers to host: every link listing this node in `servers`, deduplicated by TCP
    // port, since one instance per port is sufficient and a phantun server accepts many
    // clients. Each port also records which peers arrive through it. The agent uses that to
    // establish that a peer's packets can only enter through this node's tun, so a public
    // endpoint means roaming has moved it. Without this list, roaming on the passive side
    // cannot be repaired: the config never wrote an endpoint for these peers, the watchdog
    // has no value to compare against, and `wg syncconf` does not modify an endpoint the
    // config does not name.
    let mut by_port = BTreeMap::<u16, BTreeSet<String>>::new();
    for link in &touching {
        let LinkWrap::FakeTcp { servers } = &link.wrap else {
            continue;
        };
        let Some(endpoint) = servers.get(node_id) else {
            continue;
        };
        let Some(port) = endpoint
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
        else {
            continue;
        };
        let peer_id = if link.a == node_id { &link.b } else { &link.a };
        by_port.entry(port).or_default().insert(peer_id.clone());
    }

    let locals = fake_tcp_local_ports(me, &touching);

    // TUN indices are allocated continuously across servers and clients: each instance owns
    // a /30, and one shared counter keeps the two groups from colliding. Servers come first
    // in port order and clients after in peer-id order, both pure functions that compute the
    // same result from the same model on every build.
    let servers = my_wg
        .listen_port
        .map(|udp_port| {
            by_port
                .iter()
                .enumerate()
                .map(|(index, (&tcp_port, peers))| PhantunServerPlan {
                    tcp_port,
                    forward_to_udp_port: udp_port,
                    peers: peers.iter().cloned().collect(),
                    tun: phantun_tun("s", index),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut clients = locals
        .iter()
        .enumerate()
        .map(|(index, (peer_id, (local, remote)))| PhantunClientPlan {
            peer_node_id: peer_id.clone(),
            listen_udp_port: *local,
            remote_tcp_endpoint: remote.clone(),
            tun: phantun_tun("c", servers.len() + index),
        })
        .collect::<Vec<_>>();
    // `locals` is a BTreeMap, so its iteration order is already sorted by peer id, which is
    // what makes the index above stable
    clients.sort_by(|a, b| a.peer_node_id.cmp(&b.peer_node_id));

    if servers.is_empty() && clients.is_empty() {
        return None;
    }
    Some(PhantunPlan {
        node_id: node_id.to_owned(),
        servers,
        clients,
    })
}

fn wireguard_plan(sys: &SystemIr, node_id: &str) -> Option<WireGuardPlan> {
    let me = sys.nodes.iter().find(|node| node.id == node_id)?;
    // Off the backbone means no wg config. This is where a machine with a public address
    // that stays off the overlay ends up: `wireguard::build` takes the Disabled branch and
    // the agent removes wg0. Such a machine can still relay, because its relay port runs
    // over the public internet; see the note on `SystemNode`.
    let my_wg = me.wireguard.as_ref()?;
    let overlay_addr = me.overlay_addr?;
    let touching = sys
        .links
        .iter()
        .filter(|link| link.a == node_id || link.b == node_id)
        .collect::<Vec<_>>();
    let locals = fake_tcp_local_ports(me, &touching);
    let mut peers = touching
        .iter()
        .filter_map(|link| peer_plan(sys, me, link, &locals))
        .collect::<Vec<_>>();

    peers.sort_by(|a, b| a.node_id.cmp(&b.node_id));

    Some(WireGuardPlan {
        private_key: my_wg.private_key.clone(),
        address: overlay_addr,
        listen_port: my_wg.listen_port,
        // Taken from the node without combining adjacent links: a wg interface has exactly
        // one MTU, `[Peer]` has no such key, and the value is a node property.
        mtu: Some(me.mtu),
        peers,
    })
}

/// Which peers this machine starts a phantun client for, and which loopback port each one
/// uses to dial which TCP endpoint. Assigned in peer-id order, so the same model computes
/// the same result on every build.
fn fake_tcp_local_ports(me: &SystemNode, touching: &[&Link]) -> BTreeMap<String, (u16, String)> {
    let mut peers = touching
        .iter()
        .filter_map(|link| {
            let peer_id = if link.a == me.id { &link.b } else { &link.a };
            // A client is needed only where this node dials the peer
            let i_dial = link.dial == Dial::Both
                || (link.dial == Dial::AtoB && link.a == me.id)
                || (link.dial == Dial::BtoA && link.b == me.id);
            if !i_dial {
                return None;
            }
            // The endpoint is read from the link: phantun is used only where the server
            // sits on the peer. A peer absent from `servers`, such as one whose link
            // hosts its server on this node, uses plain UDP.
            match &link.wrap {
                LinkWrap::FakeTcp { servers } => servers
                    .get(peer_id)
                    .map(|remote| (peer_id.clone(), remote.clone())),
                LinkWrap::Udp => None,
            }
        })
        .collect::<Vec<_>>();
    peers.sort();
    peers.dedup();
    peers
        .into_iter()
        .enumerate()
        .map(|(index, (id, remote))| (id, (PHANTUN_LOCAL_PORT_BASE + index as u16, remote)))
        .collect()
}

fn peer_plan(
    sys: &SystemIr,
    me: &SystemNode,
    link: &Link,
    locals: &BTreeMap<String, (u16, String)>,
) -> Option<WireGuardPeerPlan> {
    let peer_id = if link.a == me.id { &link.b } else { &link.a };
    let peer = sys.nodes.iter().find(|node| node.id == *peer_id)?;
    let i_dial = link.dial == Dial::Both
        || (link.dial == Dial::AtoB && link.a == me.id)
        || (link.dial == Dial::BtoA && link.b == me.id);

    let endpoint = if i_dial {
        // For a fake-TCP peer, wg dials local loopback and the phantun client forwards
        // the packets outward. `wg0.conf` therefore contains no reference to phantun, wg
        // addresses only the local machine, and the existing `wg syncconf` convergence
        // needs no change.
        match locals.get(&peer.id) {
            Some((local, _)) => Some(format!("127.0.0.1:{local}")),
            None => peer.wireguard.as_ref()?.endpoint.clone(),
        }
    } else {
        None
    };
    let persistent_keepalive = if endpoint.is_some() && link.dial != Dial::Both {
        Some(link.keepalive_secs)
    } else {
        None
    };

    Some(WireGuardPeerPlan {
        node_id: peer.id.clone(),
        public_key: peer.wireguard.as_ref()?.public_key.clone(),
        allowed_ip: peer.overlay_addr?,
        endpoint,
        persistent_keepalive,
    })
}

fn xray_plan(sys: &SystemIr, apps: &[AppIr], node_id: &str) -> Option<XrayPlan> {
    let app_node = sorted_apps(apps)
        .into_iter()
        .flat_map(|app| app.nodes.iter())
        .find(|node| node.id == node_id)?;
    let system_node = sys.nodes.iter().find(|node| node.id == node_id);

    let mut plan = XrayPlan {
        node_id: node_id.to_owned(),
        api_port: app_node.api_port,
        reality_client: sys.settings.reality_client.clone(),
        dns: app_node.dns.clone(),
        domain_strategy: app_node.domain_strategy,
        // Each field falls back independently, so a machine can override the buffer size
        // without also overriding the idle timeout.
        connection: {
            let global = sys.settings.connection;
            let mine = app_node.connection;
            ResolvedConnection {
                conn_idle_secs: mine.conn_idle_secs.unwrap_or(global.conn_idle_secs),
                uplink_only_secs: mine.uplink_only_secs.unwrap_or(global.uplink_only_secs),
                downlink_only_secs: mine.downlink_only_secs.unwrap_or(global.downlink_only_secs),
                // `or` rather than `unwrap_or`: absent at both levels has to stay absent,
                // or the artifact would write a buffer size and override xray's own
                // architecture-based choice.
                buffer_size_kb: mine.buffer_size_kb.or(global.buffer_size_kb),
                handshake_secs: global.handshake_secs,
                stats_user_online: sys.settings.stats_user_online,
            }
        },
        dns_route: None,
        egress_dns: xray_egress_dns(&app_node.egress_dns),
        inbounds: xray_ingresses(apps, node_id, app_node.api_port),
        hop_inbounds: xray_hop_inbounds(system_node, apps, node_id),
        forward_outbounds: xray_forward_outbounds(apps, node_id),
        egress_outbounds: xray_egress_outbounds(apps, node_id, &app_node.egress_dns),
        external_outbounds: xray_external_outbounds(apps, node_id),
        reverse_portals: xray_reverse_portals(apps, node_id),
        reverse_bridges: xray_reverse_bridges(apps, node_id),
        // Counts only `Action::Block` step rules, not ingress guards — a known gap, left as-is
        // for now. A guard also compiles to deny rules pointing at `out:block` (the guard loop in
        // `xray_routing_rules`), so a node whose only blocking comes from a guard — a guarded
        // ingress with no `Action::Block` among its steps — leaves this false, and the artifact
        // never builds the `out:block` blackhole while those deny rules still name it. xray
        // 26.3.27 loads that config anyway and drops traffic dispatched to the undefined tag, so
        // the guard still fails closed and blocks as intended (verified end to end). It is not the
        // designed shape: `BLOCK_OUTBOUND_TAG` is meant to be built wherever a rule denies, and
        // relying on an undefined outbound tag is not guaranteed across xray versions. The fix
        // would be to also set this where any ingress on the node carries a non-empty guard;
        // affects VLESS guards identically, so it is not specific to Hysteria 2.
        block_outbound: apps.iter().any(|app| {
            app.steps
                .iter()
                .filter(|step| step.node == node_id)
                .flat_map(|step| step.rules.iter())
                .any(|rule| matches!(rule.action, Action::Block))
        }),
        routing_rules: xray_routing_rules(sys, apps, node_id),
        geodata: sys.settings.geodata.clone(),
    };
    plan.dns_route = node_dns_route(&plan);
    Some(plan)
}

fn xray_ingresses(apps: &[AppIr], node_id: &str, api_port: Option<u16>) -> Vec<XrayIngressPlan> {
    let mut inbounds = sorted_apps(apps)
        .into_iter()
        .flat_map(|app| app.ingresses.iter().map(move |ingress| (app, ingress)))
        .filter(|(_, ingress)| ingress.node == node_id)
        // One ingress, up to two inbounds. The halves share a credential and a port number and
        // nothing else: one listens on TCP and the other on UDP, and xray needs a separate
        // inbound for each. The UDP one takes a `:hy2` suffix on the tag, the same way REALITY's
        // fallback cover inbound takes `:cover`. Routing, grants and usage all key off the tag,
        // so the two stay separate downstream without any of those layers needing a new concept.
        .flat_map(|(app, ingress)| {
            let mut download_ports = ingress
                .wires
                .xhttp()
                .and_then(|xhttp| xhttp.download.as_ref())
                .into_iter()
                .flat_map(|download| [download.v4.as_ref(), download.v6.as_ref()])
                .flatten()
                .map(|download| download.node_port())
                .collect::<Vec<_>>();
            // Read the historical nested representation while old snapshots are still being
            // served. New writes and new checkpoints use Xhttp.download exclusively.
            if download_ports.is_empty() {
                download_ports = [
                    ingress.projection.v4.as_ref(),
                    ingress.projection.v6.as_ref(),
                ]
                .into_iter()
                .flatten()
                .filter_map(|endpoint| endpoint.download.as_ref())
                .map(|download| download.node_port())
                .collect();
            }
            download_ports.sort_unstable();
            download_ports.dedup();
            let split = matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)))
                && !download_ports.is_empty();

            let base_tag = ingress_tag(app, &ingress.id);
            let vless = ingress.wires.vless().map(|_| XrayIngressPlan {
                id: ingress.id.clone(),
                tag: base_tag.clone(),
                listen: ingress.bind,
                port: ingress.port,
                sniff: ingress.sniff,
                protocol: IngressProtocol::Vless,
                security: match ingress.wires.reality() {
                    Some(params) => IngressSecurity::Reality {
                        params: params.clone(),
                        private_key: ingress.identity.private_key.clone(),
                        short_ids: ingress.identity.short_ids.clone(),
                    },
                    None => IngressSecurity::Tls,
                },
                certificate_name: ingress.certificate_name.clone(),
                xhttp: ingress.wires.xhttp().cloned(),
                split: split.then_some(XraySplitIngressPlan {
                    core_port: 0,
                    download_ports: download_ports.clone(),
                }),
                cover_port: ingress
                    .wires
                    .reality()
                    .is_some_and(RealitySettings::uses_node_certificate_fallback)
                    .then_some(0),
                guard_port: ingress
                    .wires
                    .reality()
                    .is_some_and(RealitySettings::guards_fallback)
                    .then_some(0),
            });
            let anytls = ingress.wires.anytls().map(|settings| XrayIngressPlan {
                id: ingress.id.clone(),
                tag: format!("{base_tag}:anytls"),
                listen: ingress.bind,
                port: settings.port,
                sniff: ingress.sniff,
                protocol: IngressProtocol::AnyTls(settings.clone()),
                security: IngressSecurity::Tls,
                certificate_name: ingress.certificate_name.clone(),
                xhttp: None,
                split: None,
                cover_port: None,
                guard_port: None,
            });
            let quic = ingress.wires.hysteria2().map(|settings| XrayIngressPlan {
                tag: format!("{base_tag}:hy2"),
                // Its own port, taken from the wire rather than from the ingress. The TCP
                // listeners may have independent ports, and QUIC has its own port as well.
                port: settings.port,
                protocol: IngressProtocol::Hysteria2(settings.clone()),
                // Hysteria 2 always presents this machine's certificate: REALITY cannot be
                // carried over QUIC, so there is no external site and no fallback to cover.
                security: IngressSecurity::Tls,
                xhttp: None,
                split: None,
                cover_port: None,
                guard_port: None,
                id: ingress.id.clone(),
                listen: ingress.bind,
                sniff: ingress.sniff,
                certificate_name: ingress.certificate_name.clone(),
            });
            vless.into_iter().chain(anytls).chain(quic)
        })
        .collect::<Vec<_>>();
    inbounds.sort_by(|a, b| a.tag.cmp(&b.tag));

    // Internal ports are compiler-owned. They are allocated after sorting so the same snapshot
    // always yields the same artifact, and they skip every port this xray process already binds.
    let mut used = BTreeSet::new();
    used.extend(api_port);
    for app in apps {
        used.extend(
            app.ingresses
                .iter()
                .filter(|ingress| ingress.node == node_id)
                .filter_map(|ingress| ingress.wires.vless().map(|_| ingress.port)),
        );
        used.extend(
            apps.iter()
                .flat_map(|app| app.ingresses.iter())
                .filter(|ingress| ingress.node == node_id)
                .filter_map(|ingress| ingress.wires.anytls().map(|anytls| anytls.port)),
        );
        used.extend(
            app.steps
                .iter()
                .filter(|step| step.node == node_id)
                .filter_map(|step| step.hop_in.as_ref().map(|hop| hop.port)),
        );
    }
    for ingress in &inbounds {
        if let Some(split) = &ingress.split {
            used.extend(split.download_ports.iter().copied());
        }
    }
    // Above `net.ipv4.ip_local_port_range`, which defaults to 32768–60999. That range is the one
    // part of the port space no process on the machine can reserve: the kernel allocates it as
    // the source port of outgoing connections, and this xray opens many of those over loopback.
    // A base inside it would not only risk sharing a port; `bind` fails and the machine's xray
    // does not start, on whichever boot an outgoing connection took the port first.
    let mut candidate = 61_101u16;
    let mut take = |used: &mut BTreeSet<u16>, what: &str| {
        while used.contains(&candidate) {
            candidate = candidate
                .checked_add(1)
                .unwrap_or_else(|| panic!("内部 {what} 端口耗尽"));
        }
        let port = candidate;
        used.insert(port);
        candidate = candidate
            .checked_add(1)
            .unwrap_or_else(|| panic!("内部 {what} 端口耗尽"));
        port
    };
    for ingress in &mut inbounds {
        if let Some(split) = &mut ingress.split {
            split.core_port = take(&mut used, "XHTTP");
        }
        if let Some(cover_port) = &mut ingress.cover_port {
            *cover_port = take(&mut used, "REALITY cover");
        }
        if let Some(guard_port) = &mut ingress.guard_port {
            *guard_port = take(&mut used, "REALITY guard");
        }
    }
    inbounds
}

/// Expand a named fallback policy into xray's byte-rate triples.
///
/// The jitter is deterministic: the ingress tag and field name are hashed, producing a stable
/// value within 90%..=110% of the preset. This keeps artifacts reproducible and avoids a single
/// fleet-wide tuple that would identify every generated REALITY listener.
pub fn reality_fallback_limits(
    policy: &RealityFallbackLimits,
    ingress_tag: &str,
) -> Option<XrayFallbackLimitsPlan> {
    let rate = |value: RealityFallbackRateLimit| XrayFallbackRateLimitPlan {
        after_bytes: value.after_bytes,
        bytes_per_sec: value.bytes_per_sec,
        burst_bytes_per_sec: value.burst_bytes_per_sec,
    };
    let preset = match policy {
        RealityFallbackLimits::Off => return None,
        RealityFallbackLimits::Custom { upload, download } => {
            return Some(XrayFallbackLimitsPlan {
                upload: rate(*upload),
                download: rate(*download),
            });
        }
        RealityFallbackLimits::Balanced => (
            RealityFallbackRateLimit {
                after_bytes: 1_048_576,
                bytes_per_sec: 262_144,
                burst_bytes_per_sec: 524_288,
            },
            RealityFallbackRateLimit {
                after_bytes: 8_388_608,
                bytes_per_sec: 1_048_576,
                burst_bytes_per_sec: 2_097_152,
            },
        ),
        RealityFallbackLimits::Strict => (
            RealityFallbackRateLimit {
                after_bytes: 262_144,
                bytes_per_sec: 65_536,
                burst_bytes_per_sec: 131_072,
            },
            RealityFallbackRateLimit {
                after_bytes: 1_048_576,
                bytes_per_sec: 262_144,
                burst_bytes_per_sec: 524_288,
            },
        ),
    };
    let jitter = |direction: &str, field: &str, value: u64| {
        let mut hasher = Sha256::new();
        hasher.update(b"brocade/reality-fallback-jitter/v1");
        hasher.update(ingress_tag.as_bytes());
        hasher.update(direction.as_bytes());
        hasher.update(field.as_bytes());
        let digest = hasher.finalize();
        let percent = 90 + u64::from(digest[0] % 21);
        value.saturating_mul(percent) / 100
    };
    let expand = |direction: &str, value: RealityFallbackRateLimit| XrayFallbackRateLimitPlan {
        after_bytes: jitter(direction, "after", value.after_bytes),
        bytes_per_sec: jitter(direction, "rate", value.bytes_per_sec),
        burst_bytes_per_sec: jitter(direction, "burst", value.burst_bytes_per_sec),
    };
    Some(XrayFallbackLimitsPlan {
        upload: expand("upload", preset.0),
        download: expand("download", preset.1),
    })
}

/// The relay inbounds on this machine, one per chain.
fn xray_hop_inbounds(
    system_node: Option<&SystemNode>,
    apps: &[AppIr],
    node_id: &str,
) -> Vec<XrayHopInboundPlan> {
    let Some(system_node) = system_node else {
        return Vec::new();
    };

    let mut plans = Vec::new();
    for app in sorted_apps(apps) {
        for step in app.steps.iter().filter(|step| step.node == node_id) {
            // No `hop_in` means this chain accepts no relay here.
            // Admission is decided by whether the port ends up with any client, not by
            // whether an accept exists: a head acting as reverse access's upstream has no
            // accept, which `ir/routing.rs` still clears, yet the port has to open,
            // because the downstream connects with its own credential. The port is
            // therefore built first and checked for an empty client list afterwards.
            let Some(hop_in) = step.hop_in.as_ref() else {
                continue;
            };

            // The bind address follows how peers dial this chain. One hop arriving by
            // direct address forces 0.0.0.0, because such a hop dials one of the
            // machine's NIC addresses and binding only the overlay returns connection
            // refused. The overlay address is bound only where every hop goes over the
            // overlay, which exposes one port fewer. The test reads `hops`, the dialer's
            // computed result, because once relay ports moved onto the chain, whether any
            // peer dials this node from outside wg is no longer visible in the local
            // config and is known only to the dialer.
            let direct_hops = app
                .hops
                .iter()
                .filter(|hop| {
                    // The test is whether any peer enters this port from outside wg:
                    // (1) a peer dials this node directly (`Direct`), reaching one of
                    // the machine's NIC addresses; (2) reverse access's downstream
                    // connects to this node (`Reverse` with this node as `from`), which
                    // takes that path because it is off the backbone and arrives from
                    // the public internet. In case (2) this node is `from` rather than
                    // `to`, the opposite of case (1), because a reverse edge's `to` is
                    // the downstream and the downstream does not listen. Testing `to`
                    // alone misses the upstream, and the symptom is a tunnel that never
                    // establishes while both sides' configs appear correct.
                    hop.chain == step.chain
                        && ((hop.to == node_id && hop.path == HopPath::Direct)
                            || (hop.from == node_id && hop.path == HopPath::Reverse))
                })
                .collect::<Vec<_>>();
            let dialed_directly = !direct_hops.is_empty();
            let dialed_directly_v6 = direct_hops
                .iter()
                .any(|hop| hop.address.parse::<Ipv6Addr>().is_ok());
            let listen = match (
                dialed_directly,
                dialed_directly_v6,
                system_node.overlay_addr,
            ) {
                (true, true, _) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                (true, false, _) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                (false, _, Some(overlay)) => IpAddr::V4(overlay),
                // No peer dials directly and the node is not on the overlay, so this
                // inbound is unreachable. A machine present in `SystemIr` is either on
                // the backbone or has a chain opening a port on it, so reaching this arm
                // means no hop on this chain compiled; a diagnostic was already reported
                // above.
                (false, _, None) => continue,
            };

            // This port's usual clients are the upstreams that dial this node, sharing
            // this machine's `accept`, which is one key per chain. Reverse access's
            // downstream is different: it connects with its own uuid (see the credential
            // passage in `ir/hops.rs`), so it also has to appear in clients. A missing
            // entry makes that machine's connection fail as an unknown user, while the
            // chain, the compilation result and its own artifacts all appear correct.
            let mut clients = Vec::new();
            if let Some(accept) = step.accept.as_ref() {
                clients.push(XrayClientPlan {
                    uuid: accept.uuid.clone(),
                    label: accept.label.clone(),
                });
            }
            for hop in app.hops.iter().filter(|hop| {
                hop.chain == step.chain && hop.from == node_id && hop.path == HopPath::Reverse
            }) {
                if clients.iter().any(|c| c.uuid == hop.credential.uuid) {
                    continue;
                }
                clients.push(XrayClientPlan {
                    uuid: hop.credential.uuid.clone(),
                    label: hop.credential.label.clone(),
                });
            }
            // A port with no clients admits no connection, and leaving it open only
            // exposes one more listening address.
            if clients.is_empty() {
                continue;
            }

            plans.push(XrayHopInboundPlan {
                chain: step.chain.clone(),
                tag: hop_inbound_tag(app, &step.chain),
                listen,
                port: hop_in.port,
                security: hop_in.security.clone(),
                clients,
            });
        }
    }

    plans.sort_by(|a, b| a.tag.cmp(&b.tag));
    plans
}

fn xray_forward_outbounds(apps: &[AppIr], node_id: &str) -> Vec<XrayForwardOutboundPlan> {
    let mut outbounds = BTreeMap::<String, XrayForwardOutboundPlan>::new();

    for app in sorted_apps(apps) {
        for step in app.steps.iter().filter(|step| step.node == node_id) {
            for rule in &step.rules {
                let Action::Forward { to, .. } = &rule.action else {
                    continue;
                };
                let hop = app
                    .hops
                    .iter()
                    .find(|hop| hop.chain == step.chain && hop.from == node_id && hop.to == *to);
                // On the reverse variant this node does not dial the peer; the traffic
                // goes to the portal (see `action_tag`). Emitting an outbound here would
                // leave an undialable tag in the artifacts, because its address is this
                // machine's own (see the `HopPath::Reverse` note), so dialing it would
                // connect back to this machine.
                if hop.map(|hop| hop.path) == Some(HopPath::Reverse) {
                    continue;
                }
                let tag = forward_tag(app, &step.chain, to);
                outbounds
                    .entry(tag.clone())
                    .or_insert_with(|| XrayForwardOutboundPlan {
                        tag,
                        address: hop
                            .map(|hop| hop.address.clone())
                            .unwrap_or_else(|| "?".to_owned()),
                        port: hop.map(|hop| hop.port).unwrap_or(0),
                        uuid: hop
                            .map(|hop| hop.credential.uuid.clone())
                            .unwrap_or_else(|| "?".to_owned()),
                        security: hop
                            .map(|hop| hop.security.clone())
                            .unwrap_or(HopDialWire::None),
                        pool: hop.map(|hop| hop.pool).unwrap_or_default(),
                    });
            }
        }

        // Reverse access's downstream half. This is the one place an outbound is not
        // derived from this node's own rule table: traffic flows `from → to`, the rules
        // are written on `from`, and the dialer is `to`, which is this node. It therefore
        // scans hops in the opposite direction, for edges ending at this node that use a
        // reverse tunnel, and builds one outbound dialing the upstream for each. `Hop`'s
        // address, port, credential and security fields describe the initiator, and on a
        // reverse edge the initiator is this node, so they are used directly.
        for hop in app
            .hops
            .iter()
            .filter(|hop| hop.to == node_id && hop.path == HopPath::Reverse)
        {
            let tag = reverse_dial_tag(app, &hop.chain, &hop.from);
            outbounds
                .entry(tag.clone())
                .or_insert_with(|| XrayForwardOutboundPlan {
                    tag,
                    address: hop.address.clone(),
                    port: hop.port,
                    uuid: hop.credential.uuid.clone(),
                    security: hop.security.clone(),
                    // This outbound does dial, because it is the downstream end
                    // establishing the tunnel, but it is not configurable from here. The
                    // pool setting lives on the forwarding rule, and that rule is written
                    // on the upstream; applying it here would put one machine's setting
                    // on another machine's socket. The tunnel is one long-lived
                    // connection in any case, which is what pooling aims to produce.
                    pool: HopPool::None,
                });
        }
    }

    outbounds.into_values().collect()
}

fn xray_reverse_portals(apps: &[AppIr], node_id: &str) -> Vec<XrayReversePortalPlan> {
    let mut plans = Vec::new();
    for app in sorted_apps(apps) {
        for hop in app
            .hops
            .iter()
            .filter(|hop| hop.from == node_id && hop.path == HopPath::Reverse)
        {
            plans.push(XrayReversePortalPlan {
                tag: reverse_portal_tag(app, &hop.chain, &hop.to),
                peer_label: hop.credential.label.clone(),
                inbound_tag: hop_inbound_tag(app, &hop.chain),
            });
        }
    }
    plans.sort_by(|a, b| a.tag.cmp(&b.tag));
    plans
}

fn xray_reverse_bridges(apps: &[AppIr], node_id: &str) -> Vec<XrayReverseBridgePlan> {
    let mut plans = Vec::new();
    for app in sorted_apps(apps) {
        for hop in app
            .hops
            .iter()
            .filter(|hop| hop.to == node_id && hop.path == HopPath::Reverse)
        {
            plans.push(XrayReverseBridgePlan {
                tag: reverse_bridge_tag(app, &hop.chain, &hop.from),
                dial_tag: reverse_dial_tag(app, &hop.chain, &hop.from),
            });
        }
    }
    plans.sort_by(|a, b| a.tag.cmp(&b.tag));
    plans
}

fn xray_egress_outbounds(
    apps: &[AppIr],
    node_id: &str,
    policies: &[crate::model::NodeEgressDnsPolicy],
) -> Vec<XrayEgressOutboundPlan> {
    let mut outbounds = BTreeMap::<String, XrayEgressOutboundPlan>::new();

    for app in sorted_apps(apps) {
        for step in app.steps.iter().filter(|step| step.node == node_id) {
            for rule in &step.rules {
                let Action::Egress { send_through } = &rule.action else {
                    continue;
                };
                // The policy is not activated by this route, but an exact selector match still
                // supplies the Freedom address-family strategy. This is required for the ordered
                // UseIPv4v6 / UseIPv6v4 modes, which Xray's DNS-server queryStrategy cannot
                // express. Resolver selection itself remains global and independently emitted.
                let resolution = egress_dns_resolution(app, node_id, &rule.dest_match);
                let (tag, domain_strategy) = match resolution {
                    Some(resolution) => (
                        custom_egress_tag(*send_through, resolution),
                        Some(custom_dns_domain_strategy(resolution.address_strategy)),
                    ),
                    None => (egress_tag(*send_through), None),
                };
                outbounds
                    .entry(tag.clone())
                    .or_insert(XrayEgressOutboundPlan {
                        tag,
                        send_through: *send_through,
                        domain_strategy,
                    });
            }
        }
    }

    // A machine DNS policy is complete on its own. Its query path is deliberately direct and
    // source-unbound; deriving this from a chain rule would recreate an outbound association that
    // Xray cannot preserve when it later chooses the resolver.
    for policy in policies {
        if custom_dns_domains(&policy.selector).is_none() {
            continue;
        }
        let resolution = &policy.resolution;
        let tag = custom_egress_tag(None, resolution);
        outbounds
            .entry(tag.clone())
            .or_insert(XrayEgressOutboundPlan {
                tag,
                send_through: None,
                domain_strategy: Some(custom_dns_domain_strategy(resolution.address_strategy)),
            });
    }

    outbounds.into_values().collect()
}

fn xray_egress_dns(policies: &[crate::model::NodeEgressDnsPolicy]) -> Vec<XrayEgressDnsPlan> {
    // DNS priority is machine-owned and deliberately independent from app/chain/rule order.
    // Every stored entry belongs to this Xray instance and is therefore emitted; a route rule is
    // neither an activation switch nor an isolation boundary.
    let mut plans = Vec::<XrayEgressDnsPlan>::new();
    let mut ordered = policies.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        a.position.cmp(&b.position).then_with(|| {
            serde_json::to_string(&a.selector)
                .unwrap_or_default()
                .cmp(&serde_json::to_string(&b.selector).unwrap_or_default())
        })
    });

    for policy in ordered {
        let Some(mut domains) = custom_dns_domains(&policy.selector) else {
            continue;
        };
        domains.sort();
        domains.dedup();
        if policy.selector.canonical_egress_dns_selector().is_none() {
            continue;
        }
        let resolution = &policy.resolution;
        plans.push(XrayEgressDnsPlan {
            tag: custom_dns_policy_tag(None, resolution, policy.position, &domains),
            outbound_tag: custom_egress_tag(None, resolution),
            address: resolution.address.clone(),
            port: resolution.port,
            transport: resolution.transport,
            address_strategy: resolution.address_strategy,
            fallback: resolution.fallback,
            domains,
        });
    }
    plans
}

fn custom_dns_domains(dest_match: &DestMatch) -> Option<Vec<String>> {
    match dest_match {
        DestMatch::DomainSuffix(values) => Some(
            values
                .iter()
                .map(|value| format!("domain:{value}"))
                .collect(),
        ),
        DestMatch::DomainKeyword(values) => Some(values.clone()),
        DestMatch::DomainRegex(value) => Some(vec![format!("regexp:{value}")]),
        DestMatch::Geosite(values) => Some(
            values
                .iter()
                .map(|value| format!("geosite:{value}"))
                .collect(),
        ),
        _ => None,
    }
}

fn egress_dns_resolution<'a>(
    app: &'a AppIr,
    node_id: &str,
    dest_match: &DestMatch,
) -> Option<&'a EgressDnsResolution> {
    let selector = dest_match.canonical_egress_dns_selector()?;
    app.nodes
        .iter()
        .find(|node| node.id == node_id)?
        .egress_dns
        .iter()
        .find(|policy| policy.selector.canonical_egress_dns_selector().as_ref() == Some(&selector))
        .map(|policy| &policy.resolution)
}

fn custom_dns_domain_strategy(strategy: EgressDnsAddressStrategy) -> DomainStrategy {
    match strategy {
        EgressDnsAddressStrategy::UseIp => DomainStrategy::UseIp,
        EgressDnsAddressStrategy::UseIpv4 => DomainStrategy::UseIpv4,
        EgressDnsAddressStrategy::UseIpv6 => DomainStrategy::UseIpv6,
        EgressDnsAddressStrategy::UseIpv4v6 => DomainStrategy::UseIpv4v6,
        EgressDnsAddressStrategy::UseIpv6v4 => DomainStrategy::UseIpv6v4,
    }
}

fn custom_dns_identity(send_through: Option<IpAddr>, resolution: &EgressDnsResolution) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"brocade/egress-dns/v1");
    hasher.update(send_through.map(|ip| ip.to_string()).unwrap_or_default());
    hasher.update([0]);
    hasher.update(resolution.address.trim().as_bytes());
    hasher.update([0]);
    hasher.update(resolution.port.to_le_bytes());
    hasher.update([
        resolution.transport as u8,
        resolution.address_strategy as u8,
        resolution.fallback as u8,
    ]);
    hex_lower(&hasher.finalize()[..6])
}

fn custom_egress_tag(send_through: Option<IpAddr>, resolution: &EgressDnsResolution) -> String {
    format!(
        "out:egress:dns:{}",
        custom_dns_identity(send_through, resolution)
    )
}

fn custom_dns_policy_tag(
    send_through: Option<IpAddr>,
    resolution: &EgressDnsResolution,
    position: u32,
    domains: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(custom_dns_identity(send_through, resolution));
    hasher.update(position.to_le_bytes());
    for domain in domains {
        hasher.update([0]);
        hasher.update(domain.as_bytes());
    }
    format!("dns:egress:{}", hex_lower(&hasher.finalize()[..6]))
}

fn xray_external_outbounds(apps: &[AppIr], node_id: &str) -> Vec<XrayExternalOutboundPlan> {
    let mut outbounds = BTreeMap::<String, XrayExternalOutboundPlan>::new();

    for app in sorted_apps(apps) {
        for step in app.steps.iter().filter(|step| step.node == node_id) {
            for rule in &step.rules {
                let Action::Proxy { outbound } = &rule.action else {
                    continue;
                };
                let Some(target) = app
                    .external_outbounds
                    .iter()
                    .find(|target| target.id == *outbound)
                else {
                    // Validation reports the missing reference. Keeping the plan total lets the
                    // caller return the full diagnostic set instead of failing while lowering it.
                    continue;
                };
                let tag = external_outbound_tag(outbound);
                let (protocol, address, port, wireguard_workers) = match &target.protocol {
                    ExternalOutboundProtocol::Warp {
                        mtu,
                        keep_alive,
                        allowed_ips,
                        no_kernel_tun,
                        domain_strategy,
                        workers,
                    } => {
                        let Some(binding) = target
                            .bindings
                            .iter()
                            .find(|binding| binding.node == node_id)
                        else {
                            // Validation reports the missing per-machine identity. Do not emit a
                            // half-configured WireGuard outbound which Xray would accept but could
                            // never authenticate as this machine.
                            continue;
                        };
                        let effective_domain_strategy =
                            binding.domain_strategy.as_ref().unwrap_or(domain_strategy);
                        (
                            ExternalOutboundProtocol::Wireguard {
                                credential: binding.private_key.clone(),
                                peer_public_key: binding.peer_public_key.clone(),
                                // A WARP registration normally returns one address from each
                                // family. Keep the full identity in the model, but only put the
                                // selected family on Xray's virtual interface for a single-stack
                                // exit. `allowedIPs` and `domainStrategy` then enforce the same
                                // decision for literal and domain targets respectively.
                                local_addresses: warp_local_addresses(
                                    &binding.local_addresses,
                                    effective_domain_strategy,
                                ),
                                mtu: binding.mtu.unwrap_or(*mtu),
                                reserved: binding.reserved.clone(),
                                keep_alive: binding.keep_alive.unwrap_or(*keep_alive),
                                allowed_ips: binding
                                    .allowed_ips
                                    .clone()
                                    .unwrap_or_else(|| allowed_ips.clone()),
                                no_kernel_tun: binding.no_kernel_tun.unwrap_or(*no_kernel_tun),
                                domain_strategy: effective_domain_strategy.clone(),
                            },
                            binding
                                .endpoint_address
                                .clone()
                                .unwrap_or_else(|| target.address.clone()),
                            binding.endpoint_port.unwrap_or(target.port),
                            binding.workers.unwrap_or(*workers),
                        )
                    }
                    protocol => (protocol.clone(), target.address.clone(), target.port, 0),
                };
                outbounds
                    .entry(tag.clone())
                    .or_insert_with(|| XrayExternalOutboundPlan {
                        tag,
                        address,
                        port,
                        protocol,
                        security: target.security.clone(),
                        wireguard_workers,
                    });
            }
        }
    }

    outbounds.into_values().collect()
}

fn warp_local_addresses(addresses: &[String], domain_strategy: &str) -> Vec<String> {
    let family = match domain_strategy {
        "ForceIPv4" => Some(true),
        "ForceIPv6" => Some(false),
        _ => None,
    };
    let Some(ipv4) = family else {
        return addresses.to_vec();
    };
    addresses
        .iter()
        .filter(|address| {
            address
                .split_once('/')
                .map_or(address.as_str(), |(ip, _)| ip)
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_ipv4() == ipv4)
        })
        .cloned()
        .collect()
}

fn xray_routing_rules(sys: &SystemIr, apps: &[AppIr], node_id: &str) -> Vec<XrayRoutingRulePlan> {
    let mut rules = Vec::new();

    // Every ingress's refusals, emitted before any chain's rules.
    //
    // The order is the mechanism rather than a preference. A chain ending in `Any → Forward`,
    // which is what an ordinary chain is, matches every connection, and xray takes the first
    // matching rule. Placed after, these rules would never match, while the console would still
    // show them as enabled.
    for app in sorted_apps(apps) {
        for ingress in app
            .ingresses
            .iter()
            .filter(|ingress| ingress.node == node_id && !ingress.guard.blocks_nothing())
        {
            let inbound_tags = ingress_inbound_tags(app, ingress);
            if inbound_tags.is_empty() {
                continue;
            }
            let selector = XrayRuleSelector::InboundTags(inbound_tags);
            for dest_match in guard_matches(&ingress.guard, sys.overlay_cidr) {
                rules.push(XrayRoutingRulePlan {
                    selector: selector.clone(),
                    dest_match,
                    // The same tag `Action::Block` compiles to. Written out here rather than
                    // shared through a constant, because the artifact layer owns that name and
                    // this layer already writes it once, twelve lines below.
                    outbound_tag: "out:block".to_owned(),
                });
            }
        }
    }

    for app in sorted_apps(apps) {
        for step in app.steps.iter().filter(|step| step.node == node_id) {
            let selector = if let Some(accept) = &step.accept {
                XrayRuleSelector::Users(vec![accept.label.clone()])
            } else {
                let inbound_tags = app
                    .ingresses
                    .iter()
                    .filter(|ingress| ingress.chain == step.chain && ingress.node == node_id)
                    .flat_map(|ingress| ingress_inbound_tags(app, ingress))
                    .collect::<Vec<_>>();
                if inbound_tags.is_empty() {
                    continue;
                }
                XrayRuleSelector::InboundTags(inbound_tags)
            };

            // An ordinary ingress selects on the credential that arrived. Reverse access's
            // downstream has a second entry path: the portal pushes traffic along the
            // reverse tunnel, it arrives at the bridge, and it carries no VLESS user
            // identity, so a `Users` rule cannot select it, this machine's whole rule
            // table falls through, and the traffic has no outbound. An identical set of
            // rules is therefore emitted again, keyed by the bridge's inbound tag.
            let mut selectors = vec![selector];
            for hop in app.hops.iter().filter(|hop| {
                hop.chain == step.chain && hop.to == node_id && hop.path == HopPath::Reverse
            }) {
                selectors.push(XrayRuleSelector::InboundTags(vec![reverse_bridge_tag(
                    app, &hop.chain, &hop.from,
                )]));
            }

            for selector in selectors {
                for rule in &step.rules {
                    rules.push(XrayRoutingRulePlan {
                        selector: selector.clone(),
                        dest_match: rule.dest_match.clone(),
                        outbound_tag: action_tag(app, &step.chain, &step.node, rule),
                    });
                }
            }
        }
    }

    rules
}

fn grant_sync_plan(apps: &[AppIr], node_id: &str) -> GrantSyncPlan {
    let mut updates = Vec::new();

    for app in sorted_apps(apps) {
        for ingress in app
            .ingresses
            .iter()
            .filter(|ingress| ingress.node == node_id)
        {
            let mut clients = app
                .grants
                .iter()
                .filter(|grant| grant.ingress == ingress.id)
                .filter_map(|grant| {
                    let user = app
                        .users
                        .iter()
                        .find(|user| user.tenant == grant.tenant && user.id == grant.user)?;
                    Some(GrantClientPlan {
                        uuid: user.uuid.clone(),
                        label: grant.label.clone(),
                        flow: ingress.wires.flow().map(str::to_owned),
                    })
                })
                .collect::<Vec<_>>();

            // The end-to-end probe's credential ships down the same channel as a real
            // user's. No other channel is available: xray's client list supports only this
            // hot-synced path, and a second one would require restarting xray, which drops
            // every connection, so checking whether the line works would itself break it.
            // The probe is not a `User`, so subscriptions, billing and user listings need
            // no change: they identify users by `{user}@{tenant}#{ingress}`, fail to match
            // `probe#{ingress}`, and therefore exclude it (`model::probe_label`).
            clients.push(GrantClientPlan {
                uuid: crate::model::probe_uuid(&ingress.identity.private_key, &ingress.id),
                label: crate::model::probe_label(&ingress.id),
                flow: ingress.wires.flow().map(str::to_owned),
            });

            clients.sort_by(|a, b| a.label.cmp(&b.label));

            // One update per listener rather than per ingress. A two-wire ingress has two
            // inbounds, and xray's account list is per inbound: pushing to only one leaves every
            // client holding a subscription for the other half rejected as an unknown user.
            let tag = ingress_tag(app, &ingress.id);
            if ingress.wires.vless().is_some() {
                updates.push(GrantInboundUpdatePlan {
                    inbound_tag: tag.clone(),
                    clients: clients.clone(),
                });
            }
            if ingress.wires.anytls().is_some() {
                updates.push(GrantInboundUpdatePlan {
                    inbound_tag: format!("{tag}:anytls"),
                    clients: clients
                        .clone()
                        .into_iter()
                        .map(|client| GrantClientPlan {
                            flow: None,
                            ..client
                        })
                        .collect(),
                });
            }
            if ingress.wires.has_udp() {
                updates.push(GrantInboundUpdatePlan {
                    inbound_tag: format!("{tag}:hy2"),
                    // Flow belongs to VLESS. A Hysteria account has no such field, and carrying
                    // one here would make the desired state disagree with what is read back from
                    // the machine, so every convergence round would report a difference that
                    // cannot be resolved.
                    clients: clients
                        .iter()
                        .cloned()
                        .map(|client| GrantClientPlan {
                            flow: None,
                            ..client
                        })
                        .collect(),
                });
            }
        }
    }

    updates.sort_by(|a, b| a.inbound_tag.cmp(&b.inbound_tag));
    GrantSyncPlan {
        node_id: node_id.to_owned(),
        updates,
    }
}

fn node_dns_route(plan: &XrayPlan) -> Option<String> {
    if !plan.dns.needs_route() {
        return None;
    }

    let routes = plan
        .egress_outbounds
        .iter()
        .filter(|outbound| outbound.domain_strategy.is_none())
        .map(|outbound| outbound.tag.clone())
        .collect::<BTreeSet<_>>();
    if routes.len() == 1 {
        routes.into_iter().next()
    } else {
        None
    }
}

fn sorted_apps(apps: &[AppIr]) -> Vec<&AppIr> {
    let mut apps = apps.iter().collect::<Vec<_>>();
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    apps
}

fn action_tag(app: &AppIr, chain: &str, node: &str, rule: &Rule) -> String {
    match &rule.action {
        Action::Forward { to, .. } => {
            // Reverse traffic goes to the portal rather than to an outbound. An outbound
            // dials, whereas this hop's connection was established by the peer, and the
            // artifacts hold no address that would reach it.
            let reverse = app.hops.iter().any(|hop| {
                hop.chain == chain
                    && hop.from == node
                    && hop.to == *to
                    && hop.path == HopPath::Reverse
            });
            if reverse {
                reverse_portal_tag(app, chain, to)
            } else {
                forward_tag(app, chain, to)
            }
        }
        Action::Egress { send_through } => egress_dns_resolution(app, node, &rule.dest_match)
            .map_or_else(
                || egress_tag(*send_through),
                |resolution| custom_egress_tag(*send_through, resolution),
            ),
        Action::Proxy { outbound } => external_outbound_tag(outbound),
        Action::Block => "out:block".to_owned(),
    }
}

/// One ingress's refusals, in the order they are emitted.
///
/// Each refusal is its own rule rather than one combined match. An xray condition holds one slot
/// per kind (domain, ip, port, network, protocol) and ANDs the slots that are filled, so combining
/// these into one rule would mean private *and* bittorrent *and* port 25, which matches nothing.
/// Separate rules provide the OR.
///
/// Their order is fixed rather than incidental, so a machine's rule table reads the same on every
/// build and a diff between two compilations indicates an actual change.
fn guard_matches(guard: &IngressGuard, overlay: ipnet::Ipv4Net) -> Vec<DestMatch> {
    let mut matches = Vec::new();
    if guard.no_private {
        // The overlay is listed as an explicit CIDR beside `geoip:private` rather than relying
        // on it. The overlay is an RFC1918 range and geoip's private list currently covers it,
        // but that list is a downloaded file the fleet does not control, and the fleet's own
        // range is the one this rule must never stop covering.
        matches.push(DestMatch::IpCidr(vec![
            "geoip:private".to_owned(),
            overlay.to_string(),
        ]));
    }
    if guard.no_bittorrent {
        matches.push(DestMatch::Protocol(vec!["bittorrent".to_owned()]));
    }
    if guard.no_mail {
        // Submission and SMTPS alongside 25. Blocking only 25 moves the abuse to 587 with no
        // other effect.
        matches.push(DestMatch::Port(vec![
            "25".to_owned(),
            "465".to_owned(),
            "587".to_owned(),
        ]));
    }
    if guard.no_udp_amplification {
        // UDP only: 53 over TCP is ordinary DNS that a client may legitimately use, and 389 over
        // TCP is LDAP. Only the UDP side is usable for reflection.
        matches.push(DestMatch::All(vec![
            DestMatch::Network(Network::Udp),
            DestMatch::Port(
                AMPLIFICATION_PORTS
                    .iter()
                    .map(|port| port.to_string())
                    .collect(),
            ),
        ]));
    }
    if guard.tcp_and_quic_only {
        matches.push(DestMatch::All(vec![
            DestMatch::Network(Network::Udp),
            DestMatch::PortExcept(vec![443]),
        ]));
    }
    matches
}

/// The UDP services used for reflection attacks: chargen, DNS, NTP, SNMP, CLDAP, SSDP,
/// memcached. Each answers a small request with a large reply, which is what makes it usable.
const AMPLIFICATION_PORTS: [u16; 7] = [19, 53, 123, 161, 389, 1900, 11211];

fn ingress_tag(app: &AppIr, ingress: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("in:{app_id}/{ingress}"),
        None => format!("in:{ingress}"),
    }
}

/// The xray inbound tags an ingress actually registers.
///
/// An ingress is up to two inbounds: the base tag for its TCP/VLESS listener and a
/// `:hy2`-suffixed tag for its UDP/Hysteria 2 listener, exactly as `xray_ingresses` builds them.
/// Routing and guard rules select on these, so they have to be enumerated the same way here —
/// writing `ingress_tag` alone selects only the TCP half, and traffic arriving on the `:hy2`
/// inbound matches no rule, falls through the whole chain table, and lands on the default
/// outbound: every Hysteria 2 ingress reaching one fixed link regardless of its port range or its
/// chain. Mirrors the per-listener split the grant sync already makes (`grant_sync_plan`).
fn ingress_inbound_tags(app: &AppIr, ingress: &Ingress) -> Vec<String> {
    let base = ingress_tag(app, &ingress.id);
    let mut tags = Vec::new();
    if ingress.wires.has_tcp() {
        if ingress.wires.vless().is_some() {
            tags.push(base.clone());
        }
        if ingress.wires.anytls().is_some() {
            tags.push(format!("{base}:anytls"));
        }
    }
    if ingress.wires.has_udp() {
        tags.push(format!("{base}:hy2"));
    }
    tags
}

fn hop_inbound_tag(app: &AppIr, chain: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("in:hop:{app_id}/{chain}"),
        None => format!("in:hop:{chain}"),
    }
}

fn forward_tag(app: &AppIr, chain: &str, to: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("out:{app_id}/{chain}>{to}"),
        None => format!("out:{chain}>{to}"),
    }
}

fn external_outbound_tag(outbound: &str) -> String {
    format!("out:external/{outbound}")
}

/// The upstream's portal: traffic arrives here and leaves through one of the connections
/// the downstream attached.
fn reverse_portal_tag(app: &AppIr, chain: &str, to: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("rev:portal:{app_id}/{chain}>{to}"),
        None => format!("rev:portal:{chain}>{to}"),
    }
}

/// The downstream's bridge: it attaches connections to the upstream and then waits for
/// traffic to arrive along them.
fn reverse_bridge_tag(app: &AppIr, chain: &str, from: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("rev:bridge:{app_id}/{chain}<{from}"),
        None => format!("rev:bridge:{chain}<{from}"),
    }
}

/// The outbound the downstream dials the upstream with. Its direction is the inverse of
/// `forward_tag`, so the angle bracket points the other way.
fn reverse_dial_tag(app: &AppIr, chain: &str, from: &str) -> String {
    match app.app_id.as_deref() {
        Some(app_id) => format!("out:rev:{app_id}/{chain}<{from}"),
        None => format!("out:rev:{chain}<{from}"),
    }
}

// The egress outbound's tag. Delegated to `ir::routing` so the routing rules and the
// outbound list derive the tag from one definition.
fn egress_tag(send_through: Option<IpAddr>) -> String {
    routing_egress_tag(send_through.as_ref())
}

#[cfg(test)]
mod tests {
    use super::warp_local_addresses;

    #[test]
    fn warp_address_family_follows_the_exit_strategy() {
        let addresses = vec![
            "172.16.0.2/32".to_owned(),
            "2606:4700:110:8::2/128".to_owned(),
        ];

        assert_eq!(
            warp_local_addresses(&addresses, "ForceIPv4"),
            ["172.16.0.2/32"]
        );
        assert_eq!(
            warp_local_addresses(&addresses, "ForceIPv6"),
            ["2606:4700:110:8::2/128"]
        );
        assert_eq!(warp_local_addresses(&addresses, "ForceIP"), addresses);
    }
}
