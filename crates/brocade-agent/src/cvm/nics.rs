//! Allocated IP NIC convergence is independent of compute and the main Xray publication.
use crate::{
    command::{capture_command_with_timeout, run_command},
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::{ApplyMode, Options},
};
use brocade_deployment::cvm::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::IpAddr,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

mod fences;
mod flows;
mod host;
mod meter;
mod probe;
mod wireguard;
mod xray;
pub(crate) use host::enable_forwarding;
pub(crate) use probe::service as probe_service;
mod public_addresses;
#[cfg(test)]
mod tests;
type Result<T> = std::result::Result<T, String>;
const NODE_BINDINGS: usize = 64;
const REPORT_BATCH_SIZE: usize = 8;
static ROOT: OnceLock<PathBuf> = OnceLock::new();
fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no pointers and has no memory effects.
    unsafe { libc::geteuid() }
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|_| "cannot create private NIC state directory")?;
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "cannot inspect NIC state directory")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != effective_uid()
    {
        return Err("NIC state directory ownership is invalid".into());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot protect NIC state directory".into())
}
fn desired_path(root: &Path) -> PathBuf {
    root.join("cvm-nics-desired.json")
}
fn snapshot(root: &Path) -> Result<NicDesired> {
    let Some(bytes) = read_bounded(&desired_path(root), 16 * 1024 * 1024)? else {
        return Ok(NicDesired::default());
    };
    let desired: NicDesired =
        serde_json::from_slice(&bytes).map_err(|_| "invalid private NIC desired state")?;
    validate(&desired)?;
    Ok(desired)
}
fn validate(desired: &NicDesired) -> Result<()> {
    if desired.bindings.len() > NODE_BINDINGS * 4
        || desired.fences.len() > 32768
        || desired.management_networks.len() > 2048
        || desired.budgets.len() > NODE_BINDINGS * 2
    {
        return Err("NIC binding transition capacity exceeded".into());
    }
    for fence in &desired.fences {
        if !valid_id(&fence.interface_id)
            || fence.address.is_unspecified()
            || fence.address.is_loopback()
            || fence.address.is_multicast()
            || match fence.protocol {
                Some(_) => fence.port_start == 0 || fence.port_end < fence.port_start,
                None => fence.port_start != 0 || fence.port_end != 0,
            }
        {
            return Err("invalid held NIC resource fence".into());
        }
    }
    if !desired.bindings.is_empty() && !valid_id(&desired.node_id) {
        return Err("NIC participant identity is unavailable".into());
    }
    let mut identities = BTreeSet::new();
    for binding in &desired.bindings {
        if (if binding.side == BindingSide::Consumer {
            &binding.consumer_node
        } else {
            &binding.provider_node
        }) != &desired.node_id
            || ![
                &binding.id,
                &binding.interface_id,
                &binding.instance_id,
                &binding.provider_node,
                &binding.consumer_node,
                &binding.nic.allocation_id,
                &binding.nic.publication_id,
            ]
            .into_iter()
            .all(|id| valid_id(id))
            || !identities.insert((&binding.id, binding.side == BindingSide::Provider))
            || binding.generation == 0
            || binding.nic.slot >= 8192
            || binding.nic.mtu < 1280
            || binding.nic.mtu > 1500
            || binding.offer.listen_side == TransportListenSide::Auto
            || binding.offer.kind != binding.nic.transport
            || binding.port < binding.offer.port_start
            || binding.port > binding.offer.port_end
            || binding.credential.len() > 4096
            || binding.peer_credential.len() > 32768
            || binding.nic.destinations.len() > 256
            || binding.routing.routes.len() > MAX_NETWORK_ROUTES
            || binding.entry_source_ipv4.is_some_and(|ip| {
                !entry_address(ip) || binding.nic.address_mode == NicAddressMode::DedicatedRouted
            })
        {
            return Err("invalid NIC binding identity, bounds or transport".into());
        }
        if binding.nic.ipv4.is_none() && binding.nic.ipv6.is_none()
            || binding.nic.ipv4.is_some_and(|ip| {
                !ip.is_ipv4() || ip.is_unspecified() || ip.is_multicast() || ip.is_loopback()
            })
            || binding.nic.ipv6.is_some_and(|ip| {
                ip.is_ipv4() || ip.is_unspecified() || ip.is_multicast() || ip.is_loopback()
            })
        {
            return Err("invalid allocated NIC addresses".into());
        }
        if binding.nic.mac.len() != 17
            || !binding
                .nic
                .mac
                .split(':')
                .all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_hexdigit()))
            || !binding.nic.mac.starts_with("02:")
        {
            return Err("invalid allocated NIC hardware identity".into());
        }
    }
    Ok(())
}

