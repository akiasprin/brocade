//! Container access entries on this node.
//!
//! As an entry machine the node maps an entry address (whole IP or protocol port) to the
//! container's host-side primary address. A container on this machine is reached directly
//! through Cube's gateway device; a remote one through its host's managed IP tunnel,
//! selected by the tunnel slot's packet mark and translated to this end's tunnel address.
//! As a container host the node accepts tunnel traffic only for its listed deliveries and
//! hands it to the primary NIC from the Cube gateway address, which CubeVS
//! `primary_delivery` lets the Guest answer. Each round converges the whole desired
//! state: one owned nftables table, owned tunnel devices and their policy routes.
use crate::{
    command::{capture_command_with_timeout, run_command},
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::*;
use std::{
    collections::BTreeSet,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::Duration,
};

use super::diagnostics;
mod flows;
mod phantun;
pub(in crate::cvm) mod plan;
#[cfg(test)]
mod tests;
mod tunnels;
type Result<T> = std::result::Result<T, String>;

static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn desired_path(root: &Path) -> PathBuf {
    root.join("cvm-entries-desired.json")
}
fn snapshot(root: &Path) -> Result<EntryDesired> {
    let Some(bytes) = read_bounded(&desired_path(root), 16 * 1024 * 1024)? else {
        return Ok(EntryDesired::default());
    };
    let desired: EntryDesired =
        serde_json::from_slice(&bytes).map_err(|_| "invalid private entry desired state")?;
    if !desired.node_id.is_empty() {
        desired.validate()?;
    }
    Ok(desired)
}
fn persist(root: &Path, desired: &EntryDesired) -> Result<()> {
    atomic_write_private(
        &desired_path(root),
        &serde_json::to_vec(desired).map_err(|_| "cannot persist entry desired state")?,
    )
}
pub(crate) fn tracking_addresses() -> Result<BTreeSet<std::net::IpAddr>> {
    let Some(root) = ROOT.get() else {
        return Ok(BTreeSet::new());
    };
    let desired = snapshot(root)?;
    Ok(desired
        .rules
        .iter()
        .map(|r| r.address.into())
        .chain(
            desired
                .rules
                .iter()
                .filter_map(|r| r.target)
                .map(Into::into),
        )
        .chain(desired.deliveries.iter().map(|d| d.target.into()))
        .collect())
}

pub(crate) fn tracking_ports() -> Result<BTreeSet<(std::net::IpAddr, u16)>> {
    let Some(root) = ROOT.get() else {
        return Ok(BTreeSet::new());
    };
    Ok(snapshot(root)?
        .tunnels
        .iter()
        .filter(|t| t.protocol() == L4Protocol::Tcp)
        .map(|t| (t.offer.endpoint, t.port))
        .collect())
}

fn linux(options: &Options) -> bool {
    // SAFETY: geteuid takes no pointers and has no memory effects.
    options.apply_mode == ApplyMode::Linux && unsafe { libc::geteuid() } == 0
}

pub(super) fn spawn(options: Options) -> Result<()> {
    ROOT.set(options.state_dir.clone())
        .map_err(|_| "entry worker already initialized")?;
    std::thread::Builder::new()
        .name("cvm-entries".into())
        .spawn(move || {
            let mut wake =
                crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::Desired);
            let mut previous = String::new();
            loop {
                if let Err(error) = round(&options) {
                    // The Console may be unreachable: keep the last proven state applied.
                    if linux(&options) {
                        if let Ok(saved) = snapshot(&options.state_dir) {
                            if !saved.node_id.is_empty()
                                || flows::has_checkpoint(&options.state_dir)
                            {
                                converge(&options.state_dir, &saved);
                            }
                        }
                    }
                    if error != previous {
                        eprintln!("cvm-entries: {error}");
                        previous = error;
                    }
                } else {
                    previous.clear();
                }
                wake.wait(Duration::from_secs(15));
            }
        })
        .map_err(|_| "cannot start entry worker".to_owned())?;
    Ok(())
}

