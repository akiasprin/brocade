use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

use crate::{
    diagnostic::Diagnostic,
    model::{ModelSettings, ModelSnapshot, WgTransport},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemIr {
    pub revision: u64,
    pub overlay_cidr: Ipv4Net,
    pub settings: ModelSettings,
    pub node_count: usize,
    pub nodes: Vec<SystemNode>,
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// A machine's system-layer half.
///
/// The system layer is not the overlay. A relay hop can dial a concrete address, so
/// "can relay" and "is on the overlay" are two different things — a machine a chain
/// names by public address can avoid wg entirely. Hence the two overlay fields are
/// `Option`: absent means not on the backbone.
///
/// Relay-port matters do not live here. Port, wire format, and which address to
/// dial all hang off the chain (`Step.hop_in` and `HopDial`), leaving the system
/// layer with only what this machine looks like on the backbone.
pub struct SystemNode {
    pub id: String,
    pub tenant: String,
    pub certificate_track: Option<crate::model::CertificateTrack>,
    /// Backbone address. `None` means this machine is not on the overlay: no wg
    /// config is generated and it enters no `Link`.
    pub overlay_addr: Option<Ipv4Addr>,
    /// `None` means not on the backbone. It lives and dies with `overlay_addr`.
    pub wireguard: Option<WireGuard>,
    /// This machine's `wg0` MTU, with "fall back to the global default when the node
    /// sets none" already resolved. It is an interface property and therefore hangs
    /// off the node — a `Link` carries no MTU.
    pub mtu: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuard {
    pub private_key: String,
    pub public_key: String,
    pub listen_port: Option<u16>,
    /// The UDP endpoint for direct dialing. Always empty on a fake-TCP node — it
    /// declared its own inbound UDP unreachable, and leaving a UDP endpoint would only
    /// have others dial a port already known to be unreachable.
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dial {
    Both,
    AtoB,
    BtoA,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// A `Link` carries no MTU: it is a property of the wg interface, not of the link.
// See `SystemNode.mtu`.
pub struct Link {
    pub id: String,
    pub a: String,
    pub b: String,
    pub dial: Dial,
    pub keepalive_secs: u16,
    /// For bare UDP, the node being dialed to the Endpoint the other end can
    /// actually reach. Endpoint selection belongs to the link rather than the
    /// node: a v4-only machine cannot dial a peer's otherwise-public v6 address.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endpoints: BTreeMap<String, String>,
    /// Whether this link wears a TCP disguise, and if so which end hosts the server.
    /// See `LinkWrap`.
    pub wrap: LinkWrap,
}

/// A link's disguise.
///
/// Whether to wear one hangs off the link; which end hosts the server is derived.
/// Attached to each other, a machine behind NAT contradicts itself the moment it
/// declares one: it says "come dial me" while having no dialable address.
///
/// A phantun client only ever dials out, so the server always sits on the dialed
/// side — not a choice, but a consequence of phantun's shape. The operator says only
/// that this link should be disguised; where that lands follows from reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum LinkWrap {
    /// Bare UDP, with each end dialing the other's `endpoint` directly.
    Udp,
    /// Disguised. The key is the node id hosting the server, the value the
    /// `host:port` the client dials — the server takes its port from it, the client
    /// uses the whole string.
    ///
    /// Both ends present means each hosts a server and starts a client (which is what
    /// happens when both are public and both declared fake TCP, each dialing its
    /// own).
    FakeTcp { servers: BTreeMap<String, String> },
}

pub fn compile_system(doc: &ModelSnapshot, diagnostics: &mut Vec<Diagnostic>) -> SystemIr {
    // Which machines can relay: those a chain gave a `hop_in`. Once relay ports moved
    // onto the chain this became invisible from the node — no field on a machine
    // carries any trace of "I accept relays" any more, so the chains must be consulted.
    // Skipping this step keeps a relay-only machine that is not on the backbone out of
    // the system layer entirely, so its xray artifact vanishes into thin air while the
    // compile stays green.
    let relays = doc
        .apps
        .iter()
        .flat_map(|app| app.steps.iter())
        .filter(|step| step.hop_in.is_some())
        .map(|step| step.node.as_str())
        .collect::<BTreeSet<_>>();

    let mut nodes = doc
        .nodes
        .iter()
        // A decommissioned machine does not enter the system layer: it is gone from
        // everyone else's wg0.conf and holds no backbone membership itself, so
        // wireguard::build takes the Disabled branch — exactly the desired state a
        // teardown wants. A non-member enters as soon as some chain opened a relay port
        // on it: it can relay, it just does not go through wg. A machine with neither
        // (pure ingress, pure exit) has no presence in the system layer and does not
        // enter this array.
        .filter(|node| !node.retired && (node.overlay || relays.contains(node.id.as_str())))
        .map(|node| {
            // Declaring fake TCP leaves no usable UDP endpoint: it said its own
            // inbound UDP does not work, and leaving one would only have others dial a
            // port already known to be unreachable.
            let endpoint = match &node.wireguard.transport {
                WgTransport::Udp => public_endpoint_host(node)
                    .map(|host| host_port(host, node.wireguard.listen_port)),
                WgTransport::FakeTcp { .. } => None,
            };

            SystemNode {
                id: node.id.clone(),
                tenant: node.tenant.clone(),
                certificate_track: node.certificate_track,
                overlay_addr: node.overlay.then_some(node.overlay_addr),
                wireguard: node.overlay.then(|| WireGuard {
                    private_key: node.wireguard.private_key.clone(),
                    public_key: node.wireguard.public_key.clone(),
                    // Settled first on "UDP is dialable". A machine hosting a phantun
                    // server also needs a fixed port (the server forwards packets to
                    // the local wg), but that is not known until the links are
                    // computed and is filled in below — see
                    // `backfill_listen_ports`.
                    listen_port: endpoint.is_some().then_some(node.wireguard.listen_port),
                    endpoint,
                }),
                mtu: node.mtu.unwrap_or(doc.settings.overlay.mtu),
            }
        })
        .collect::<Vec<_>>();

    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    // Links are generated only between backbone members. A relay machine off the
    // backbone (`wireguard` is None) enters no Link — it has no wg, and nobody should
    // be writing a peer section for it in their own wg0.conf.
    let members = nodes
        .iter()
        .filter(|node| node.wireguard.is_some())
        .cloned()
        .collect::<Vec<_>>();
    let disabled_links = doc
        .settings
        .overlay
        .disabled_links
        .iter()
        .map(|link| {
            if link.a <= link.b {
                (link.a.as_str(), link.b.as_str())
            } else {
                (link.b.as_str(), link.a.as_str())
            }
        })
        .collect::<BTreeSet<_>>();
    let mut links = Vec::new();
    for i in 0..members.len() {
        for j in (i + 1)..members.len() {
            let a = &members[i];
            let b = &members[j];
            if disabled_links.contains(&(a.id.as_str(), b.id.as_str())) {
                diagnostics.push(Diagnostic::info(
                    "link.disabled",
                    format!("{}|{}", a.id, b.id),
                    format!(
                        "{} 与 {} 的 WireGuard 直连已由运营者禁用；双方配置都不生成该 peer",
                        a.id, b.id
                    ),
                ));
                continue;
            }
            let wrap = link_wrap(doc, a, b, diagnostics);
            let endpoints = direct_endpoints(doc, a, b, &wrap);

            // Dial direction rests on one test: whether the far side has an endpoint I
            // can reach. On a disguised link that endpoint is the far side's phantun
            // server, on a bare one its UDP port. No further rule that "the fake-TCP
            // side must not be dialed bare" is needed — the disguise hangs off the
            // link, so wearing it means both directions wear it, and "phantun one way,
            // bare the other" does not exist. Packets always leave through the local
            // phantun, the peer's roaming learns its own tun's address, and the return
            // path holds.
            let a_can = has_landing(&wrap, &endpoints, &b.id);
            let b_can = has_landing(&wrap, &endpoints, &a.id);

            // With neither side able to reach the other, no link is generated. The
            // level is `Info` rather than error or warning: the compiler does not know
            // whether this pair needs to interconnect, the full mesh put every pair on
            // the table, and under a full mesh this combination is inevitable at scale.
            // So the wording is about premises ("a premise does not hold" rather than
            // "the connection failed") — phrased as a failure it would send someone to
            // fix a link that may not need to exist. What genuinely should block is
            // reported by `hop.unreachable` in `hops.rs`, and that one is an Error.
            if !a_can && !b_can {
                diagnostics.push(Diagnostic::info(
                    "link.no-endpoint",
                    format!("{}|{}", a.id, b.id),
                    format!(
                        "{} 与 {} 互联的前提不成立：至少要有一端能通过双方共有的地址族拨入，\
                         而这对机器之间没有可达的非 NAT Endpoint。不生成这条链路——\
                         没有链走这一跳的话，这一对不通是无害的",
                        a.id, b.id
                    ),
                ));
                continue;
            }

            links.push(Link {
                id: format!("l-{}-{}", a.id, b.id),
                a: a.id.clone(),
                b: b.id.clone(),
                // a_can means "a can dial b", so a alone being able to dial is
                // AtoB
                dial: if a_can && b_can {
                    Dial::Both
                } else if a_can {
                    Dial::AtoB
                } else {
                    Dial::BtoA
                },
                keepalive_secs: doc.settings.overlay.keepalive_secs,
                endpoints,
                wrap,
            });
        }
    }

    backfill_listen_ports(doc, &mut nodes, &links);

    if links.len() > 64 {
        diagnostics.push(Diagnostic::warn(
            "system.mesh-size",
            "overlay",
            format!(
                "全互联规模过大：{} 台，{} 条链路，{} peer/台",
                members.len(),
                links.len(),
                members.len().saturating_sub(1)
            ),
        ));
    }

    links.sort_by(|a, b| a.id.cmp(&b.id));

    SystemIr {
        revision: doc.revision,
        overlay_cidr: doc.overlay_cidr,
        settings: doc.settings.clone(),
        node_count: members.len(),
        nodes,
        links,
    }
}

/// The direct UDP landing on each end, selected against the address families the
/// other end can use. The map key is the node being dialed.
fn direct_endpoints(
    doc: &ModelSnapshot,
    a: &SystemNode,
    b: &SystemNode,
    wrap: &LinkWrap,
) -> BTreeMap<String, String> {
    if !matches!(wrap, LinkWrap::Udp) {
        return BTreeMap::new();
    }
    let (Some(a_node), Some(b_node)) = (find_node(doc, &a.id), find_node(doc, &b.id)) else {
        return BTreeMap::new();
    };

    let mut endpoints = BTreeMap::new();
    if let Some(endpoint) = public_endpoint_for(a_node, b_node, b_node.wireguard.listen_port) {
        endpoints.insert(b.id.clone(), endpoint);
    }
    if let Some(endpoint) = public_endpoint_for(b_node, a_node, a_node.wireguard.listen_port) {
        endpoints.insert(a.id.clone(), endpoint);
    }
    endpoints
}

fn host_port(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// A link's disguise, and which end the server lands on.
///
/// Three rules, all derived from "a phantun client only ever dials out":
///
/// 1. Neither side declared fake TCP → bare UDP.
/// 2. The declaring side is itself reachable (has a public endpoint) → the server is
///    hosted there and the peer dials it.
/// 3. The declaring side is behind NAT → it cannot host a server and can only dial
///    out, so the server moves to the peer, using the declaring side's port. This is
///    the rule the old model lacked: it only ever asked "dial me on this port", a
///    machine behind NAT had no answer, and the whole declaration was silently
///    dropped.
///
/// With both ends behind NAT neither can host one, which is an error — this used to
/// pass without a word.
fn link_wrap(
    doc: &ModelSnapshot,
    a: &SystemNode,
    b: &SystemNode,
    diagnostics: &mut Vec<Diagnostic>,
) -> LinkWrap {
    let (a_node, b_node) = match (find_node(doc, &a.id), find_node(doc, &b.id)) {
        (Some(a_node), Some(b_node)) => (a_node, b_node),
        _ => return LinkWrap::Udp,
    };
    let fake_port = |node: &crate::model::Node| match node.wireguard.transport {
        WgTransport::FakeTcp { port } => Some(port),
        WgTransport::Udp => None,
    };
    let (a_ft, b_ft) = (fake_port(a_node), fake_port(b_node));
    if a_ft.is_none() && b_ft.is_none() {
        return LinkWrap::Udp;
    }

    let mut servers = BTreeMap::new();
    // A declaring side reachable from this peer hosts it itself; otherwise it
    // borrows a peer address which it can reach and uses its own port.
    for (me, me_ft, peer, peer_id) in [(a_node, a_ft, b_node, &b.id), (b_node, b_ft, a_node, &a.id)]
    {
        let Some(port) = me_ft else { continue };
        if let Some(endpoint) = public_endpoint_for(peer, me, port) {
            servers.insert(me.id.clone(), endpoint);
        } else if let Some(endpoint) = public_endpoint_for(me, peer, port) {
            servers.entry(peer_id.to_string()).or_insert(endpoint);
        }
    }

    // The same ailment and the same level as `link.no-endpoint`: this pair was never
    // meant to be required to interconnect, the full mesh put them together. With both
    // ends behind NAT a phantun server has nowhere to go, but with no traffic between
    // this pair that is entirely harmless. When a chain really does take this hop, what
    // blocks is `hop.unreachable` in `hops.rs`.
    // The wording covers only what is unique to this case — "there is no link" is said
    // by the `link.no-endpoint` that necessarily accompanies it.
    // phantun is not a NAT traversal tool: it evades UDP blocking and QoS, and changes
    // reachability not at all.
    if servers.is_empty() {
        diagnostics.push(Diagnostic::info(
            "link.fake-tcp-unhostable",
            format!("{}|{}", a.id, b.id),
            format!(
                "{} 与 {} 上声明的伪 TCP 在这一对上前提不成立：\
                 phantun 服务端只能架在能被拨到的那一端，而两台都在 NAT 后面。\
                 伪 TCP 换的是流量形状，不是可达性",
                a.id, b.id
            ),
        ));
        return LinkWrap::Udp;
    }
    LinkWrap::FakeTcp { servers }
}

/// Whether there is an endpoint when I dial the far side. On a disguised link that
/// means whether the far side hosts a server; on a bare one, its UDP port.
fn has_landing(
    wrap: &LinkWrap,
    direct_endpoints: &BTreeMap<String, String>,
    peer_id: &str,
) -> bool {
    match wrap {
        LinkWrap::Udp => direct_endpoints.contains_key(peer_id),
        LinkWrap::FakeTcp { servers } => servers.contains_key(peer_id),
    }
}

/// A machine hosting a phantun server needs a fixed wg UDP port — the server forwards
/// packets to it.
///
/// This step can only follow the links: whether a machine hosts a server is decided by
/// all of its links together, and the links cannot be computed until every machine's
/// endpoint is known.
fn backfill_listen_ports(doc: &ModelSnapshot, nodes: &mut [SystemNode], links: &[Link]) {
    let hosts = links
        .iter()
        .filter_map(|link| match &link.wrap {
            LinkWrap::FakeTcp { servers } => Some(servers.keys()),
            LinkWrap::Udp => None,
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    for node in nodes.iter_mut() {
        if !hosts.contains(&node.id) {
            continue;
        }
        let Some(wg) = node.wireguard.as_mut() else {
            continue;
        };
        if wg.listen_port.is_none() {
            wg.listen_port = find_node(doc, &node.id).map(|n| n.wireguard.listen_port);
        }
    }
}

fn find_node<'a>(doc: &'a ModelSnapshot, id: &str) -> Option<&'a crate::model::Node> {
    doc.nodes.iter().find(|node| node.id == id)
}

fn public_endpoint_host(node: &crate::model::Node) -> Option<&str> {
    node.public_ipv4
        .as_deref()
        .filter(|_| !node.public_ipv4_nat)
        .or_else(|| {
            node.public_ipv6
                .as_deref()
                .filter(|_| !node.public_ipv6_nat)
        })
}

/// Pick a non-NAT address on `target` that `dialer` has a usable address family
/// for. An addressless node retains the historical IPv4-egress assumption; once a
/// node declares only IPv6, inventing IPv4 reachability would be worse than
/// declining the link.
fn public_endpoint_for(
    dialer: &crate::model::Node,
    target: &crate::model::Node,
    port: u16,
) -> Option<String> {
    let dialer_v6 = dialer.public_ipv6.is_some();
    let dialer_v4 = dialer.public_ipv4.is_some() || !dialer_v6;

    if dialer_v4 {
        if let Some(host) = target
            .public_ipv4
            .as_deref()
            .filter(|_| !target.public_ipv4_nat)
        {
            return Some(host_port(host, port));
        }
    }
    if dialer_v6 {
        if let Some(host) = target
            .public_ipv6
            .as_deref()
            .filter(|_| !target.public_ipv6_nat)
        {
            return Some(host_port(host, port));
        }
    }
    None
}
