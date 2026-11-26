//! Native instances adopt the current OS. No disk/image or host power operation lives here.
use crate::{
    command::run_command_with_timeout,
    fsutil::{atomic_write_private, read_bounded},
};
use brocade_deployment::cvm::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

mod network;
pub(crate) mod terminal;
type Result<T> = std::result::Result<T, String>;
static CONTROL_SERVER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
pub(super) fn initialize(options: &crate::options::Options) {
    CONTROL_SERVER.get_or_init(|| options.server.clone());
}
fn control_destinations() -> Result<Vec<IpPrefix>> {
    let server = CONTROL_SERVER
        .get()
        .ok_or("native control endpoint has not been initialized")?;
    let url = reqwest::Url::parse(server).map_err(|_| "invalid native control URL")?;
    let host = url.host_str().ok_or("native control URL has no host")?;
    let addresses = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => command("getent", &["ahosts", host])?
            .lines()
            .filter_map(|l| {
                l.split_whitespace()
                    .next()?
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .collect(),
    };
    if addresses.is_empty() || addresses.len() > 256 {
        return Err("native control addresses cannot be verified".into());
    }
    addresses
        .into_iter()
        .map(|ip| format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 }).parse())
        .collect()
}

#[derive(Clone, Deserialize)]
pub(super) struct Config {
    pub terminal_user: String,
    pub primary_device: String,
    #[serde(default)]
    pub protected_destinations: Vec<brocade_deployment::cvm::IpPrefix>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    desired: InstanceDesired,
    deleted: bool,
}

pub(super) fn chain_configured(root: &Path) -> Result<bool> {
    if !root.try_exists().map_err(|_| "无法读取裸机恢复状态")? {
        return Ok(false);
    }
    let mut configured = false;
    for (index, item) in std::fs::read_dir(root)
        .map_err(|_| "无法读取裸机恢复状态")?
        .enumerate()
    {
        if index >= 8192 {
            return Err("裸机恢复记录数量超出上限".into());
        }
        let path = item.map_err(|_| "无法读取裸机恢复记录")?.path();
        if path.extension().is_some_and(|value| value == "json") {
            let record: Record = serde_json::from_slice(&read(&path, 256 * 1024)?)
                .map_err(|_| "裸机恢复记录损坏，不能替换 Agent")?;
            configured |= !record.deleted && !record.desired.spec.routing.chain_ids().is_empty();
        }
    }
    Ok(configured)
}