pub(super) fn round(options: &Options) -> Result<()> {
    if options.apply_mode == ApplyMode::Linux && super::maintenance()? {
        return Ok(());
    }
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/cvm/entries/desired", &options.token, None)?;
    if matches!(response.status, 401 | 403) {
        // Revoked authority: withdraw everything this worker owns.
        let empty = EntryDesired::default();
        flows::seed(&options.state_dir, &snapshot(&options.state_dir)?)?;
        persist(&options.state_dir, &empty)?;
        if linux(options) {
            converge(&options.state_dir, &empty);
        }
        return Err(format!(
            "entry desired request refused: HTTP {}",
            response.status
        ));
    }
    if response.status != 200 {
        return Err(format!(
            "entry desired request failed: HTTP {}",
            response.status
        ));
    }
    let desired: EntryDesired =
        serde_json::from_str(&response.body).map_err(|_| "invalid entry desired response")?;
    desired.validate()?;
    let previous = snapshot(&options.state_dir)?;
    flows::seed(&options.state_dir, &previous)?;
    if desired.node_id == previous.node_id && desired.generation < previous.generation {
        return Err("stale entry desired generation rejected".into());
    }
    if desired.generation == 0 && previous.node_id.is_empty() {
        return Ok(());
    }
    persist(&options.state_dir, &desired)?;
    let report = if linux(options) {
        converge(&options.state_dir, &desired)
    } else {
        EntryNodeReport {
            generation: desired.generation,
            error: Some("访问入口需要具备网络管理权限的 Linux Agent".into()),
            ..Default::default()
        }
    };
    let body = serde_json::to_string(&report).map_err(|_| "cannot encode entry report")?;
    let response = client.request(
        "POST",
        "/agent/v1/cvm/entries/report",
        &options.token,
        Some(&body),
    )?;
    if response.status != 200 {
        return Err(format!("entry report failed: HTTP {}", response.status));
    }
    match &report.error {
        Some(error) => Err(error.clone()),
        None => Ok(()),
    }
}

