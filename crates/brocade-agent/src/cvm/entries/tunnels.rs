//! WireGuard machine-pair tunnels in the root namespace. Devices carry an ownership
//! alias; anything without it is never modified, and owned devices that the desired
//! state no longer lists are removed together with their policy route.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

const ALIAS: &str = "brocade-entry:";
/// One policy rule priority for every entry tunnel, ahead of the main table.
const RULE_PRIORITY: u64 = 31700;

fn links() -> Result<Vec<serde_json::Value>> {
    serde_json::from_str(
        &run_command("ip", &["-j", "link", "show"]).map_err(|_| "cannot list host links")?,
    )
    .map_err(|_| "invalid host link inventory".into())
}
fn owned(link: &serde_json::Value) -> Option<&str> {
    link["ifalias"].as_str()?.strip_prefix(ALIAS)
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
    inventory: &[serde_json::Value],
) -> Result<bool> {
    for key in [&tunnel.credential, &tunnel.peer_credential] {
        if STANDARD.decode(key).map_or(true, |key| key.len() != 32) {
            return Err("invalid WireGuard entry tunnel credentials".into());
        }
    }
    let dev = plan::device(tunnel);
    let (local, _) = plan::addresses(tunnel).ok_or("invalid entry tunnel slot")?;
    match inventory.iter().find(|link| link["ifname"] == dev.as_str()) {
        Some(link) if owned(link) == Some(tunnel.id.as_str()) => {}
        Some(_) => return Err("入口通道设备名已被其他网卡占用，未修改".into()),
        None => {
            ip(&["link", "add", &dev, "type", "wireguard"])?;
            ip(&[
                "link",
                "set",
                "dev",
                &dev,
                "alias",
                &format!("{ALIAS}{}", tunnel.id),
            ])?;
        }
    }
    let directory = root.join("cvm-entries");
    let config = directory.join(format!("{dev}.conf"));
    let mut text = format!("[Interface]\nPrivateKey = {}\n", tunnel.credential);
    // The dialing side keeps the port the kernel chose for it: a configuration without
    // one would move it on every round and strand the peer's learned endpoint.
    let port = if tunnel.listens() {
        Some(tunnel.port.to_string())
    } else {
        run_command("wg", &["show", &dev, "listen-port"])
            .ok()
            .map(|port| port.trim().to_owned())
            .filter(|port| port.parse::<u16>().is_ok_and(|port| port != 0))
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
            std::net::SocketAddr::new(tunnel.offer.endpoint, tunnel.port)
        ));
    }
    atomic_write_private(&config, text.as_bytes())?;
    run_command(
        "wg",
        &[
            "syncconf",
            &dev,
            config.to_str().ok_or("invalid entry tunnel path")?,
        ],
    )
    .map_err(|_| "WireGuard entry tunnel configuration failed")?;
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
    let handshakes = run_command("wg", &["show", &dev, "latest-handshakes"])
        .map_err(|_| "cannot read WireGuard entry tunnel state")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid host clock")?
        .as_secs();
    Ok(handshakes
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        .any(|at| at > 0 && now.saturating_sub(at) < 180))
}

/// Removes owned tunnel devices and policy rules the desired state no longer lists.
pub(super) fn prune(
    root: &Path,
    desired: &EntryDesired,
    inventory: &[serde_json::Value],
) -> Result<()> {
    let keep: BTreeSet<String> = desired.tunnels.iter().map(plan::device).collect();
    for link in inventory {
        let (Some(name), Some(_)) = (link["ifname"].as_str(), owned(link)) else {
            continue;
        };
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
