//! Keeping Xray's public inbound and health-checked Mux TCP out of conntrack.
//!
//! Every flow entering a machine is tracked from the moment any nat table exists, and a VLESS
//! client without mux opens one TCP connection per proxied connection — a minute of ordinary
//! browsing is hundreds of them. Worse, closing one does not release its entry: it sits in
//! TIME_WAIT for `nf_conntrack_tcp_timeout_time_wait`, two minutes by default. What the table
//! holds is therefore not the concurrent connections but every connection of the last two
//! minutes, and on a small machine (the kernel sizes the table from RAM — 431MB yields 3584
//! entries) that is what fills it. A full table drops *new* flows while established ones keep
//! working, so the symptom is "it stopped taking connections", never a clean failure.
//!
//! # Scope
//!
//! `notrack` is legal on a raw-priority chain and exempts exactly the packets a rule matches, so
//! the match is the whole design. Two properties make this pair the safe one:
//!
//! - **The first packet of an inbound connection is a SYN, which is never `established`.** A
//!   machine that accepts users at all therefore already has an explicit ACCEPT for that port in
//!   whatever firewall it runs; untracked packets hit that same rule. Traffic gated only by
//!   `ct state established,related accept` — which is what ufw and firewalld do for return
//!   traffic — is never what arrives here.
//! - **Outbound exemptions name an exact peer IP and TCP port.** Only VLESS/Shadowsocks
//!   outbounds with the explicit health-checked `mux.workerPool` qualify. Ordinary direct,
//!   external-provider and control-plane connections are not exempted as a class. A plain TCP
//!   probe to the same peer endpoint shares the exemption: packet filters cannot distinguish
//!   its application protocol. Stateful host firewalls must explicitly allow these replies.
//!   UDP and loopback remain outside this policy. Eligible peer traffic must not require
//!   host-local NAT, because NOTRACK bypasses it as well.
//!
//! # Its own nft table
//!
//! `inet brocade_conntrack`, distinct from phantun's `inet brocade` and from
//! `brocade_hy2_port_hop`. Each of those is deleted and rebuilt wholesale on its own
//! convergence, so sharing one would have each feature silently delete the others' rules —
//! `hy2_port_hop.rs` documents the same reasoning and the same symptom.
//!
//! # What must not be matched
//!
//! - **Guest traffic.** CVM routing and NAT require conntrack. Bridge ingress returns
//!   before exemptions, including traffic to another local host address. Other prerouting
//!   exemptions require a host-local destination, so forwarded traffic stays tracked even
//!   when it uses the same port or source endpoint as this machine's Xray listeners.
//! - **UDP.** Hysteria 2's hop range is folded onto the listener by a `redirect`, which is NAT,
//!   and NAT is implemented *in* conntrack. Those flows are the ones that cannot be exempted, and
//!   they are also the ones that churn hardest. Nothing to do about it.
//! - **Loopback listens.** xray's gRPC api inbound sits on `127.0.0.1`, as do the XHTTP inbounds
//!   a public front hands off to. They cost one long-lived entry each, and exempting them would
//!   trade nothing for risk.
//! - **A port phantun DNATs.** raw (priority -300) runs before nat (-100), so a notracked packet
//!   never reaches the DNAT and the redirect silently stops happening. Ports are read from
//!   `xray.json`, and phantun's DNAT fronts wireguard rather than an xray inbound, so the two sets
//!   do not intersect — a machine where they did would already be broken, with phantun and xray
//!   claiming one port.

use std::{collections::BTreeSet, net::IpAddr};

use serde_json::Value;

use crate::{
    probe::{xray_listen_ports, XrayListenProtocol},
    run_shell, DesiredArtifact,
};

