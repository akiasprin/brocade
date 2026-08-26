use std::{collections::BTreeMap, net::Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::{
    diagnostic::Diagnostic,
    model::{Action, HopDial, HopPool, HopWire, IpFamily},
};

use super::{
    routing::{Accept, AppIr, AppNode, HopIn, Step},
    system::SystemIr,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    pub chain: String,
    pub app_id: Option<String>,
    pub from: String,
    pub to: String,
    pub link: String,
    pub address: String,
    pub port: u16,
    /// Whether this hop runs inside or outside wg. The address is already on
    /// `address`/`port`; this field exists so that "is this hop inside WireGuard" is
    /// answerable at a glance — the first field to look at when chasing why a chain
    /// runs in the clear. Kept here rather than left for each consumer to infer from
    /// the address.
    pub path: HopPath,
    /// The dialer's half of the transport material. The peer's private key stays in
    /// its own `Step.hop_in` and never flows here.
    pub security: HopDialWire,
    pub credential: HopCredential,
    /// What this hop does with the connections it opens. Carried through rather than
    /// re-read from the rule downstream: by this point the rules have been collapsed to
    /// one hop per `(chain, from, to)`, and re-deriving it would mean answering "which of
    /// the rules pointing here decides" a second time, in a second place.
    pub pool: HopPool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HopPath {
    /// Dial the peer's overlay address, wrapped in WireGuard.
    Overlay,
    /// Dial the address written on the chain, bypassing WireGuard.
    Direct,
    /// Do not dial — the peer connects to us and traffic travels back along that
    /// connection (xray reverse).
    ///
    /// Here `address`/`port` is the address the initiator dials, which is `from`'s —
    /// the reverse of the other two variants. Consumers must check `path` before
    /// reading it, or they will dial the upstream's address as the target.
    Reverse,
}

/// The dialer's half of `HopWire`. One variant per model-layer variant, carrying only what
/// the dialer needs.
///
/// For the asymmetric variants that means the public half only: a private key never enters a
/// `Hop`, because a `Hop` is projected along with the App IR into the initiating machine's
/// artifacts. Shadowsocks 2022 has no halves — its key is symmetric, both ends hold the same
/// bytes, and so the secret does travel here. That is the protocol's shape rather than a leak,
/// and it is the cost noted on `HopWire::Shadowsocks2022`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum HopDialWire {
    None,
    Encryption {
        public_key: String,
    },
    /// No `flow`. `Reality.flow` (xtls-rprx-vision) is for the user-facing segment,
    /// where it hides the TLS-in-TLS signature; on a relay hop the outer layer is
    /// already proxied traffic, so the gain is small while any disagreement between
    /// the two ends takes the hop down outright. Adding it would have to land both in
    /// the relay inbound's `clients[]` and here, which is its own change.
    Reality {
        public_key: String,
        server_name: String,
        short_id: String,
        fingerprint: String,
    },
    /// The same two keys the listener holds. Symmetric, so there is no half to withhold.
    Shadowsocks2022 {
        server_psk: String,
        user_psk: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopCredential {
    pub uuid: String,
    pub label: String,
}

// Established facts about a hop's two ends, looked up from the system / app IR before
// dialing.
//
// A struct rather than separate arguments: passed side by side, swapping
// `source_on_overlay` and `linked` draws no complaint from the compiler, and the
// symptom is artifacts generated as usual while traffic vanishes inside wg.
struct HopEndpoints<'a> {
    source: Option<&'a AppNode>,
    target: Option<&'a AppNode>,
    target_overlay: Option<std::net::Ipv4Addr>,
    source_on_overlay: bool,
    linked: bool,
}

/// Resolve the chain's `dial` into a concrete `address:port`. On failure, report a
/// diagnostic and return `None`.
///
/// On failure it never routes elsewhere. A chain written to take a given address
/// that quietly switched to overlay because that address did not hold would produce
/// artifacts that look entirely normal while the traffic took a different path —
/// preventing exactly that is the whole point of writing an address on a chain.
/// Better that it fails to compile.
fn dial_target(
    at: &str,
    from: &str,
    to: &str,
    dial: &HopDial,
    entry_hop_in: &HopIn,
    ends: &HopEndpoints<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, u16, HopPath)> {
    let target_hop_in = entry_hop_in;
    match dial {
        // Both ends being on the overlay is not enough; the pair must actually have a
        // link: wg does no intermediate forwarding, so without that `[Peer]` there is
        // no dialing it, overlay addresses on both sides notwithstanding. The two were
        // equivalent while the backbone was unconditionally fully meshed, and diverged
        // once pairs with both sides behind NAT stopped generating a link (argued in
        // `system.rs`). Without this check, artifacts are generated as usual, the
        // compile is all green, and traffic vanishes inside wg.
        // The port comes from the peer's own hop `hop_in.port` — on the overlay one
        // must dial whichever port the far side listens on.
        HopDial::Overlay => match (ends.target_overlay, ends.source_on_overlay && ends.linked) {
            (Some(addr), true) => Some((addr.to_string(), target_hop_in.port, HopPath::Overlay)),
            (Some(_), false) if ends.source_on_overlay => {
                diagnostics.push(Diagnostic::error(
                    "hop.unreachable",
                    at.to_owned(),
                    format!(
                        "{from} 与 {to} 都在 overlay 里，但两侧都没有可拨入的地址、\
                         没有链路，这一跳没有 overlay 可走"
                    ),
                ));
                None
            }
            (Some(_), false) => {
                diagnostics.push(Diagnostic::error(
                    "hop.unreachable",
                    at.to_owned(),
                    format!("{from} 不在 overlay 里，拨不到 {to} 的 overlay 地址"),
                ));
                None
            }
            (None, _) => {
                diagnostics.push(Diagnostic::error(
                    "hop.unreachable",
                    at.to_owned(),
                    format!("{to} 不在 overlay 里，这一跳没有 overlay 可走"),
                ));
                None
            }
        },
        HopDial::Addr(raw) => {
            let parsed = parse_addr(at, raw, diagnostics)?;
            if let Some(node) = ends.target {
                if let Some((family, address)) = nat_public_match(node, &parsed.0) {
                    diagnostics.push(Diagnostic::error(
                        "hop.nat-public",
                        at.to_owned(),
                        format!("NAT 公网地址不可直拨：{to} {family} {address}"),
                    ));
                    return None;
                }
                if public_match(node, &parsed.0).is_some() {
                    // Public IPv4/IPv6 are the target node's automatic addresses, with
                    // the port following this chain's hop_in.
                    return Some((parsed.0, target_hop_in.port, parsed.2));
                }
            }
            Some(parsed)
        }
        // Reverse access: I do not dial it, it connects to me. So what resolves is the
        // address the peer dials me on, which is this machine's — the meaning of
        // `address`/`port` is the inverse of the other two variants, and
        // `HopPath::Reverse` is the marker that says so to consumers. The address is
        // derived rather than written on the chain (the same reason as `Overlay`): what
        // the peer dials is me, and my address is a node property. The port comes from
        // this machine's `hop_in.port` on this chain.
        HopDial::Reverse(family) => {
            let Some(node) = ends.source else {
                diagnostics.push(Diagnostic::error(
                    "hop.unreachable",
                    at.to_owned(),
                    format!("{from} 不在这个项目里，推不出反向接入地址"),
                ));
                return None;
            };
            let Some(host) = dialable_public_host(node, *family) else {
                // No retry in the other family: whichever was chosen is the one
                // (argued on `HopDial::Reverse`). Addresses behind NAT were already
                // skipped in `dialable_public_host` — such an address receives no
                // reverse connection, for the same reason direct dialing hits
                // `hop.nat-public`.
                diagnostics.push(Diagnostic::error(
                    "hop.unreachable",
                    at.to_owned(),
                    format!(
                        "{from} 没有可拨入的{}，收不到 {to} 的反向接入",
                        family.label()
                    ),
                ));
                return None;
            };
            Some((host.to_owned(), entry_hop_in.port, HopPath::Reverse))
        }
    }
}

/// This machine's dialable public address in a given family.
///
/// No cross-family fallback: asking for v6 looks only at v6, and absent is absent
/// (argued on `HopDial::Reverse`). Addresses marked NAT do not count either — such an
/// address receives no connection from outside, which is precisely why
/// `hop.nat-public` blocks direct dialing. So this variant needs no separate NAT
/// check; the derivation skips them already.
fn dialable_public_host(node: &AppNode, family: IpFamily) -> Option<&str> {
    let (host, nat) = match family {
        IpFamily::V4 => (node.public_ipv4.as_deref(), node.public_ipv4_nat),
        IpFamily::V6 => (node.public_ipv6.as_deref(), node.public_ipv6_nat),
    };
    if nat {
        return None;
    }
    host.filter(|host| !host.is_empty())
}

fn public_match<'a>(node: &'a AppNode, host: &str) -> Option<(&'static str, &'a str)> {
    if !node.public_ipv4_nat {
        if let Some(address) = node
            .public_ipv4
            .as_deref()
            .filter(|address| *address == host)
        {
            return Some(("公网 IPv4", address));
        }
    }
    if !node.public_ipv6_nat {
        if let Some(address) = node
            .public_ipv6
            .as_deref()
            .filter(|address| *address == host)
        {
            return Some(("公网 IPv6", address));
        }
    }
    None
}

fn nat_public_match<'a>(node: &'a AppNode, host: &str) -> Option<(&'static str, &'a str)> {
    if node.public_ipv4_nat {
        if let Some(address) = node
            .public_ipv4
            .as_deref()
            .filter(|address| *address == host)
        {
            return Some(("公网 IPv4", address));
        }
    }
    if node.public_ipv6_nat {
        if let Some(address) = node
            .public_ipv6
            .as_deref()
            .filter(|address| *address == host)
        {
            return Some(("公网 IPv6", address));
        }
    }
    None
}