#[derive(Clone)]
pub(super) struct Backend(Arc<Inner>);
pub(super) struct Inner {
    config: Config,
    root: PathBuf,
    records: Mutex<BTreeMap<String, Record>>,
    runtime: tokio::runtime::Runtime,
    _lock: File,
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    read_bounded(path, limit)?
        .ok_or_else(|| format!("required host file is absent: {}", path.display()))
}
fn load_config(path: &Path) -> Result<Config> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "bare-metal configuration is absent")?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("bare-metal configuration must be private and owned by the Agent UID".into());
    }
    let config: Config = serde_json::from_slice(&read(path, 32 * 1024)?)
        .map_err(|_| "invalid bare-metal configuration")?;
    let sample = HostInventory {
        identity: HostIdentity {
            machine: "a".repeat(64),
            root_filesystem: "a".repeat(64),
        },
        os: "configured".into(),
        terminal_user: config.terminal_user.clone(),
        primary_device: config.primary_device.clone(),
    };
    sample.validate()?;
    if config.primary_device == "lo" || config.protected_destinations.len() > 256 {
        return Err("invalid bare-metal management interface or protected routes".into());
    }
    terminal::account(&config.terminal_user)?;
    Ok(config)
}
fn command(program: &str, args: &[&str]) -> Result<String> {
    run_command_with_timeout(program, args, Duration::from_secs(5))
}
pub(super) fn identity() -> Result<HostIdentity> {
    let machine = read(Path::new("/etc/machine-id"), 128)?;
    if machine.len() < 32 {
        return Err("host machine identity is invalid".into());
    }
    let uuid = command("findmnt", &["-n", "-o", "UUID", "-T", "/"])?;
    if uuid.trim().is_empty() {
        return Err("root filesystem must have a stable UUID before adoption".into());
    }
    Ok(HostIdentity {
        machine: brocade_core::hash::sha256_hex(machine.trim_ascii()),
        root_filesystem: brocade_core::hash::sha256_hex(uuid.trim().as_bytes()),
    })
}
pub(super) fn boot_id() -> Result<String> {
    let boot = String::from_utf8(read(Path::new("/proc/sys/kernel/random/boot_id"), 128)?)
        .map_err(|_| "invalid host boot ID")?
        .trim()
        .to_owned();
    HostEvidence {
        identity: HostIdentity {
            machine: "a".repeat(64),
            root_filesystem: "a".repeat(64),
        },
        boot_id: boot.clone(),
    }
    .validate()?;
    Ok(boot)
}
fn primary(config: &Config, id: &str) -> Result<InterfaceObservation> {
    let raw = command(
        "ip",
        &["-j", "address", "show", "dev", &config.primary_device],
    )?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| "invalid management interface inventory")?;
    let row = rows.first().ok_or("management interface is absent")?;
    let mac = row["address"]
        .as_str()
        .ok_or("management interface has no hardware identity")?
        .to_owned();
    let addresses = row["addr_info"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let ip = a["local"].as_str()?.parse::<std::net::IpAddr>().ok()?;
            let prefix = a["prefixlen"].as_u64()?;
            Some(format!("{ip}/{prefix}"))
        })
        .take(16)
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("management interface has no address".into());
    }
    Ok(InterfaceObservation {
        id: id.into(),
        device: config.primary_device.clone(),
        mac,
        addresses,
        attached: true,
    })
}
fn metrics(interfaces: &[InterfaceObservation]) -> Result<GuestMetrics> {
    let raw = command("/bin/sh", &["-c", SYSTEM_METRICS_SCRIPT])?;
    parse_system_metrics(raw.as_bytes(), interfaces, now_millis()?)
}