/// Applies the desired state and describes the result. A report without `error`
/// proves that everything withdrawn is gone from this node.
fn converge(root: &Path, desired: &EntryDesired) -> EntryNodeReport {
    diagnostics::begin(root, desired);
    let mut report = EntryNodeReport {
        generation: desired.generation,
        ..Default::default()
    };
    match apply(root, desired, &mut report) {
        Ok(()) => {}
        Err(error) => report.error = Some(error),
    }
    diagnostics::finish(root, desired, &mut report);
    report
}
fn apply(root: &Path, desired: &EntryDesired, report: &mut EntryNodeReport) -> Result<()> {
    std::fs::create_dir_all(root.join("cvm-entries"))
        .map_err(|_| "cannot create private entry state directory")?;
    if !desired.rules.is_empty() || !desired.deliveries.is_empty() || !desired.tunnels.is_empty() {
        super::nics::enable_forwarding()?;
    }
    // Fence withdrawn mappings before stopping a carrier or removing its routes.
    let mut guard_ports = transport_ports(root)?.all;
    guard_ports.extend(
        desired
            .tunnels
            .iter()
            .filter(|t| t.listens())
            .map(|t| (t.protocol(), t.port)),
    );
    let cleanup = flows::begin(root, desired, &guard_ports)?;
    if plan::needs_gateway(desired) && gateway()?.is_none() {
        return Err("容器宿主没有 Cube 网关设备，无法投递到主网卡".into());
    }
    let xray_ids = desired
        .tunnels
        .iter()
        .filter(|t| t.offer.kind == TransportKind::Xray)
        .map(|t| t.id.as_str())
        .collect();
    super::nics::xray::retain_entries(&xray_ids)?;
    phantun::retain(desired)?;
    let inventory = tunnels::inventory()?;
    let mut ready = BTreeSet::new();
    for tunnel in &desired.tunnels {
        let mut targets = plan::targets(desired, tunnel);
        targets.extend(
            desired
                .deliveries
                .iter()
                .filter(|d| d.tunnel == tunnel.id)
                .map(|d| d.target),
        );
        let (connected, error) = match tunnels::ensure(
            root,
            tunnel,
            &targets,
            plan::reply_source(desired, tunnel)?,
            &inventory,
        ) {
            Ok((configured, connected)) => {
                if configured {
                    ready.insert(tunnel.id.as_str());
                }
                (connected, None)
            }
            Err(error) => (false, Some(error)),
        };
        report.tunnels.push(EntryTunnelReport {
            id: tunnel.id.clone(),
            connected,
            error,
        });
    }
    crate::conntrack::track_delivery_scopes(
        &super::nics::tracking_addresses()?,
        &super::nics::tracking_ports()?,
    )?;
    let ports = transport_ports(root)?;
    let script = plan::ruleset(desired, &ready, &ports.all)?;
    let path = root.join("cvm-entries").join("rules.nft");
    atomic_write_private(&path, script.as_bytes())?;
    let path = path.to_str().ok_or("invalid private entry rules path")?;
    run_command("nft", &["-c", "-f", path]).map_err(|error| {
        diagnostics::command_error(root, None, "入口转发规则校验失败", &error, &[])
    })?;
    run_command("nft", &["-f", path]).map_err(|error| {
        diagnostics::command_error(root, None, "入口转发规则安装失败", &error, &[])
    })?;
    phantun::apply_rules(root, desired)?;
    reconcile_tcp_flows(root, desired, &ports.all)?;
    reconcile_transport_flows(root, &ports.root_wireguard)?;
    tunnels::prune(root, desired, &tunnels::inventory()?)?;
    flows::finish(root, cleanup)?;
    let item = |id: &String, tunnel: Option<&String>| EntryItemReport {
        id: id.clone(),
        configured: tunnel.is_none_or(|t| ready.contains(t.as_str())),
        // An asynchronously starting tunnel is pending. Its actual failure is carried
        // by the tunnel report, while ruleset failures belong to the node report.
        error: None,
    };
    for rule in &desired.rules {
        report.rules.push(item(&rule.id, rule.tunnel.as_ref()));
    }
    for delivery in &desired.deliveries {
        report
            .deliveries
            .push(item(&delivery.id, Some(&delivery.tunnel)));
    }
    Ok(())
}

/// Every root WireGuard socket includes dialers whose ephemeral port may differ
/// from the offered listener. Keeping just the advertised port strands these nodes.
struct TransportPorts {
    all: BTreeSet<(L4Protocol, u16)>,
    root_wireguard: BTreeSet<u16>,
}
fn transport_ports(root: &Path) -> Result<TransportPorts> {
    let devices: Vec<serde_json::Value> = serde_json::from_str(
        &run_command("ip", &["-d", "-j", "link", "show", "type", "wireguard"])
            .map_err(|_| "cannot inspect root WireGuard devices")?,
    )
    .map_err(|_| "invalid root WireGuard device inventory")?;
    let raw = if devices.is_empty() {
        Ok(String::new())
    } else {
        run_command("wg", &["show", "all", "listen-port"])
    };
    let mut ports = wireguard_listener_ports(raw)?;
    let wireguard_ports = ports.iter().map(|(_, port)| *port).collect();
    ports.extend(super::nics::transport_listeners()?);
    ports.extend(
        crate::phantun::transport_listeners(root)?
            .into_iter()
            .map(|port| (L4Protocol::Tcp, port)),
    );
    Ok(TransportPorts {
        all: ports,
        root_wireguard: wireguard_ports,
    })
}

fn wireguard_listener_ports(raw: Result<String>) -> Result<BTreeSet<(L4Protocol, u16)>> {
    let raw =
        raw.map_err(|error| diagnostics::wireguard_error("WireGuard 入口传输端口检查", &error))?;
    let mut ports = BTreeSet::new();
    for line in raw.lines() {
        let values = line.split_whitespace().collect::<Vec<_>>();
        if values.len() != 2 {
            return Err("invalid WireGuard listener inventory".into());
        }
        let port: u16 = values[1]
            .parse()
            .map_err(|_| "invalid WireGuard listener port")?;
        if port != 0 {
            ports.insert((L4Protocol::Udp, port));
        }
    }
    Ok(ports)
}