pub(crate) fn tracking_addresses() -> Result<BTreeSet<IpAddr>> {
    let Some(root) = ROOT.get() else {
        return Ok(BTreeSet::new());
    };
    let desired = snapshot(root)?;
    let mut addresses = BTreeSet::new();
    for binding in &desired.bindings {
        addresses.extend(
            [binding.nic.public_ipv4, binding.nic.public_ipv6]
                .into_iter()
                .flatten(),
        );
        let layout = host::layout(binding);
        addresses.insert(layout.namespace_v4.into());
        addresses.insert(layout.business_v4.into());
        addresses.insert(layout.namespace_v6.into());
        addresses.insert(layout.business_v6.into());
    }
    Ok(addresses)
}
pub(crate) fn tracking_ports() -> Result<BTreeSet<(IpAddr, u16)>> {
    let Some(root) = ROOT.get() else {
        return Ok(BTreeSet::new());
    };
    let desired = snapshot(root)?;
    let mut ports = BTreeSet::new();
    for b in desired.bindings {
        if b.offer.kind == TransportKind::Xray {
            ports.insert((b.offer.endpoint, b.port));
        }
    }
    Ok(ports)
}

fn local_management_networks(desired: &mut NicDesired) -> Result<()> {
    let raw = run_command("ip", &["-j", "address", "show"])
        .map_err(|_| "cannot verify local NIC management isolation")?;
    let links: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid local management address inventory")?;
    for link in links {
        if link["ifalias"]
            .as_str()
            .is_some_and(|alias| alias.starts_with("brocade-nic:"))
        {
            continue;
        }
        for item in link["addr_info"].as_array().into_iter().flatten() {
            let Some(address) = item["local"]
                .as_str()
                .and_then(|s| s.parse::<IpAddr>().ok())
            else {
                continue;
            };
            if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
                continue;
            }
            desired
                .management_networks
                .push(format!("{address}/{}", if address.is_ipv4() { 32 } else { 128 }).parse()?);
        }
    }
    desired.management_networks.sort();
    desired.management_networks.dedup();
    if desired.management_networks.len() > 2048 {
        return Err("management isolation prefix capacity exceeded".into());
    }
    Ok(())
}
fn inventory(options: &Options) -> NicNodeCapabilities {
    let mut caps = NicNodeCapabilities {
        contract: NIC_PROTOCOL_VERSION,
        max_bindings: NODE_BINDINGS as u16,
        ..Default::default()
    };
    if options.apply_mode != ApplyMode::Linux || effective_uid() != 0 {
        caps.error = Some("远端网卡需要具备网络管理权限的 Linux Agent".into());
        return caps;
    }
    for (program, flag) in [
        ("ip", "-V"),
        ("nft", "--version"),
        ("tc", "-V"),
        ("conntrack", "-V"),
        ("nsenter", "--version"),
        ("mount", "--version"),
        ("umount", "--version"),
    ] {
        if !capture_command_with_timeout(program, &[flag], Duration::from_secs(5))
            .is_ok_and(|o| o.status.success())
        {
            caps.error = Some(format!("远端网卡缺少 {program}"));
            return caps;
        }
    }
    if !capture_command_with_timeout("nft", &["-j", "list", "tables"], Duration::from_secs(5))
        .is_ok_and(|o| o.status.success())
    {
        caps.error = Some("远端网卡缺少实际网络管理权限".into());
        return caps;
    }
    let namespace_probe=capture_command_with_timeout("unshare",&["--mount","--net","--","/bin/sh","-c","mount --make-rprivate / && mount -t tmpfs tmpfs /run && mkdir /run/netns && ip netns add bcn-probe && ip netns exec bcn-probe sysctl -qw net.ipv4.ip_forward=1"],Duration::from_secs(5));
    if !namespace_probe.is_ok_and(|probe| probe.status.success()) {
        caps.error = Some("远端网卡缺少实际创建和管理隔离网络的权限".into());
        return caps;
    }
    caps.provide = true;
    caps.consume = super::backend::current()
        .map(|backend| backend.inventory())
        .unwrap_or_else(|| {
            let config = std::env::var_os("BROCADE_CVM_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/etc/brocade/cvm-cube.json"));
            brocade_cube::Backend::inspect(&config)
        })
        .is_ok_and(|caps| caps.managed_networks);
    if capture_command_with_timeout("wg", &["--version"], Duration::from_secs(5))
        .is_ok_and(|o| o.status.success())
        && capture_command_with_timeout(
            "unshare",
            &[
                "--net",
                "--",
                "ip",
                "link",
                "add",
                "bcn-probe",
                "type",
                "wireguard",
            ],
            Duration::from_secs(5),
        )
        .is_ok_and(|o| o.status.success())
    {
        caps.transports.push(TransportKind::Wireguard);
    }
    if let Ok(certificate) = xray::certificate(&options.state_dir) {
        caps.transports.push(TransportKind::Xray);
        caps.xray_certificate_pem = Some(certificate);
    }
    // Entry machine-pair tunnels are WireGuard devices; port-selected egress is enforced
    // by the host forward rules of each NIC (see host::route_rules).
    caps.entries = caps.transports.contains(&TransportKind::Wireguard);
    caps.route_service = true;
    if let Ok(raw) = run_command("ip", &["-j", "address", "show"]) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            caps.addresses = value
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| {
                    !entry["ifalias"]
                        .as_str()
                        .is_some_and(|alias| alias.starts_with("brocade-nic:"))
                })
                .flat_map(|entry| entry["addr_info"].as_array().into_iter().flatten())
                .filter_map(|entry| entry["local"].as_str()?.parse::<IpAddr>().ok())
                .filter(|ip| {
                    !ip.is_loopback()
                        && !ip.is_unspecified()
                        && !ip.is_multicast()
                        && !match ip {
                            IpAddr::V4(ip) => ip.is_link_local(),
                            IpAddr::V6(ip) => ip.is_unicast_link_local(),
                        }
                })
                .take(256)
                .collect();
        }
    }
    if let Ok(raw) = run_command("ss", &["-H", "-lnt"]) {
        caps.host_ports
            .extend(listening_ports(&raw, L4Protocol::Tcp));
    }
    if let Ok(raw) = run_command("ss", &["-H", "-lnu"]) {
        caps.host_ports
            .extend(listening_ports(&raw, L4Protocol::Udp));
    }
    if caps.transports.is_empty() {
        caps.provide = false;
        caps.consume = false;
        caps.error = Some("机器尚无可用的 Xray／WireGuard IP 承载".into());
    }
    caps
}
fn listening_ports(raw: &str, protocol: L4Protocol) -> Vec<NicHostPort> {
    raw.lines()
        .filter_map(|line| {
            line.split_whitespace()
                .nth(3)?
                .rsplit_once(':')?
                .1
                .parse::<u16>()
                .ok()
        })
        .filter(|port| *port != 0)
        .take(1024)
        .map(|port| NicHostPort { protocol, port })
        .collect()
}