impl Backend {
    pub(super) fn inspect_empty(config: &Path, root: &Path) -> Result<()> {
        load_config(config)?;
        network::inspect_empty()?;
        if !root
            .try_exists()
            .map_err(|_| "native state cannot be inspected")?
        {
            return Ok(());
        }
        let lock =
            File::open(root.join("lock")).map_err(|_| "native state ownership lock is absent")?;
        // SAFETY: lock owns its descriptor and is held throughout the inspection.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("stop the native Agent before checking its released state".into());
        }
        for (count, entry) in std::fs::read_dir(root)
            .map_err(|_| "native state cannot be read")?
            .enumerate()
        {
            if count >= 8192 {
                return Err("native state exceeds inspection limit".into());
            }
            let path = entry.map_err(|_| "native state cannot be read")?.path();
            if path.extension().is_some_and(|v| v == "json") {
                let record: Record = serde_json::from_slice(&read(&path, 256 * 1024)?)
                    .map_err(|_| "native state is corrupt; resources retained")?;
                if !record.deleted {
                    return Err("release the native binding before switching backend".into());
                }
            }
        }
        if ownership_file_exists(root)? {
            return Err("native network cleanup is not complete".into());
        }
        Ok(())
    }
    pub(super) fn open(config: &Path, root: &Path) -> Result<Self> {
        if !config.is_absolute() || !root.is_absolute() {
            return Err("native config and state paths must be absolute".into());
        }
        let config = load_config(config)?;
        super::nics::private_dir(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.join("lock"))
            .map_err(|_| "cannot open native executor lock")?;
        // SAFETY: lock owns this descriptor; the lock remains held with Inner.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another native executor owns this state".into());
        }
        let mut records = BTreeMap::new();
        for (index, entry) in std::fs::read_dir(root)
            .map_err(|_| "cannot inspect native execution state")?
            .enumerate()
        {
            if index >= 8192 {
                return Err("native execution state exceeds limit".into());
            }
            let path = entry
                .map_err(|_| "cannot inspect native execution record")?
                .path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let record: Record = serde_json::from_slice(&read(&path, 256 * 1024)?)
                .map_err(|_| "invalid native execution record; state retained")?;
            if !valid_id(&record.desired.id)
                || path.file_stem().and_then(|x| x.to_str()) != Some(&record.desired.id)
                || record.desired.spec.execution.is_cube()
            {
                return Err("native execution record identity mismatch".into());
            }
            records.insert(record.desired.id.clone(), record);
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "native runtime unavailable")?;
        let backend = Self(Arc::new(Inner {
            config,
            root: root.into(),
            records: Mutex::new(records),
            runtime,
            _lock: lock,
        }));
        backend.restore_saved();
        let weak = Arc::downgrade(&backend.0);
        std::thread::Builder::new()
            .name("native-recovery".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(15));
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                Backend(inner).restore_saved();
            })
            .map_err(|_| "cannot start native recovery worker")?;
        Ok(backend)
    }
    fn restore_saved(&self) {
        let desired = self.0.records.lock().ok().map(|records| {
            records
                .values()
                .filter(|r| !r.deleted)
                .map(|r| r.desired.clone())
                .collect::<Vec<_>>()
        });
        if let Some(desired) = desired {
            for item in desired {
                if let Err(error) = self.submit(&item) {
                    eprintln!("native recovery: {error}");
                }
            }
        }
    }
    pub(super) fn system_inventory(&self) -> Result<HostSystemInventory> {
        system_inventory_from_config(&self.0.config)
    }
    pub(super) fn augment_capabilities(&self, capabilities: &mut Capabilities) -> Result<()> {
        network::capabilities(capabilities)?;
        if !capabilities
            .network_sources
            .iter()
            .any(|source| source.id == "host")
        {
            capabilities.network_sources.push(NetworkSource {
                id: "host".into(),
                name: "宿主管理网卡".into(),
                kind: NetworkKind::Public,
                description: "原有网络与管理联络".into(),
                max_attachments: Some(1),
                supports_ipv6: Some(true),
            });
        }
        let terminal =
            Path::new(cube_terminal::ROOT).join(format!("host-ready-{}.sock", boot_id()?));
        let terminal_ready = std::os::unix::net::UnixStream::connect(&terminal).is_ok();
        capabilities.guest_terminal |= terminal_ready;
        capabilities.guest_terminal_persistent |= terminal_ready;
        Ok(())
    }
    pub(super) fn owns(&self, instance: &str) -> Result<bool> {
        Ok(self
            .0
            .records
            .lock()
            .map_err(|_| "native state unavailable")?
            .contains_key(instance))
    }
    pub(super) fn submit(&self, desired: &InstanceDesired) -> Result<InstanceReport> {
        let expected = desired
            .spec
            .execution
            .host_identity()
            .ok_or("native executor refuses Cube assignments")?;
        if !valid_id(&desired.id)
            || !valid_id(&desired.operation_id)
            || desired.generation == 0
            || desired.spec_sha256
                != brocade_core::hash::sha256_hex(
                    &serde_json::to_vec(&desired.spec)
                        .map_err(|_| "invalid native specification")?,
                )
            || desired.spec.image.is_some()
            || desired.spec.login_revision != 0
            || desired.delete_data
            || desired.restart_epoch != 0
            || desired.reinstall_epoch != 0
            || desired.power == PowerState::Stopped
            || expected != &identity()?
        {
            return Err("native assignment identity, storage or power contract is invalid".into());
        }
        let mut records = self
            .0
            .records
            .lock()
            .map_err(|_| "native state unavailable")?;
        if records
            .values()
            .any(|r| !r.deleted && r.desired.id != desired.id)
        {
            return Err("host already belongs to another instance".into());
        }
        if let Some(old) = records.get(&desired.id) {
            if old.desired.generation > desired.generation
                || (old.desired.generation == desired.generation && old.desired != *desired)
                || (old.deleted && desired.power != PowerState::Deleted)
            {
                return Err("stale native assignment or released identity cannot be reused".into());
            }
        } else if records.len() >= 8190 {
            return Err("native operation history exceeds limit".into());
        }
        let record = Record {
            desired: desired.clone(),
            deleted: false,
        };
        self.0.save(&record)?;
        records.insert(desired.id.clone(), record);
        let result = network::converge(&self.0.config, &self.0.root, desired);
        let mut report = brocade_cube::report(desired, ApplyStatus::Applied, None);
        report.host = Some(HostEvidence {
            identity: expected.clone(),
            boot_id: boot_id()?,
        });
        match result {
            Ok(network::Convergence::Applied) => {}
            Ok(network::Convergence::WaitingForTransport) => {
                report.status = ApplyStatus::Pending;
                return Ok(report);
            }
            Err(error) => {
                report.status = ApplyStatus::Failed;
                report.error = Some(error.chars().take(120).collect());
                return Ok(report);
            }
        }
        if desired.power == PowerState::Deleted {
            self.0.runtime.block_on(brocade_cube::revoke_terminal(
                &runtime_id(&desired.id, &boot_id()?),
                u64::MAX,
            ))?;
            report.power = Some(PowerState::Deleted);
            report.network_absent = true;
            report.detached_interfaces = desired.detached_interfaces.clone();
            let record = Record {
                desired: desired.clone(),
                deleted: true,
            };
            self.0.save(&record)?;
            records.insert(desired.id.clone(), record);
        } else {
            report.power = Some(PowerState::Running);
            report.runtime_id = Some(runtime_id(&desired.id, &boot_id()?));
            report.interfaces = network::interfaces(&self.0.config, desired)?;
            report.access_address = network::delivery(&self.0.config, desired)?;
            report.network = Some(NetworkObservation {
                chain_egress_configured: !desired.spec.routing.chain_ids().is_empty()
                    && super::chain_egress::ready(&desired.id, &desired.spec.routing),
                routing_revision: desired.spec.routing.revision,
                contact_route_preserved: true,
                business_fallback_blocked: true,
                default_ipv4_interface: desired
                    .spec
                    .routing
                    .default_interface(true)
                    .map(str::to_owned),
                default_ipv6_interface: desired
                    .spec
                    .routing
                    .default_interface(false)
                    .map(str::to_owned),
            });
            report.detached_interfaces = desired.detached_interfaces.clone();
        }
        report.validate_for(desired)?;
        Ok(report)
    }
    pub(super) fn observe(
        &self,
        targets: &[ObservationTarget],
        _: &[String],
    ) -> Result<Vec<InstanceObservation>> {
        if targets.len() > 1 {
            return Err("bare-metal host accepts only one observation target".into());
        }
        let records = self
            .0
            .records
            .lock()
            .map_err(|_| "native state unavailable")?
            .clone();
        let mut out = Vec::new();
        for target in targets {
            let record = records
                .get(&target.id)
                .filter(|r| !r.deleted && r.desired.generation == target.generation)
                .ok_or("native observation is outside assigned ownership")?;
            if record.desired.spec.execution.host_identity() != Some(&identity()?) {
                return Err("native host identity changed".into());
            }
            let interfaces = network::interfaces(&self.0.config, &record.desired)?;
            let sample = metrics(&interfaces)?;
            let ssh = primary(&self.0.config, "host")?
                .addresses
                .into_iter()
                .find_map(|a| a.split('/').next().map(str::to_owned))
                .map_or(Reachability::Unknown, |a| brocade_cube::probe_tcp(&a, 22).0);
            out.push(InstanceObservation {
                id: target.id.clone(),
                generation: target.generation,
                observed_at_unix_millis: sample.observed_at_unix_millis,
                runtime_id: Some(runtime_id(&target.id, &sample.boot_id)),
                runtime_state: ObservedRuntimeState::Running,
                guest_state: Reachability::Reachable,
                ssh_state: ssh,
                metrics: Some(sample),
                error: None,
                services: target
                    .services
                    .iter()
                    .map(|s| {
                        let allowed = interfaces.iter().any(|n| {
                            n.id == s.interface_id
                                && n.addresses
                                    .iter()
                                    .any(|a| a.split_once('/').is_some_and(|(ip, _)| ip == s.host))
                        });
                        let (state, latency_millis) = if allowed {
                            brocade_cube::probe_tcp(&s.host, s.port)
                        } else {
                            (Reachability::Unknown, None)
                        };
                        ServiceProbeResult {
                            id: s.id.clone(),
                            state,
                            latency_millis,
                        }
                    })
                    .collect(),
            });
        }
        Ok(out)
    }
    pub(super) fn persistent_terminal(
        &self,
        offer: brocade_deployment::cvm::terminal::Offer,
    ) -> Result<brocade_cube::TerminalSession> {
        brocade_cube::connect_terminal(self.0.runtime.handle().clone(), self.0.clone(), offer)
    }
    pub(super) fn revoke_persistent_terminals(&self, through: u64) -> Result<()> {
        let records = self
            .0
            .records
            .lock()
            .map_err(|_| "native state unavailable")?;
        for record in records.values().filter(|r| !r.deleted) {
            self.0.runtime.block_on(brocade_cube::revoke_terminal(
                &runtime_id(&record.desired.id, &boot_id()?),
                through,
            ))?;
        }
        Ok(())
    }
}
impl Inner {
    fn save(&self, record: &Record) -> Result<()> {
        atomic_write_private(
            &self.root.join(format!("{}.json", record.desired.id)),
            &serde_json::to_vec(record).map_err(|_| "invalid native intent")?,
        )
    }
}
impl brocade_cube::TerminalTarget for Inner {
    fn validate(&self, target: &brocade_deployment::cvm::terminal::Target, _: bool) -> Result<()> {
        let records = self
            .records
            .lock()
            .map_err(|_| "native state unavailable")?;
        let record = records
            .get(&target.instance_id)
            .ok_or("native terminal assignment unavailable")?;
        if record.deleted
            || record.desired.power != PowerState::Running
            || record.desired.spec.execution.host_identity() != Some(&identity()?)
            || target.runtime_id != runtime_id(&target.instance_id, &boot_id()?)
            || target.restart_epoch != 0
            || target.reinstall_epoch != 0
        {
            return Err("native terminal host or binding has changed".into());
        }
        Ok(())
    }
}
pub(super) fn system_inventory(path: &Path) -> Result<HostSystemInventory> {
    let config = load_config(path)?;
    system_inventory_from_config(&config)
}

