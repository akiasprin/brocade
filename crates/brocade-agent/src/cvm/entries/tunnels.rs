//! WireGuard and Xray machine-pair tunnels in the root namespace. Devices carry an ownership
//! alias and an atomic creation group; foreign devices are never modified. Devices the desired
//! state no longer lists are removed together with their policy route.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

const ALIAS: &str = "brocade-entry:";
/// One policy rule priority for every entry tunnel, ahead of the main table.
const RULE_PRIORITY: u64 = 31700;

fn links() -> Result<Vec<serde_json::Value>> {
    serde_json::from_str(
        &run_command("ip", &["-d", "-j", "link", "show"]).map_err(|_| "cannot list host links")?,
    )
    .map_err(|_| "invalid host link inventory".into())
}
fn owned(link: &serde_json::Value) -> Option<&str> {
    link["ifalias"].as_str()?.strip_prefix(ALIAS)
}
// IFLA_IFALIAS is ignored on link creation. IFLA_GROUP is applied in the
// creation request, so a crash before setting the human-readable alias still
// leaves an owned device. Bind that reserved group to the exact device name.
fn creation_group(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix("bxe")?;
    if suffix.len() != 12 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(0x6bc00000 | (u64::from_str_radix(suffix, 16).ok()? as u32 & 0xfffff))
}
fn unfinished_creation(link: &serde_json::Value) -> bool {
    let Some(group) = link["ifname"].as_str().and_then(creation_group) else {
        return false;
    };
    let actual = link["group"]
        .as_u64()
        .or_else(|| link["group"].as_str()?.parse().ok());
    link["ifalias"].as_str().is_none_or(str::is_empty)
        && link["linkinfo"]["info_kind"] == "wireguard"
        && actual == Some(u64::from(group))
}
fn ip(args: &[&str]) -> Result<String> {
    run_command("ip", args)
        .map_err(|_| format!("entry tunnel routing failed: ip {}", args.join(" ")))
}
fn sysctl(key: &str, value: &str) -> Result<()> {
    run_command("sysctl", &["-qw", &format!("{key}={value}")])
        .map(|_| ())
        .map_err(|_| format!("cannot set {key}"))
}

/// Brings one tunnel to its desired configuration and reports whether a handshake
/// happened recently. `wg syncconf` applies only differences, so a stable tunnel is
/// not reset by repeated rounds.
pub(super) fn ensure(
    root: &Path,
    tunnel: &EntryTunnel,
    targets: &BTreeSet<Ipv4Addr>,
    reply_source: Option<Ipv4Addr>,
    inventory: &[serde_json::Value],
) -> Result<(bool, bool)> {
    let dev = plan::device(tunnel);
    let (local, _) = plan::addresses(tunnel).ok_or("invalid entry tunnel slot")?;
    let connected = if tunnel.offer.kind == TransportKind::Xray {
        let (configured, connected) = super::super::nics::xray::apply_entry(root, tunnel, targets)?;
        if !configured {
            return Ok((false, false));
        }
        connected
    } else {
        wireguard(root, tunnel, inventory)?;
        false
    };
    ip(&["address", "replace", &format!("{local}/30"), "dev", &dev])?;
    ip(&[
        "link",
        "set",
        "dev",
        &dev,
        "mtu",
        &tunnel.offer.mtu.to_string(),
        "up",
    ])?;
    if tunnel.side == EntryTunnelSide::Target {
        let (_, peer) = plan::addresses(tunnel).ok_or("invalid entry tunnel slot")?;
        let prefix = format!("{peer}/32");
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&["-4", "-N", "-j", "route", "show", "exact", &prefix])?)
                .map_err(|_| "invalid native entry reply route inventory")?;
        if rows
            .iter()
            .any(|r| r["dev"] != dev || !(r["protocol"] == "186" || r["protocol"] == 186))
        {
            return Err("native entry reply route belongs to another owner".into());
        }
        if let Some(source) = reply_source {
            ip(&[
                "-4",
                "route",
                "replace",
                &prefix,
                "dev",
                &dev,
                "src",
                &source.to_string(),
                "proto",
                "186",
            ])?;
        } else if !rows.is_empty() {
            ip(&[
                "-4", "route", "delete", &prefix, "dev", &dev, "proto", "186",
            ])?;
        }
    }
    if tunnel.side == EntryTunnelSide::Entry {
        // Replies come from container addresses the main table does not route here;
        // with src_valid_mark the marked reply validates against this tunnel's table.
        sysctl(&format!("net.ipv4.conf.{dev}.src_valid_mark"), "1")?;
        let table = plan::mark(tunnel).to_string();
        // An empty table does not exist yet; listing every table avoids that error.
        let routes: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&["-4", "-j", "route", "show", "table", "all"])?)
                .map_err(|_| "invalid entry tunnel routes")?;
        for route in routes
            .iter()
            .filter(|route| route["table"].to_string().trim_matches('"') == table)
        {
            let stale = route["dst"]
                .as_str()
                .and_then(|dst| dst.parse::<Ipv4Addr>().ok())
                .is_none_or(|dst| !targets.contains(&dst) || route["dev"] != dev.as_str());
            if stale {
                let dst = route["dst"].as_str().unwrap_or("default");
                ip(&["-4", "route", "delete", dst, "table", &table])?;
            }
        }
        for target in targets {
            ip(&[
                "-4",
                "route",
                "replace",
                &format!("{target}/32"),
                "dev",
                &dev,
                "table",
                &table,
            ])?;
        }
        let rules: Vec<serde_json::Value> =
            serde_json::from_str(&ip(&["-4", "-j", "rule", "show"])?)
                .map_err(|_| "invalid host routing rules")?;
        let mark = format!("{:#x}", plan::mark(tunnel));
        if !rules.iter().any(|rule| {
            rule["priority"].as_u64() == Some(RULE_PRIORITY)
                && rule["fwmark"].as_str() == Some(mark.as_str())
                && rule["table"].to_string().trim_matches('"') == table
        }) {
            ip(&[
                "-4",
                "rule",
                "add",
                "pref",
                &RULE_PRIORITY.to_string(),
                "fwmark",
                &mark,
                "lookup",
                &table,
            ])?;
        }
    }
    if tunnel.offer.kind == TransportKind::Xray {
        return Ok((true, connected));
    }
    let handshakes = run_command("wg", &["show", &dev, "latest-handshakes"]).map_err(|error| {
        super::super::diagnostics::wireguard_error("WireGuard 入口通道状态读取", &error)
    })?;
    let transport_connected =
        tunnel.wireguard_transport.is_udp() || super::phantun::connected(tunnel)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid host clock")?
        .as_secs();
    Ok((
        true,
        transport_connected
            && handshakes
                .lines()
                .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
                .any(|at| at > 0 && now.saturating_sub(at) < 180),
    ))
}