pub(crate) fn controller_lock(root: &Path) -> Result<std::fs::File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let directory = root.join("cvm-nics");
    private_dir(&directory)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.join("controller.lock"))
        .map_err(|_| "cannot open NIC controller ownership lock")?;
    if file
        .metadata()
        .map_err(|_| "cannot inspect NIC controller lock")?
        .uid()
        != effective_uid()
    {
        return Err("NIC controller lock belongs to another owner".into());
    }
    // SAFETY: flock operates only on this live owned descriptor.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another NIC controller owns this state directory".into());
    }
    Ok(file)
}
pub(super) fn initialize(options: &Options) {
    ROOT.get_or_init(|| options.state_dir.clone());
}
pub(super) fn spawn(options: Options) -> Result<()> {
    initialize(&options);
    let controller = controller_lock(&options.state_dir)?;
    std::thread::Builder::new()
        .name("cvm-nics".into())
        .spawn(move || {
            let _controller = controller;
            let mut wake =
                crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::Desired);
            let mut previous = String::new();
            loop {
                match round(&options) {
                    Ok(()) => previous.clear(),
                    Err(error) => {
                        if let Err(restore) = restore_saved(&options) {
                            if restore != previous {
                                eprintln!("cvm-nics: persisted state convergence: {restore}");
                            }
                        }
                        if error != previous {
                            eprintln!("cvm-nics: {error}");
                            previous = error;
                        }
                    }
                }
                wake.wait(Duration::from_secs(15));
            }
        })
        .map_err(|_| "cannot start allocated NIC worker".to_owned())?;
    Ok(())
}
fn authority_path(root: &Path) -> PathBuf {
    root.join("cvm-nic-authority.sha256")
}
fn authority(options: &Options) -> String {
    brocade_core::hash::sha256_hex(options.token.as_bytes())
}
fn revoke_cached(root: &Path) -> Result<()> {
    let mut revoked = snapshot(root)?;
    if revoked.bindings.is_empty() {
        return Ok(());
    }
    for binding in &mut revoked.bindings {
        binding.nic.blocked = true;
        binding.phase = BindingPhase::Withdrawing;
    }
    atomic_write_private(
        &desired_path(root),
        &serde_json::to_vec(&revoked).map_err(|_| "cannot persist withdrawn NIC authority")?,
    )
}
fn round(options: &Options) -> Result<()> {
    private_dir(&options.state_dir.join("cvm-nics"))?;
    if read_bounded(&authority_path(&options.state_dir), 128)?.as_deref()
        != Some(authority(options).as_bytes())
    {
        revoke_cached(&options.state_dir)?;
    }
    let client = HttpClient::new(&options.server)?;
    let caps = inventory(options);
    let inventory_result = upload(&client, options, "/agent/v1/cvm/nics/inventory", &caps);
    // A failed inventory upload must not prevent reading revocation or restoring data.

    if super::maintenance()? {
        return Ok(());
    }
    let response = client.request("GET", "/agent/v1/cvm/nics/desired", &options.token, None)?;
    if matches!(response.status, 401 | 403) {
        revoke_cached(&options.state_dir)?;
    }
    if response.status != 200 {
        return Err(format!(
            "NIC desired request failed: HTTP {}",
            response.status
        ));
    }
    let mut desired: NicDesired =
        serde_json::from_str(&response.body).map_err(|_| "invalid NIC desired response")?;
    validate(&desired)?;
    atomic_write_private(
        &authority_path(&options.state_dir),
        authority(options).as_bytes(),
    )?;
    let previous = snapshot(&options.state_dir)?;
    if desired.node_id == previous.node_id && desired.generation < previous.generation {
        return Err("stale NIC desired generation rejected".into());
    }
    if desired.bindings.is_empty()
        && desired.fences.is_empty()
        && previous.bindings.is_empty()
        && previous.fences.is_empty()
    {
        atomic_write_private(
            &desired_path(&options.state_dir),
            &serde_json::to_vec(&desired).map_err(|_| "cannot encode empty NIC intent")?,
        )?;
        return inventory_result;
    }
    local_management_networks(&mut desired)?;

    if let Some(deadlines) = UNREACHABLE.get() {
        deadlines
            .lock()
            .map_err(|_| "NIC connectivity clock unavailable")?
            .retain(|key, _| {
                desired.bindings.iter().any(|binding| {
                    key == &format!(
                        "{}-{}",
                        binding.id,
                        if binding.side == BindingSide::Consumer {
                            "c"
                        } else {
                            "p"
                        }
                    )
                })
            });
    }
    let (effective, local_blocks) = meter::effective(&options.state_dir, &desired);
    fences::apply(&options.state_dir, &effective)?;
    for old in &previous.bindings {
        if !desired
            .bindings
            .iter()
            .any(|current| current.id == old.id && current.side == old.side)
        {
            xray::stop(old)?;
            wireguard::remove(old)?;
            let retained = desired.bindings.iter().any(|current| {
                current.interface_id == old.interface_id && current.side == old.side
            });
            if !retained {
                host::forget(&options.state_dir, old)?;
            }
        }
    }
    atomic_write_private(
        &desired_path(&options.state_dir),
        &serde_json::to_vec(&desired).map_err(|_| "cannot encode private NIC intent")?,
    )?;
    crate::conntrack::track_delivery_scopes(&tracking_addresses()?, &tracking_ports()?)?;
    let reported = reconcile_reports(
        &effective.bindings,
        |binding| {
            let before = meter::read(binding).ok();
            let outcome = apply(&options.state_dir, binding, &effective);
            let traffic = meter::read(binding).ok().or(before);
            let (configured, reachable, rx_bytes, tx_bytes, error) = match outcome {
                Ok(observed) => (
                    observed.configured,
                    observed.reachable,
                    observed.rx,
                    observed.tx,
                    None,
                ),
                Err(error) => (false, None, 0, 0, Some(error.chars().take(500).collect())),
            };
            NicBindingReport {
                local_block: local_blocks.get(&binding.interface_id).cloned(),
                traffic,
                id: binding.id.clone(),
                side: binding.side,
                generation: binding.generation,
                phase: binding.phase,
                configured,
                reachable,
                rx_bytes,
                tx_bytes,
                error,
            }
        },
        |bindings| {
            upload(
                &client,
                options,
                "/agent/v1/cvm/nics/report",
                &NicNodeReport {
                    generation: desired.generation,
                    bindings,
                },
            )
        },
    );
    inventory_result.and(reported)
}