fn system_inventory_from_config(config: &Config) -> Result<HostSystemInventory> {
    primary(config, "host")?;
    let sample = metrics(&[primary(config, "host")?])?;
    let inventory = HostSystemInventory {
        os: os_name()?,
        primary_device: config.primary_device.clone(),
        identity: Some(identity()?),
        terminal_user: Some(config.terminal_user.clone()),
        resources: Some(Resources {
            vcpus: std::thread::available_parallelism()
                .map_err(|_| "CPU capacity unavailable")?
                .get()
                .try_into()
                .map_err(|_| "CPU capacity exceeds limit")?,
            memory_bytes: sample.memory_total_bytes,
            root_disk_bytes: sample.root_total_bytes,
        }),
        terminal_ready: std::os::unix::net::UnixStream::connect(
            Path::new(cube_terminal::ROOT).join(format!("host-ready-{}.sock", boot_id()?)),
        )
        .is_ok(),
    };
    inventory.validate()?;
    Ok(inventory)
}

fn os_name() -> Result<String> {
    let release_path =
        std::fs::canonicalize("/etc/os-release").map_err(|_| "OS identity is unavailable")?;
    let release =
        String::from_utf8(read(&release_path, 8192)?).map_err(|_| "invalid OS identity")?;
    Ok(release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .unwrap_or("Linux")
        .trim_matches('"')
        .to_owned())
}

fn now_millis() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "invalid system clock")?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock overflow".into())
}

fn runtime_id(instance: &str, boot: &str) -> String {
    format!(
        "host-{}-{boot}",
        &brocade_core::hash::sha256_hex(instance.as_bytes())[..16]
    )
}

pub(crate) fn tracking_addresses() -> Result<std::collections::BTreeSet<std::net::IpAddr>> {
    let Some(path) = super::backend::host_config_path()? else {
        return Ok(std::collections::BTreeSet::new());
    };
    let config = load_config(&path)?;
    primary(&config, "host")?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&command("ip", &["-j", "address", "show"])?)
            .map_err(|_| "invalid native address inventory")?;
    Ok(rows
        .iter()
        .flat_map(|row| row["addr_info"].as_array().into_iter().flatten())
        .filter_map(|a| a["local"].as_str()?.parse::<std::net::IpAddr>().ok())
        .filter(|a| !a.is_loopback() && !a.is_unspecified() && !a.is_multicast())
        .collect())
}

fn ownership_file_exists(root: &Path) -> Result<bool> {
    root.join("network/state.json")
        .try_exists()
        .map_err(|_| "native network ownership cannot be inspected".into())
}