fn wireguard(root: &Path, tunnel: &EntryTunnel, inventory: &[serde_json::Value]) -> Result<()> {
    for key in [&tunnel.credential, &tunnel.peer_credential] {
        if STANDARD.decode(key).map_or(true, |key| key.len() != 32) {
            return Err("invalid WireGuard entry tunnel credentials".into());
        }
    }
    let dev = plan::device(tunnel);
    match inventory.iter().find(|link| link["ifname"] == dev.as_str()) {
        Some(link) if owned(link) == Some(tunnel.id.as_str()) => {}
        Some(link) if unfinished_creation(link) => {}
        Some(_) => return Err("入口通道设备名已被其他网卡占用，未修改".into()),
        None => {
            ip(&[
                "link",
                "add",
                &dev,
                "group",
                &creation_group(&dev)
                    .ok_or("invalid entry device identity")?
                    .to_string(),
                "type",
                "wireguard",
            ])?;
        }
    }
    ip(&[
        "link",
        "set",
        "dev",
        &dev,
        "alias",
        &format!("{ALIAS}{}", tunnel.id),
    ])?;
    let adapter = if !tunnel.wireguard_transport.is_udp() && !tunnel.listens() {
        Some(super::phantun::ensure(root, tunnel, None)?)
    } else {
        None
    };
    if adapter.as_ref().is_some_and(|adapter| adapter.restarted) {
        reset_peer(&dev, tunnel)?;
    }
    let directory = root.join("cvm-entries");
    let config = directory.join(format!("{dev}.conf"));
    let mut text = format!("[Interface]\nPrivateKey = {}\n", tunnel.credential);
    // The dialing side keeps the port the kernel chose for it: a configuration without
    // one would move it on every round and strand the peer's learned endpoint.
    let port = if tunnel.listens() && tunnel.wireguard_transport.is_udp() {
        Some(tunnel.port.to_string())
    } else {
        let port = run_command("wg", &["show", &dev, "listen-port"])
            .map_err(|error| {
                super::super::diagnostics::wireguard_error("WireGuard 入口监听端口读取", &error)
            })?
            .trim()
            .parse::<u16>()
            .map_err(|_| "invalid entry WireGuard socket")?;
        (port != 0).then(|| port.to_string())
    };
    if let Some(port) = port {
        text.push_str(&format!("ListenPort = {port}\n"));
    }
    text.push_str(&format!(
        "\n[Peer]\nPublicKey = {}\nAllowedIPs = 0.0.0.0/0\n",
        tunnel.peer_credential
    ));
    if !tunnel.listens() {
        text.push_str(&format!(
            "Endpoint = {}\nPersistentKeepalive = 25\n",
            if tunnel.wireguard_transport.is_udp() {
                std::net::SocketAddr::new(tunnel.offer.endpoint, tunnel.port)
            } else {
                adapter
                    .as_ref()
                    .ok_or("missing entry Phantun adapter")?
                    .endpoint
            }
        ));
    }
    atomic_write_private(&config, text.as_bytes())?;
    crate::command::run_command_with_input(
        "wg",
        &["syncconf", &dev, "/dev/stdin"],
        text.as_bytes(),
    )
    .map_err(|error| {
        super::super::diagnostics::wireguard_error("WireGuard 入口通道配置", &error)
    })?;
    if tunnel.wireguard_transport == EntryWireguardTransport::Phantun && tunnel.listens() {
        ip(&["link", "set", "dev", &dev, "up"])?;
        let port: u16 = run_command("wg", &["show", &dev, "listen-port"])
            .map_err(|error| {
                super::super::diagnostics::wireguard_error("WireGuard 入口监听端口读取", &error)
            })?
            .trim()
            .parse()
            .map_err(|_| "invalid entry WireGuard socket")?;
        if super::phantun::ensure(root, tunnel, Some(port))?.restarted {
            reset_peer(&dev, tunnel)?;
            run_command(
                "wg",
                &[
                    "set",
                    &dev,
                    "peer",
                    &tunnel.peer_credential,
                    "allowed-ips",
                    "0.0.0.0/0",
                ],
            )
            .map_err(|error| {
                super::super::diagnostics::wireguard_error("WireGuard 入口对端更新", &error)
            })?;
        }
    }
    Ok(())
}
/// A replacement Phantun adapter has a new UDP socket. Reset this peer exactly once
/// so stale crypto/loopback state cannot masquerade as a working replacement path.
fn reset_peer(dev: &str, tunnel: &EntryTunnel) -> Result<()> {
    run_command(
        "wg",
        &["set", dev, "peer", &tunnel.peer_credential, "remove"],
    )
    .map(|_| ())
    .map_err(|error| super::super::diagnostics::wireguard_error("WireGuard 入口对端重置", &error))
}