/// Finish the bounded sweep even when one endpoint fails. Reporting and offline
/// recovery both need progress for unrelated NICs before the next polling wait.
fn visit_bindings(
    bindings: &[NicBinding],
    mut visit: impl FnMut(&NicBinding) -> Result<()>,
) -> Result<()> {
    let mut first_error = None;
    for binding in bindings {
        if let Err(error) = visit(binding) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn reconcile_reports(
    bindings: &[NicBinding],
    mut observe: impl FnMut(&NicBinding) -> NicBindingReport,
    mut publish: impl FnMut(Vec<NicBindingReport>) -> Result<()>,
) -> Result<()> {
    let mut reports = Vec::with_capacity(REPORT_BATCH_SIZE);
    let visited = visit_bindings(bindings, |binding| {
        reports.push(observe(binding));
        if reports.len() == REPORT_BATCH_SIZE {
            publish(std::mem::take(&mut reports))?;
        }
        Ok(())
    });
    let flushed = if reports.is_empty() {
        Ok(())
    } else {
        publish(reports)
    };
    visited.and(flushed)
}

fn restore_saved(options: &Options) -> Result<()> {
    if options.apply_mode != ApplyMode::Linux || effective_uid() != 0 {
        return Ok(());
    }
    let mut desired = snapshot(&options.state_dir)?;
    if desired.bindings.is_empty() && desired.fences.is_empty() {
        return Ok(());
    }
    local_management_networks(&mut desired)?;
    let (desired, _) = meter::effective(&options.state_dir, &desired);
    fences::apply(&options.state_dir, &desired)?;
    visit_bindings(&desired.bindings, |binding| {
        apply(&options.state_dir, binding, &desired).map(|_| ())
    })
}
static UNREACHABLE: OnceLock<Mutex<std::collections::BTreeMap<String, std::time::Instant>>> =
    OnceLock::new();
fn connectivity(binding: &NicBinding, observation: &LinkObservation) -> Result<()> {
    let key = format!(
        "{}-{}",
        binding.id,
        if binding.side == BindingSide::Consumer {
            "c"
        } else {
            "p"
        }
    );
    let mut deadlines = UNREACHABLE
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "NIC connectivity clock unavailable")?;
    if observation.reachable == Some(true) || binding.phase == BindingPhase::Withdrawing {
        deadlines.remove(&key);
        return Ok(());
    }
    if !deadlines.contains_key(&key) && deadlines.len() >= NODE_BINDINGS * 4 {
        return Err("NIC connectivity observation capacity exceeded".into());
    }
    if deadlines
        .entry(key)
        .or_insert_with(std::time::Instant::now)
        .elapsed()
        > Duration::from_secs(120)
    {
        return Err("承载在 120 秒内未能确认连通，网卡和地址分配保留".into());
    }
    Ok(())
}
fn upload(
    client: &HttpClient,
    options: &Options,
    path: &str,
    value: &impl Serialize,
) -> Result<()> {
    let body = serde_json::to_string(value).map_err(|_| "cannot encode NIC observation")?;
    let response = client.request("POST", path, &options.token, Some(&body))?;
    if response.status != 200 && response.status != 204 {
        return Err(format!("NIC observation failed: HTTP {}", response.status));
    }
    Ok(())
}
#[derive(Default)]
struct LinkObservation {
    configured: bool,
    reachable: Option<bool>,
    rx: u64,
    tx: u64,
}
fn apply(root: &Path, binding: &NicBinding, desired: &NicDesired) -> Result<LinkObservation> {
    if binding.phase == BindingPhase::Withdrawing {
        xray::stop(binding)?;
        wireguard::remove(binding)?;
        let retain = desired.bindings.iter().any(|other| {
            other.interface_id == binding.interface_id
                && other.side == binding.side
                && other.id != binding.id
                && other.phase != BindingPhase::Withdrawing
        });
        host::withdraw(root, binding, retain)?;
        return Ok(LinkObservation {
            configured: true,
            ..Default::default()
        });
    }
    host::prepare(root, binding)?;
    host::prepare_transport(root, binding, desired)?;
    let observed = match binding.nic.transport {
        TransportKind::Wireguard => wireguard::apply(root, binding),
        TransportKind::Xray => xray::apply(root, binding),
    };
    let configured = host::configure(root, binding, desired);
    let observed = observed?;
    configured?;
    connectivity(binding, &observed)?;
    Ok(observed)
}
