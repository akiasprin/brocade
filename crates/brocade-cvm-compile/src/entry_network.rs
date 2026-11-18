//! Pure nftables plan for entries. Everything this node does for entries lives in one
//! owned `ip` table that is replaced atomically each round. Entries apply to traffic
//! that arrives at the machine; connections the machine opens itself are not forwarded.
use crate::Result;
use brocade_deployment::cvm::*;
use std::{collections::BTreeSet, net::Ipv4Addr};
const TABLE: &str = "brocade_cvm_entries";

/// The Cube gateway device. Traffic delivered from its address is what CubeVS
/// `primary_delivery` lets the Guest answer through the primary NIC.
pub const CUBE_DEVICE: &str = "cube-dev";

pub fn device(tunnel: &EntryTunnel) -> String {
    format!(
        "bxe{}",
        &brocade_core::hash::sha256_hex(tunnel.id.as_bytes())[..12]
    )
}
pub fn mark(tunnel: &EntryTunnel) -> u32 {
    ENTRY_TUNNEL_MARK + u32::from(tunnel.slot)
}
/// `(this end, the other end)` of a tunnel's /30.
pub fn addresses(tunnel: &EntryTunnel) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let (entry, target) = tunnel_addresses(tunnel.slot)?;
    Some(match tunnel.side {
        EntryTunnelSide::Entry => (entry, target),
        EntryTunnelSide::Target => (target, entry),
    })
}

fn protocols(mode: &EntryMode) -> Vec<Option<&'static str>> {
    match mode {
        EntryMode::L3 => vec![None],
        EntryMode::L4 { protocol, .. } => protocol
            .protocols()
            .into_iter()
            .map(|protocol| {
                Some(match protocol {
                    L4Protocol::Tcp => "tcp",
                    L4Protocol::Udp => "udp",
                })
            })
            .collect(),
    }
}
/// Packet selectors of an entry on this machine, one per protocol, before translation.
fn selectors(address: Ipv4Addr, mode: &EntryMode) -> Vec<(String, String)> {
    protocols(mode)
        .into_iter()
        .map(|protocol| match (protocol, mode) {
            (
                Some(protocol),
                EntryMode::L4 {
                    listen_port,
                    target_port,
                    ..
                },
            ) => (
                format!("ip daddr {address} {protocol} dport {listen_port}"),
                format!(":{target_port}"),
            ),
            _ => (format!("ip daddr {address}"), String::new()),
        })
        .collect()
}
/// Delivered traffic at the container: its address and, for L4, its protocol port.
fn delivered(target: Ipv4Addr, mode: &EntryMode) -> Vec<String> {
    protocols(mode)
        .into_iter()
        .map(|protocol| match (protocol, mode) {
            (Some(protocol), EntryMode::L4 { target_port, .. }) => {
                format!("ip daddr {target} {protocol} dport {target_port}")
            }
            _ => format!("ip daddr {target}"),
        })
        .collect()
}

/// Container addresses an entry-side tunnel delivers to; its routing table holds
/// exactly these /32 routes, so the table never captures traffic for anything else.
pub fn targets(desired: &EntryDesired, tunnel: &EntryTunnel) -> BTreeSet<Ipv4Addr> {
    desired
        .rules
        .iter()
        .filter(|rule| rule.tunnel.as_deref() == Some(tunnel.id.as_str()))
        .filter_map(|rule| rule.target)
        .collect()
}

/// Native UDP sockets bound to 0.0.0.0 select their reply source from this route.
/// A tunnel normally prefers its transit IP, which would break the entry's NAT tuple.
pub fn reply_source(desired: &EntryDesired, tunnel: &EntryTunnel) -> Result<Option<Ipv4Addr>> {
    let sources: BTreeSet<_> = desired
        .deliveries
        .iter()
        .filter(|d| d.host_local && d.tunnel == tunnel.id)
        .map(|d| d.target)
        .collect();
    if sources.len() > 1 {
        return Err("native entry deliveries disagree on the reply address".into());
    }
    Ok(sources.first().copied())
}

/// Whether this desired state needs the Cube gateway on this node.
pub fn needs_gateway(desired: &EntryDesired) -> bool {
    desired.deliveries.iter().any(|d| !d.host_local)
        || desired
            .rules
            .iter()
            .any(|rule| rule.tunnel.is_none() && rule.target.is_some() && !rule.host_local)
}