/// A dialing peer may send before this machine has installed its new listener.
/// A pre-existing L3 rule can then cache a DNAT of that handshake to the old target.
/// Exempting the port does not undo conntrack NAT: remove only poisoned DNAT flows,
/// after installing the exception. Healthy root WireGuard flows are never DNATed.
fn reconcile_transport_flows(root: &Path, ports: &BTreeSet<u16>) -> Result<()> {
    let path = root.join("cvm-entries").join("wireguard-ports.json");
    let previous: BTreeSet<u16> = read_bounded(&path, 32768)?
        .map(|b| serde_json::from_slice(&b).map_err(|_| "invalid entry transport cleanup evidence"))
        .transpose()?
        .unwrap_or_default();
    if ports.len() > 4096 || previous.len() > 4096 || ports.contains(&0) || previous.contains(&0) {
        return Err("entry transport cleanup capacity exceeded".into());
    }
    for port in ports.difference(&previous) {
        let port = port.to_string();
        let args = ["-f", "ipv4", "-p", "udp", "--dport", &port, "--dst-nat"];
        let delete = capture_command_with_timeout(
            "conntrack",
            &[&["-D"][..], &args].concat(),
            Duration::from_secs(10),
        )
        .map_err(|_| "cannot remove stale entry transport translations")?;
        if !delete.status.success() && delete.status.code() != Some(1) {
            return Err("entry transport translation cleanup failed".into());
        }
        let remaining = capture_command_with_timeout(
            "conntrack",
            &[&["-L"][..], &args].concat(),
            Duration::from_secs(10),
        )
        .map_err(|_| "cannot verify entry transport translation cleanup")?;
        if !remaining.status.success() || !remaining.stdout.is_empty() {
            return Err("entry transport translation cleanup is unconfirmed".into());
        }
    }
    atomic_write_private(
        &path,
        &serde_json::to_vec(ports).map_err(|_| "cannot encode entry transport cleanup evidence")?,
    )
}