/// The nft table name. Distinct from phantun's and from port hopping's — see the module docs.
const TABLE: &str = "brocade_conntrack";
// CVM can refresh NAT exclusions while Xray converges its own listeners.
static POLICY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TcpPeer {
    address: IpAddr,
    port: u16,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Exemptions {
    delivery_addresses: BTreeSet<IpAddr>,
    delivery_ports: BTreeSet<(IpAddr, u16)>,
    inbound_ports: BTreeSet<u16>,
    mux_peers: BTreeSet<TcpPeer>,
}

impl Exemptions {
    fn is_empty(&self) -> bool {
        self.inbound_ports.is_empty() && self.mux_peers.is_empty()
    }
}

type Rules = BTreeSet<(String, String)>;
const TRACK_BRIDGE_INGRESS: &str = "meta iifkind \"bridge\" return";

/// Apply for this desired xray artifact, or take the table down when there is no xray.
pub(crate) fn apply(desired: &DesiredArtifact) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => apply_content(content),
        DesiredArtifact::Disabled { .. } => disable(),
        // Nothing was asked of us. An artifact this agent does not manage must be left exactly as
        // it is, including a table somebody else put there.
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

/// Apply from the xray config text alone. The local reconcile has only the file on disk.
pub(crate) fn apply_content(content: &str) -> Result<(), String> {
    let exemptions = exemptions(content)?;
    if exemptions.is_empty() {
        return disable();
    }
    install(&exemptions)
}

/// Whether the machine already matches this xray config.
pub(crate) fn matches_content(content: &str) -> bool {
    let Ok(exemptions) = exemptions(content) else {
        return false;
    };
    match installed() {
        None => exemptions.is_empty(),
        Some(rules) => !exemptions.is_empty() && rules == rules_for(&exemptions),
    }
}

fn exemptions(content: &str) -> Result<Exemptions, String> {
    let config: Value = serde_json::from_str(content)
        .map_err(|error| format!("解析 Xray 连接跟踪配置失败：{error}"))?;
    if !config.is_object() {
        return Err("Xray 连接跟踪配置必须是对象".to_owned());
    }
    let mut exemptions = Exemptions {
        inbound_ports: ports(content),
        delivery_addresses: crate::network_tracking::addresses()?,
        delivery_ports: crate::network_tracking::ports()?,
        ..Exemptions::default()
    };
    for outbound in config["outbounds"].as_array().into_iter().flatten() {
        if outbound.pointer("/mux/enabled").and_then(Value::as_bool) != Some(true)
            || !outbound
                .pointer("/mux/workerPool")
                .is_some_and(Value::is_object)
        {
            continue;
        }
        let network = outbound
            .pointer("/streamSettings/network")
            .and_then(Value::as_str)
            .unwrap_or("tcp");
        if !matches!(network, "tcp" | "raw") {
            continue;
        }
        let settings = &outbound["settings"];
        let peers: Vec<&Value> = match outbound["protocol"].as_str() {
            Some("vless") if settings.get("address").is_some() => vec![settings],
            Some("vless") => settings["vnext"].as_array().into_iter().flatten().collect(),
            Some("shadowsocks") => settings["servers"]
                .as_array()
                .into_iter()
                .flatten()
                .collect(),
            _ => continue,
        };
        if peers.is_empty() {
            return Err("健康 Mux TCP 出站缺少连接跟踪免除端点".to_owned());
        }
        for peer in peers {
            // No DNS lookup here: a cached address can differ from the one Xray dials, and
            // exempting a hostname's unrelated later address would silently change scope.
            let address = peer["address"]
                .as_str()
                .and_then(|address| address.parse::<IpAddr>().ok())
                .ok_or("健康 Mux TCP 免除连接跟踪需要明确的 IP 端点")?;
            let port = peer["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .ok_or("健康 Mux TCP 免除连接跟踪端口无效")?;
            if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
                continue;
            }
            exemptions.mux_peers.insert(TcpPeer { address, port });
        }
    }
    Ok(exemptions)
}

/// The ports to exempt: every public TCP inbound in `xray.json`.
///
/// A `BTreeSet` so the result is deduplicated and ordered, which is what makes comparing it
/// against what the machine reports a plain equality rather than a sort at each call site.
fn ports(content: &str) -> BTreeSet<u16> {
    xray_listen_ports(content)
        .into_iter()
        .filter(|(_, listen, _, protocol)| {
            *protocol == XrayListenProtocol::Tcp && !is_loopback(listen)
        })
        .map(|(_, _, port, _)| port)
        .collect()
}

/// Whether an inbound's `listen` keeps it on this machine.
///
/// An unparseable value counts as public. xray defaults the field to `0.0.0.0` when it is absent
/// and `xray_listen_ports` reproduces that, so the remaining unparseable cases are a hostname or
/// something malformed — and treating those as loopback would silently skip a port that is in
/// fact reachable, which is the failure that is invisible.
fn is_loopback(listen: &str) -> bool {
    listen
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// The rule lines, split out so a test can read them without a machine to run them on.
///
/// Two per port, and both are needed. Untracked packets have no flow for the reverse direction to
/// be recognised by, so the reply has to be matched on its own: `dport` on the way in, `sport` on
/// the way back out. With only the first, xray's replies would build the conntrack entry the
/// first rule declined to create, and the exemption would save nothing — the same trap the ICMP
/// rules in `hy2_port_hop.rs` document.
fn rules_for(exemptions: &Exemptions) -> Rules {
    let mut rules = Rules::new();
    if !exemptions.is_empty() {
        // This sorts before every TCP exemption in this chain. A bridge may
        // deliver a Guest packet to another local host address as well as forward
        // it; both paths need conntrack for CVM routing and reply isolation.
        rules.insert(("prerouting_raw".to_owned(), TRACK_BRIDGE_INGRESS.to_owned()));
        for ip in &exemptions.delivery_addresses {
            add_delivery_guards(&mut rules, *ip);
        }
        for (ip, port) in &exemptions.delivery_ports {
            add_delivery_port_guards(&mut rules, *ip, *port);
        }
    }
    for port in &exemptions.inbound_ports {
        rules.insert((
            "prerouting_raw".to_owned(),
            format!("tcp dport {port} fib daddr type local notrack"),
        ));
        rules.insert(("output_raw".to_owned(), format!("tcp sport {port} notrack")));
    }
    for peer in &exemptions.mux_peers {
        let family = if peer.address.is_ipv4() { "ip" } else { "ip6" };
        rules.insert((
            "output_raw".to_owned(),
            format!(
                "{family} daddr {} tcp dport {} notrack",
                peer.address, peer.port
            ),
        ));
        rules.insert((
            "prerouting_raw".to_owned(),
            format!(
                "tcp sport {} {family} saddr {} fib daddr type local notrack",
                peer.port, peer.address
            ),
        ));
    }
    rules
}

fn rule_lines(exemptions: &Exemptions) -> Vec<String> {
    ordered_rules(rules_for(exemptions))
        .into_iter()
        .map(|(chain, rule)| format!("add rule inet {TABLE} {chain} {rule}"))
        .collect()
}
fn add_delivery_guards(rules: &mut Rules, ip: IpAddr) {
    let family = if ip.is_ipv4() { "ip" } else { "ip6" };
    for chain in ["prerouting_raw", "output_raw"] {
        rules.insert((chain.into(), format!("{family} daddr {ip} return")));
        rules.insert((chain.into(), format!("{family} saddr {ip} return")));
    }
}
fn add_delivery_port_guards(rules: &mut Rules, ip: IpAddr, port: u16) {
    let family = if ip.is_ipv4() { "ip" } else { "ip6" };
    for chain in ["prerouting_raw", "output_raw"] {
        rules.insert((
            chain.into(),
            format!("{family} daddr {ip} tcp dport {port} return"),
        ));
        rules.insert((
            chain.into(),
            format!("{family} saddr {ip} tcp sport {port} return"),
        ));
    }
}
fn ordered_rules(rules: Rules) -> Vec<(String, String)> {
    let mut rules: Vec<_> = rules.into_iter().collect();
    rules.sort_by_key(|(chain, rule)| {
        (
            chain.clone(),
            if rule == TRACK_BRIDGE_INGRESS {
                0
            } else if rule.ends_with(" return") {
                1
            } else {
                2
            },
            rule.clone(),
        )
    });
    rules
}
/// Preserve the NAT requirement even before the next Xray configuration round.
#[cfg(feature = "cvm")]
pub(crate) fn track_delivery_scopes(
    addresses: &BTreeSet<IpAddr>,
    ports: &BTreeSet<(IpAddr, u16)>,
) -> Result<(), String> {
    let _policy = POLICY_LOCK
        .lock()
        .map_err(|_| "conntrack policy lock unavailable")?;
    let Some(mut rules) = installed() else {
        if crate::command::command_success("nft", &["list", "table", "inet", TABLE]) {
            return Err("cannot verify existing NOTRACK policy for CVM delivery".into());
        }
        return Ok(());
    };
    // This table is owned by this module: its only return rules are the bridge
    // prerequisite and delivery guards. Replace the delivery guards with the current scopes.
    rules.retain(|(_, rule)| !rule.ends_with(" return") || rule == TRACK_BRIDGE_INGRESS);
    for ip in addresses {
        add_delivery_guards(&mut rules, *ip);
    }
    for (ip, port) in ports {
        add_delivery_port_guards(&mut rules, *ip, *port);
    }
    let lines = ordered_rules(rules)
        .into_iter()
        .map(|(chain, rule)| format!("add rule inet {TABLE} {chain} {rule}"))
        .collect::<Vec<_>>()
        .join("\n");
    run_shell(&format!("set -eu\nnft -f - <<'CVM_TRACK'\nflush chain inet {TABLE} prerouting_raw\nflush chain inet {TABLE} output_raw\n{lines}\nCVM_TRACK\n")).map(|_|())
}

fn install(exemptions: &Exemptions) -> Result<(), String> {
    let _policy = POLICY_LOCK
        .lock()
        .map_err(|_| "conntrack policy lock unavailable")?;
    let current = Exemptions {
        delivery_addresses: crate::network_tracking::addresses()?,
        delivery_ports: crate::network_tracking::ports()?,
        inbound_ports: exemptions.inbound_ports.clone(),
        mux_peers: exemptions.mux_peers.clone(),
    };
    let rules = rule_lines(&current).join("\n");
    // Add/delete/recreate in one transaction works for an absent or existing table. Never
    // expose a gap in NOTRACK while replacing policy, or briefly tracked flows can survive it.
    run_shell(&format!(
        "set -eu\n\
         command -v nft >/dev/null 2>&1 || {{ echo '需要 nft（nftables）才能免除连接跟踪' >&2; exit 1; }}\n\
         nft -f - <<'NFT'\n\
add table inet {TABLE}\n\
delete table inet {TABLE}\n\
table inet {TABLE} {{\n\
  chain prerouting_raw {{ type filter hook prerouting priority raw; }}\n\
  chain output_raw {{ type filter hook output priority raw; }}\n\
}}\n\
{rules}\n\
NFT\n"
    ))
    .map(|_| ())
}

pub(crate) fn disable() -> Result<(), String> {
    let _policy = POLICY_LOCK
        .lock()
        .map_err(|_| "conntrack policy lock unavailable")?;
    // Absent table is success: this runs on every convergence of a machine that has no public TCP
    // inbound, and `nft delete` on a missing table exits non-zero.
    run_shell(&format!(
        "nft delete table inet {TABLE} 2>/dev/null || true"
    ))
    .map(|_| ())
}

/// Which ports are actually exempt on this machine.
///
/// Reads the machine rather than remembering what was written: the failure this exists to catch is
/// somebody else's `nft flush ruleset` (or a firewalld reload) taking our table with it, and a
/// remembered value would report the rules as present for as long as the agent kept running.
///
/// Both directions and raw hooks are checked: losing only the return exemption still creates
/// conntrack entries, even when the initiating packet was exempted.
fn installed() -> Option<Rules> {
    let Ok(text) = run_shell(&format!(
        "nft -n list table inet {TABLE} 2>/dev/null || true"
    )) else {
        return None;
    };
    parse_installed(&text)
}

/// Split from `installed` so a test can feed it `nft list table` output without a machine.
fn parse_installed(text: &str) -> Option<Rules> {
    let mut chain = None;
    let mut raw_hooks = BTreeSet::new();
    let mut rules = Rules::new();
    let mut bridge_guard = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix("chain ")
            .and_then(|line| line.strip_suffix(" {"))
        {
            chain = Some(name);
        } else if line == "}" {
            chain = None;
        } else if let Some(name @ ("prerouting_raw" | "output_raw")) = chain {
            if line.starts_with("type ") {
                let hook = if name == "prerouting_raw" {
                    "prerouting"
                } else {
                    "output"
                };
                let expected = format!("type filter hook {hook} priority ");
                if line.strip_prefix(&expected).is_some_and(|rest| {
                    matches!(rest, "raw; policy accept;" | "-300; policy accept;")
                }) {
                    raw_hooks.insert(name);
                }
            } else if !line.is_empty() {
                if name == "prerouting_raw" {
                    if line == TRACK_BRIDGE_INGRESS {
                        if bridge_guard {
                            return None;
                        }
                        bridge_guard = true;
                    } else if !bridge_guard {
                        return None;
                    }
                }
                // `nft -n` prints RTN_LOCAL numerically on some versions.
                let normalized =
                    line.replace("fib daddr type 2 notrack", "fib daddr type local notrack");
                rules.insert((name.to_owned(), normalized));
            }
        }
    }
    (raw_hooks.len() == 2 && bridge_guard).then_some(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_delivery_tracks_only_its_tcp_tuple_and_preserves_other_exemptions() {
        let rules = rule_lines(&Exemptions {
            inbound_ports: BTreeSet::from([443, 30222]),
            delivery_ports: BTreeSet::from([("198.51.100.20".parse().unwrap(), 30222)]),
            ..Exemptions::default()
        })
        .join("\n");
        assert!(rules.contains("ip daddr 198.51.100.20 tcp dport 30222 return"));
        assert!(rules.contains("ip saddr 198.51.100.20 tcp sport 30222 return"));
        assert!(rules.contains("tcp dport 443 fib daddr type local notrack"));
        assert!(!rules.contains("ip daddr 198.51.100.20 return"));
        assert!(!rules.contains("ip saddr 198.51.100.20 return"));
    }

    const CONFIG: &str = r#"{"inbounds":[
        {"tag":"api","listen":"127.0.0.1","port":10085,"protocol":"dokodemo-door"},
        {"tag":"in:vless","listen":"0.0.0.0","port":444,"protocol":"vless"},
        {"tag":"in:xhttp","listen":"::1","port":8080,"protocol":"vless"},
        {"tag":"in:hy2","listen":"0.0.0.0","port":443,"protocol":"hysteria"}
    ]}"#;

    /// The selection is the whole feature. Taking the hysteria port would break port hopping,
    /// because `redirect` is NAT and NAT lives in conntrack; taking a loopback port would trade
    /// nothing for risk.
    #[test]
    fn only_public_tcp_inbounds_are_exempted() {
        assert_eq!(ports(CONFIG), BTreeSet::from([444]));
    }

    /// An `nft` error at convergence is loud, but a *plausible* rule — `drop` instead of
    /// `notrack`, a missing direction — loads fine and quietly does the wrong thing to real
    /// traffic. Both directions are pinned here for that reason.
    #[test]
    fn each_port_becomes_one_rule_in_each_direction() {
        assert_eq!(
            rule_lines(&Exemptions {
                inbound_ports: BTreeSet::from([444, 8443]),
                ..Exemptions::default()
            }),
            vec![
                "add rule inet brocade_conntrack output_raw tcp sport 444 notrack",
                "add rule inet brocade_conntrack output_raw tcp sport 8443 notrack",
                "add rule inet brocade_conntrack prerouting_raw meta iifkind \"bridge\" return",
                "add rule inet brocade_conntrack prerouting_raw tcp dport 444 fib daddr type local notrack",
                "add rule inet brocade_conntrack prerouting_raw tcp dport 8443 fib daddr type local notrack",
            ]
        );
    }

    /// What `nft list table` prints has to come back out as what went in, or the reconcile loop
    /// rebuilds a table that already matches on every single round.
    #[test]
    fn what_nft_prints_parses_back_to_what_was_written() {
        let printed = r#"table inet brocade_conntrack {
 chain prerouting_raw {
  type filter hook prerouting priority raw; policy accept;
  meta iifkind "bridge" return
  tcp dport 444 fib daddr type local notrack
  tcp dport 8443 fib daddr type local notrack
 }
 chain output_raw {
  type filter hook output priority raw; policy accept;
  tcp sport 444 notrack
  tcp sport 8443 notrack
 }
}"#;
        assert_eq!(
            parse_installed(printed),
            Some(rules_for(&Exemptions {
                inbound_ports: BTreeSet::from([444, 8443]),
                ..Exemptions::default()
            }))
        );
        assert_ne!(
            parse_installed(&printed.replace("tcp sport 8443 notrack", "")),
            parse_installed(printed)
        );
        assert_eq!(
            parse_installed(&printed.replace("fib daddr type local", "fib daddr type 2")),
            parse_installed(printed)
        );
        assert!(parse_installed(&printed.replace("priority raw", "priority filter")).is_none());
        let reordered = printed
            .replace("  meta iifkind \"bridge\" return\n", "")
            .replace(
                "  tcp dport 8443 fib daddr type local notrack",
                "  tcp dport 8443 fib daddr type local notrack\n  meta iifkind \"bridge\" return",
            );
        assert!(
            parse_installed(&reordered).is_none(),
            "bridge guard must precede exemptions"
        );
    }

    /// An xray with no public TCP inbound must take the table down rather than leave a stale one:
    /// the rules name ports, and a port that moved would keep being exempted at its old number.
    #[test]
    fn a_config_with_nothing_to_exempt_yields_no_ports() {
        let hysteria_only = r#"{"inbounds":[
            {"tag":"api","listen":"127.0.0.1","port":10085,"protocol":"dokodemo-door"},
            {"tag":"in:hy2","listen":"0.0.0.0","port":443,"protocol":"hysteria"}
        ]}"#;
        assert!(ports(hysteria_only).is_empty());
    }

    /// A `listen` that is neither an address nor absent counts as public. Reading it as loopback
    /// would skip a port that is in fact reachable, and nothing downstream would say so.
    #[test]
    fn an_unparseable_listen_counts_as_public() {
        assert!(!is_loopback("example.net"));
        assert!(!is_loopback("0.0.0.0"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
    }

    fn health_mux(protocol: &str, settings: Value) -> Value {
        serde_json::json!({
            "protocol": protocol,
            "settings": settings,
            "streamSettings": { "network": "tcp" },
            "mux": { "enabled": true, "workerPool": { "probeIntervalMs": 5000 } }
        })
    }

    #[test]
    fn health_checked_mux_peers_are_exact_bidirectional_and_deduplicated() {
        let vless = health_mux(
            "vless",
            serde_json::json!({
                "vnext": [{"address": "192.0.2.17", "port": 20001}]
            }),
        );
        let shadowsocks = health_mux(
            "shadowsocks",
            serde_json::json!({
                "servers": [{"address": "2001:db8::17", "port": 20002}]
            }),
        );
        let config = serde_json::json!({"outbounds": [vless.clone(), shadowsocks, vless]});
        let exemptions = exemptions(&config.to_string()).unwrap();
        assert!(exemptions.inbound_ports.is_empty());
        assert_eq!(exemptions.mux_peers.len(), 2);
        assert_eq!(
            rule_lines(&exemptions),
            vec![
                "add rule inet brocade_conntrack output_raw ip daddr 192.0.2.17 tcp dport 20001 notrack",
                "add rule inet brocade_conntrack output_raw ip6 daddr 2001:db8::17 tcp dport 20002 notrack",
                "add rule inet brocade_conntrack prerouting_raw meta iifkind \"bridge\" return",
                "add rule inet brocade_conntrack prerouting_raw tcp sport 20001 ip saddr 192.0.2.17 fib daddr type local notrack",
                "add rule inet brocade_conntrack prerouting_raw tcp sport 20002 ip6 saddr 2001:db8::17 fib daddr type local notrack",
            ]
        );
    }

    #[test]
    fn ordinary_mux_other_transports_and_local_peers_remain_tracked() {
        let base = health_mux(
            "vless",
            serde_json::json!({
                "vnext": [{"address": "192.0.2.17", "port": 20001}]
            }),
        );
        let mut ordinary = base.clone();
        ordinary["mux"]
            .as_object_mut()
            .unwrap()
            .remove("workerPool");
        let mut disabled = base.clone();
        disabled["mux"]["enabled"] = serde_json::json!(false);
        let mut xhttp = base.clone();
        xhttp["streamSettings"]["network"] = serde_json::json!("xhttp");
        let freedom = health_mux("freedom", serde_json::json!({}));
        let local = health_mux(
            "vless",
            serde_json::json!({
                "vnext": [{"address": "127.0.0.1", "port": 20001},
                          {"address": "::1", "port": 20001},
                          {"address": "0.0.0.0", "port": 20001},
                          {"address": "ff02::1", "port": 20001}]
            }),
        );
        let config = serde_json::json!({"outbounds": [ordinary, disabled, xhttp, freedom, local]});
        assert!(exemptions(&config.to_string()).unwrap().is_empty());
    }

    #[test]
    fn simplified_vless_health_mux_is_supported() {
        let outbound = health_mux(
            "vless",
            serde_json::json!({"address": "192.0.2.17", "port": 20001}),
        );
        let config = serde_json::json!({"outbounds": [outbound]});
        assert_eq!(exemptions(&config.to_string()).unwrap().mux_peers.len(), 1);
    }

    #[test]
    fn invalid_health_mux_endpoints_fail_without_interpolating_input() {
        for settings in [
            serde_json::json!({"vnext": [{"address": "peer.example", "port": 20001}]}),
            serde_json::json!({"vnext": [{"address": "192.0.2.17", "port": 0}]}),
            serde_json::json!({"vnext": [{"address": "192.0.2.17", "port": 65536}]}),
            serde_json::json!({"vnext": [{"address": "192.0.2.17", "port": "20001"}]}),
            serde_json::json!({"vnext": []}),
        ] {
            let config = serde_json::json!({"outbounds": [health_mux("vless", settings)]});
            let error = exemptions(&config.to_string()).unwrap_err();
            assert!(!error.contains("peer.example"));
            assert!(!matches_content(&config.to_string()));
        }
        assert!(exemptions("invalid json").is_err());
        assert!(exemptions("null").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires tests/conntrack_netns.py in a private user/network namespace"]
    fn installed_health_mux_policy_in_private_namespace() {
        use std::{
            fs,
            net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
            time::Duration,
        };

        for namespace in ["user", "net"] {
            let ancestor_fd = std::env::var(format!(
                "BROCADE_TEST_ANCESTOR_{}_FD",
                namespace.to_uppercase()
            ))
            .unwrap();
            let ancestor = fs::read_link(format!("/proc/self/fd/{ancestor_fd}")).unwrap();
            assert!(ancestor
                .to_string_lossy()
                .starts_with(&format!("{namespace}:[")));
            assert_ne!(
                fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
                ancestor,
                "this test must never modify the host network namespace"
            );
        }
        assert_eq!(
            std::env::var("BROCADE_TEST_CONNTRACK_PEER").as_deref(),
            Ok("ready")
        );
        let config = serde_json::json!({"inbounds": [{"tag": "in:vless", "listen": "::", "port": 23001, "protocol": "vless"}], "outbounds": [
            health_mux("vless", serde_json::json!({"vnext": [
                {"address": "192.0.2.17", "port": 20001},
                {"address": "2001:db8:1::17", "port": 20003}
            ]}))
        ]})
        .to_string();
        // Force normal conntrack hooks on for the positive controls; an empty table alone
        // must not make the NOTRACK assertions pass on an otherwise untracked namespace.
        run_shell("nft -f - <<'NFT'\nadd table inet conntrack_test\nadd chain inet conntrack_test output { type filter hook output priority filter; policy accept; }\nadd rule inet conntrack_test output ct state established counter\nNFT\n").unwrap();
        apply_content(&config).unwrap();
        assert!(
            matches_content(&config),
            "installed rules: {:?}",
            installed()
        );
        apply_content(&config).unwrap();
        assert!(
            matches_content(&config),
            "atomic replacement must work for an existing table"
        );
        let fixture = std::env::var("BROCADE_TEST_CONNTRACK_FIXTURE").unwrap();
        let tracked = |protocol: &str, local: SocketAddr, target: SocketAddr| {
            let source_ip = local.ip().to_string();
            let source_port = local.port().to_string();
            let target_ip = target.ip().to_string();
            let target_port = target.port().to_string();
            let result = crate::command::run_command(
                "python3",
                &[
                    &fixture,
                    "--tracked",
                    protocol,
                    &source_ip,
                    &source_port,
                    &target_ip,
                    &target_port,
                ],
            )
            .unwrap();
            match result.trim() {
                "present" => true,
                "absent" => false,
                other => panic!("unexpected conntrack query result: {other}"),
            }
        };
        let mut streams = Vec::new();
        for (target, expected_tracking) in [
            ("192.0.2.17:20001", false),
            ("[2001:db8:1::17]:20003", false),
            ("[2001:db8:1::17]:20004", true),
            ("192.0.2.17:20002", true),
            ("192.0.2.18:20001", true),
        ] {
            let target: SocketAddr = target.parse().unwrap();
            let stream = TcpStream::connect_timeout(&target, Duration::from_secs(3)).unwrap();
            let local = stream.local_addr().unwrap();
            assert_eq!(
                tracked("tcp", local, target),
                expected_tracking,
                "tracking for {target}"
            );
            streams.push(stream);
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let loopback = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_accepted, _) = listener.accept().unwrap();
        assert!(tracked(
            "tcp",
            loopback.local_addr().unwrap(),
            listener.local_addr().unwrap()
        ));
        let udp = UdpSocket::bind("192.0.2.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        udp.connect("192.0.2.17:20001").unwrap();
        udp.send(b"conntrack-test").unwrap();
        let mut reply = [0; 32];
        let length = udp.recv(&mut reply).unwrap();
        assert_eq!(&reply[..length], b"conntrack-test");
        assert!(tracked(
            "udp",
            udp.local_addr().unwrap(),
            udp.peer_addr().unwrap()
        ));
        crate::command::run_command("python3", &[&fixture, "--bridge-check"]).unwrap();
        run_shell("nft flush chain inet brocade_conntrack prerouting_raw").unwrap();
        assert!(
            !matches_content(&config),
            "missing reply exemptions must be detected"
        );
        apply_content(&config).unwrap();
        assert!(matches_content(&config));
        disable().unwrap();
        assert!(matches_content("{}"));
    }
}