/// The complete owned table. Entries whose tunnel is not `ready` stay fenced like an
/// entry without a target: the address keeps dropping new traffic instead of reaching
/// a host service.
pub fn ruleset(
    desired: &EntryDesired,
    ready: &BTreeSet<&str>,
    transport_ports: &BTreeSet<(L4Protocol, u16)>,
) -> Result<String> {
    let mut out = format!(
        "add table ip {TABLE}\ndelete table ip {TABLE}\ntable ip {TABLE} {{\n\
         chain steer {{ type filter hook prerouting priority -150; policy accept; }}\n\
         chain incoming {{ type nat hook prerouting priority -110; }}\n\
         chain outgoing {{ type nat hook postrouting priority 110; }}\n\
         chain fence {{ type filter hook prerouting priority -90; policy accept; }}\n\
         chain forward {{ type filter hook forward priority -5; policy accept; }}\n\
         chain input {{ type filter hook input priority -5; policy accept; }}\n}}\n"
    );
    let mut push = |chain: &str, rule: String| {
        out.push_str(&format!("add rule ip {TABLE} {chain} {rule}\n"));
    };
    // The transport must reach its socket, including during setup and target outages.
    // These are actual tunnel listeners, never SSH or other host application ports.
    let mut ports = transport_ports.clone();
    ports.extend(
        desired
            .tunnels
            .iter()
            .filter(|t| t.listens())
            .map(|t| (t.protocol(), t.port)),
    );
    for (protocol, port) in ports {
        let protocol = if protocol == L4Protocol::Tcp {
            "tcp"
        } else {
            "udp"
        };
        for chain in ["steer", "incoming", "fence"] {
            push(chain, format!("{protocol} dport {port} return"));
        }
    }
    let tunnel = |id: &str| desired.tunnels.iter().find(|t| t.id == id);
    let mut local = BTreeSet::new();
    for rule in &desired.rules {
        let path = match (&rule.target, &rule.tunnel) {
            (Some(target), None) => Some((*target, None)),
            (Some(target), Some(id)) if ready.contains(id.as_str()) => Some((
                *target,
                Some(tunnel(id).ok_or("entry rule tunnel is not listed")?),
            )),
            _ => None,
        };
        for (packet, port) in selectors(rule.address, &rule.mode) {
            let Some((target, via)) = path else {
                // Claimed but undeliverable: never fall through to a host service.
                push(
                    "fence",
                    format!("{packet} ct direction original ct state new drop"),
                );
                continue;
            };
            push(
                "incoming",
                format!("{packet} ct direction original dnat ip to {target}{port}"),
            );
            match via {
                None => {
                    if !rule.host_local {
                        local.insert(rule.address);
                    }
                }
                // Original-direction packets are marked before translation, so the
                // tunnel's routing table carries them even when container addresses
                // repeat across hosts.
                Some(tunnel) => push(
                    "steer",
                    format!(
                        "{packet} ct direction original meta mark set {:#x}",
                        mark(tunnel)
                    ),
                ),
            }
        }
    }
    // Local delivery leaves from the Cube gateway address, which the Guest answers
    // through its primary NIC; the original entry address identifies our translations.
    for address in local {
        push(
            "outgoing",
            format!(
                "oifname \"{CUBE_DEVICE}\" ct status dnat ct original ip daddr {address} masquerade"
            ),
        );
    }
    for tunnel in &desired.tunnels {
        let dev = device(tunnel);
        let (local, peer) = addresses(tunnel).ok_or("invalid entry tunnel slot")?;
        // Both TCP endpoints live outside the narrower tunnel. Clamp SYN and SYN/ACK before
        // any forwarding accept, including replies delivered through Cube's primary NAT.
        // Never raise a peer's smaller MSS; IPv4 entries reserve 20 bytes for each header.
        let mss = tunnel
            .offer
            .mtu
            .checked_sub(40)
            .ok_or("invalid entry tunnel MTU")?;
        for direction in ["iifname", "oifname"] {
            push("forward", format!("{direction} \"{dev}\" meta l4proto tcp tcp flags & (syn | rst) == syn tcp option maxseg size > {mss} tcp option maxseg size set {mss}"));
        }
        match tunnel.side {
            EntryTunnelSide::Entry => {
                // Replies carry the tunnel mark too, so source validation (with the
                // device's src_valid_mark) finds the container route of this tunnel,
                // while their forwarding to the visitor still uses the main table.
                push(
                    "steer",
                    format!("iifname \"{dev}\" meta mark set {:#x}", mark(tunnel)),
                );
                // Translated traffic leaves from this end's tunnel address, which keeps
                // every flow unique at the host and pins its replies to this tunnel.
                push(
                    "outgoing",
                    format!("oifname \"{dev}\" ct status dnat snat ip to {local}"),
                );
                push(
                    "forward",
                    format!("iifname \"{dev}\" ct direction reply accept"),
                );
            }
            EntryTunnelSide::Target => {
                for delivery in desired.deliveries.iter().filter(|d| d.tunnel == tunnel.id) {
                    for packet in delivered(delivery.target, &delivery.mode) {
                        if delivery.host_local {
                            push("input", format!("iifname \"{dev}\" ip saddr {peer} {packet} ct direction original accept"));
                            continue;
                        }
                        push(
                            "forward",
                            format!(
                                "iifname \"{dev}\" ip saddr {peer} {packet} ct direction original accept"
                            ),
                        );
                    }
                    if !delivery.host_local {
                        push("forward", format!("iifname \"{dev}\" ip saddr {peer} ip daddr {} ct state related accept", delivery.target));
                    }
                }
                push(
                    "outgoing",
                    format!("iifname \"{dev}\" oifname \"{CUBE_DEVICE}\" masquerade"),
                );
            }
        }
        push("forward", format!("iifname \"{dev}\" drop"));
        // Tunnel peers never reach services on this machine.
        push(
            "input",
            format!("iifname \"{dev}\" ip daddr {local} ct state established,related accept"),
        );
        push("input", format!("iifname \"{dev}\" drop"));
    }
    Ok(out)
}
