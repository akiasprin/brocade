//! Container access entries on this node.
//!
//! As an entry machine the node maps an entry address (whole IP or protocol port) to the
//! container's host-side primary address. A container on this machine is reached directly
//! through Cube's gateway device; a remote one through the WireGuard tunnel of its host,
//! selected by the tunnel slot's packet mark and translated to this end's tunnel address.
//! As a container host the node accepts tunnel traffic only for its listed deliveries and
//! hands it to the primary NIC from the Cube gateway address, which CubeVS
//! `primary_delivery` lets the Guest answer. Each round converges the whole desired
//! state: one owned nftables table, owned tunnel devices and their policy routes.
use crate::{
    command::run_command,
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

mod plan;
#[cfg(test)]
mod tests;
mod tunnels;
type Result<T> = std::result::Result<T, String>;
const TABLE: &str = "brocade_cvm_entries";

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
fn linux(options: &Options) -> bool {
    // SAFETY: geteuid takes no pointers and has no memory effects.
    options.apply_mode == ApplyMode::Linux && unsafe { libc::geteuid() } == 0
}

pub(super) fn spawn(options: Options) -> Result<()> {
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
                            if !saved.node_id.is_empty() {
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
    if super::maintenance()? {
        return Ok(());
    }
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/cvm/entries/desired", &options.token, None)?;
    if matches!(response.status, 401 | 403) {
        // Revoked authority: withdraw everything this worker owns.
        let empty = EntryDesired::default();
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
    let mut report = EntryNodeReport {
        generation: desired.generation,
        ..Default::default()
    };
    match apply(root, desired, &mut report) {
        Ok(()) => {}
        Err(error) => report.error = Some(error),
    }
    report
}
fn apply(root: &Path, desired: &EntryDesired, report: &mut EntryNodeReport) -> Result<()> {
    std::fs::create_dir_all(root.join("cvm-entries"))
        .map_err(|_| "cannot create private entry state directory")?;
    if !desired.rules.is_empty() || !desired.deliveries.is_empty() {
        super::nics::enable_forwarding()?;
    }
    if plan::needs_gateway(desired) && gateway()?.is_none() {
        return Err("容器宿主没有 Cube 网关设备，无法投递到主网卡".into());
    }
    let inventory = tunnels::inventory()?;
    let mut ready = BTreeSet::new();
    for tunnel in &desired.tunnels {
        let targets = plan::targets(desired, tunnel);
        let (connected, error) = match tunnels::ensure(root, tunnel, &targets, &inventory) {
            Ok(connected) => {
                ready.insert(tunnel.id.as_str());
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
    let script = plan::ruleset(desired, &ready)?;
    let path = root.join("cvm-entries").join("rules.nft");
    atomic_write_private(&path, script.as_bytes())?;
    let path = path.to_str().ok_or("invalid private entry rules path")?;
    run_command("nft", &["-c", "-f", path]).map_err(|_| "entry nftables validation failed")?;
    run_command("nft", &["-f", path]).map_err(|_| "entry nftables transaction failed")?;
    tunnels::prune(root, desired, &tunnels::inventory()?)?;
    let item = |id: &String, tunnel: Option<&String>| EntryItemReport {
        id: id.clone(),
        configured: tunnel.is_none_or(|t| ready.contains(t.as_str())),
        error: tunnel
            .filter(|t| !ready.contains(t.as_str()))
            .map(|_| "入口通道尚未就绪".into()),
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