/// Split `host:port`. On failure it errors rather than guesses — handing an
/// unparseable string downstream puts an undialable address in the artifacts while the
/// compile stays green.
///
/// `rsplit_once` rather than `split_once`, so that IPv6 literals
/// (`[2001:db8::1]:20000`) split correctly.
fn parse_addr(
    at: &str,
    raw: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, u16, HopPath)> {
    let raw = raw.trim();
    let mut bad = |why: &str| {
        diagnostics.push(Diagnostic::error(
            "hop.dial-malformed",
            at.to_owned(),
            format!("拨号地址无效：{raw}（{why}）"),
        ));
        None
    };

    let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
        let Some((host, port)) = rest.split_once("]:") else {
            return bad("IPv6 需写成 [addr]:port");
        };
        (host, port)
    } else {
        let Some((host, port)) = raw.rsplit_once(':') else {
            return bad("没有端口");
        };
        if host.contains(':') && host.parse::<Ipv6Addr>().is_ok() {
            return bad("IPv6 需写成 [addr]:port");
        }
        (host, port)
    };
    if host.trim().is_empty() {
        return bad("没有主机名");
    }
    let Ok(port) = port.trim().parse::<u16>() else {
        return bad("端口不是 1..=65535");
    };
    if port == 0 {
        return bad("端口是 0");
    }

    Some((host.trim().to_owned(), port, HopPath::Direct))
}