/// New TCP carriers may have been DNATed by an older whole-IP entry before their
/// exception existed. Remove only translations to entry targets; healthy Phantun
/// DNATs terminate at its reserved TUN address and are deliberately retained.
fn reconcile_tcp_flows(
    root: &Path,
    desired: &EntryDesired,
    listeners: &BTreeSet<(L4Protocol, u16)>,
) -> Result<()> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Evidence {
        ports: BTreeSet<u16>,
        targets: BTreeSet<Ipv4Addr>,
    }
    let path = root.join("cvm-entries/tcp-transports.json");
    let previous: Evidence = read_bounded(&path, 1024 * 1024)?
        .map(|b| {
            serde_json::from_slice(&b).map_err(|_| "invalid TCP entry transport cleanup evidence")
        })
        .transpose()?
        .unwrap_or_default();
    let mut ports: BTreeSet<_> = desired
        .tunnels
        .iter()
        .filter(|t| t.listens() && t.protocol() == L4Protocol::Tcp)
        .map(|t| t.port)
        .collect();
    ports.extend(
        listeners
            .iter()
            .filter(|(protocol, _)| *protocol == L4Protocol::Tcp)
            .map(|(_, port)| *port),
    );
    let targets: BTreeSet<_> = desired.rules.iter().filter_map(|r| r.target).collect();
    if ports.len() > 4096
        || previous.ports.len() > 4096
        || previous.ports.contains(&0)
        || previous.targets.len() > MAX_ENTRIES_PER_NODE
        || previous.targets.iter().any(|ip| !entry_address(*ip))
    {
        return Err("TCP entry cleanup evidence exceeds capacity".into());
    }
    let mut removed = 0;
    for port in ports.difference(&previous.ports) {
        let port_text = port.to_string();
        let output = capture_command_with_timeout(
            "conntrack",
            &[
                "-L",
                "-f",
                "ipv4",
                "-p",
                "tcp",
                "--dport",
                &port_text,
                "--dst-nat",
            ],
            Duration::from_secs(10),
        )
        .map_err(|_| "cannot inspect TCP entry transport translations")?;
        if !output.status.success() {
            return Err("TCP entry transport inventory failed".into());
        }
        let translated = tcp_translated_targets(&output.stdout)?;
        for target in targets
            .union(&previous.targets)
            .filter(|target| translated.contains(*target))
        {
            if removed >= 64 {
                return Err("TCP entry transport cleanup continues next round".into());
            }
            removed += 1;
            let port = port.to_string();
            let target = target.to_string();
            let args = [
                "-f",
                "ipv4",
                "-p",
                "tcp",
                "--dport",
                &port,
                "--dst-nat",
                &target,
            ];
            let output = capture_command_with_timeout(
                "conntrack",
                &[&["-D"][..], &args].concat(),
                Duration::from_secs(10),
            )
            .map_err(|_| "cannot remove poisoned TCP entry transport flows")?;
            if !output.status.success() && output.status.code() != Some(1) {
                return Err("TCP entry transport flow cleanup failed".into());
            }
            let output = capture_command_with_timeout(
                "conntrack",
                &[&["-L"][..], &args].concat(),
                Duration::from_secs(10),
            )
            .map_err(|_| "cannot verify TCP entry transport cleanup")?;
            if !output.status.success() || !output.stdout.is_empty() {
                return Err("TCP entry transport cleanup unconfirmed".into());
            }
        }
    }
    atomic_write_private(
        &path,
        &serde_json::to_vec(&Evidence { ports, targets })
            .map_err(|_| "cannot encode TCP entry cleanup evidence")?,
    )
}

fn tcp_translated_targets(raw: &[u8]) -> Result<BTreeSet<Ipv4Addr>> {
    let text = std::str::from_utf8(raw).map_err(|_| "invalid TCP translation inventory")?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .filter_map(|value| value.strip_prefix("src="))
                .nth(1)
                .ok_or("missing TCP translation reply tuple")?
                .parse()
                .map_err(|_| "invalid TCP translation reply address".into())
        })
        .collect()
}

/// The Cube gateway address, or none when this machine runs no Cube network.
fn gateway() -> Result<Option<Ipv4Addr>> {
    let Ok(raw) = run_command(
        "ip",
        &["-4", "-j", "address", "show", "dev", plan::CUBE_DEVICE],
    ) else {
        return Ok(None);
    };
    let links: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid Cube gateway inventory")?;
    Ok(links
        .iter()
        .flat_map(|link| link["addr_info"].as_array().into_iter().flatten())
        .find_map(|item| item["local"].as_str()?.parse().ok()))
}

/// TCP probe of an entry service from this host to a container's primary address.
/// Only addresses that the host routes through the Cube gateway are probed.
pub(crate) fn probe(host: &str, port: u16) -> Result<Option<(Reachability, Option<u32>)>> {
    let address: Ipv4Addr = host.parse().map_err(|_| "invalid entry probe address")?;
    if port == 0 {
        return Err("invalid entry probe port".into());
    }
    let route = run_command("ip", &["-4", "-j", "route", "get", &address.to_string()])
        .map_err(|_| "cannot resolve entry probe route")?;
    let route: Vec<serde_json::Value> =
        serde_json::from_str(&route).map_err(|_| "invalid entry probe route")?;
    if route.first().and_then(|r| r["dev"].as_str()) != Some(plan::CUBE_DEVICE) {
        return Ok(None);
    }
    let started = std::time::Instant::now();
    Ok(Some(
        match std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::new(address.into(), port),
            Duration::from_millis(250),
        ) {
            Ok(_) => (
                Reachability::Reachable,
                Some(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32),
            ),
            Err(_) => (Reachability::Unreachable, None),
        },
    ))
}