/// Removes owned tunnel devices and policy rules the desired state no longer lists.
pub(super) fn prune(
    root: &Path,
    desired: &EntryDesired,
    inventory: &[serde_json::Value],
) -> Result<()> {
    let keep: BTreeSet<String> = desired.tunnels.iter().map(plan::device).collect();
    for link in inventory {
        let Some(name) = link["ifname"].as_str() else {
            continue;
        };
        if owned(link).is_none() && !unfinished_creation(link) {
            continue;
        }
        if keep.contains(name) {
            continue;
        }
        ip(&["link", "delete", "dev", name])?;
        let config = root.join("cvm-entries").join(format!("{name}.conf"));
        if config.exists() {
            std::fs::remove_file(&config).map_err(|_| "cannot remove entry tunnel key")?;
        }
    }
    let marks: BTreeSet<String> = desired
        .tunnels
        .iter()
        .filter(|t| t.side == EntryTunnelSide::Entry)
        .map(|t| format!("{:#x}", plan::mark(t)))
        .collect();
    let rules: Vec<serde_json::Value> = serde_json::from_str(&ip(&["-4", "-j", "rule", "show"])?)
        .map_err(|_| "invalid host routing rules")?;
    for rule in rules {
        if rule["priority"].as_u64() != Some(RULE_PRIORITY) {
            continue;
        }
        let Some(mark) = rule["fwmark"].as_str() else {
            continue;
        };
        // Only marks in the entry range belong to us.
        let Ok(value) = u32::from_str_radix(mark.trim_start_matches("0x"), 16) else {
            continue;
        };
        if !(ENTRY_TUNNEL_MARK..ENTRY_TUNNEL_MARK + u32::from(ENTRY_TUNNEL_SLOTS)).contains(&value)
            || marks.contains(mark)
        {
            continue;
        }
        let table = value.to_string();
        ip(&[
            "-4",
            "rule",
            "del",
            "pref",
            &RULE_PRIORITY.to_string(),
            "fwmark",
            mark,
            "lookup",
            &table,
        ])?;
        ip(&["-4", "route", "flush", "table", &table])?;
    }
    Ok(())
}

pub(super) fn inventory() -> Result<Vec<serde_json::Value>> {
    links()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_device_requires_its_creation_group_kind_and_name() {
        let name = "bxe0123456789ab";
        let mut link = serde_json::json!({"ifname":name, "group":creation_group(name).unwrap().to_string(), "linkinfo":{"info_kind":"wireguard"}});
        assert!(unfinished_creation(&link));
        link["ifalias"] = serde_json::json!("foreign-owner");
        assert!(!unfinished_creation(&link));
        link["ifalias"] = serde_json::json!("");
        link["group"] = serde_json::json!(0);
        assert!(!unfinished_creation(&link));
        link["group"] = serde_json::json!(creation_group(name).unwrap());
        link["linkinfo"]["info_kind"] = serde_json::json!("dummy");
        assert!(!unfinished_creation(&link));
    }
}