pub fn compile_hops(mut app_ir: AppIr, sys: &SystemIr, diagnostics: &mut Vec<Diagnostic>) -> AppIr {
    let mut hops = BTreeMap::<(String, String, String), Hop>::new();

    for step in &app_ir.steps {
        for rule in &step.rules {
            let Action::Forward { to, dial, pool } = &rule.action else {
                continue;
            };

            let at = format!("{}/{}->{}", step.chain, step.node, to);
            let target_step: Option<&Step> = app_ir
                .steps
                .iter()
                .find(|candidate| candidate.chain == step.chain && candidate.node == *to);
            let Some(accept) = target_step.and_then(|target| target.accept.as_ref()) else {
                diagnostics.push(Diagnostic::error(
                    "relay.no-accept",
                    format!("{}/{}", step.chain, to),
                    format!("缺少接受凭据：{to}"),
                ));
                continue;
            };

            // The dialing material comes from the peer's inbound on this chain, not
            // from that machine's global settings — with relay ports split per chain,
            // one machine's wire format can differ entirely between two chains, and
            // taking the wrong one dials chain B's port with chain A's public key.
            // Reverse access takes the other end: there the peer connects to me, so what
            // is wanted is my own relay port on this chain. This is the only place
            // Reverse genuinely breaks symmetry; the edge direction, `Hop.from/to`, and
            // every test below stay as they were.
            let reverse = matches!(dial, HopDial::Reverse(_));
            let entry_step = if reverse { Some(step) } else { target_step };
            let Some(entry_hop_in) = entry_step.and_then(|entry| entry.hop_in.as_ref()) else {
                diagnostics.push(Diagnostic::error(
                    "relay.no-hop-in",
                    at.clone(),
                    if reverse {
                        format!("反向接入缺少中转口：{}/{}", step.node, step.chain)
                    } else {
                        format!("缺少中转口：{to}/{}", step.chain)
                    },
                ));
                continue;
            };

            // Only the overlay path requires the initiator to be on the overlay;
            // dialing a concrete address does not — a machine that never touches wg can
            // still relay, and equating reachability with backbone membership asserts a
            // premise that does not hold.
            //
            // The authorization boundary does not rest on this: whether relaying is
            // allowed is decided by the `accept` credential, which the operator signs
            // per chain.
            let source_on_overlay = sys
                .nodes
                .iter()
                .any(|node| node.id == step.node && node.overlay_addr.is_some());
            let target_overlay = sys
                .nodes
                .iter()
                .find(|node| node.id == *to)
                .and_then(|node| node.overlay_addr);
            // Links are undirected, so both directions count.
            let linked = sys.links.iter().any(|link| {
                (link.a == step.node && link.b == *to) || (link.a == *to && link.b == step.node)
            });
            let ends = HopEndpoints {
                source: app_ir.nodes.iter().find(|node| node.id == step.node),
                target: app_ir.nodes.iter().find(|node| node.id == *to),
                target_overlay,
                source_on_overlay,
                linked,
            };

            let key = (step.chain.clone(), step.node.clone(), to.clone());
            if hops.contains_key(&key) {
                continue;
            }
            let Some((address, port, path)) =
                dial_target(&at, &step.node, to, dial, entry_hop_in, &ends, diagnostics)
            else {
                continue;
            };

            hops.insert(
                key,
                Hop {
                    chain: step.chain.clone(),
                    app_id: step.app_id.clone(),
                    from: step.node.clone(),
                    to: to.clone(),
                    link: link_key(&step.node, to),
                    address,
                    port,
                    path,
                    // Both are material the initiator uses; only whose material it is
                    // follows the initiator. The wire format comes from the relay
                    // port of whichever end is connected to (the peer's when forward, my
                    // own when reverse, already chosen by `entry_hop_in`); the credential
                    // comes from `to`'s `accept` — forward that is the key into the peer,
                    // reverse it is the downstream's own identity, and they happen to be
                    // the same field. The upstream adds it to clients accordingly and
                    // routes that uuid's connection to the portal.
                    security: dial_wire(&entry_hop_in.security),
                    credential: credential(accept),
                    // Reverse opens no outbound, so there is nothing to pool. Zeroed here
                    // as well as refused in `validate`: the diagnostic tells the operator
                    // to fix the model, while this keeps the artifact honest in the
                    // meantime — an artifact must never carry a setting that the machine
                    // has no connection to apply it to.
                    pool: if reverse { HopPool::None } else { *pool },
                },
            );
        }
    }

    app_ir.hops = hops.into_values().collect();
    app_ir
}

/// Project the peer's relay-port material into the dialer's half. The REALITY variant
/// takes the first of `server_names` and `short_ids`, matching how the user projection
/// (`physical/user.rs`) picks — were the two to pick differently, the string one
/// ingress hands a user and the config it hands a relay would point at different
/// camouflage sites.
fn dial_wire(wire: &HopWire) -> HopDialWire {
    match wire {
        HopWire::None => HopDialWire::None,
        HopWire::Encryption(encryption) => HopDialWire::Encryption {
            public_key: encryption.public_key.clone(),
        },
        HopWire::Reality(reality) => HopDialWire::Reality {
            public_key: reality.public_key.clone(),
            server_name: reality.server_names.first().cloned().unwrap_or_default(),
            short_id: reality.short_ids.first().cloned().unwrap_or_default(),
            fingerprint: reality.fingerprint.clone(),
        },
        // Both keys, not a public half — see the note on the enum. The dialer presents them
        // joined; the joining itself is xray's syntax and waits for the artifact layer.
        HopWire::Shadowsocks2022 {
            server_psk,
            user_psk,
        } => HopDialWire::Shadowsocks2022 {
            server_psk: server_psk.clone(),
            user_psk: user_psk.clone(),
        },
    }
}

fn credential(accept: &Accept) -> HopCredential {
    HopCredential {
        uuid: accept.uuid.clone(),
        label: accept.label.clone(),
    }
}

fn link_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}|{b}")
    } else {
        format!("{b}|{a}")
    }
}
