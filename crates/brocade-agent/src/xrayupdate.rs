//! Pull, validate and safely install an explicitly released Xray binary.
//!
//! Downloading runs off the convergence thread because a slow transfer must not delay desired
//! state. The irreversible part is handed back to the main loop, which is the sole owner of Xray
//! restarts and can hold the same usage-meter lock as configuration convergence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    net::{IpAddr, SocketAddr},
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    os::unix::fs::FileTypeExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use brocade_deployment::plan::GrantInbound;
use brocade_deployment::protocol::{
    ConfigXrayForceRetirementCommand, XrayHandoffCapability, XrayHandoffState, XrayReleaseOffer,
    XrayReleaseOutcome, XrayReleaseReport, XrayReleaseStrategy, XrayUpgradePhase,
    XrayUpgradeProcess, XrayUpgradeRuntime, XrayUpgradeSource, XRAY_HANDOFF_DEFAULT_DRAIN_SECS,
    XRAY_HANDOFF_MAX_DRAIN_SECS, XRAY_HANDOFF_MIN_DRAIN_SECS,
};
use serde::{Deserialize, Serialize};

use crate::{
    command::run_command_with_timeout, file_sha256_hex, http::HttpClient, identity, shell_quote,
    ApplyMode, Options, WorkloadRuntime, ARCH_HEADER,
};

mod crash_recovery;
mod startup;

pub(crate) use startup::failure as xray_startup_failure;

const UPDATE_INTERVAL: Duration = Duration::from_secs(10 * 60);
const STARTUP_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_HEALTH_INTERVAL: Duration = Duration::from_millis(50);
// A candidate starts while the old generation is still serving, so allow slow or CPU-starved
// nodes more time to construct the complete runtime. This does not extend destructive restart
// recovery: only the handoff candidate uses this wider window.
const HANDOFF_CANDIDATE_START_TIMEOUT: Duration = Duration::from_secs(15);
const UPDATE_JITTER_MAX_SECS: u64 = 2 * 60;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(310);
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;
const CANDIDATE_FILE: &str = "xray-release.candidate";
const DOWNLOAD_FILE: &str = "xray-release.download";
const PREVIOUS_FILE: &str = "xray-release.previous";
const CONFIG_PREVIOUS_FILE: &str = "xray-config.previous.json";
const CONFIG_GRANTS_FILE: &str = "xray-config.candidate-grants.json";
const REPORT_FILE: &str = "xray-release-report.json";
// Keep file mutation brief; a Console request must never hold up local crash recovery.
static REPORT_STATE_LOCK: Mutex<()> = Mutex::new(());
static REPORT_RETRY_LOCK: Mutex<()> = Mutex::new(());
pub(crate) const UPGRADE_RUNTIME_FILE: &str = "xray-upgrade.json";
// This capability keeps TCP generations drainable while treating UDP as reconnect-only: the
// candidate binds the public SO_REUSEPORT socket, then the old generation drops its UDP socket.
// Requiring the explicit marker prevents an older v2 process from receiving `drop_udp`.
const XRAY_HANDOFF_CAPABILITY: &str = "tcp-handoff-udp-reset-v1";
const XRAY_HANDOFF_MANIFEST_ENV: &[u8] = b"XRAY_TCP_HANDOFF_MANIFEST=";
const MAX_UPGRADE_RUNTIME_BYTES: u64 = 64 * 1024;
const MAX_HANDOFF_MESSAGE_BYTES: usize = 256 * 1024;
const HANDOFF_DIR: &str = "xray-handoff";

#[derive(Debug, Clone)]
struct PreparedUpdate {
    offer: XrayReleaseOffer,
    candidate: PathBuf,
}

#[derive(Default)]
pub(crate) struct XrayUpdateQueue {
    pending: Mutex<Option<PreparedUpdate>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredReport {
    report: XrayReleaseReport,
}

#[derive(Debug, Deserialize)]
struct ReportAck {
    accepted: bool,
}

/// Observe the temporary two-generation state written by the release executor. The file is
/// deliberately separate from desired state and versions: it exists only for an executing Xray
/// attempt and is removed at every terminal boundary.
pub(crate) fn read_upgrade_runtime(state_dir: &Path) -> Result<Option<XrayUpgradeRuntime>, String> {
    let path = state_dir.join(UPGRADE_RUNTIME_FILE);
    // Validate and read through one descriptor. Checking the path and then reopening it would
    // leave a rename/symlink race exactly where release-time process identities are trusted.
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "读取 Xray 双进程状态 {} 失败：{error}",
                path.display()
            ))
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("检查 Xray 双进程状态 {} 失败：{error}", path.display()))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_UPGRADE_RUNTIME_BYTES
    {
        return Err(format!(
            "Xray 双进程状态 {} 的类型、属主、链接数、权限或大小无效",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_UPGRADE_RUNTIME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取 Xray 双进程状态 {} 失败：{error}", path.display()))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!(
            "Xray 双进程状态 {} 在读取期间发生变化",
            path.display()
        ));
    }
    let mut runtime: XrayUpgradeRuntime = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析 Xray 双进程状态 {} 失败：{error}", path.display()))?;
    if !valid_upgrade_runtime(&runtime) {
        return Err(format!(
            "Xray 双进程状态 {} 的发布身份或进程身份无效",
            path.display()
        ));
    }
    refresh_upgrade_process(&mut runtime.old);
    if let Some(candidate) = runtime.candidate.as_mut() {
        refresh_upgrade_process(candidate);
    }
    Ok(Some(runtime))
}

#[cfg(test)]
fn observe_upgrade_runtime(state_dir: &Path) -> Option<XrayUpgradeRuntime> {
    read_upgrade_runtime(state_dir).ok().flatten()
}

pub(crate) fn observe_handoff_capability(
    state_dir: &Path,
    upgrade_in_progress: bool,
    xray_version_output: Option<&str>,
) -> XrayHandoffCapability {
    if upgrade_in_progress {
        return XrayHandoffCapability {
            state: XrayHandoffState::Busy,
            reason: Some("Xray 发布正在进行".to_owned()),
        };
    }
    if !xray_version_output
        .is_some_and(|output| crate::xray_version_has_capability(output, XRAY_HANDOFF_CAPABILITY))
    {
        return XrayHandoffCapability {
            state: XrayHandoffState::Unsupported,
            reason: Some("运行版本没有 tcp-handoff-udp-reset-v1 能力".to_owned()),
        };
    }
    match observe_serving_handoff(state_dir) {
        Ok(status) if status.retirement_supported && status.ready && status.accepting => {
            XrayHandoffCapability {
                state: XrayHandoffState::Ready,
                reason: None,
            }
        }
        Ok(status) => XrayHandoffCapability {
            state: XrayHandoffState::Blocked,
            reason: Some(handoff_blocked_reason(&status)),
        },
        Err(error) => XrayHandoffCapability {
            state: XrayHandoffState::Blocked,
            reason: Some(format!("运行中的 Xray 尚未完成代际接管：{error}")),
        },
    }
}

pub(crate) fn installed_xray_supports_handoff() -> bool {
    let Ok(path) = crate::which_xray() else {
        return false;
    };
    let program = path.to_string_lossy().into_owned();
    crate::run_command(&program, &["version"])
        .is_ok_and(|output| crate::xray_version_has_capability(&output, XRAY_HANDOFF_CAPABILITY))
}

pub(crate) fn config_handoff_unavailable_reason(state_dir: &Path, desired: &str) -> Option<String> {
    let result = (|| -> Result<(), String> {
        let (process, _, status) = observe_serving_handoff_generation(state_dir)?;
        if !status.ready || !status.accepting || !status.retirement_supported {
            return Err(handoff_blocked_reason(&status));
        }
        let version = run_command_with_timeout(
            &format!("/proc/{}/exe", process.pid),
            &["version"],
            Duration::from_secs(5),
        )?;
        for capability in [XRAY_HANDOFF_CAPABILITY, "config-listener-handoff-v1"] {
            if !crate::xray_version_has_capability(&version, capability) {
                return Err(format!("运行中的 Xray 缺少 {capability} 能力"));
            }
        }
        let config: serde_json::Value =
            serde_json::from_str(desired).map_err(|error| error.to_string())?;
        if ["/api/listen", "/metrics/listen"].iter().any(|pointer| {
            config
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|listen| !listen.is_empty())
        }) && !crate::xray_version_has_capability(&version, "control-listener-handoff-v1")
        {
            return Err("运行中的 Xray 缺少 control-listener-handoff-v1 能力".to_owned());
        }
        Ok(())
    })();
    result.err()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandoffManifestIdentity {
    version: u32,
    #[serde(default)]
    scope: String,
    generation_id: String,
    control_path: PathBuf,
    #[serde(default)]
    listeners: Vec<HandoffEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HandoffEndpoint {
    address: String,
    bind_address: String,
    network: String,
    role: String,
    #[serde(default, skip_serializing_if = "is_false")]
    prepare: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    depends_on: String,
    fd: i32,
    socket_inode: u64,
    socket_cookie: u64,
    protocol: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    socket_config_sha256: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone)]
pub(crate) struct BootstrapHandoff {
    manifest_path: PathBuf,
    manifest: HandoffManifestIdentity,
}

impl BootstrapHandoff {
    pub(crate) fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub(crate) fn preflight(&self) -> Result<(), String> {
        validate_handoff_manifest_path(&self.manifest_path)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct HandoffRuntimeStatus {
    scope: String,
    retirement_supported: bool,
    generation_id: String,
    pid: i32,
    boot_id: String,
    pid_start_ticks: String,
    ready: bool,
    accepting: bool,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    epoch: u64,
    #[serde(default)]
    retire_ready: bool,
    #[serde(default)]
    active: u64,
    #[serde(default)]
    public_active: u64,
    #[serde(default)]
    cover_active: u64,
    #[serde(default)]
    udp_dropped: bool,
    #[serde(default)]
    listeners: Vec<HandoffListenerStatus>,
    #[serde(default)]
    unsupported_reasons: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HandoffListenerStatus {
    address: String,
    bind_address: String,
    network: String,
    role: String,
    #[serde(default)]
    depends_on: String,
    socket_inode: u64,
    socket_cookie: u64,
    protocol: i32,
    #[serde(default)]
    socket_config_sha256: String,
    #[serde(default)]
    active: u64,
    // A listener blocks inside accept while it is healthy, so this is observation only and must
    // never be used as a liveness condition.
    #[serde(default, rename = "in_accept")]
    _in_accept: bool,
    #[serde(default)]
    closed: bool,
}

#[derive(Debug, Deserialize)]
struct HandoffRuntimeResponse {
    #[serde(default)]
    error: String,
    status: Option<HandoffRuntimeStatus>,
}

pub(crate) fn prepare_bootstrap_handoff(
    state_dir: &Path,
    xray_content: &str,
) -> Result<BootstrapHandoff, String> {
    write_bootstrap_handoff(
        state_dir,
        configured_handoff_endpoints(state_dir, xray_content)?,
    )
}

// The desired generation, not the exporting generation, owns the expected listener set.
// Sharing this with bootstrap keeps static and dynamic forwarding listeners in one contract.
fn configured_handoff_endpoints(
    state_dir: &Path,
    xray_content: &str,
) -> Result<Vec<HandoffEndpoint>, String> {
    let config: serde_json::Value = serde_json::from_str(xray_content)
        .map_err(|error| format!("解析 Xray 配置以生成代际 manifest 失败：{error}"))?;
    let api_tag = config
        .get("api")
        .and_then(|api| api.get("tag"))
        .and_then(serde_json::Value::as_str);
    let inbounds = config
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Xray 配置没有 inbounds 数组".to_owned())?;

    let mut static_rows = Vec::with_capacity(inbounds.len());
    for inbound in inbounds {
        let tag = inbound
            .get("tag")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Xray inbound 缺少 tag".to_owned())?;
        let listen = inbound
            .get("listen")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Xray inbound {tag} 缺少明确 listen"))?;
        let port = inbound
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("Xray inbound {tag} 端口无效"))?;
        let address = canonical_listener_address(listen, port)?;
        let reality_dest = inbound
            .pointer("/streamSettings/realitySettings/dest")
            .and_then(serde_json::Value::as_str)
            .and_then(canonical_existing_socket_address);
        static_rows.push((tag.to_owned(), inbound, address, reality_dest));
    }
    let cover_addresses = static_rows
        .iter()
        .filter_map(|(_, _, _, dependency)| dependency.clone())
        .collect::<BTreeSet<_>>();
    let static_addresses = static_rows
        .iter()
        .map(|(_, _, address, _)| address.clone())
        .collect::<BTreeSet<_>>();
    let mut endpoints = Vec::new();
    let direct_api = config
        .pointer("/api/listen")
        .and_then(serde_json::Value::as_str)
        .filter(|listen| !listen.is_empty());
    for (listen, prepare) in [
        (direct_api, true),
        (
            config
                .pointer("/metrics/listen")
                .and_then(serde_json::Value::as_str)
                .filter(|listen| !listen.is_empty()),
            false,
        ),
    ] {
        if let Some(listen) = listen {
            let address = canonical_existing_socket_address(listen)
                .ok_or("Xray API/metrics listener 不是明确的 IP:port")?;
            endpoints.push(new_bootstrap_endpoint(
                "tcp",
                "control",
                prepare,
                address,
                String::new(),
            )?);
        }
    }
    for (tag, inbound, address, dependency) in static_rows {
        let protocol = inbound
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let transport = inbound
            .pointer("/streamSettings/network")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tcp");
        let dokodemo_network = inbound
            .pointer("/settings/network")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tcp");
        let (has_tcp, has_udp) = if protocol == "dokodemo-door" {
            (
                dokodemo_network
                    .split(',')
                    .any(|network| network.trim() == "tcp"),
                dokodemo_network
                    .split(',')
                    .any(|network| network.trim() == "udp"),
            )
        } else if transport == "hysteria" {
            (false, true)
        } else {
            (true, false)
        };
        let role = if direct_api.is_none() && api_tag == Some(tag.as_str()) {
            "control"
        } else if cover_addresses.contains(&address) {
            "cover"
        } else {
            "public"
        };
        if has_tcp {
            endpoints.push(new_bootstrap_endpoint(
                "tcp",
                role,
                role == "control",
                address.clone(),
                dependency
                    .filter(|dependency| static_addresses.contains(dependency))
                    .unwrap_or_default(),
            )?);
        }
        if has_udp {
            if role != "public" {
                return Err(format!(
                    "Xray inbound {tag} 的 UDP listener 不能作为 {role}"
                ));
            }
            endpoints.push(new_bootstrap_endpoint(
                "udp",
                "public",
                false,
                address,
                String::new(),
            )?);
        }
    }
    if let Some(forwarding) = crate::forwarding::desired_state_on_disk(state_dir)? {
        use brocade_deployment::protocol::ForwardingProtocol;
        for rule in forwarding.rules {
            let address = canonical_listener_address("0.0.0.0", rule.listen_port)?;
            if matches!(
                rule.protocol,
                ForwardingProtocol::Tcp | ForwardingProtocol::TcpUdp
            ) {
                endpoints.push(new_bootstrap_endpoint(
                    "tcp",
                    "public",
                    false,
                    address.clone(),
                    String::new(),
                )?);
            }
            if matches!(
                rule.protocol,
                ForwardingProtocol::Udp | ForwardingProtocol::TcpUdp
            ) {
                endpoints.push(new_bootstrap_endpoint(
                    "udp",
                    "public",
                    false,
                    address,
                    String::new(),
                )?);
            }
        }
    }
    endpoints.sort_by(|left, right| {
        left.network
            .cmp(&right.network)
            .then_with(|| left.address.cmp(&right.address))
    });
    if endpoints.is_empty() || endpoints.len() > 512 {
        return Err("Xray 代际 manifest listener 数量不在 1..=512".to_owned());
    }
    for pair in endpoints.windows(2) {
        if pair[0].network == pair[1].network && pair[0].address == pair[1].address {
            return Err(format!(
                "Xray 代际 manifest 出现重复 listener {} {}",
                pair[0].network, pair[0].address
            ));
        }
    }
    if endpoints
        .iter()
        .filter(|endpoint| endpoint.role == "control" && endpoint.prepare)
        .count()
        != 1
    {
        return Err("Xray 代际 manifest 必须且只能包含一个 loopback API listener".to_owned());
    }

    Ok(endpoints)
}

fn write_bootstrap_handoff(
    state_dir: &Path,
    endpoints: Vec<HandoffEndpoint>,
) -> Result<BootstrapHandoff, String> {
    let generation_id = format!("brocade-{}", random_token()?);
    let root = state_dir.join(HANDOFF_DIR);
    crate::create_private_dir(&root)?;
    let generation_dir = root.join(&generation_id);
    crate::create_private_dir(&generation_dir)?;
    let control_path = generation_dir.join("control.sock");
    if control_path.as_os_str().as_encoded_bytes().len() >= 108 {
        return Err("Xray 代际控制 socket 路径超过 Linux sockaddr_un 上限".to_owned());
    }
    let manifest = HandoffManifestIdentity {
        version: 2,
        scope: "whole-generation".to_owned(),
        generation_id,
        control_path,
        listeners: endpoints,
    };
    let manifest_path = generation_dir.join("manifest.json");
    crate::fsutil::atomic_write_private(
        &manifest_path,
        &serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )?;
    Ok(BootstrapHandoff {
        manifest_path,
        manifest,
    })
}

fn new_bootstrap_endpoint(
    network: &str,
    role: &str,
    prepare: bool,
    address: String,
    depends_on: String,
) -> Result<HandoffEndpoint, String> {
    let bind_address = address.clone();
    Ok(HandoffEndpoint {
        address,
        bind_address,
        network: network.to_owned(),
        role: role.to_owned(),
        prepare,
        depends_on,
        fd: 0,
        socket_inode: 0,
        socket_cookie: 0,
        protocol: if network == "udp" {
            libc::IPPROTO_UDP
        } else {
            0
        },
        socket_config_sha256: String::new(),
    })
}

fn canonical_listener_address(listen: &str, port: u16) -> Result<String, String> {
    let ip = listen
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .map_err(|_| format!("Xray listener 地址不是明确 IP：{listen}"))?;
    Ok(SocketAddr::new(ip, port).to_string())
}

fn canonical_existing_socket_address(value: &str) -> Option<String> {
    value
        .parse::<SocketAddr>()
        .ok()
        .map(|address| address.to_string())
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_handoff_manifest_path(path: &Path) -> Result<(), String> {
    let mut parent = path
        .parent()
        .ok_or_else(|| "代际 manifest 没有父目录".to_owned())?;
    let mut exact_parent = true;
    loop {
        let metadata = fs::symlink_metadata(parent)
            .map_err(|error| format!("检查代际目录 {} 失败：{error}", parent.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!("代际目录 {} 不是可信目录", parent.display()));
        }
        let mode = metadata.mode() & 0o7777;
        if exact_parent {
            if metadata.uid() != unsafe { libc::geteuid() } || mode & 0o077 != 0 {
                return Err(format!(
                    "代际 manifest 目录 {} 必须由 Agent 账号持有且不向其他账号开放（当前 {:04o}）",
                    parent.display(),
                    mode & 0o7777
                ));
            }
            exact_parent = false;
        } else if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(format!(
                "代际 manifest 祖先目录 {} 可被其他账号写入且没有 sticky bit（当前 {:04o}）",
                parent.display(),
                mode
            ));
        }
        let Some(next) = parent.parent() else {
            break;
        };
        parent = next;
    }
    Ok(())
}

fn handoff_blocked_reason(status: &HandoffRuntimeStatus) -> String {
    if !status.ready {
        return "运行中的 Xray 监听组尚未完整就绪".to_owned();
    }
    if !status.retirement_supported {
        if !status.unsupported_reasons.is_empty() {
            return format!(
                "运行中的 Xray 仍有未纳入整代退役的监听：{}",
                status.unsupported_reasons.join(", ")
            );
        }
        return "运行中的 Xray 没有给出 whole-generation 退役能力".to_owned();
    }
    if !status.accepting {
        return "运行中的 Xray 当前已暂停接入，不能作为稳定服务代".to_owned();
    }
    "运行中的 Xray 没有给出完整整代退役证明".to_owned()
}

fn observe_serving_handoff(state_dir: &Path) -> Result<HandoffRuntimeStatus, String> {
    observe_serving_handoff_generation(state_dir).map(|(_, _, status)| status)
}

fn observe_serving_handoff_generation(
    state_dir: &Path,
) -> Result<
    (
        crate::ProcessRef,
        HandoffManifestIdentity,
        HandoffRuntimeStatus,
    ),
    String,
> {
    let process = crate::serving_xray_process_in(state_dir)
        .ok_or_else(|| "找不到当前配置对应的服务进程".to_owned())?;
    let (manifest, status) = observe_handoff_process(process)?;
    Ok((process, manifest, status))
}

fn observe_handoff_process(
    process: crate::ProcessRef,
) -> Result<(HandoffManifestIdentity, HandoffRuntimeStatus), String> {
    let environ = fs::read(format!("/proc/{}/environ", process.pid))
        .map_err(|error| format!("读取进程环境失败：{error}"))?;
    let raw_path = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(XRAY_HANDOFF_MANIFEST_ENV))
        .ok_or_else(|| "服务进程没有代际 manifest".to_owned())?;
    let manifest_path = PathBuf::from(
        std::str::from_utf8(raw_path).map_err(|_| "代际 manifest 路径不是 UTF-8".to_owned())?,
    );
    let manifest = read_handoff_manifest_identity(&manifest_path)?;
    if manifest.version != 2
        || manifest.generation_id.len() < 16
        || manifest.generation_id.len() > 128
    {
        return Err("代际 manifest 不是完整退役 v2".to_owned());
    }
    let status = handoff_status_rpc(&manifest, process.pid)?;
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| format!("读取 boot id 失败：{error}"))?;
    if status.scope != "whole-generation"
        || status.generation_id != manifest.generation_id
        || status.pid != process.pid
        || status.pid_start_ticks != process.start_ticks.to_string()
        || status.boot_id != boot_id.trim()
    {
        return Err("控制端状态与运行进程身份不一致".to_owned());
    }
    Ok((manifest, status))
}

fn read_handoff_manifest_identity(path: &Path) -> Result<HandoffManifestIdentity, String> {
    if !path.is_absolute() {
        return Err("代际 manifest 不是绝对路径".to_owned());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("打开代际 manifest 失败：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("读取代际 manifest 属性失败：{error}"))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_HANDOFF_MESSAGE_BYTES as u64
    {
        return Err("代际 manifest 权限或文件身份不安全".to_owned());
    }
    // from_reader(File) reads JSON byte by byte. The one-second guard checks several manifests,
    // so collect the already size-checked file in bounded batches before parsing it.
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_HANDOFF_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取代际 manifest 失败：{error}"))?;
    if bytes.len() as u64 != metadata.len() {
        return Err("代际 manifest 在读取期间发生变化".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("解析代际 manifest 失败：{error}"))
}

fn handoff_status_rpc(
    manifest: &HandoffManifestIdentity,
    expected_pid: i32,
) -> Result<HandoffRuntimeStatus, String> {
    handoff_rpc(manifest, expected_pid, "status", 0, "")
}

fn handoff_status_any(manifest: &HandoffManifestIdentity) -> Result<HandoffRuntimeStatus, String> {
    handoff_rpc_inner(manifest, None, "status", 0, "", false).map(|(status, _)| status)
}

fn handoff_rpc(
    manifest: &HandoffManifestIdentity,
    expected_pid: i32,
    operation: &str,
    epoch: u64,
    intent: &str,
) -> Result<HandoffRuntimeStatus, String> {
    handoff_rpc_inner(
        manifest,
        Some(expected_pid),
        operation,
        epoch,
        intent,
        false,
    )
    .map(|(status, _)| status)
}

fn handoff_export_rpc(
    manifest: &HandoffManifestIdentity,
    expected_pid: i32,
    epoch: u64,
    intent: &str,
) -> Result<(HandoffRuntimeStatus, Vec<OwnedFd>), String> {
    handoff_rpc_inner(manifest, Some(expected_pid), "export", epoch, intent, true)
}

fn drop_generation_udp(
    manifest: &HandoffManifestIdentity,
    expected_pid: i32,
    status: &HandoffRuntimeStatus,
    intent: &str,
) -> Result<HandoffRuntimeStatus, String> {
    if status.udp_dropped {
        return Ok(status.clone());
    }
    let epoch = status
        .epoch
        .checked_add(1)
        .ok_or("断开旧代 UDP 时 fence epoch 溢出")?;
    let dropped = handoff_rpc(manifest, expected_pid, "drop_udp", epoch, intent)?;
    if !dropped.udp_dropped {
        return Err("旧代没有确认 UDP 已断开".to_owned());
    }
    Ok(dropped)
}

fn handoff_rpc_inner(
    manifest: &HandoffManifestIdentity,
    expected_pid: Option<i32>,
    operation: &str,
    epoch: u64,
    intent: &str,
    receive_fds: bool,
) -> Result<(HandoffRuntimeStatus, Vec<OwnedFd>), String> {
    let path = manifest.control_path.as_os_str().as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if !manifest.control_path.is_absolute()
        || path.is_empty()
        || path.len() >= address.sun_path.len()
        || path.contains(&0)
    {
        return Err("代际控制 socket 路径无效".to_owned());
    }
    let metadata = fs::symlink_metadata(&manifest.control_path)
        .map_err(|error| format!("读取代际控制 socket 失败：{error}"))?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("代际控制端不是私有同账号 socket".to_owned());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(path) {
        *slot = *byte as libc::c_char;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!(
            "创建代际控制连接失败：{}",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let address_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as libc::socklen_t;
    // SO_SNDTIMEO also bounds a blocking Unix connect when a frozen control server's
    // accept backlog is full. Installing it after connect leaves every recovery worker stuck.
    let timeout = libc::timeval {
        tv_sec: 2,
        tv_usec: 0,
    };
    for option in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of_val(&timeout) as libc::socklen_t,
            )
        } != 0
        {
            return Err(format!(
                "设置代际控制超时失败：{}",
                io::Error::last_os_error()
            ));
        }
    }
    if unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &address as *const _ as *const libc::sockaddr,
            address_len,
        )
    } != 0
    {
        return Err(format!(
            "代际控制 {operation} 连接失败（PID {:?}，超时上限 2 秒）：{}",
            expected_pid,
            io::Error::last_os_error()
        ));
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut peer as *mut _ as *mut libc::c_void,
            &mut peer_len,
        )
    } != 0
        || peer_len as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != unsafe { libc::geteuid() }
        || expected_pid.is_some_and(|expected| peer.pid != expected)
    {
        return Err("代际控制端 PID/UID 身份不一致".to_owned());
    }
    let request = serde_json::json!({
        "operation": operation,
        "generation_id": manifest.generation_id,
        "epoch": epoch,
        "intent": intent
    });
    let request = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    let sent = unsafe {
        libc::send(
            fd.as_raw_fd(),
            request.as_ptr().cast(),
            request.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if sent != request.len() as isize {
        return Err(format!(
            "发送代际状态请求失败：{}",
            io::Error::last_os_error()
        ));
    }
    let mut response = vec![0u8; MAX_HANDOFF_MESSAGE_BYTES + 1];
    let mut iovec = libc::iovec {
        iov_base: response.as_mut_ptr().cast(),
        iov_len: response.len(),
    };
    let rights_bytes = std::mem::size_of::<i32>() * 512;
    let mut control = if receive_fds {
        vec![0_u8; unsafe { libc::CMSG_SPACE(rights_bytes as u32) } as usize]
    } else {
        Vec::new()
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    if receive_fds {
        message.msg_control = control.as_mut_ptr().cast();
        // libc models this field as usize on glibc and socklen_t/u32 on musl. The checked
        // conversion is intentionally retained even though it is an identity conversion on
        // the development host.
        #[allow(clippy::useless_conversion)]
        let control_bytes = control
            .len()
            .try_into()
            .map_err(|_| "代际控制辅助数据缓冲区长度超出平台上限".to_owned())?;
        message.msg_controllen = control_bytes;
    }
    let received = unsafe {
        libc::recvmsg(
            fd.as_raw_fd(),
            &mut message,
            if receive_fds {
                libc::MSG_CMSG_CLOEXEC
            } else {
                0
            },
        )
    };
    if received < 0 {
        return Err(format!(
            "代际控制 {operation} 等待响应失败（PID {}，超时上限 2 秒）：{}",
            peer.pid,
            io::Error::last_os_error()
        ));
    }
    if received == 0 {
        return Err(format!(
            "代际控制 {operation} 响应提前关闭（PID {}）",
            peer.pid
        ));
    }
    if received as usize > MAX_HANDOFF_MESSAGE_BYTES {
        return Err(format!(
            "代际控制 {operation} 响应超出大小上限（PID {}）",
            peer.pid
        ));
    }
    if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err("代际控制响应或监听 FD 被截断".to_owned());
    }
    let mut received_fds = Vec::new();
    if receive_fds {
        let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        while !header.is_null() {
            let header_ref = unsafe { &*header };
            if header_ref.cmsg_level != libc::SOL_SOCKET || header_ref.cmsg_type != libc::SCM_RIGHTS
            {
                return Err("代际控制端返回了非 SCM_RIGHTS 辅助数据".to_owned());
            }
            // glibc exposes cmsg_len as usize, while musl exposes it as socklen_t/u32. Convert
            // once at the trust boundary so the length checks are identical on both release
            // targets and no narrowing arithmetic happens below.
            #[allow(clippy::useless_conversion)]
            let message_bytes = usize::try_from(header_ref.cmsg_len)
                .map_err(|_| "代际控制端返回的 SCM_RIGHTS 长度超出平台上限".to_owned())?;
            let header_bytes = unsafe { libc::CMSG_LEN(0) } as usize;
            if message_bytes < header_bytes
                || !(message_bytes - header_bytes).is_multiple_of(std::mem::size_of::<i32>())
            {
                return Err("代际控制端返回的 SCM_RIGHTS 长度无效".to_owned());
            }
            let count = (message_bytes - header_bytes) / std::mem::size_of::<i32>();
            let values =
                unsafe { std::slice::from_raw_parts(libc::CMSG_DATA(header).cast::<i32>(), count) };
            for raw in values {
                if *raw < 3 || received_fds.len() >= 512 {
                    return Err("代际控制端返回的监听 FD 无效或过多".to_owned());
                }
                received_fds.push(unsafe { OwnedFd::from_raw_fd(*raw) });
            }
            header = unsafe { libc::CMSG_NXTHDR(&message, header) };
        }
    }
    response.truncate(received as usize);
    let response: HandoffRuntimeResponse =
        serde_json::from_slice(&response).map_err(|error| format!("解析代际状态失败：{error}"))?;
    if !response.error.is_empty() {
        return Err(format!("代际控制端拒绝状态请求：{}", response.error));
    }
    let status = response
        .status
        .ok_or_else(|| "代际状态响应为空".to_owned())?;
    if status.pid != peer.pid {
        return Err("代际状态响应 PID 与控制端 peer 不一致".to_owned());
    }
    if receive_fds && received_fds.is_empty() {
        return Err("代际 export 没有返回监听 FD".to_owned());
    }
    if !receive_fds && !received_fds.is_empty() {
        return Err("非 export 代际响应携带了监听 FD".to_owned());
    }
    Ok((status, received_fds))
}

pub(crate) fn activate_bootstrap_handoff(
    bootstrap: &BootstrapHandoff,
    expected_pid: i32,
) -> Result<(), String> {
    let observed = handoff_status_rpc(&bootstrap.manifest, expected_pid)?;
    if observed.generation_id != bootstrap.manifest.generation_id || observed.pid != expected_pid {
        return Err("首次接管控制状态与候选进程身份不一致".to_owned());
    }
    let seal_epoch = observed
        .epoch
        .checked_add(1)
        .ok_or("首次接管 fence epoch 溢出")?;
    let sealed = handoff_rpc(
        &bootstrap.manifest,
        expected_pid,
        "seal",
        seal_epoch,
        &format!("bootstrap-seal-{}", random_token()?),
    )?;
    if !sealed.ready || !sealed.retirement_supported || !sealed.paused || sealed.accepting {
        return Err(format!(
            "首次接管 seal 后状态不安全：ready={} retirement_supported={} paused={} accepting={}",
            sealed.ready, sealed.retirement_supported, sealed.paused, sealed.accepting
        ));
    }
    let resume_epoch = sealed
        .epoch
        .checked_add(1)
        .ok_or("首次接管 fence epoch 溢出")?;
    let resumed = handoff_rpc(
        &bootstrap.manifest,
        expected_pid,
        "resume",
        resume_epoch,
        &format!("bootstrap-resume-{}", random_token()?),
    )?;
    if !resumed.ready || !resumed.retirement_supported || !resumed.accepting {
        return Err(format!(
            "首次接管 resume 后没有接入：ready={} retirement_supported={} accepting={}",
            resumed.ready, resumed.retirement_supported, resumed.accepting
        ));
    }
    Ok(())
}

pub(crate) fn serving_handoff_has_udp_listener(
    state_dir: &Path,
    listen: &str,
    port: u16,
) -> Option<bool> {
    let wanted = canonical_listener_address(listen, port).ok()?;
    let statuses = crate::serving_xray_processes_in(state_dir)
        .into_iter()
        .filter_map(|process| {
            observe_handoff_process(process)
                .ok()
                .map(|(_, status)| status)
        })
        .collect::<Vec<_>>();
    (!statuses.is_empty()).then(|| any_handoff_generation_serves_udp(&statuses, &wanted))
}

fn any_handoff_generation_serves_udp(statuses: &[HandoffRuntimeStatus], wanted: &str) -> bool {
    statuses.iter().any(|status| {
        status.ready
            && status.accepting
            && status
                .listeners
                .iter()
                .any(|listener| handoff_listener_is_serving_udp(listener, wanted))
    })
}

fn handoff_listener_is_serving_udp(listener: &HandoffListenerStatus, wanted: &str) -> bool {
    listener.network == "udp"
        && listener.role == "public"
        && listener.address == wanted
        && !listener.closed
        && listener
            .bind_address
            .parse::<SocketAddr>()
            .is_ok_and(|bind| bind.port() != 0)
}

#[derive(Debug)]
struct CandidateHandoff {
    manifest: HandoffManifestIdentity,
    manifest_path: PathBuf,
    executable: PathBuf,
}

const HANDOFF_CHILD_FD_MIN: i32 = 64;

fn relocate_handoff_descriptors(exported: &[OwnedFd]) -> Result<Vec<OwnedFd>, String> {
    let mut relocated = Vec::with_capacity(exported.len());
    for descriptor in exported {
        // The candidate is launched through a small shell pipeline so its stderr can use the
        // bounded log sink. Shells are allowed to allocate low-numbered descriptors while
        // constructing that pipeline; an SCM_RIGHTS result at fd 9/10 can otherwise be silently
        // replaced before exec. Duplicating the same open file description into a reserved high
        // range preserves the listener inode/cookie while keeping it away from shell plumbing.
        let raw = unsafe {
            libc::fcntl(
                descriptor.as_raw_fd(),
                libc::F_DUPFD_CLOEXEC,
                HANDOFF_CHILD_FD_MIN,
            )
        };
        if raw < 0 {
            return Err(format!(
                "迁移候选 listener FD 到保留区失败：{}",
                io::Error::last_os_error()
            ));
        }
        relocated.push(unsafe { OwnedFd::from_raw_fd(raw) });
    }
    Ok(relocated)
}

fn prepare_candidate_handoff(
    state_dir: &Path,
    candidate: &Path,
    old: &HandoffRuntimeStatus,
    exported: &[OwnedFd],
) -> Result<CandidateHandoff, String> {
    prepare_candidate_handoff_for_listeners(
        state_dir,
        candidate,
        old,
        exported,
        None,
        &BTreeSet::new(),
    )
}

fn prepare_candidate_handoff_for_listeners(
    state_dir: &Path,
    candidate: &Path,
    old: &HandoffRuntimeStatus,
    exported: &[OwnedFd],
    desired: Option<Vec<HandoffEndpoint>>,
    fresh_tcp: &BTreeSet<String>,
) -> Result<CandidateHandoff, String> {
    let public_active = old
        .listeners
        .iter()
        .filter(|listener| listener.role == "public")
        .try_fold(0_u64, |total, listener| total.checked_add(listener.active))
        .ok_or("旧代 public active 计数溢出")?;
    let cover_active = old
        .listeners
        .iter()
        .filter(|listener| listener.role == "cover")
        .try_fold(0_u64, |total, listener| total.checked_add(listener.active))
        .ok_or("旧代 cover active 计数溢出")?;
    if public_active != old.public_active
        || cover_active != old.cover_active
        || public_active.checked_add(cover_active) != Some(old.active)
    {
        return Err("旧代 listener active 汇总与代际状态不一致".to_owned());
    }
    let tcp = old
        .listeners
        .iter()
        .filter(|listener| listener.network == "tcp")
        .collect::<Vec<_>>();
    if tcp.len() != exported.len() || tcp.is_empty() {
        return Err(format!(
            "旧代 export 返回 {} 个 FD，但状态声明 {} 个 TCP listener",
            exported.len(),
            tcp.len()
        ));
    }
    let generation_id = format!("brocade-{}", random_token()?);
    let directory = state_dir.join(HANDOFF_DIR).join(&generation_id);
    crate::create_private_dir(&directory)?;
    let executable = directory.join("xray");
    copy_atomic(candidate, &executable, 0o755)?;
    let control_path = directory.join("control.sock");
    if control_path.as_os_str().as_encoded_bytes().len() >= 108 {
        return Err("候选 Xray 代际控制 socket 路径超过 Linux 上限".to_owned());
    }
    let mut descriptors = exported.iter();
    let mut listeners = Vec::with_capacity(old.listeners.len());
    for listener in &old.listeners {
        let mut endpoint = HandoffEndpoint {
            address: listener.address.clone(),
            bind_address: listener.bind_address.clone(),
            network: listener.network.clone(),
            role: listener.role.clone(),
            prepare: listener.role == "control",
            depends_on: listener.depends_on.clone(),
            fd: 0,
            socket_inode: 0,
            socket_cookie: 0,
            protocol: listener.protocol,
            socket_config_sha256: String::new(),
        };
        match listener.network.as_str() {
            "tcp" => {
                let descriptor = descriptors
                    .next()
                    .ok_or("候选 TCP listener 缺少导出的 FD")?;
                if listener.socket_inode == 0
                    || listener.socket_cookie == 0
                    || !matches!(listener.protocol, libc::IPPROTO_TCP | libc::IPPROTO_MPTCP)
                    || !valid_sha256(&listener.socket_config_sha256)
                {
                    return Err("旧代 TCP listener 身份不完整".to_owned());
                }
                endpoint.fd = descriptor.as_raw_fd();
                endpoint.socket_inode = listener.socket_inode;
                endpoint.socket_cookie = listener.socket_cookie;
                endpoint.socket_config_sha256 = listener.socket_config_sha256.clone();
            }
            "udp" => {
                endpoint.bind_address = listener.address.clone();
                endpoint.protocol = libc::IPPROTO_UDP;
            }
            _ => return Err("旧代声明了不支持的 listener network".to_owned()),
        }
        listeners.push(endpoint);
    }
    if descriptors.next().is_some() {
        return Err("旧代 export 返回了多余的监听 FD".to_owned());
    }
    if let Some(desired) = desired {
        // Retain the desired role/dependency and only borrow the old kernel identity. Removed
        // listeners are intentionally absent; requiring them to register would prevent seal.
        listeners = desired
            .into_iter()
            .map(|mut endpoint| {
                if endpoint.network == "tcp" && !fresh_tcp.contains(&endpoint.address) {
                    if let Some(inherited) = listeners.iter().find(|old| {
                        old.network == endpoint.network && old.address == endpoint.address
                    }) {
                        endpoint.bind_address = inherited.bind_address.clone();
                        endpoint.fd = inherited.fd;
                        endpoint.socket_inode = inherited.socket_inode;
                        endpoint.socket_cookie = inherited.socket_cookie;
                        endpoint.protocol = inherited.protocol;
                        endpoint.socket_config_sha256 = inherited.socket_config_sha256.clone();
                    }
                }
                endpoint
            })
            .collect();
    }
    let manifest = HandoffManifestIdentity {
        version: 2,
        scope: "whole-generation".to_owned(),
        generation_id,
        control_path,
        listeners,
    };
    let manifest_path = directory.join("manifest.json");
    crate::fsutil::atomic_write_private(
        &manifest_path,
        &serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )?;
    Ok(CandidateHandoff {
        manifest,
        manifest_path,
        executable,
    })
}

fn launch_handoff_candidate(
    state_dir: &Path,
    candidate: &CandidateHandoff,
    exported: &[OwnedFd],
) -> Result<(HandoffRuntimeStatus, crate::ProcessRef), String> {
    let config = state_dir.join("xray.json");
    let content = fs::read_to_string(&config)
        .map_err(|error| format!("读取候选 splice 配置失败：{error}"))?;
    let splice_mode = if crate::xray_needs_splice_disabled(
        &content,
        crate::xray_supports_secure_dokodemo_splice(),
    )? {
        "disable"
    } else {
        "auto"
    };
    let accounting = fs::canonicalize(crate::accounting::journal_path(state_dir))
        .map_err(|error| format!("resolve accounting journal: {error}"))?;
    let accounting_dir = accounting
        .parent()
        .ok_or("accounting directory is absent")?;
    let log_dir = state_dir.join("logs");
    crate::create_private_dir(&log_dir)?;
    let log_path = log_dir.join(format!("xray-{}.log", candidate.manifest.generation_id));
    let sink = crate::logcap::command(&log_path, state_dir, crate::logcap::WorkloadLog::Xray)?;
    // Spawn the log sink while every listener FD is still CLOEXEC. A shell pipeline would leak
    // those FDs into both its wait shell and the sink, keeping removed ports bound during drain.
    let mut sink_process = Command::new("sh")
        .args(["-c", &format!("exec {sink}")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动候选日志进程失败：{error}"))?;
    let output = match sink_process.stdin.take() {
        Some(output) => output,
        None => {
            let _ = sink_process.kill();
            let _ = sink_process.wait();
            return Err("候选日志进程缺少输入管道".to_owned());
        }
    };
    let error_output = match output.as_fd().try_clone_to_owned() {
        Ok(output) => output,
        Err(error) => {
            drop(output);
            let _ = sink_process.kill();
            let _ = sink_process.wait();
            return Err(format!("复制候选日志管道失败：{error}"));
        }
    };
    let inherited = exported
        .iter()
        .filter(|descriptor| {
            candidate
                .manifest
                .listeners
                .iter()
                .any(|endpoint| endpoint.fd == descriptor.as_raw_fd())
        })
        .map(AsRawFd::as_raw_fd)
        .collect::<Vec<_>>();
    crate::user_traffic::verify_executable(accounting_dir, &candidate.executable)?;
    crate::source_access::verify_executable(accounting_dir, &candidate.executable)?;
    let mut command = Command::new(&candidate.executable);
    command
        .args(["run", "-config"])
        .arg(&config)
        .envs(crate::xray_launch_environment(splice_mode))
        .env("XRAY_BROCADE_ACCOUNTING_FILE", &accounting)
        .env("XRAY_BROCADE_ACCOUNTING_SHARED", "join")
        .env(
            "XRAY_BROCADE_USER_LIMIT_FILE",
            crate::user_traffic::control_path(accounting_dir),
        )
        .env(
            crate::source_access::ENV,
            crate::source_access::policy_path(accounting_dir),
        )
        .env(
            "XRAY_BROCADE_MACHINE_LIMIT_FILE",
            crate::traffic_limit_budget::control_path(accounting_dir),
        )
        .env(
            "XRAY_BROCADE_MACHINE_LIMIT_REQUIRED",
            u8::from(crate::traffic_limit_budget::required_at_start(
                accounting_dir,
            ))
            .to_string(),
        )
        .env(
            "XRAY_BROCADE_SNIFFING_FAILED_CONTROL_FILE",
            crate::other_domain::failed_control_path(accounting_dir),
        )
        .env(
            "XRAY_BROCADE_OTHER_DOMAIN_CONTROL_FILE",
            crate::other_domain::control_path(accounting_dir),
        )
        .env(
            "XRAY_BROCADE_SITE_USAGE_CONTROL_FILE",
            crate::site_usage::control_path(accounting_dir),
        )
        .env(
            "XRAY_BROCADE_SITE_ACCOUNTING_FILE",
            crate::accounting::site_journal_path(accounting_dir),
        )
        .env("XRAY_TCP_HANDOFF_MANIFEST", &candidate.manifest_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error_output));
    // SAFETY: Only async-signal-safe syscalls run between fork and exec. The owned exported FDs
    // stay live through spawn; clearing CLOEXEC happens only in this Xray child, never the Agent.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            for fd in &inherited {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let spawn = command.spawn();
    drop(command);
    let process = match spawn {
        Ok(process) => process,
        Err(error) => {
            let _ = sink_process.kill();
            let _ = sink_process.wait();
            return Err(format!("启动候选 Xray 失败：{error}"));
        }
    };
    let spawned_process = crate::process_ref(process.id() as i32);
    let children = Arc::new(Mutex::new(Some((process, sink_process))));
    let reaper_children = Arc::clone(&children);
    if let Err(error) = thread::Builder::new()
        .name("xray-child-reaper".to_owned())
        .spawn(move || {
            if let Some((mut process, mut sink)) = reaper_children
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = process.wait();
                let _ = sink.wait();
            }
        })
    {
        if let Some((mut process, mut sink)) = children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = process.kill();
            let _ = process.wait();
            let _ = sink.kill();
            let _ = sink.wait();
        }
        return Err(format!("启动候选进程回收器失败：{error}"));
    }

    let observation = (|| {
        let deadline = Instant::now() + HANDOFF_CANDIDATE_START_TIMEOUT;
        loop {
            if let Some(error) = startup::root_failure(&candidate.manifest, spawned_process) {
                return Err(error);
            }
            match handoff_status_any(&candidate.manifest) {
                Ok(status) => {
                    let ticks = status
                        .pid_start_ticks
                        .parse::<u64>()
                        .map_err(|_| "候选 Xray start ticks 无效".to_owned())?;
                    let process = crate::process_ref(status.pid)
                        .filter(|process| process.start_ticks == ticks)
                        .ok_or("候选 Xray 控制端对应进程已经退出")?;
                    let digest =
                        file_sha256_hex(&PathBuf::from(format!("/proc/{}/exe", process.pid)))?;
                    let expected = file_sha256_hex(&candidate.executable)?;
                    if digest != expected {
                        return Err("候选 Xray 控制端的进程摘要不符".to_owned());
                    }
                    return Ok((status, process));
                }
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    thread::sleep(STARTUP_HEALTH_INTERVAL);
                }
                Err(error) => {
                    return Err(xray_startup_failure(
                        &log_path,
                        &format!("候选 Xray 控制端没有就绪：{error}"),
                        Some(&candidate.manifest_path),
                        spawned_process,
                    ));
                }
            }
        }
    })();
    let observation = observation.map_err(|error| {
        startup::root_failure(&candidate.manifest, spawned_process).unwrap_or(error)
    });
    if let Err(error) = &observation {
        // This child cannot admit business traffic before the caller's fenced resume. Reap a
        // failed startup as well, even when it never created its private control socket.
        if let Some(process) = spawned_process {
            crate::terminate_processes(&[process], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)
                .map_err(|cleanup| format!("{error}；清理候选进程失败：{cleanup}"))?;
        }
    }
    observation
}

fn upgrade_process(
    status: &HandoffRuntimeStatus,
    sha256: String,
) -> Result<XrayUpgradeProcess, String> {
    Ok(XrayUpgradeProcess {
        pid: status.pid,
        pid_start_ticks: status
            .pid_start_ticks
            .parse()
            .map_err(|_| "Xray 代际 start ticks 无效".to_owned())?,
        boot_id: status.boot_id.clone(),
        sha256,
        alive: true,
        accepting: Some(status.accepting),
        active_connections: Some(status.active),
    })
}

fn write_upgrade_runtime(state_dir: &Path, runtime: &XrayUpgradeRuntime) -> Result<(), String> {
    if !valid_upgrade_runtime(runtime) {
        return Err("拒绝写入无效的 Xray 双进程状态".to_owned());
    }
    crate::fsutil::atomic_write_private(
        &state_dir.join(UPGRADE_RUNTIME_FILE),
        &serde_json::to_vec(runtime).map_err(|error| error.to_string())?,
    )
}

fn valid_upgrade_runtime(runtime: &XrayUpgradeRuntime) -> bool {
    valid_upgrade_source(runtime)
        && valid_upgrade_process(&runtime.old)
        && runtime.candidate.as_ref().is_none_or(valid_upgrade_process)
        && runtime
            .candidate
            .as_ref()
            .is_none_or(|candidate| candidate.pid != runtime.old.pid)
        && runtime
            .blocked_reason
            .as_ref()
            .is_none_or(|reason| reason.chars().count() <= 4_000)
}

fn valid_upgrade_source(runtime: &XrayUpgradeRuntime) -> bool {
    match &runtime.source {
        XrayUpgradeSource::BinaryRelease => runtime.release_id > 0 && runtime.attempt > 0,
        XrayUpgradeSource::ConfigDeployment {
            deployment_id,
            claim_generation: _,
        } => {
            *deployment_id > 0
                && runtime.release_id == deployment_id.saturating_neg()
                && runtime.attempt == 0
        }
    }
}

fn config_handoff_source(runtime: &XrayUpgradeRuntime) -> Option<(i64, u64)> {
    match &runtime.source {
        XrayUpgradeSource::ConfigDeployment {
            deployment_id,
            claim_generation,
        } => Some((*deployment_id, *claim_generation)),
        XrayUpgradeSource::BinaryRelease => None,
    }
}

fn valid_upgrade_process(process: &XrayUpgradeProcess) -> bool {
    process.pid > 1
        && process.pid_start_ticks > 0
        && valid_boot_id(&process.boot_id)
        && valid_sha256(&process.sha256)
}

fn valid_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn refresh_upgrade_process(process: &mut XrayUpgradeProcess) {
    process.alive = false;
    let Ok(boot_id) = fs::read_to_string("/proc/sys/kernel/random/boot_id") else {
        return;
    };
    if boot_id.trim() != process.boot_id {
        return;
    }
    let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", process.pid)) else {
        return;
    };
    let Some(after_comm) = stat.rsplit_once(") ").map(|(_, value)| value) else {
        return;
    };
    let fields = after_comm.split_whitespace().collect::<Vec<_>>();
    let Some(state) = fields.first().and_then(|value| value.chars().next()) else {
        return;
    };
    let Some(start_ticks) = fields.get(19).and_then(|value| value.parse::<u64>().ok()) else {
        return;
    };
    if state == 'Z' || start_ticks != process.pid_start_ticks {
        return;
    }
    // The digest is part of the frozen process identity, not an informational field to refresh.
    // A matching PID/start-time that now points at different executable bytes must remain dead
    // to this release attempt, just like PID reuse.
    // The one-second guard revisits both generations; unchanged executable identities can
    // reuse their verified digest without reading both binaries again on every pass.
    let Ok(actual) =
        crate::cached_xray_sha256(&PathBuf::from(format!("/proc/{}/exe", process.pid)))
    else {
        return;
    };
    process.alive = actual == process.sha256;
}

fn exact_upgrade_process(process: &XrayUpgradeProcess) -> Option<crate::ProcessRef> {
    if fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()?
        .trim()
        != process.boot_id
    {
        return None;
    }
    let current = crate::process_ref(process.pid)?;
    if current.start_ticks != process.pid_start_ticks {
        return None;
    }
    let digest =
        crate::cached_xray_sha256(&PathBuf::from(format!("/proc/{}/exe", current.pid))).ok()?;
    (digest == process.sha256).then_some(current)
}

fn discover_interrupted_candidate(
    state_dir: &Path,
    old: crate::ProcessRef,
) -> Result<
    Option<(
        XrayUpgradeProcess,
        HandoffManifestIdentity,
        HandoffRuntimeStatus,
    )>,
    String,
> {
    let root = state_dir.join(HANDOFF_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("扫描 Xray 代际目录失败：{error}")),
    };
    let mut found = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path().join("manifest.json");
        let Ok(manifest) = read_handoff_manifest_identity(&path) else {
            continue;
        };
        let Ok(status) = handoff_status_any(&manifest) else {
            continue;
        };
        if status.pid == old.pid {
            continue;
        }
        let Some(process) = crate::process_ref(status.pid) else {
            continue;
        };
        if status.pid_start_ticks != process.start_ticks.to_string() {
            continue;
        }
        let expected_executable = path.parent().unwrap_or(&root).join("xray");
        let expected = match fs::canonicalize(&expected_executable) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let actual = match fs::canonicalize(format!("/proc/{}/exe", process.pid)) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if actual != expected {
            continue;
        }
        let digest = file_sha256_hex(&actual)?;
        found.push((upgrade_process(&status, digest)?, manifest, status));
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        count => Err(format!(
            "发现 {count} 个未登记的 Xray 候选代，拒绝猜测服务身份"
        )),
    }
}

fn interrupted_handoff_runtime(state_dir: &Path) -> Result<Option<XrayUpgradeRuntime>, String> {
    let Some(mut runtime) = read_upgrade_runtime(state_dir)? else {
        return Ok(None);
    };
    if runtime.candidate.is_none() {
        if let Some(old) = exact_upgrade_process(&runtime.old) {
            if let Some((candidate, _, _)) = discover_interrupted_candidate(state_dir, old)? {
                runtime.candidate = Some(candidate);
                write_upgrade_runtime(state_dir, &runtime)?;
            }
        }
    }
    Ok(Some(runtime))
}

fn config_force_retirement_authorized(
    options: &Options,
    runtime: &XrayUpgradeRuntime,
) -> Result<bool, String> {
    let XrayUpgradeSource::ConfigDeployment {
        deployment_id,
        claim_generation,
    } = &runtime.source
    else {
        return Ok(false);
    };
    if !matches!(
        runtime.phase,
        XrayUpgradePhase::Draining | XrayUpgradePhase::Blocked
    ) {
        return Ok(false);
    }
    let client = HttpClient::new(&options.server)?;
    let response = client.request("POST", "/agent/v1/xray-handoff/force", &options.token, None)?;
    if response.status == 204 {
        return Ok(false);
    }
    if response.status != 200 {
        return Err(format!(
            "Xray 配置交接强制退役授权查询失败：HTTP {} {}",
            response.status, response.body
        ));
    }
    let command: ConfigXrayForceRetirementCommand = serde_json::from_str(&response.body)
        .map_err(|error| format!("Xray 配置交接强制退役授权无效：{error}"))?;
    if (command.deployment_id, command.claim_generation) != (*deployment_id, *claim_generation) {
        return Err(format!(
            "拒绝不属于当前配置交接的强制退役授权：当前 {deployment_id}/{claim_generation}，收到 {}/{}",
            command.deployment_id, command.claim_generation
        ));
    }
    Ok(true)
}

fn should_force_handoff_retirement(
    runtime: &XrayUpgradeRuntime,
    deadline_expired: bool,
    control_plane_authorized: bool,
) -> bool {
    // A newer executable configuration work order is already authorized by its release. Do not
    // keep it blocked behind the preceding generation's remaining grace window.
    if control_plane_authorized {
        return true;
    }
    if !deadline_expired {
        return false;
    }
    runtime.strategy == XrayReleaseStrategy::HandoffForce
        // Configuration releases use availability-first retirement. Include durable runtimes
        // written by protocol-v32 Agents: after this Agent is installed, an expired legacy
        // config drain must be retired before another config generation can be claimed.
        || matches!(&runtime.source, XrayUpgradeSource::ConfigDeployment { .. })
}

fn status_for_upgrade_process(
    process: &XrayUpgradeProcess,
) -> Result<
    Option<(
        crate::ProcessRef,
        HandoffManifestIdentity,
        HandoffRuntimeStatus,
    )>,
    String,
> {
    let Some(process_ref) = exact_upgrade_process(process) else {
        return Ok(None);
    };
    let (manifest, status) = observe_handoff_process(process_ref)?;
    Ok(Some((process_ref, manifest, status)))
}

type ObservedGeneration = (
    crate::ProcessRef,
    HandoffManifestIdentity,
    HandoffRuntimeStatus,
);

fn observe_upgrade_generations(
    runtime: &XrayUpgradeRuntime,
) -> Result<(Option<ObservedGeneration>, Option<ObservedGeneration>), String> {
    let old = status_for_upgrade_process(&runtime.old)
        .map_err(|error| format!("旧代 PID {} 控制失联：{error}", runtime.old.pid))?;
    let candidate = runtime
        .candidate
        .as_ref()
        .map(|candidate| {
            status_for_upgrade_process(candidate)
                .map_err(|error| format!("候选代 PID {} 控制失联：{error}", candidate.pid))
        })
        .transpose()?
        .flatten();
    Ok((old, candidate))
}

fn recover_unresponsive_handoff(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    cause: &str,
) -> Result<bool, String> {
    // A lost old control channel must not restart a healthy admitted candidate. Persist the
    // retirement decision first so interruption after SIGKILL can complete through the normal
    // single-candidate path. The accounting journal remains shared until promotion.
    if runtime.old.accepting == Some(false) {
        if let Some((candidate, manifest, status)) = runtime
            .candidate
            .as_ref()
            .map(status_for_upgrade_process)
            .transpose()
            .ok()
            .flatten()
            .flatten()
        {
            if status.ready && status.accepting && status.retirement_supported {
                candidate_grants_for_recovery(state_dir, runtime)?;
                crate::accounting::handoff_journal_epochs(state_dir)?;
                let options = Options {
                    command: "repair".to_owned(),
                    server: String::new(),
                    token: String::new(),
                    state_dir: state_dir.to_owned(),
                    apply_mode: ApplyMode::Linux,
                    vpngate_stats_window: Duration::from_secs(900),
                };
                crate::collect_usage_report(&options)?;
                let mut retiring = runtime.clone();
                retiring.phase = XrayUpgradePhase::Forcing;
                retiring.blocked_reason = Some(format!(
                    "旧代控制失联，强制退役旧代并保留已接流候选：{cause}"
                ));
                write_upgrade_runtime(state_dir, &retiring)?;
                if let Some(old) = exact_upgrade_process(&runtime.old) {
                    crate::terminate_processes(
                        &[old],
                        crate::XRAY_TERM_GRACE,
                        crate::XRAY_KILL_GRACE,
                    )?;
                }
                let old = crate::ProcessRef {
                    pid: runtime.old.pid,
                    start_ticks: runtime.old.pid_start_ticks,
                };
                finalize_recovered_candidate(
                    &options,
                    &retiring,
                    old,
                    candidate,
                    &manifest,
                    runtime.strategy == XrayReleaseStrategy::HandoffForce,
                )?;
                crate::record_local_reconcile(
                    state_dir,
                    &[format!(
                        "Xray 旧代控制失联，已退役旧代，候选继续服务：{cause}"
                    )],
                    None,
                )?;
                return Ok(true);
            }
        }
    }
    crash_recovery::begin_unresponsive(state_dir, runtime, cause)
}

/// Restore one accepting generation before ordinary guard logic is allowed to consider a
/// restart. A lost candidate after UDP teardown enters durable crash recovery; healthy handoffs
/// continue to pause only the old generation and preserve its draining TCP sessions.
pub(crate) fn guard_interrupted_handoff(state_dir: &Path) -> Result<bool, String> {
    if crash_recovery::resume(state_dir)? {
        return Ok(true);
    }
    let Some(runtime) = interrupted_handoff_runtime(state_dir)? else {
        return Ok(false);
    };
    let (old, candidate) = match observe_upgrade_generations(&runtime) {
        Ok(generations) => generations,
        Err(error) => {
            return recover_unresponsive_handoff(state_dir, &runtime, &error);
        }
    };

    if let Some((_, candidate_manifest, candidate_status)) = &candidate {
        if candidate_status.ready
            && candidate_status.retirement_supported
            && candidate_status.accepting
        {
            if let Some((old_process, old_manifest, old_status)) = &old {
                let paused = if old_status.accepting {
                    let epoch = old_status
                        .epoch
                        .checked_add(1)
                        .ok_or("恢复旧代 pause 时 fence epoch 溢出")?;
                    handoff_rpc(
                        old_manifest,
                        old_process.pid,
                        "pause",
                        epoch,
                        &format!("recover-{}-pause-old", runtime.release_id),
                    )?
                } else {
                    old_status.clone()
                };
                let udp_dropped = drop_generation_udp(
                    old_manifest,
                    old_process.pid,
                    &paused,
                    &format!("recover-{}-drop-old-udp", runtime.release_id),
                )?;
                drop_config_tcp_admission(
                    state_dir,
                    &runtime,
                    old_manifest,
                    old_process.pid,
                    &udp_dropped,
                )?;
            }
            commit_config_handoff_state(state_dir, &runtime)?;
            return Ok(true);
        }
        if old.is_none() && candidate_status.ready && candidate_status.retirement_supported {
            // A sealed candidate already passed runtime recovery, but a configuration attempt
            // must still have its frozen authorization checkpoint before reopening admission.
            candidate_grants_for_recovery(state_dir, &runtime)?;
            let epoch = candidate_status
                .epoch
                .checked_add(1)
                .ok_or("恢复候选 admission 时 fence epoch 溢出")?;
            let resumed = handoff_rpc(
                candidate_manifest,
                candidate_status.pid,
                "resume",
                epoch,
                &format!("recover-{}-resume-candidate", runtime.release_id),
            )?;
            if !resumed.accepting {
                return Err("Agent 恢复后候选代没有重新接入".to_owned());
            }
            return Ok(true);
        }
    }
    if let Some((old_process, old_manifest, old_status)) = old {
        if old_status.udp_dropped {
            return crash_recovery::begin(state_dir, &runtime);
        }
        if !old_status.accepting {
            let epoch = old_status
                .epoch
                .checked_add(1)
                .ok_or("恢复旧代 admission 时 fence epoch 溢出")?;
            let resumed = handoff_rpc(
                &old_manifest,
                old_process.pid,
                "resume",
                epoch,
                &format!("recover-{}-resume-old", runtime.release_id),
            )?;
            if !resumed.accepting {
                return Err("Agent 恢复后旧代没有重新接入".to_owned());
            }
        }
        return Ok(true);
    }
    if candidate.is_some() {
        return Err("候选代仍存活但不能恢复接入，拒绝破坏性重启".to_owned());
    }
    if runtime.candidate.is_some() {
        return crash_recovery::begin(state_dir, &runtime);
    }
    Ok(false)
}

fn restore_old_executable_from_state(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
) -> Result<(), String> {
    if !matches!(&runtime.source, XrayUpgradeSource::BinaryRelease) {
        return Err("配置交接不能恢复 Xray 可执行文件".to_owned());
    }
    let target = managed_target()?;
    validate_managed_target(&target)?;
    if file_sha256_hex(&target).ok().as_deref() == Some(runtime.old.sha256.as_str()) {
        return Ok(());
    }
    let backup = state_dir.join(PREVIOUS_FILE);
    if file_sha256_hex(&backup).ok().as_deref() != Some(runtime.old.sha256.as_str()) {
        return Err("中断恢复找不到摘要匹配的旧 Xray 备份".to_owned());
    }
    replace_executable(&backup, &target)?;
    verify_xray_path(&target, &runtime.old.sha256)
}

fn restore_old_config_from_state(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
) -> Result<(), String> {
    if config_handoff_source(runtime).is_none() {
        return Err("二进制发布不能恢复配置交接备份".to_owned());
    }
    let backup = state_dir.join(CONFIG_PREVIOUS_FILE);
    let previous =
        fs::read(&backup).map_err(|error| format!("读取旧 Xray 配置备份失败：{error}"))?;
    crate::fsutil::atomic_write_private(&state_dir.join("xray.json"), &previous)
}

fn restore_old_handoff_state(state_dir: &Path, runtime: &XrayUpgradeRuntime) -> Result<(), String> {
    match &runtime.source {
        XrayUpgradeSource::BinaryRelease => restore_old_executable_from_state(state_dir, runtime),
        XrayUpgradeSource::ConfigDeployment { .. } => {
            restore_old_config_from_state(state_dir, runtime)
        }
    }
}

/// Retain the live generation and two recent retired generations for diagnosis. Finished
/// handoffs otherwise leave a complete executable per release and eventually exhaust small nodes.
/// The runtime mutex is held by the caller; an in-flight handoff/recovery never permits pruning.
pub(crate) fn prune_retired_generations(state_dir: &Path) -> Result<(), String> {
    if read_upgrade_runtime(state_dir)?.is_some()
        || state_dir
            .join(crash_recovery::RECOVERY_FILE)
            .try_exists()
            .map_err(|error| error.to_string())?
        || crate::pending_config_convergence_path(state_dir)
            .try_exists()
            .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let root = state_dir.join(HANDOFF_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取旧代目录失败：{error}")),
    };
    let mut live = BTreeSet::new();
    for process in crate::serving_xray_processes_in(state_dir) {
        let environment = match fs::read(format!("/proc/{}/environ", process.pid)) {
            Ok(environment) => environment,
            Err(_) if !crate::process_is_live(process) => continue,
            Err(error) => return Err(format!("确认存活 Xray 代际失败，暂缓回收：{error}")),
        };
        if let Some(path) = environment
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(XRAY_HANDOFF_MANIFEST_ENV))
        {
            let path = std::str::from_utf8(path).map_err(|_| "存活代际路径无效，暂缓回收")?;
            let manifest = read_handoff_manifest_identity(Path::new(path))?;
            live.insert(manifest.generation_id);
        } else {
            // Legacy launches have no generation directory, but may still execute a copied
            // binary. Never remove its directory based only on an absent environment variable.
            let executable = fs::read_link(format!("/proc/{}/exe", process.pid))
                .map_err(|error| format!("确认存活 Xray 路径失败，暂缓回收：{error}"))?;
            if let Some(name) = executable
                .parent()
                .filter(|parent| parent.parent() == Some(root.as_path()))
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
            {
                live.insert(name.to_owned());
            }
        }
    }
    let mut retired = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取旧代条目失败：{error}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(token) = name.strip_prefix("brocade-") else {
            continue;
        };
        if token.len() != 32
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || live.contains(&name)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err("旧代目录类型、属主或权限无效，暂缓回收".to_owned());
        }
        let manifest_path = entry.path().join("manifest.json");
        match fs::symlink_metadata(&manifest_path) {
            Ok(_) => {
                let manifest = read_handoff_manifest_identity(&manifest_path)?;
                if manifest.generation_id != name
                    || manifest.control_path.parent() != Some(entry.path().as_path())
                {
                    return Err("旧代 manifest 与目录身份不一致，暂缓回收".to_owned());
                }
            }
            // Preparation writes the executable before the manifest. Failed preparation can
            // leave this incomplete private directory, but cannot have launched a child from it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("读取旧代 manifest 元数据失败：{error}")),
        }
        retired.push((
            metadata.modified().map_err(|error| error.to_string())?,
            name,
            entry.path(),
        ));
    }
    retired.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    for (_, name, directory) in retired.into_iter().skip(2).take(4) {
        // New candidates are the newest entries and are retained even across a manual executor's
        // pre-registration window. Recheck the durable fence before deleting any older entry.
        if read_upgrade_runtime(state_dir)?.is_some() {
            return Ok(());
        }
        fs::remove_dir_all(&directory).map_err(|error| format!("回收旧代 {name} 失败：{error}"))?;
        let log = state_dir.join("logs").join(format!("xray-{name}.log"));
        for path in [&log, &crate::logcap::archive_path(&log)] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("回收旧代日志 {name} 失败：{error}")),
            }
        }
    }
    Ok(())
}

fn cleanup_handoff_backup(state_dir: &Path, runtime: &XrayUpgradeRuntime) {
    let name = match &runtime.source {
        XrayUpgradeSource::BinaryRelease => PREVIOUS_FILE,
        XrayUpgradeSource::ConfigDeployment { .. } => CONFIG_PREVIOUS_FILE,
    };
    cleanup_file(&state_dir.join(name));
    if config_handoff_source(runtime).is_some() {
        cleanup_file(&state_dir.join(CONFIG_GRANTS_FILE));
    }
}

/// None means a legacy runtime has no full checkpoint (absent or null); [] is a complete set.
/// Never print decoding errors: malformed field values may themselves contain credentials.
fn read_config_handoff_grants(state_dir: &Path) -> Result<Option<Vec<GrantInbound>>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state_dir.join(CONFIG_GRANTS_FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("打开候选授权检查点失败：{error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("检查候选授权检查点失败：{error}"))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > crate::MAX_CONFIG_CONVERGENCE_BYTES
    {
        return Err("候选授权检查点的类型、属主、链接数、权限或大小无效".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(crate::MAX_CONFIG_CONVERGENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取候选授权检查点失败：{error}"))?;
    if bytes.len() as u64 != metadata.len() {
        return Err("候选授权检查点在读取期间发生变化".to_owned());
    }
    serde_json::from_slice::<Option<Vec<GrantInbound>>>(&bytes)
        .map_err(|_| "候选授权检查点不是有效的完整授权数组".to_owned())
}

fn candidate_grants_for_recovery(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
) -> Result<Option<Vec<GrantInbound>>, String> {
    match &runtime.source {
        XrayUpgradeSource::BinaryRelease => Ok(None),
        XrayUpgradeSource::ConfigDeployment { .. } => read_config_handoff_grants(state_dir)?
            .map(Some)
            .ok_or_else(|| "配置候选缺少完整目标授权检查点，拒绝恢复接入".to_owned()),
    }
}

pub(crate) fn config_handoff_has_grants(
    state_dir: &Path,
    deployment_id: i64,
    claim_generation: u64,
    wanted: &[GrantInbound],
) -> Result<bool, String> {
    let Some(runtime) = read_upgrade_runtime(state_dir)? else {
        return Ok(false);
    };
    Ok(
        config_handoff_source(&runtime) == Some((deployment_id, claim_generation))
            && runtime.phase == XrayUpgradePhase::Draining
            && runtime
                .candidate
                .as_ref()
                .and_then(|candidate| candidate.accepting)
                == Some(true)
            && read_config_handoff_grants(state_dir)?.as_deref() == Some(wanted),
    )
}

fn freeze_config_handoff_grants(
    state_dir: &Path,
    desired: &str,
    desired_grants: Option<&[GrantInbound]>,
) -> Result<Vec<GrantInbound>, String> {
    if let Some(grants) = desired_grants {
        return Ok(grants.to_vec());
    }
    // An explicitly unmanaged authorization set keeps its prior semantics, but the fallback
    // must be resolved now, before pause, not from a mutable formal checkpoint after a crash.
    crate::desired_grants_on_disk(state_dir)?
        .map(|grants| crate::recovery_grants_for_config(desired, grants))
        .transpose()
        .map(Option::unwrap_or_default)
}

// Stage the full snapshot before pause, but commit only after admission. Recovery repeats this
// if the Agent exits between candidate resume and the local checkpoint write.
fn commit_config_handoff_state(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
) -> Result<(), String> {
    if config_handoff_source(runtime).is_none() {
        return Ok(());
    }
    if let Some(grants) = read_config_handoff_grants(state_dir)? {
        crate::fsutil::atomic_write_private(
            &state_dir.join("grants.desired.json"),
            &serde_json::to_vec(&grants).map_err(|error| error.to_string())?,
        )?;
    }
    let content = fs::read_to_string(state_dir.join("xray.json"))
        .map_err(|error| format!("读取已接入配置失败：{error}"))?;
    crate::persist_xray_splice_mode(state_dir, &content)?;
    crate::fsutil::atomic_write_private(
        &state_dir.join(crate::XRAY_BOUNDED_LOG_MARKER),
        b"dynamic\n",
    )
}

fn drop_config_tcp_admission(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    manifest: &HandoffManifestIdentity,
    old_pid: i32,
    status: &HandoffRuntimeStatus,
) -> Result<HandoffRuntimeStatus, String> {
    // The private candidate snapshot distinguishes new attempts from legacy configuration
    // runtimes. Never send the additive RPC to an old Xray that did not advertise it.
    if config_handoff_source(runtime).is_none()
        || !state_dir
            .join(CONFIG_GRANTS_FILE)
            .try_exists()
            .map_err(|error| error.to_string())?
        || status
            .listeners
            .iter()
            .filter(|listener| listener.network == "tcp" && listener.role != "cover")
            .all(|listener| listener.closed)
    {
        return Ok(status.clone());
    }
    let dropped = handoff_rpc(
        manifest,
        old_pid,
        "drop_tcp",
        status
            .epoch
            .checked_add(1)
            .ok_or("旧代 TCP 退役 epoch 溢出")?,
        &format!("config-{}-drop-old-tcp", runtime.release_id),
    )?;
    if dropped
        .listeners
        .iter()
        .any(|listener| listener.network == "tcp" && listener.role != "cover" && !listener.closed)
    {
        return Err("旧代仍有业务/API TCP listener 未关闭".to_owned());
    }
    Ok(dropped)
}

fn finalize_recovered_candidate(
    options: &Options,
    runtime: &XrayUpgradeRuntime,
    old: crate::ProcessRef,
    candidate: crate::ProcessRef,
    candidate_manifest: &HandoffManifestIdentity,
    forced_retirement: bool,
) -> Result<(), String> {
    if matches!(&runtime.source, XrayUpgradeSource::BinaryRelease) {
        let target = managed_target()?;
        validate_managed_target(&target)?;
        verify_xray_path(
            &target,
            runtime
                .candidate
                .as_ref()
                .ok_or("恢复状态缺少候选代")?
                .sha256
                .as_str(),
        )?;
    }
    crate::accounting::promote_shared_writer(&options.state_dir, old, candidate)?;
    crate::collect_usage_report(options)?;
    let status = handoff_status_rpc(candidate_manifest, candidate.pid)?;
    if !status.accepting || !status.ready || !status.retirement_supported {
        return Err("中断恢复后的候选代没有保持完整接入能力".to_owned());
    }
    if matches!(&runtime.source, XrayUpgradeSource::BinaryRelease) {
        store_recovered_success_report(&options.state_dir, runtime, forced_retirement)?;
    }
    commit_config_handoff_state(&options.state_dir, runtime)?;
    cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
    cleanup_handoff_backup(&options.state_dir, runtime);
    Ok(())
}

fn store_recovered_success_report(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    forced_retirement: bool,
) -> Result<(), String> {
    write_stored_report(
        state_dir,
        REPORT_FILE,
        recovered_success_report(state_dir, runtime, forced_retirement),
    )
}

fn recovered_success_report(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    forced_retirement: bool,
) -> XrayReleaseReport {
    XrayReleaseReport {
        release_id: runtime.release_id,
        attempt: runtime.attempt,
        outcome: XrayReleaseOutcome::Succeeded,
        performed_update: true,
        strategy: runtime.strategy,
        forced_retirement,
        xray_enabled: xray_enabled(state_dir),
        installed_sha256: crate::installed_xray_sha256(),
        running_sha256: crate::serving_xray_process_in(state_dir).and_then(|process| {
            crate::cached_xray_sha256(&PathBuf::from(format!("/proc/{}/exe", process.pid))).ok()
        }),
        error: None,
    }
}

/// Advance an interrupted handoff by at most one bounded retirement step. It is called at the
/// main convergence boundary, under the same accounting mutex as ordinary releases. Long-lived
/// connections therefore keep the runtime in Draining rather than blocking all Agent work.
pub(crate) fn recover_interrupted_handoff(
    options: &Options,
    meter: &Arc<Mutex<()>>,
) -> Result<bool, String> {
    {
        let _switch_guard = meter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if crash_recovery::resume(&options.state_dir)? {
            return Ok(true);
        }
    }
    let Some(mut runtime) = interrupted_handoff_runtime(&options.state_dir)? else {
        return Ok(false);
    };
    // Pull only while a configuration generation is already draining. A newer executable config
    // release may authorize early retirement; otherwise a Console outage leaves the local bounded
    // policy in charge and cannot invent a destructive transition.
    let config_force_authorized = match config_force_retirement_authorized(options, &runtime) {
        Ok(authorized) => authorized,
        Err(error) => {
            eprintln!("xray-update: {error}");
            false
        }
    };
    // Recovery performs at most one bounded state transition. Serialize that transition with
    // forwarding and accounting, but return before the 15-second convergence sleep so long-lived
    // drains never monopolize the shared runtime boundary.
    let _switch_guard = meter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(current) = interrupted_handoff_runtime(&options.state_dir)? else {
        // The one-second guard can finish recovery while the optional Console query runs.
        return Ok(true);
    };
    if current.source != runtime.source
        || current.release_id != runtime.release_id
        || current.attempt != runtime.attempt
    {
        return Ok(true);
    }
    runtime = current;
    let (old, candidate) = match observe_upgrade_generations(&runtime) {
        Ok(generations) => generations,
        Err(error) => {
            return recover_unresponsive_handoff(&options.state_dir, &runtime, &error);
        }
    };

    match (old, candidate) {
        (None, None) => {
            if runtime.candidate.is_some() {
                return crash_recovery::begin(&options.state_dir, &runtime);
            }
            cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
            if config_handoff_source(&runtime).is_some() {
                cleanup_handoff_backup(&options.state_dir, &runtime);
            }
            Ok(true)
        }
        (None, Some((candidate_process, candidate_manifest, mut candidate_status))) => {
            let needs_runtime_recovery =
                !candidate_status.ready || !candidate_status.retirement_supported;
            let desired_grants = if needs_runtime_recovery || !candidate_status.accepting {
                candidate_grants_for_recovery(&options.state_dir, &runtime)?
            } else {
                None
            };
            if needs_runtime_recovery {
                let config = fs::read_to_string(options.state_dir.join("xray.json"))
                    .map_err(|error| format!("恢复孤立候选时读取 Xray 配置失败：{error}"))?;
                let api_address =
                    crate::xray_api_address(&config).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
                let recovered = crate::restore_xray_runtime_with_grants(
                    &options.state_dir,
                    &config,
                    api_address,
                    desired_grants.as_deref(),
                );
                if let Some(error) = recovered.error() {
                    return Err(format!("旧代消失后候选动态状态无法恢复：{error}"));
                }
                let epoch = candidate_status
                    .epoch
                    .checked_add(1)
                    .ok_or("恢复孤立候选 seal 时 fence epoch 溢出")?;
                candidate_status = handoff_rpc(
                    &candidate_manifest,
                    candidate_process.pid,
                    "seal",
                    epoch,
                    &format!("recover-{}-seal-candidate", runtime.release_id),
                )?;
                if !candidate_status.ready || !candidate_status.retirement_supported {
                    return Err("旧代消失后候选代不能完成完整退役认证".to_owned());
                }
            }
            if matches!(&runtime.source, XrayUpgradeSource::BinaryRelease) {
                let target = managed_target()?;
                validate_managed_target(&target)?;
                if file_sha256_hex(&target).ok().as_deref()
                    != runtime
                        .candidate
                        .as_ref()
                        .map(|candidate| candidate.sha256.as_str())
                {
                    replace_executable(
                        &PathBuf::from(format!("/proc/{}/exe", candidate_process.pid)),
                        &target,
                    )?;
                }
            }
            if !candidate_status.accepting {
                let epoch = candidate_status
                    .epoch
                    .checked_add(1)
                    .ok_or("恢复候选代时 fence epoch 溢出")?;
                candidate_status = handoff_rpc(
                    &candidate_manifest,
                    candidate_process.pid,
                    "resume",
                    epoch,
                    &format!("recover-{}-candidate", runtime.release_id),
                )?;
            }
            if !candidate_status.accepting
                || !candidate_status.ready
                || !candidate_status.retirement_supported
            {
                return Err("旧代消失后候选代不能恢复接入".to_owned());
            }
            let old_process = crate::ProcessRef {
                pid: runtime.old.pid,
                start_ticks: runtime.old.pid_start_ticks,
            };
            finalize_recovered_candidate(
                options,
                &runtime,
                old_process,
                candidate_process,
                &candidate_manifest,
                false,
            )?;
            Ok(true)
        }
        (Some((old_process, old_manifest, old_status)), None) => {
            if old_status.udp_dropped {
                return crash_recovery::begin(&options.state_dir, &runtime);
            }
            if !old_status.accepting {
                let epoch = old_status
                    .epoch
                    .checked_add(1)
                    .ok_or("回滚旧代时 fence epoch 溢出")?;
                let resumed = handoff_rpc(
                    &old_manifest,
                    old_process.pid,
                    "resume",
                    epoch,
                    &format!("recover-{}-rollback-old", runtime.release_id),
                )?;
                if !resumed.accepting {
                    return Err("中断恢复未能恢复旧代接入".to_owned());
                }
            }
            restore_old_handoff_state(&options.state_dir, &runtime)?;
            cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
            cleanup_handoff_backup(&options.state_dir, &runtime);
            Ok(true)
        }
        (
            Some((old_process, old_manifest, mut old_status)),
            Some((candidate_process, candidate_manifest, candidate_status)),
        ) => {
            if !candidate_status.ready
                || !candidate_status.retirement_supported
                || !candidate_status.accepting
            {
                if candidate_status.active != 0 {
                    runtime.phase = XrayUpgradePhase::Blocked;
                    runtime.blocked_reason = Some(format!(
                        "候选代未接入但仍有 {} 条业务连接，拒绝自动终止",
                        candidate_status.active
                    ));
                    write_upgrade_runtime(&options.state_dir, &runtime)?;
                    return Ok(true);
                }
                if old_status.udp_dropped {
                    return Err("候选代未接入且旧代 UDP 已断开，拒绝伪装成完整回滚".to_owned());
                }
                if !old_status.accepting {
                    let epoch = old_status
                        .epoch
                        .checked_add(1)
                        .ok_or("中断回滚旧代时 fence epoch 溢出")?;
                    old_status = handoff_rpc(
                        &old_manifest,
                        old_process.pid,
                        "resume",
                        epoch,
                        &format!("recover-{}-rollback-old", runtime.release_id),
                    )?;
                }
                if !old_status.accepting {
                    return Err("中断回滚后旧代没有恢复接入".to_owned());
                }
                crate::terminate_processes(
                    &[candidate_process],
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                )?;
                restore_old_handoff_state(&options.state_dir, &runtime)?;
                cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
                cleanup_handoff_backup(&options.state_dir, &runtime);
                return Ok(true);
            }

            if old_status.accepting {
                let epoch = old_status
                    .epoch
                    .checked_add(1)
                    .ok_or("中断恢复暂停旧代时 fence epoch 溢出")?;
                old_status = handoff_rpc(
                    &old_manifest,
                    old_process.pid,
                    "pause",
                    epoch,
                    &format!("recover-{}-pause-old", runtime.release_id),
                )?;
            }
            old_status = drop_generation_udp(
                &old_manifest,
                old_process.pid,
                &old_status,
                &format!("recover-{}-drop-old-udp", runtime.release_id),
            )?;
            old_status = drop_config_tcp_admission(
                &options.state_dir,
                &runtime,
                &old_manifest,
                old_process.pid,
                &old_status,
            )?;
            if old_status.public_active == 0 && !old_status.retire_ready {
                let epoch = old_status
                    .epoch
                    .checked_add(1)
                    .ok_or("中断恢复排空 cover 时 fence epoch 溢出")?;
                old_status = handoff_rpc(
                    &old_manifest,
                    old_process.pid,
                    "drain",
                    epoch,
                    &format!("recover-{}-drain-old", runtime.release_id),
                )?;
            }
            let deadline_expired = runtime
                .drain_deadline_unix_secs
                .zip(crate::current_unix_secs().ok())
                .is_some_and(|(deadline, now)| now >= deadline);
            let force = should_force_handoff_retirement(
                &runtime,
                deadline_expired,
                config_force_authorized,
            );
            if !old_status.retire_ready && !force {
                runtime.phase = if runtime
                    .drain_deadline_unix_secs
                    .zip(crate::current_unix_secs().ok())
                    .is_some_and(|(deadline, now)| now >= deadline)
                {
                    XrayUpgradePhase::Blocked
                } else {
                    XrayUpgradePhase::Draining
                };
                runtime.old.accepting = Some(false);
                runtime.old.active_connections = Some(old_status.active);
                if let Some(candidate) = runtime.candidate.as_mut() {
                    candidate.accepting = Some(true);
                    candidate.active_connections = Some(candidate_status.active);
                }
                runtime.blocked_reason = (runtime.phase == XrayUpgradePhase::Blocked).then(|| {
                    format!(
                        "旧代仍有 {} 条连接，Agent 重启后继续等待自然排空",
                        old_status.active
                    )
                });
                write_upgrade_runtime(&options.state_dir, &runtime)?;
                return Ok(true);
            }

            if force && !old_status.retire_ready {
                runtime.phase = XrayUpgradePhase::Forcing;
                runtime.strategy = XrayReleaseStrategy::HandoffForce;
                runtime.blocked_reason = None;
                runtime.old.active_connections = Some(old_status.active);
                if let Some(candidate) = runtime.candidate.as_mut() {
                    candidate.accepting = Some(true);
                    candidate.active_connections = Some(candidate_status.active);
                }
                write_upgrade_runtime(&options.state_dir, &runtime)?;
                let _ = crate::publish_runtime_now(options);
            }

            crate::collect_usage_report(options)?;
            crate::terminate_processes(
                &[old_process],
                crate::XRAY_TERM_GRACE,
                crate::XRAY_KILL_GRACE,
            )?;
            crate::accounting::promote_shared_writer(
                &options.state_dir,
                old_process,
                candidate_process,
            )?;
            crate::collect_usage_report(options)?;
            let final_status = handoff_status_rpc(&candidate_manifest, candidate_process.pid)?;
            if !final_status.accepting || !final_status.ready || !final_status.retirement_supported
            {
                return Err("中断恢复完成旧代退役后候选代状态异常".to_owned());
            }
            if matches!(&runtime.source, XrayUpgradeSource::BinaryRelease) {
                let target = managed_target()?;
                verify_xray_path(
                    &target,
                    &runtime
                        .candidate
                        .as_ref()
                        .ok_or("恢复状态缺少候选代")?
                        .sha256,
                )?;
                store_recovered_success_report(&options.state_dir, &runtime, force)?;
            }
            commit_config_handoff_state(&options.state_dir, &runtime)?;
            cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
            cleanup_handoff_backup(&options.state_dir, &runtime);
            Ok(true)
        }
    }
}

pub(crate) fn spawn_xray_updates(options: &Options, queue: &Arc<XrayUpdateQueue>) {
    let options = options.clone();
    let queue = Arc::clone(queue);
    let spawned = thread::Builder::new()
        .name("xray-update".to_owned())
        .spawn(move || loop {
            crate::each_round("xray-update", || {
                if let Err(error) = poll_cycle(&options, &queue) {
                    crate::warn(format!("xray-update: {error}"));
                }
            });
            thread::sleep(update_interval(&options.token));
        });
    if let Err(error) = spawned {
        crate::warn(format!(
            "xray-update: 起不了更新线程（{error}）；Xray 只能靠 install.sh 升级"
        ));
    }
}

fn update_interval(node_token: &str) -> Duration {
    // Enrollment tokens are random per machine. Folding their bytes produces a stable offset
    // without another RNG or ever logging the credential, so a wave does not wake every Agent on
    // the same ten-minute boundary.
    let jitter = node_token.as_bytes().iter().fold(0_u64, |state, byte| {
        state.wrapping_mul(131).wrapping_add(u64::from(*byte))
    }) % (UPDATE_JITTER_MAX_SECS + 1);
    UPDATE_INTERVAL + Duration::from_secs(jitter)
}

pub(crate) fn apply_pending(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    queue: &Arc<XrayUpdateQueue>,
) -> Result<(), String> {
    let recovered = recover_interrupted_handoff(options, meter)?;
    retry_stored_report(options)?;
    if recovered {
        // Recovery may have reached the terminal boundary and persisted its receipt. Submit it in
        // this same convergence round rather than waiting up to the ten-minute offer poll.
        let _ = crate::publish_runtime_now(options);
        return Ok(());
    }
    let Some(prepared) = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return Ok(());
    };

    let current = match fetch_offer(options) {
        Ok(current) => current,
        Err(error) => {
            put_back(queue, prepared);
            return Err(format!("替换前无法复核发布：{error}"));
        }
    };
    let Some(current) = current else {
        cleanup_file(&prepared.candidate);
        return Ok(());
    };
    if current.release_id != prepared.offer.release_id
        || current.attempt != prepared.offer.attempt
        || current.sha256 != prepared.offer.sha256
    {
        cleanup_file(&prepared.candidate);
        return Ok(());
    }
    let staged_sha = match file_sha256_hex(&prepared.candidate) {
        Ok(sha) => sha,
        Err(error) => {
            cleanup_file(&prepared.candidate);
            return finish_report(
                options,
                failure_report(
                    options,
                    &current,
                    pre_update_failure_outcome(options, &current),
                    format!("安装前读不到暂存的 Xray 摘要，没有替换：{error}"),
                ),
            );
        }
    };
    if staged_sha != current.sha256 {
        cleanup_file(&prepared.candidate);
        return finish_report(
            options,
            failure_report(
                options,
                &current,
                pre_update_failure_outcome(options, &current),
                "暂存的 Xray 在安装前摘要发生变化，没有替换".to_owned(),
            ),
        );
    }

    let report = perform_update(options, meter, &current, &prepared.candidate);
    cleanup_file(&prepared.candidate);
    let Some(report) = report else {
        // A graceful handoff may outlive its drain deadline. The durable two-generation runtime
        // is the pending result in that case; the next convergence round advances it by one
        // bounded recovery step without reporting a terminal release outcome.
        let _ = crate::publish_runtime_now(options);
        return Ok(());
    };
    if report.outcome != XrayReleaseOutcome::FailedDirty {
        cleanup_file(&options.state_dir.join(PREVIOUS_FILE));
    }
    let _ = crate::publish_runtime_now(options);
    finish_report(options, report)
}

fn put_back(queue: &Arc<XrayUpdateQueue>, prepared: PreparedUpdate) {
    let mut pending = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        *pending = Some(prepared);
    }
}

fn poll_cycle(options: &Options, queue: &Arc<XrayUpdateQueue>) -> Result<(), String> {
    // The convergence owner must first finish or roll back the durable two-generation state.
    // Reporting a same-hash no-op here would otherwise mark a release successful while the old
    // PID is still draining after an Agent restart.
    if read_upgrade_runtime(&options.state_dir)?.is_some() {
        return Ok(());
    }
    retry_stored_report(options)?;
    if queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
    {
        return Ok(());
    }
    let Some(offer) = fetch_offer(options)? else {
        return Ok(());
    };
    if let Err(error) = validate_offer(&offer) {
        return finish_report(
            options,
            failure_report(options, &offer, XrayReleaseOutcome::Unsupported, error),
        );
    }

    if options.apply_mode != ApplyMode::Linux {
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                XrayReleaseOutcome::Unsupported,
                "Agent 运行在 state-dir 模式，不能替换系统 Xray".to_owned(),
            ),
        );
    }

    let enabled = xray_enabled(&options.state_dir);
    let installed = crate::installed_xray_sha256();
    let running = crate::running_xray_sha256();
    if installed.as_deref() == Some(offer.sha256.as_str())
        && (!enabled || running.as_deref() == Some(offer.sha256.as_str()))
    {
        return finish_report(
            options,
            success_report(&offer, false, enabled, installed, running),
        );
    }
    if installed.as_deref() != Some(offer.previous_sha256.as_str())
        && running.as_deref() != Some(offer.previous_sha256.as_str())
        && !valid_previous_backup(options, &offer)
    {
        return finish_report(
            options,
            report_with_state(
                &offer,
                XrayReleaseOutcome::FailedDirty,
                false,
                enabled,
                installed,
                running,
                Some("本机 Xray 与发布创建时冻结的摘要都不一致，没有替换".to_owned()),
            ),
        );
    }

    let download = options.state_dir.join(DOWNLOAD_FILE);
    let candidate = options.state_dir.join(CANDIDATE_FILE);
    cleanup_file(&download);
    cleanup_file(&candidate);
    if let Err(error) = fetch(&offer.url, &download) {
        return record_preparation_failure(options, &offer, &download, &candidate, error);
    }
    if let Err(error) = validate_download_size(&download) {
        return record_preparation_failure(options, &offer, &download, &candidate, error);
    }
    let got = match file_sha256_hex(&download) {
        Ok(got) => got,
        Err(error) => {
            return record_preparation_failure(
                options,
                &offer,
                &download,
                &candidate,
                format!("计算下载的 Xray 摘要失败：{error}"),
            )
        }
    };
    if got != offer.sha256 {
        cleanup_file(&download);
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                pre_update_failure_outcome(options, &offer),
                format!(
                    "下载的 Xray 摘要不符：需要 {}，实际 {got}，没有替换",
                    offer.sha256
                ),
            ),
        );
    }
    if let Err(error) = fs::set_permissions(&download, fs::Permissions::from_mode(0o700)) {
        return record_preparation_failure(
            options,
            &offer,
            &download,
            &candidate,
            format!("给候选 Xray 加执行位失败：{error}"),
        );
    }
    if let Err(error) = preflight(&download, &offer, options) {
        cleanup_file(&download);
        return finish_report(
            options,
            failure_report(
                options,
                &offer,
                pre_update_failure_outcome(options, &offer),
                format!("候选 Xray 预检失败，没有替换：{error}"),
            ),
        );
    }
    if let Err(error) = fs::rename(&download, &candidate) {
        return record_preparation_failure(
            options,
            &offer,
            &download,
            &candidate,
            format!("暂存候选 Xray 失败：{error}"),
        );
    }

    let mut pending = queue
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_none() {
        println!(
            "xray-update: 已准备发布 #{} 第 {} 次尝试，等待收敛轮次边界",
            offer.release_id, offer.attempt
        );
        *pending = Some(PreparedUpdate { offer, candidate });
    } else {
        cleanup_file(&candidate);
    }
    Ok(())
}

fn fetch_offer(options: &Options) -> Result<Option<XrayReleaseOffer>, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request_with_headers(
        "GET",
        "/agent/v1/xray-release",
        &options.token,
        None,
        &[(ARCH_HEADER, identity::self_arch().to_owned())],
    )?;
    if response.status == 204 {
        return Ok(None);
    }
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "问不到 Xray 发布：HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body)
        .map(Some)
        .map_err(|error| format!("Xray 发布响应读不懂：{error}"))
}

fn validate_offer(offer: &XrayReleaseOffer) -> Result<(), String> {
    if offer.release_id <= 0 || offer.attempt == 0 {
        return Err("Xray 发布身份超出范围".to_owned());
    }
    for (name, value) in [
        ("sha256", offer.sha256.as_str()),
        ("previous_sha256", offer.previous_sha256.as_str()),
    ] {
        if value.len() != 64
            || !value
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        {
            return Err(format!("Xray 发布的 {name} 不成形"));
        }
    }
    if !(offer.url.starts_with("https://") || offer.url.starts_with("http://")) {
        return Err("Xray 发布下载地址必须使用 HTTP 或 HTTPS".to_owned());
    }
    if offer.version.trim().is_empty() || offer.version.chars().count() > 128 {
        return Err("Xray 发布版本缺失或过长".to_owned());
    }
    match offer.strategy {
        XrayReleaseStrategy::Restart if offer.drain_timeout_secs.is_some() => {
            return Err("restart 发布不能携带排空超时".to_owned())
        }
        XrayReleaseStrategy::Handoff | XrayReleaseStrategy::HandoffForce => {
            if !offer.drain_timeout_secs.is_some_and(|seconds| {
                (XRAY_HANDOFF_MIN_DRAIN_SECS..=XRAY_HANDOFF_MAX_DRAIN_SECS).contains(&seconds)
            }) {
                return Err(format!(
                    "handoff 发布的排空超时必须在 {XRAY_HANDOFF_MIN_DRAIN_SECS}..={XRAY_HANDOFF_MAX_DRAIN_SECS} 秒"
                ));
            }
        }
        XrayReleaseStrategy::Restart => {}
    }
    Ok(())
}

fn fetch(url: &str, destination: &Path) -> Result<(), String> {
    crate::run_shell_with_timeout(
        &format!(
            "curl -fsSL --proto '=http,https' --proto-redir '=http,https' --connect-timeout 15 --max-time 300 --max-filesize {MAX_DOWNLOAD_BYTES} {} -o {}",
            shell_quote(url),
            shell_quote(&destination.to_string_lossy())
        ),
        DOWNLOAD_TIMEOUT,
    )
    .map(|_| ())
    .map_err(|error| format!("下载 Xray 失败：{error}"))
}

fn validate_download_size(path: &Path) -> Result<(), String> {
    let bytes = fs::metadata(path)
        .map_err(|error| format!("读取下载的 Xray 大小失败：{error}"))?
        .len();
    if bytes == 0 || bytes > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "下载的 Xray 大小 {bytes} 字节不在 1..={MAX_DOWNLOAD_BYTES} 的安全范围内"
        ));
    }
    Ok(())
}

fn record_preparation_failure(
    options: &Options,
    offer: &XrayReleaseOffer,
    download: &Path,
    candidate: &Path,
    error: String,
) -> Result<(), String> {
    cleanup_file(download);
    cleanup_file(candidate);
    finish_report(
        options,
        failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            format!("候选 Xray 准备失败，没有替换：{error}"),
        ),
    )
}

fn preflight(candidate: &Path, offer: &XrayReleaseOffer, options: &Options) -> Result<(), String> {
    let program = candidate.to_string_lossy().into_owned();
    let version = run_command_with_timeout(&program, &["version"], Duration::from_secs(10))?;
    let expected = offer.version.trim().trim_start_matches('v');
    if !reports_expected_version(&version, expected) {
        return Err(format!(
            "候选 Xray 没报告预期版本 {}：{}",
            offer.version,
            version.lines().next().unwrap_or_default()
        ));
    }
    let config = options.state_dir.join("xray.json");
    if xray_enabled(&options.state_dir) {
        let config = config.to_string_lossy().into_owned();
        run_command_with_timeout(
            &program,
            &["-test", "-config", &config],
            Duration::from_secs(30),
        )?;
    }
    Ok(())
}

fn reports_expected_version(output: &str, expected: &str) -> bool {
    let mut fields = output.lines().next().unwrap_or_default().split_whitespace();
    fields.next() == Some("Xray")
        && fields
            .next()
            .is_some_and(|version| version.trim_start_matches('v') == expected)
}

/// Execute one prepared release at the convergence boundary.
///
/// `None` is not success or failure: it means a graceful handoff is still draining and left a
/// durable two-generation runtime for later bounded convergence rounds.
fn perform_update(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
) -> Option<XrayReleaseReport> {
    let target = match managed_target() {
        Ok(target) => target,
        Err(error) => {
            return Some(failure_report(
                options,
                offer,
                XrayReleaseOutcome::Unsupported,
                error,
            ))
        }
    };
    if let Err(error) = validate_managed_target(&target) {
        return Some(failure_report(
            options,
            offer,
            XrayReleaseOutcome::Unsupported,
            error,
        ));
    }
    if offer.strategy != XrayReleaseStrategy::Restart {
        return perform_handoff_update_at_target(options, meter, offer, candidate, &target);
    }
    Some(perform_update_at_target(
        options, meter, offer, candidate, &target,
    ))
}

/// Start a configuration-only two-generation handoff.
///
/// The caller owns the shared Xray accounting/forwarding mutex for this whole function. A clean
/// return means the desired generation is accepting new connections and the durable runtime owns
/// retirement of the old generation; it does not mean the old TCP connections have drained yet.
/// A returned error before admission restores the old config and resumes the old generation.
pub(crate) fn begin_config_handoff(
    state_dir: &Path,
    deployment_id: i64,
    claim_generation: u64,
    previous: &str,
    desired_path: &Path,
    desired_grants: Option<&[brocade_deployment::plan::GrantInbound]>,
) -> Result<(), String> {
    if deployment_id <= 0 {
        return Err("配置交接身份超出范围".to_owned());
    }
    if read_upgrade_runtime(state_dir)?.is_some() {
        return Err("已有 Xray 双进程交接正在进行".to_owned());
    }
    if !installed_xray_supports_handoff() {
        return Err(format!("当前 Xray 没有 {XRAY_HANDOFF_CAPABILITY} 能力"));
    }
    let desired = fs::read_to_string(desired_path)
        .map_err(|error| format!("读取候选 Xray 配置失败：{error}"))?;
    let candidate_grants = freeze_config_handoff_grants(state_dir, &desired, desired_grants)?;
    let encoded_grants =
        serde_json::to_vec(&candidate_grants).map_err(|error| error.to_string())?;
    if encoded_grants.len() as u64 > crate::MAX_CONFIG_CONVERGENCE_BYTES {
        return Err("候选授权检查点大小超出范围".to_owned());
    }
    let desired_listeners = configured_handoff_endpoints(state_dir, &desired)?;
    let previous_socket_options = configured_socket_options(previous)?;
    let fresh_tcp = configured_socket_options(&desired)?
        .into_iter()
        .filter(|(address, options)| previous_socket_options.get(address) != Some(options))
        .map(|(address, _)| address)
        .collect::<BTreeSet<_>>();
    let (old_process, old_manifest, old_status) = observe_serving_handoff_generation(state_dir)
        .and_then(|generation| {
            if generation.2.ready && generation.2.accepting && generation.2.retirement_supported {
                Ok(generation)
            } else {
                Err(handoff_blocked_reason(&generation.2))
            }
        })?;
    let old_executable = PathBuf::from(format!("/proc/{}/exe", old_process.pid));
    let old_executable_arg = old_executable.to_string_lossy().into_owned();
    let version =
        run_command_with_timeout(&old_executable_arg, &["version"], Duration::from_secs(5))?;
    if !crate::xray_version_has_capability(&version, "config-listener-handoff-v1") {
        return Err("当前运行的 Xray 尚未支持配置监听集合交接，请先升级 Xray".to_owned());
    }
    let config: serde_json::Value =
        serde_json::from_str(&desired).map_err(|error| error.to_string())?;
    if ["/api/listen", "/metrics/listen"].iter().any(|pointer| {
        config
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|listen| !listen.is_empty())
    }) && !crate::xray_version_has_capability(&version, "control-listener-handoff-v1")
    {
        return Err("当前运行的 Xray 尚未支持直接 API/metrics 监听交接，请先升级 Xray".to_owned());
    }
    let desired_path_arg = desired_path.to_string_lossy().into_owned();
    run_command_with_timeout(
        &old_executable_arg,
        &["-test", "-config", &desired_path_arg],
        Duration::from_secs(30),
    )?;
    let old_sha = file_sha256_hex(&old_executable)?;
    let old_upgrade = upgrade_process(&old_status, old_sha)?;
    let deadline_unix = crate::current_unix_secs()
        .ok()
        .and_then(|now| now.checked_add(i64::from(XRAY_HANDOFF_DEFAULT_DRAIN_SECS)));
    let mut runtime = XrayUpgradeRuntime {
        source: XrayUpgradeSource::ConfigDeployment {
            deployment_id,
            claim_generation,
        },
        // Keep binary-release SQL fences disjoint while retaining a stable value for private RPC
        // intent labels and old Agent/runtime readers.
        release_id: deployment_id.saturating_neg(),
        attempt: 0,
        // Configuration releases prefer availability of the current desired state after the
        // bounded grace period. They still drain naturally for two minutes, then retire the old
        // generation instead of blocking this and every later configuration release forever.
        strategy: XrayReleaseStrategy::HandoffForce,
        phase: XrayUpgradePhase::Preparing,
        old: old_upgrade,
        candidate: None,
        drain_deadline_unix_secs: deadline_unix,
        blocked_reason: None,
    };
    let previous_path = state_dir.join(CONFIG_PREVIOUS_FILE);
    crate::fsutil::atomic_write_private(&previous_path, previous.as_bytes())?;
    crate::fsutil::atomic_write_private(&state_dir.join(CONFIG_GRANTS_FILE), &encoded_grants)?;
    if let Err(error) = write_upgrade_runtime(state_dir, &runtime) {
        cleanup_file(&previous_path);
        cleanup_file(&state_dir.join(CONFIG_GRANTS_FILE));
        return Err(error);
    }

    let pause_epoch = old_status
        .epoch
        .checked_add(1)
        .ok_or("旧代配置交接 pause fence epoch 溢出")?;
    let paused = match handoff_rpc(
        &old_manifest,
        old_process.pid,
        "pause",
        pause_epoch,
        &format!("config-{deployment_id}-{claim_generation}-pause"),
    ) {
        Ok(status) if status.paused && !status.accepting => status,
        Ok(status) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &status,
                None,
                "旧代没有完成 pause barrier",
            )
        }
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &old_status,
                None,
                &error,
            )
        }
    };
    runtime.phase = XrayUpgradePhase::Switching;
    runtime.old.accepting = Some(false);
    runtime.old.active_connections = Some(paused.active);
    if let Err(error) = write_upgrade_runtime(state_dir, &runtime) {
        return rollback_config_handoff_before_switch(
            state_dir,
            &runtime,
            &old_manifest,
            old_process,
            &paused,
            None,
            &format!("持久化旧代 pause 状态失败：{error}"),
        );
    }

    let export_epoch = paused
        .epoch
        .checked_add(1)
        .ok_or("旧代配置交接 export fence epoch 溢出")?;
    let (exported_status, exported) = match handoff_export_rpc(
        &old_manifest,
        old_process.pid,
        export_epoch,
        &format!("config-{deployment_id}-{claim_generation}-export"),
    ) {
        Ok(exported) => exported,
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &paused,
                None,
                &error,
            )
        }
    };
    let exported = match relocate_handoff_descriptors(&exported) {
        Ok(exported) => exported,
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                None,
                &error,
            )
        }
    };
    let candidate_handoff = match prepare_candidate_handoff_for_listeners(
        state_dir,
        &old_executable,
        &exported_status,
        &exported,
        Some(desired_listeners),
        &fresh_tcp,
    ) {
        Ok(candidate) => candidate,
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                None,
                &error,
            )
        }
    };

    let live_path = state_dir.join("xray.json");
    if let Err(error) = crate::write_private(&live_path, &desired) {
        return rollback_config_handoff_before_switch(
            state_dir,
            &runtime,
            &old_manifest,
            old_process,
            &exported_status,
            None,
            &format!("切换候选配置失败：{error}"),
        );
    }
    let (candidate_status, candidate_process) =
        match launch_handoff_candidate(state_dir, &candidate_handoff, &exported) {
            Ok(candidate) => candidate,
            Err(error) => {
                return rollback_config_handoff_before_switch(
                    state_dir,
                    &runtime,
                    &old_manifest,
                    old_process,
                    &exported_status,
                    None,
                    &error,
                )
            }
        };
    drop(exported);
    runtime.phase = XrayUpgradePhase::CandidateReady;
    let candidate_upgrade = match file_sha256_hex(&candidate_handoff.executable)
        .and_then(|sha256| upgrade_process(&candidate_status, sha256))
    {
        Ok(candidate) => candidate,
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                &format!("记录候选进程身份失败：{error}"),
            )
        }
    };
    runtime.candidate = Some(candidate_upgrade);
    if let Err(error) = write_upgrade_runtime(state_dir, &runtime) {
        return rollback_config_handoff_before_switch(
            state_dir,
            &runtime,
            &old_manifest,
            old_process,
            &exported_status,
            Some((&candidate_handoff.manifest, candidate_process)),
            &format!("持久化候选进程身份失败：{error}"),
        );
    }

    #[cfg(test)]
    if std::env::var_os("BROCADE_TEST_CONFIG_HANDOFF_INTERRUPT_BEFORE_RECOVERY").is_some() {
        return Err("test interruption before config candidate recovery".to_owned());
    }

    let api_address = crate::xray_api_address(&desired).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
    // Preparation admission is enabled only after Core.Start. Reading the candidate API also
    // prevents sealing a process whose Unix control socket appeared before its core was ready.
    let api_ready =
        wait_for_candidate_api(api_address, &candidate_handoff.manifest, candidate_process);
    if let Err(error) = api_ready {
        let error = startup::root_failure(&candidate_handoff.manifest, Some(candidate_process))
            .unwrap_or_else(|| format!("候选 API 没有就绪：{error}"));
        return rollback_config_handoff_before_switch(
            state_dir,
            &runtime,
            &old_manifest,
            old_process,
            &exported_status,
            Some((&candidate_handoff.manifest, candidate_process)),
            &error,
        );
    }
    let recovery = {
        let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(5)));
        crate::restore_xray_runtime_with_grants(
            state_dir,
            &desired,
            api_address,
            Some(&candidate_grants),
        )
    };
    if let Some(error) = recovery.error() {
        return rollback_config_handoff_before_switch(
            state_dir,
            &runtime,
            &old_manifest,
            old_process,
            &exported_status,
            Some((&candidate_handoff.manifest, candidate_process)),
            &format!("候选配置动态业务状态没有完整恢复：{error}"),
        );
    }
    let sealed = match handoff_rpc(
        &candidate_handoff.manifest,
        candidate_process.pid,
        "seal",
        candidate_status.epoch.saturating_add(1),
        &format!("config-{deployment_id}-{claim_generation}-seal"),
    ) {
        Ok(status) if status.ready && status.retirement_supported && status.paused => status,
        Ok(_) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                "候选配置 seal 后状态不安全",
            )
        }
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                &error,
            )
        }
    };
    let resumed = match handoff_rpc(
        &candidate_handoff.manifest,
        candidate_process.pid,
        "resume",
        sealed.epoch.saturating_add(1),
        &format!("config-{deployment_id}-{claim_generation}-resume"),
    ) {
        Ok(status) if status.accepting && status.ready && status.retirement_supported => status,
        Ok(_) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                "候选配置没有进入 accepting 状态",
            )
        }
        Err(error) => {
            return rollback_config_handoff_before_switch(
                state_dir,
                &runtime,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                &error,
            )
        }
    };
    let old_status_after_udp = match drop_generation_udp(
        &old_manifest,
        old_process.pid,
        &exported_status,
        &format!("config-{deployment_id}-{claim_generation}-drop-old-udp"),
    ) {
        Ok(status) => status,
        Err(error) => {
            runtime.phase = XrayUpgradePhase::Blocked;
            runtime.blocked_reason = Some(format!("新配置已接入，但旧代 UDP 未能断开：{error}"));
            if let Some(candidate) = runtime.candidate.as_mut() {
                candidate.accepting = Some(true);
                candidate.active_connections = Some(resumed.active);
            }
            write_upgrade_runtime(state_dir, &runtime)?;
            return Err(runtime.blocked_reason.clone().unwrap_or_default());
        }
    };
    if let Err(error) = drop_config_tcp_admission(
        state_dir,
        &runtime,
        &old_manifest,
        old_process.pid,
        &old_status_after_udp,
    ) {
        runtime.phase = XrayUpgradePhase::Blocked;
        runtime.blocked_reason = Some(format!("新配置已接入，但旧代 TCP 监听未退役：{error}"));
        if let Some(candidate) = runtime.candidate.as_mut() {
            candidate.accepting = Some(true);
            candidate.active_connections = Some(resumed.active);
        }
        write_upgrade_runtime(state_dir, &runtime)?;
        return Err(runtime.blocked_reason.clone().unwrap_or_default());
    }
    runtime.phase = XrayUpgradePhase::Draining;
    runtime.blocked_reason = None;
    if let Some(candidate) = runtime.candidate.as_mut() {
        candidate.accepting = Some(true);
        candidate.active_connections = Some(resumed.active);
    }
    write_upgrade_runtime(state_dir, &runtime)?;
    commit_config_handoff_state(state_dir, &runtime)?;
    Ok(())
}

fn wait_for_candidate_api(
    api_address: SocketAddr,
    manifest: &HandoffManifestIdentity,
    process: crate::ProcessRef,
) -> Result<(), String> {
    let deadline = Instant::now() + HANDOFF_CANDIDATE_START_TIMEOUT;
    loop {
        if let Some(error) = startup::root_failure(manifest, Some(process)) {
            return Err(error);
        }
        match crate::xray_grpc::inbound_tags_until(api_address, Some(deadline)) {
            Ok(_) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(STARTUP_HEALTH_INTERVAL),
        }
    }
}

fn configured_socket_options(content: &str) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let config: serde_json::Value =
        serde_json::from_str(content).map_err(|error| error.to_string())?;
    let mut options = BTreeMap::new();
    for inbound in config
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(listen), Some(port)) = (
            inbound.get("listen").and_then(serde_json::Value::as_str),
            inbound
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok()),
        ) {
            options.insert(
                canonical_listener_address(listen, port)?,
                inbound
                    .pointer("/streamSettings/sockopt")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    Ok(options)
}

#[allow(clippy::too_many_arguments)]
fn rollback_config_handoff_before_switch(
    state_dir: &Path,
    runtime: &XrayUpgradeRuntime,
    old_manifest: &HandoffManifestIdentity,
    old_process: crate::ProcessRef,
    _old_status: &HandoffRuntimeStatus,
    candidate: Option<(&HandoffManifestIdentity, crate::ProcessRef)>,
    cause: &str,
) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Some((manifest, process)) = candidate {
        match handoff_status_rpc(manifest, process.pid) {
            Ok(status) if status.accepting || status.active != 0 => {
                let mut blocked = runtime.clone();
                blocked.phase = XrayUpgradePhase::Blocked;
                blocked.candidate = upgrade_process(
                    &status,
                    file_sha256_hex(&PathBuf::from(format!("/proc/{}/exe", process.pid)))?,
                )
                .ok();
                blocked.blocked_reason = Some(format!(
                    "配置交接失败：{cause}；候选已经接入或承载连接，拒绝破坏性回滚"
                ));
                write_upgrade_runtime(state_dir, &blocked)?;
                return Err(blocked.blocked_reason.unwrap_or_default());
            }
            Ok(status) => {
                let _ = handoff_rpc(
                    manifest,
                    process.pid,
                    "pause",
                    status.epoch.saturating_add(1),
                    &format!("config-{}-rollback-candidate", runtime.release_id),
                );
                if let Err(error) = crate::terminate_processes(
                    &[process],
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                ) {
                    failures.push(format!("清理候选失败：{error}"));
                }
            }
            Err(error) => failures.push(format!("候选状态不明，未清理：{error}")),
        }
    }
    let previous_path = state_dir.join(CONFIG_PREVIOUS_FILE);
    match fs::read_to_string(&previous_path)
        .map_err(|error| format!("读取上一份配置失败：{error}"))
        .and_then(|previous| crate::write_private(&state_dir.join("xray.json"), &previous))
    {
        Ok(()) => {}
        Err(error) => failures.push(error),
    }
    match handoff_status_rpc(old_manifest, old_process.pid).and_then(|status| {
        handoff_rpc(
            old_manifest,
            old_process.pid,
            "resume",
            status.epoch.saturating_add(1),
            &format!("config-{}-rollback-old", runtime.release_id),
        )
    }) {
        Ok(status) if status.accepting && status.ready => {}
        Ok(_) => failures.push("旧代回滚后没有恢复 accepting".to_owned()),
        Err(error) => failures.push(format!("恢复旧代 admission 失败：{error}")),
    }
    if failures.is_empty() {
        if let Ok(previous) = fs::read_to_string(&previous_path) {
            let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(5)));
            let api_address =
                crate::xray_api_address(&previous).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
            if let Some(error) =
                crate::restore_xray_runtime(state_dir, &previous, api_address).error()
            {
                failures.push(format!("旧代动态状态恢复失败：{error}"));
            }
        }
    }
    if failures.is_empty() {
        cleanup_file(&state_dir.join(UPGRADE_RUNTIME_FILE));
        cleanup_file(&previous_path);
        cleanup_file(&state_dir.join(CONFIG_GRANTS_FILE));
        Err(format!("配置交接未切流，旧代已恢复：{cause}"))
    } else {
        let mut blocked = runtime.clone();
        blocked.phase = XrayUpgradePhase::Blocked;
        blocked.blocked_reason = Some(format!("配置交接失败：{cause}；{}", failures.join("；")));
        write_upgrade_runtime(state_dir, &blocked)?;
        Err(blocked.blocked_reason.unwrap_or_default())
    }
}

fn perform_handoff_update_at_target(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
    target: &Path,
) -> Option<XrayReleaseReport> {
    if let Err(error) = preflight(candidate, offer, options) {
        return Some(failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            format!("候选 Xray 在交接前复检失败：{error}"),
        ));
    }
    if !xray_enabled(&options.state_dir) || !installed_xray_supports_handoff() {
        return Some(restart_handoff_fallback(
            options,
            meter,
            offer,
            candidate,
            target,
            "当前 Xray 未运行或缺少 tcp-handoff-udp-reset-v1 能力",
        ));
    }
    let candidate_version = match run_command_with_timeout(
        &candidate.to_string_lossy(),
        &["version"],
        Duration::from_secs(5),
    ) {
        Ok(version) => version,
        Err(error) => {
            return Some(failure_report(
                options,
                offer,
                pre_update_failure_outcome(options, offer),
                error,
            ))
        }
    };
    if !crate::xray_version_has_capability(&candidate_version, XRAY_HANDOFF_CAPABILITY) {
        return Some(restart_handoff_fallback(
            options,
            meter,
            offer,
            candidate,
            target,
            "候选 Xray 缺少 tcp-handoff-udp-reset-v1 能力",
        ));
    }
    let (old_process, old_manifest, old_status) =
        match observe_serving_handoff_generation(&options.state_dir) {
            Ok(generation)
                if generation.2.ready
                    && generation.2.accepting
                    && generation.2.retirement_supported =>
            {
                generation
            }
            Ok((_, _, status)) => {
                return Some(restart_handoff_fallback(
                    options,
                    meter,
                    offer,
                    candidate,
                    target,
                    &handoff_blocked_reason(&status),
                ))
            }
            Err(error) => {
                return Some(restart_handoff_fallback(
                    options,
                    meter,
                    offer,
                    candidate,
                    target,
                    &format!("当前 Xray 不具备真实 handoff 运行态：{error}"),
                ))
            }
        };
    let old_sha = match file_sha256_hex(&PathBuf::from(format!("/proc/{}/exe", old_process.pid))) {
        Ok(sha) if sha == offer.previous_sha256 => sha,
        Ok(sha) => {
            return Some(failure_report(
                options,
                offer,
                XrayReleaseOutcome::Unsupported,
                format!("运行中旧代摘要 {sha} 与发布冻结摘要不一致"),
            ))
        }
        Err(error) => {
            return Some(failure_report(
                options,
                offer,
                XrayReleaseOutcome::Unsupported,
                error,
            ))
        }
    };
    let backup = options.state_dir.join(PREVIOUS_FILE);
    if let Err(error) = prepare_previous(target, &backup, offer) {
        return Some(failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            error,
        ));
    }
    let timeout = Duration::from_secs(u64::from(
        offer
            .drain_timeout_secs
            .unwrap_or(XRAY_HANDOFF_DEFAULT_DRAIN_SECS),
    ));
    let deadline = Instant::now() + timeout;
    let deadline_unix = crate::current_unix_secs().ok().and_then(|now| {
        i64::try_from(timeout.as_secs())
            .ok()
            .and_then(|delta| now.checked_add(delta))
    });
    let old_upgrade = match upgrade_process(&old_status, old_sha) {
        Ok(process) => process,
        Err(error) => {
            return Some(failure_report(
                options,
                offer,
                XrayReleaseOutcome::Unsupported,
                error,
            ))
        }
    };
    let mut runtime = XrayUpgradeRuntime {
        source: XrayUpgradeSource::BinaryRelease,
        release_id: offer.release_id,
        attempt: offer.attempt,
        strategy: offer.strategy,
        phase: XrayUpgradePhase::Preparing,
        old: old_upgrade,
        candidate: None,
        drain_deadline_unix_secs: deadline_unix,
        blocked_reason: None,
    };
    if let Err(error) = write_upgrade_runtime(&options.state_dir, &runtime) {
        return Some(failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedRecovered,
            error,
        ));
    }

    // Forwarding and usage workers share this mutex. Hold it only across the admission switch so
    // no dynamic mutation can land on one generation halfway through pause/export/restore/resume.
    // The potentially long connection drain below deliberately runs without the lock.
    let switch_guard = meter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let pause_epoch = match old_status.epoch.checked_add(1) {
        Some(epoch) => epoch,
        None => {
            cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
            return Some(failure_report(
                options,
                offer,
                XrayReleaseOutcome::FailedRecovered,
                "旧代 fence epoch 溢出，未执行切换".to_owned(),
            ));
        }
    };
    let paused = match handoff_rpc(
        &old_manifest,
        old_process.pid,
        "pause",
        pause_epoch,
        &format!("release-{}-pause", offer.release_id),
    ) {
        Ok(status) if status.paused && !status.accepting => status,
        Ok(status) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &status,
                None,
                None,
                &backup,
                "旧代没有完成 pause barrier",
            ))
        }
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &old_status,
                None,
                None,
                &backup,
                &error,
            ))
        }
    };
    runtime.phase = XrayUpgradePhase::Switching;
    runtime.old.accepting = Some(false);
    runtime.old.active_connections = Some(paused.active);
    let _ = write_upgrade_runtime(&options.state_dir, &runtime);

    let export_epoch = match paused.epoch.checked_add(1) {
        Some(epoch) => epoch,
        None => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &paused,
                None,
                None,
                &backup,
                "旧代 export fence epoch 溢出",
            ))
        }
    };
    let (exported_status, exported) = match handoff_export_rpc(
        &old_manifest,
        old_process.pid,
        export_epoch,
        &format!("release-{}-export", offer.release_id),
    ) {
        Ok(exported) => exported,
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &paused,
                None,
                None,
                &backup,
                &error,
            ))
        }
    };
    let exported = match relocate_handoff_descriptors(&exported) {
        Ok(relocated) => relocated,
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                None,
                None,
                &backup,
                &error,
            ))
        }
    };
    let candidate_handoff =
        match prepare_candidate_handoff(&options.state_dir, candidate, &exported_status, &exported)
        {
            Ok(candidate) => candidate,
            Err(error) => {
                return Some(rollback_handoff_before_switch(
                    options,
                    offer,
                    &old_manifest,
                    old_process,
                    &exported_status,
                    None,
                    None,
                    &backup,
                    &error,
                ))
            }
        };
    let (candidate_status, candidate_process) =
        match launch_handoff_candidate(&options.state_dir, &candidate_handoff, &exported) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Some(rollback_handoff_before_switch(
                    options,
                    offer,
                    &old_manifest,
                    old_process,
                    &exported_status,
                    None,
                    None,
                    &backup,
                    &error,
                ))
            }
        };
    drop(exported);
    let candidate_sha = file_sha256_hex(&candidate_handoff.executable).unwrap_or_default();
    runtime.phase = XrayUpgradePhase::CandidateReady;
    runtime.candidate = upgrade_process(&candidate_status, candidate_sha).ok();
    let _ = write_upgrade_runtime(&options.state_dir, &runtime);
    #[cfg(test)]
    if std::env::var_os("BROCADE_TEST_HANDOFF_INTERRUPT_BEFORE_SWITCH").is_some() {
        return Some(failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedDirty,
            "test interruption before candidate admission".to_owned(),
        ));
    }

    let config = match fs::read_to_string(options.state_dir.join("xray.json")) {
        Ok(config) => config,
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                None,
                &backup,
                &format!("读取运行配置失败：{error}"),
            ))
        }
    };
    let api_address = crate::xray_api_address(&config).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
    let recovery = {
        let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(5)));
        crate::restore_xray_runtime(&options.state_dir, &config, api_address)
    };
    if let Some(error) = recovery.error() {
        let error = startup::root_failure(&candidate_handoff.manifest, Some(candidate_process))
            .unwrap_or_else(|| format!("候选动态业务状态没有完整恢复：{error}"));
        return Some(rollback_handoff_before_switch(
            options,
            offer,
            &old_manifest,
            old_process,
            &exported_status,
            Some((&candidate_handoff.manifest, candidate_process)),
            None,
            &backup,
            &error,
        ));
    }
    let seal_epoch = candidate_status.epoch.saturating_add(1);
    let sealed = match handoff_rpc(
        &candidate_handoff.manifest,
        candidate_process.pid,
        "seal",
        seal_epoch,
        &format!("release-{}-candidate-seal", offer.release_id),
    ) {
        Ok(status) if status.ready && status.retirement_supported && status.paused => status,
        Ok(_) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                None,
                &backup,
                "候选 seal 后状态不安全",
            ))
        }
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                None,
                &backup,
                &error,
            ))
        }
    };
    if let Err(error) = replace_executable(candidate, target)
        .and_then(|()| verify_xray_path(target, offer.sha256.as_str()))
    {
        return Some(rollback_handoff_before_switch(
            options,
            offer,
            &old_manifest,
            old_process,
            &exported_status,
            Some((&candidate_handoff.manifest, candidate_process)),
            Some(target),
            &backup,
            &format!("候选接入前替换受管路径失败：{error}"),
        ));
    }
    let resumed = match handoff_rpc(
        &candidate_handoff.manifest,
        candidate_process.pid,
        "resume",
        sealed.epoch.saturating_add(1),
        &format!("release-{}-candidate-resume", offer.release_id),
    ) {
        Ok(status) if status.accepting && status.ready && status.retirement_supported => status,
        Ok(_) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                Some(target),
                &backup,
                "候选没有进入 accepting 状态",
            ))
        }
        Err(error) => {
            return Some(rollback_handoff_before_switch(
                options,
                offer,
                &old_manifest,
                old_process,
                &exported_status,
                Some((&candidate_handoff.manifest, candidate_process)),
                Some(target),
                &backup,
                &error,
            ))
        }
    };
    if let Err(error) = drop_generation_udp(
        &old_manifest,
        old_process.pid,
        &exported_status,
        &format!("release-{}-drop-old-udp", offer.release_id),
    ) {
        runtime.phase = XrayUpgradePhase::Switching;
        if let Some(candidate) = runtime.candidate.as_mut() {
            candidate.accepting = Some(true);
            candidate.active_connections = Some(resumed.active);
        }
        return Some(handoff_dirty_failure(
            options,
            offer,
            &mut runtime,
            format!("新代已接入，但旧代 UDP 未能断开：{error}"),
        ));
    }
    runtime.phase = XrayUpgradePhase::Draining;
    if let Some(candidate) = runtime.candidate.as_mut() {
        candidate.accepting = Some(true);
        candidate.active_connections = Some(resumed.active);
    }
    let _ = write_upgrade_runtime(&options.state_dir, &runtime);
    #[cfg(test)]
    if std::env::var_os("BROCADE_TEST_HANDOFF_INTERRUPT_AFTER_SWITCH").is_some() {
        return Some(failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedDirty,
            "test interruption after candidate admission".to_owned(),
        ));
    }
    drop(switch_guard);

    let mut forced = false;
    let mut status = match handoff_status_rpc(&old_manifest, old_process.pid) {
        Ok(status) => status,
        Err(error) => {
            return Some(handoff_dirty_failure(
                options,
                offer,
                &mut runtime,
                format!("交接后无法确认旧代排空状态：{error}"),
            ))
        }
    };
    runtime.old.active_connections = Some(status.active);
    if !status.retire_ready && status.public_active == 0 {
        status = match handoff_rpc(
            &old_manifest,
            old_process.pid,
            "drain",
            status.epoch.saturating_add(1),
            &format!("release-{}-old-drain", offer.release_id),
        ) {
            Ok(status) => status,
            Err(error) => {
                return Some(handoff_dirty_failure(
                    options,
                    offer,
                    &mut runtime,
                    format!("旧代 public 已排空但 cover drain 失败：{error}"),
                ))
            }
        };
        runtime.old.active_connections = Some(status.active);
    }
    if !status.retire_ready {
        let expired = Instant::now() >= deadline;
        if expired && offer.strategy == XrayReleaseStrategy::HandoffForce {
            runtime.phase = XrayUpgradePhase::Forcing;
            forced = true;
            let _ = write_upgrade_runtime(&options.state_dir, &runtime);
        } else {
            runtime.phase = if expired {
                XrayUpgradePhase::Blocked
            } else {
                XrayUpgradePhase::Draining
            };
            runtime.blocked_reason = expired
                .then(|| format!("旧代仍有 {} 条连接，继续保留并等待自然排空", status.active));
            if let Err(error) = write_upgrade_runtime(&options.state_dir, &runtime) {
                return Some(handoff_dirty_failure(
                    options,
                    offer,
                    &mut runtime,
                    format!("平滑排空状态无法落盘：{error}"),
                ));
            }
            // The durable runtime is the release's in-progress result. Let the normal 15-second
            // convergence boundary advance it instead of occupying the Agent's owner thread for
            // the full drain interval.
            return None;
        }
    }

    let _meter = meter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) = crate::collect_usage_report(options) {
        return Some(handoff_dirty_failure(
            options,
            offer,
            &mut runtime,
            format!("旧代退役前共享计量未能落盘：{error}"),
        ));
    }
    if let Err(error) = crate::terminate_processes(
        &[old_process],
        crate::XRAY_TERM_GRACE,
        crate::XRAY_KILL_GRACE,
    ) {
        return Some(handoff_dirty_failure(options, offer, &mut runtime, error));
    }
    if let Err(error) =
        crate::accounting::promote_shared_writer(&options.state_dir, old_process, candidate_process)
    {
        return Some(handoff_dirty_failure(options, offer, &mut runtime, error));
    }
    if let Err(error) = crate::collect_usage_report(options) {
        return Some(handoff_dirty_failure(
            options,
            offer,
            &mut runtime,
            format!("新代接管计量后复核失败：{error}"),
        ));
    }
    let final_status = match handoff_status_rpc(&candidate_handoff.manifest, candidate_process.pid)
    {
        Ok(status) if status.accepting && status.ready && status.retirement_supported => status,
        Ok(_) => {
            return Some(handoff_dirty_failure(
                options,
                offer,
                &mut runtime,
                "旧代退役后候选不再 accepting".to_owned(),
            ))
        }
        Err(error) => return Some(handoff_dirty_failure(options, offer, &mut runtime, error)),
    };
    if final_status.generation_id != candidate_handoff.manifest.generation_id {
        return Some(handoff_dirty_failure(
            options,
            offer,
            &mut runtime,
            "最终服务代身份发生变化".to_owned(),
        ));
    }
    cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
    let mut report = success_report(
        offer,
        true,
        true,
        crate::installed_xray_sha256(),
        crate::running_xray_sha256(),
    );
    report.forced_retirement = forced;
    Some(report)
}

#[allow(clippy::too_many_arguments)]
fn rollback_handoff_before_switch(
    options: &Options,
    offer: &XrayReleaseOffer,
    old_manifest: &HandoffManifestIdentity,
    old_process: crate::ProcessRef,
    old_status: &HandoffRuntimeStatus,
    candidate: Option<(&HandoffManifestIdentity, crate::ProcessRef)>,
    replaced_target: Option<&Path>,
    backup: &Path,
    cause: &str,
) -> XrayReleaseReport {
    let mut failures = Vec::new();
    if let Some((manifest, process)) = candidate {
        match handoff_status_rpc(manifest, process.pid) {
            Ok(status) if status.accepting || status.active != 0 => {
                let mut runtime = XrayUpgradeRuntime {
                    source: XrayUpgradeSource::BinaryRelease,
                    release_id: offer.release_id,
                    attempt: offer.attempt,
                    strategy: offer.strategy,
                    phase: XrayUpgradePhase::Blocked,
                    old: upgrade_process(old_status, offer.previous_sha256.clone()).unwrap_or_else(
                        |_| XrayUpgradeProcess {
                            pid: old_process.pid,
                            pid_start_ticks: old_process.start_ticks,
                            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                                .unwrap_or_default()
                                .trim()
                                .to_owned(),
                            sha256: offer.previous_sha256.clone(),
                            alive: true,
                            accepting: Some(false),
                            active_connections: Some(old_status.active),
                        },
                    ),
                    candidate: upgrade_process(&status, offer.sha256.clone()).ok(),
                    drain_deadline_unix_secs: None,
                    blocked_reason: None,
                };
                return handoff_dirty_failure(
                    options,
                    offer,
                    &mut runtime,
                    format!("{cause}；候选已经接入或承载连接，拒绝破坏性回滚"),
                );
            }
            Ok(status) => {
                let _ = handoff_rpc(
                    manifest,
                    process.pid,
                    "pause",
                    status.epoch.saturating_add(1),
                    &format!("release-{}-rollback-candidate", offer.release_id),
                );
                if let Err(error) = crate::terminate_processes(
                    &[process],
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                ) {
                    failures.push(format!("清理候选失败：{error}"));
                }
            }
            Err(error) => failures.push(format!("候选状态不明，未清理：{error}")),
        }
    }
    if let Some(target) = replaced_target {
        if let Err(error) = replace_executable(backup, target)
            .and_then(|()| verify_xray_path(target, offer.previous_sha256.as_str()))
        {
            failures.push(format!("恢复旧受管二进制失败：{error}"));
        }
    }
    match handoff_status_rpc(old_manifest, old_process.pid).and_then(|status| {
        handoff_rpc(
            old_manifest,
            old_process.pid,
            "resume",
            status.epoch.saturating_add(1),
            &format!("release-{}-rollback-old", offer.release_id),
        )
    }) {
        Ok(status) if status.accepting && status.ready => {}
        Ok(_) => failures.push("旧代回滚后没有恢复 accepting".to_owned()),
        Err(error) => failures.push(format!("恢复旧代 admission 失败：{error}")),
    }
    if failures.is_empty() {
        cleanup_file(&options.state_dir.join(UPGRADE_RUNTIME_FILE));
        failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedRecovered,
            format!("平滑交接未切流，旧代已恢复：{cause}"),
        )
    } else {
        // A live but unreachable candidate must remain fenced after a failed rollback. The
        // guard owns recovery from this durable identity; deleting it strands the paused old
        // generation and loses the only safe way to clean up the candidate.
        if let Ok(Some(mut runtime)) = read_upgrade_runtime(&options.state_dir) {
            runtime.phase = XrayUpgradePhase::Blocked;
            runtime.blocked_reason = Some(format!(
                "平滑交接回滚尚未完成：{cause}；{}",
                failures.join("；")
            ));
            if let Err(error) = write_upgrade_runtime(&options.state_dir, &runtime) {
                failures.push(format!("保存待恢复交接失败：{error}"));
            }
        }
        failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedDirty,
            format!(
                "平滑交接失败，保留检查点等待自动恢复：{cause}；{}",
                failures.join("；")
            ),
        )
    }
}

fn handoff_dirty_failure(
    options: &Options,
    offer: &XrayReleaseOffer,
    runtime: &mut XrayUpgradeRuntime,
    error: String,
) -> XrayReleaseReport {
    runtime.phase = XrayUpgradePhase::Blocked;
    runtime.blocked_reason = Some(error.clone());
    let persisted = write_upgrade_runtime(&options.state_dir, runtime).err();
    failure_report(
        options,
        offer,
        XrayReleaseOutcome::FailedDirty,
        match persisted {
            Some(persisted) => format!("{error}；双进程状态落盘也失败：{persisted}"),
            None => error,
        },
    )
}

fn restart_handoff_fallback(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
    target: &Path,
    reason: &str,
) -> XrayReleaseReport {
    match read_upgrade_runtime(&options.state_dir) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return failure_report(
                options,
                offer,
                XrayReleaseOutcome::FailedDirty,
                "已有双进程交接，拒绝重启降级或创建第三代进程".to_owned(),
            )
        }
        Err(error) => {
            return failure_report(options, offer, XrayReleaseOutcome::FailedDirty, error)
        }
    }
    eprintln!("xray-update: 平滑条件不足，将降级为直接重启，现有连接将中断：{reason}");
    let mut report = perform_update_at_target(options, meter, offer, candidate, target);
    let outcome = if report.outcome == XrayReleaseOutcome::Succeeded {
        "已完成"
    } else {
        "未完成"
    };
    report.error = Some(format!(
        "平滑条件不足，降级直接重启{outcome}：{reason}{}",
        report
            .error
            .as_ref()
            .map(|error| format!("；{error}"))
            .unwrap_or_default()
    ));
    report
}

fn perform_update_at_target(
    options: &Options,
    meter: &Arc<Mutex<()>>,
    offer: &XrayReleaseOffer,
    candidate: &Path,
    target: &Path,
) -> XrayReleaseReport {
    let enabled = xray_enabled(&options.state_dir);
    let installed = file_sha256_hex(target).ok();
    let running = crate::running_xray_sha256();
    if installed.as_deref() == Some(offer.sha256.as_str())
        && (!enabled || running.as_deref() == Some(offer.sha256.as_str()))
    {
        if enabled {
            let _meter = meter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return match restore_current_runtime(options, offer.sha256.as_str()) {
                Ok(recovery) if recovery.is_complete() => {
                    success_report(offer, false, true, installed, running)
                }
                Ok(recovery) => report_with_state(
                    offer,
                    XrayReleaseOutcome::FailedDirty,
                    false,
                    true,
                    installed,
                    running,
                    Some(format!(
                        "Xray 版本已安装且正在运行，但动态业务状态尚未完整恢复：{}",
                        recovery.error().unwrap_or_default()
                    )),
                ),
                Err(error) => report_with_state(
                    offer,
                    XrayReleaseOutcome::FailedDirty,
                    false,
                    true,
                    installed,
                    running,
                    Some(format!("Xray 版本已运行，但业务健康复核失败：{error}")),
                ),
            };
        }
        return success_report(offer, false, enabled, installed, running);
    }

    // This candidate may have waited while a configuration deployment converged. Test the current
    // file again at the boundary so an incompatible binary never reaches the rename merely because
    // it was compatible with the configuration from ten minutes ago.
    if let Err(error) = preflight(candidate, offer, options) {
        return failure_report(
            options,
            offer,
            pre_update_failure_outcome(options, offer),
            format!("候选 Xray 在替换前复检失败，没有替换：{error}"),
        );
    }
    let backup = options.state_dir.join(PREVIOUS_FILE);
    if let Err(error) = prepare_previous(target, &backup, offer) {
        let outcome = classify_failure(
            enabled,
            installed.as_deref(),
            running.as_deref(),
            &offer.previous_sha256,
        );
        return report_with_state(
            offer,
            outcome,
            false,
            enabled,
            installed,
            running,
            Some(error),
        );
    }

    if !enabled {
        let attempted = replace_executable(candidate, target)
            .and_then(|()| verify_xray_path(target, offer.sha256.as_str()));
        return match attempted {
            Ok(()) => success_report(
                offer,
                true,
                false,
                file_sha256_hex(target).ok(),
                crate::running_xray_sha256(),
            ),
            Err(update_error) => {
                let recovered = replace_executable(&backup, target)
                    .and_then(|()| verify_xray_path(target, offer.previous_sha256.as_str()));
                match recovered {
                    Ok(()) => failure_report(
                        options,
                        offer,
                        XrayReleaseOutcome::FailedRecovered,
                        format!("新 Xray 未能完整落盘，已恢复旧版本：{update_error}"),
                    ),
                    Err(recovery_error) => failure_report(
                        options,
                        offer,
                        XrayReleaseOutcome::FailedDirty,
                        format!(
                            "新 Xray 更新失败：{update_error}；恢复旧版本也失败：{recovery_error}"
                        ),
                    ),
                }
            }
        };
    }

    let _meter = meter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) = crate::collect_usage_report(options) {
        return failure_report(
            options,
            offer,
            XrayReleaseOutcome::FailedRecovered,
            format!("重启前用量未能可靠落盘，没有替换 Xray：{error}"),
        );
    }
    let attempted = replace_executable(candidate, target)
        .and_then(|()| start_and_restore_runtime(options, offer.sha256.as_str()));
    let recovery = match attempted {
        Ok(recovery) => recovery,
        Err(update_error) => {
            let recovered = replace_executable(&backup, target)
                .and_then(|()| start_and_restore_runtime(options, offer.previous_sha256.as_str()));
            return match recovered {
                Ok(recovery) if recovery.is_complete() => report_with_state(
                    offer,
                    XrayReleaseOutcome::FailedRecovered,
                    false,
                    true,
                    crate::installed_xray_sha256(),
                    crate::running_xray_sha256(),
                    Some(format!(
                        "新 Xray 未通过启动检查，已恢复旧版本：{update_error}"
                    )),
                ),
                Ok(recovery) => report_with_state(
                    offer,
                    XrayReleaseOutcome::FailedDirty,
                    false,
                    true,
                    crate::installed_xray_sha256(),
                    crate::running_xray_sha256(),
                    Some(format!(
                        "新 Xray 更新失败；旧版本已重新启动，但动态业务状态尚未完整恢复：{update_error}；{}",
                        recovery.error().unwrap_or_default()
                    )),
                ),
                Err(recovery_error) => report_with_state(
                    offer,
                    XrayReleaseOutcome::FailedDirty,
                    false,
                    true,
                    crate::installed_xray_sha256(),
                    crate::running_xray_sha256(),
                    Some(format!(
                        "新 Xray 更新失败：{update_error}；恢复旧版本也失败：{recovery_error}"
                    )),
                ),
            };
        }
    };
    if let Some(error) = recovery.error() {
        return report_with_state(
            offer,
            XrayReleaseOutcome::FailedDirty,
            true,
            true,
            crate::installed_xray_sha256(),
            crate::running_xray_sha256(),
            Some(format!(
                "新 Xray 已运行并保留已恢复业务，但动态状态尚未完整：{error}"
            )),
        );
    }
    success_report(
        offer,
        true,
        true,
        crate::installed_xray_sha256(),
        crate::running_xray_sha256(),
    )
}

fn start_and_restore_runtime(
    options: &Options,
    expected_sha256: &str,
) -> Result<crate::XrayRuntimeRecovery, String> {
    let config = options.state_dir.join("xray.json");
    let content =
        fs::read_to_string(&config).map_err(|error| format!("读取当前 Xray 配置失败：{error}"))?;
    let api_address = crate::xray_api_address(&content).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
    let recovery = crate::apply_xray(&config, api_address)?;
    // The API listener can accept before the remaining inbound handlers finish starting. Keep
    // the all-listeners gate, but do not roll back a valid binary on that transient snapshot.
    wait_for_startup_health(STARTUP_HEALTH_TIMEOUT, || {
        crate::xray_runtime(&options.state_dir, &content)
    })?;
    verify_installed_xray(expected_sha256)?;
    let running =
        crate::running_xray_sha256().ok_or_else(|| "Xray 启动后读不到运行中摘要".to_owned())?;
    if running != expected_sha256 {
        return Err(format!(
            "Xray 启动后运行的是 {running}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(recovery)
}

fn restore_current_runtime(
    options: &Options,
    expected_sha256: &str,
) -> Result<crate::XrayRuntimeRecovery, String> {
    let config = options.state_dir.join("xray.json");
    let content =
        fs::read_to_string(&config).map_err(|error| format!("读取当前 Xray 配置失败：{error}"))?;
    wait_for_startup_health(STARTUP_HEALTH_TIMEOUT, || {
        crate::xray_runtime(&options.state_dir, &content)
    })?;
    verify_installed_xray(expected_sha256)?;
    let running = crate::running_xray_sha256().ok_or_else(|| "Xray 运行中摘要不可读".to_owned())?;
    if running != expected_sha256 {
        return Err(format!(
            "Xray 运行中摘要是 {running}，不是预期的 {expected_sha256}"
        ));
    }
    let api_address = crate::xray_api_address(&content).unwrap_or(crate::DEFAULT_XRAY_API_ADDRESS);
    Ok(crate::restore_xray_runtime(
        &options.state_dir,
        &content,
        api_address,
    ))
}

fn wait_for_startup_health(
    timeout: Duration,
    mut check: impl FnMut() -> WorkloadRuntime,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let error = match check() {
            WorkloadRuntime::Healthy => return Ok(()),
            WorkloadRuntime::Broken(error) | WorkloadRuntime::Unknown(error) => error,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Xray 启动后健康检查失败：{error}"));
        }
        thread::sleep(STARTUP_HEALTH_INTERVAL.min(remaining));
    }
}

fn verify_installed_xray(expected_sha256: &str) -> Result<(), String> {
    let installed =
        crate::installed_xray_sha256().ok_or_else(|| "读不到受管路径上的 Xray 摘要".to_owned())?;
    if installed != expected_sha256 {
        return Err(format!(
            "受管路径上的 Xray 是 {installed}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(())
}

fn verify_xray_path(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let installed = file_sha256_hex(path)?;
    if installed != expected_sha256 {
        return Err(format!(
            "受管路径上的 Xray 是 {installed}，不是预期的 {expected_sha256}"
        ));
    }
    Ok(())
}

fn managed_target() -> Result<PathBuf, String> {
    let target = crate::which_xray()?;
    if !target.is_absolute() {
        return Err(format!(
            "Xray 发布需要绝对受管路径，当前解析为 {}",
            target.display()
        ));
    }
    Ok(target)
}

fn validate_managed_target(target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| format!("读取受管 Xray {} 失败：{error}", target.display()))?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "受管 Xray 必须是 Agent 账号所有、组和其他用户不可写的普通文件，且不能是符号链接：{}",
            target.display()
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("受管 Xray 没有上级目录：{}", target.display()))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("读取 Xray 目录 {} 失败：{error}", parent.display()))?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != effective_uid
        || parent_metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "Xray 所在目录必须由 Agent 账号所有且不能被组或其他用户写入：{}",
            parent.display()
        ));
    }
    Ok(())
}

fn prepare_previous(target: &Path, backup: &Path, offer: &XrayReleaseOffer) -> Result<(), String> {
    if file_sha256_hex(backup).ok().as_deref() == Some(offer.previous_sha256.as_str()) {
        return Ok(());
    }
    cleanup_file(backup);
    let installed = file_sha256_hex(target).ok();
    let source = if installed.as_deref() == Some(offer.previous_sha256.as_str()) {
        target.to_path_buf()
    } else if crate::running_xray_sha256().as_deref() == Some(offer.previous_sha256.as_str()) {
        crate::serving_xray_executable_path()
            .ok_or_else(|| "运行中的旧 Xray 在备份前消失了".to_owned())?
    } else {
        return Err("没有与发布前摘要一致的旧 Xray 可供安全恢复".to_owned());
    };
    copy_atomic(&source, backup, 0o700)?;
    let copied = file_sha256_hex(backup)?;
    if copied != offer.previous_sha256 {
        cleanup_file(backup);
        return Err(format!(
            "旧 Xray 备份摘要不符：需要 {}，实际 {copied}",
            offer.previous_sha256
        ));
    }
    Ok(())
}

fn valid_previous_backup(options: &Options, offer: &XrayReleaseOffer) -> bool {
    file_sha256_hex(&options.state_dir.join(PREVIOUS_FILE))
        .ok()
        .as_deref()
        == Some(offer.previous_sha256.as_str())
}

fn replace_executable(source: &Path, destination: &Path) -> Result<(), String> {
    copy_atomic(source, destination, 0o755)
}

fn copy_atomic(source: &Path, destination: &Path, mode: u32) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{} 没有上级目录", destination.display()))?;
    let name = destination
        .file_name()
        .ok_or_else(|| format!("{} 没有文件名", destination.display()))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.brocade-new-{}", std::process::id()));
    cleanup_file(&temporary);
    let result = (|| {
        let mut input = File::open(source)
            .map_err(|error| format!("打开 {} 失败：{error}", source.display()))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| format!("创建 {} 失败：{error}", temporary.display()))?;
        io::copy(&mut input, &mut output)
            .map_err(|error| format!("写入 {} 失败：{error}", temporary.display()))?;
        output
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|error| format!("chmod {} 失败：{error}", temporary.display()))?;
        output
            .sync_all()
            .map_err(|error| format!("fsync {} 失败：{error}", temporary.display()))?;
        drop(output);
        fs::rename(&temporary, destination).map_err(|error| {
            format!(
                "原子替换 {} 为 {} 失败：{error}",
                temporary.display(),
                destination.display()
            )
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("fsync 目录 {} 失败：{error}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        cleanup_file(&temporary);
    }
    result
}

fn xray_enabled(state_dir: &Path) -> bool {
    state_dir.join("xray.json").exists() && !state_dir.join("xray.disabled").exists()
}

fn success_report(
    offer: &XrayReleaseOffer,
    performed_update: bool,
    enabled: bool,
    installed: Option<String>,
    running: Option<String>,
) -> XrayReleaseReport {
    report_with_state(
        offer,
        XrayReleaseOutcome::Succeeded,
        performed_update,
        enabled,
        installed,
        running,
        None,
    )
}

fn failure_report(
    options: &Options,
    offer: &XrayReleaseOffer,
    outcome: XrayReleaseOutcome,
    error: String,
) -> XrayReleaseReport {
    report_with_state(
        offer,
        outcome,
        false,
        xray_enabled(&options.state_dir),
        crate::installed_xray_sha256(),
        crate::running_xray_sha256(),
        Some(error),
    )
}

fn pre_update_failure_outcome(options: &Options, offer: &XrayReleaseOffer) -> XrayReleaseOutcome {
    let enabled = xray_enabled(&options.state_dir);
    let installed = crate::installed_xray_sha256();
    let running = crate::running_xray_sha256();
    classify_failure(
        enabled,
        installed.as_deref(),
        running.as_deref(),
        &offer.previous_sha256,
    )
}

/// “已恢复” describes the durable managed path, not merely an old process whose inode is still
/// alive after that path changed. The latter is exactly the dirty state operators must see: the
/// next restart would otherwise silently start bytes different from the frozen baseline.
fn classify_failure(
    enabled: bool,
    installed: Option<&str>,
    running: Option<&str>,
    previous_sha256: &str,
) -> XrayReleaseOutcome {
    if installed == Some(previous_sha256) && (!enabled || running == Some(previous_sha256)) {
        XrayReleaseOutcome::FailedRecovered
    } else {
        XrayReleaseOutcome::FailedDirty
    }
}

fn report_with_state(
    offer: &XrayReleaseOffer,
    outcome: XrayReleaseOutcome,
    performed_update: bool,
    enabled: bool,
    installed: Option<String>,
    running: Option<String>,
    error: Option<String>,
) -> XrayReleaseReport {
    XrayReleaseReport {
        release_id: offer.release_id,
        attempt: offer.attempt,
        outcome,
        performed_update,
        strategy: offer.strategy,
        forced_retirement: false,
        xray_enabled: enabled,
        installed_sha256: installed,
        running_sha256: running,
        error: error.map(|error| error.chars().take(4_000).collect()),
    }
}

fn finish_report(options: &Options, report: XrayReleaseReport) -> Result<(), String> {
    write_stored_report(&options.state_dir, REPORT_FILE, report)?;
    retry_stored_report(options)
}

fn write_stored_report(
    state_dir: &Path,
    name: &str,
    report: XrayReleaseReport,
) -> Result<(), String> {
    let encoded =
        serde_json::to_vec(&StoredReport { report }).map_err(|error| error.to_string())?;
    let _state = REPORT_STATE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::fsutil::atomic_write_private(&state_dir.join(name), &encoded)
}

fn retry_stored_report(options: &Options) -> Result<(), String> {
    let _retry = REPORT_RETRY_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for name in [
        crash_recovery::FAILURE_FILE,
        REPORT_FILE,
        crash_recovery::RECOVERED_REPORT_FILE,
    ] {
        retry_report_file(options, name)?;
    }
    Ok(())
}

fn retry_report_file(options: &Options, name: &str) -> Result<(), String> {
    let path = options.state_dir.join(name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取待补发 Xray 更新结果失败：{error}")),
    };
    let stored: StoredReport = serde_json::from_slice(&bytes)
        .map_err(|error| format!("读不懂待补发的 Xray 更新结果：{error}"))?;
    let _accepted = send_report(options, &stored.report)?;
    let _state = REPORT_STATE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A local recovery may publish a newer receipt while the old request is in flight.
    // Removing only the acknowledged bytes prevents that newer result from disappearing.
    if fs::read(&path).is_ok_and(|current| current == bytes) {
        cleanup_file(&path);
        cleanup_after_report(options, stored.report.outcome);
    }
    Ok(())
}

fn cleanup_after_report(options: &Options, outcome: XrayReleaseOutcome) {
    cleanup_file(&options.state_dir.join(DOWNLOAD_FILE));
    cleanup_file(&options.state_dir.join(CANDIDATE_FILE));
    // A dirty failure deliberately retains the frozen old binary for manual recovery. Every
    // other terminal result says either that the new process is healthy or that the managed path
    // contains the old bytes again, so keeping another executable indefinitely buys nothing.
    if outcome != XrayReleaseOutcome::FailedDirty {
        cleanup_file(&options.state_dir.join(PREVIOUS_FILE));
    }
}

fn send_report(options: &Options, report: &XrayReleaseReport) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/xray-release/report",
        &options.token,
        Some(&body),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "Xray 更新结果上报失败：HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str::<ReportAck>(&response.body)
        .map(|ack| ack.accepted)
        .map_err(|error| format!("Xray 更新结果响应读不懂：{error}"))
}

fn cleanup_file(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_grants_test_runtime() -> XrayUpgradeRuntime {
        retirement_test_runtime(
            XrayUpgradeSource::ConfigDeployment {
                deployment_id: 604,
                claim_generation: 7,
            },
            XrayReleaseStrategy::HandoffForce,
        )
    }

    fn grants_fixture(tag: &str, email: &str) -> GrantInbound {
        GrantInbound {
            tag: tag.to_owned(),
            clients: vec![brocade_deployment::plan::GrantClient {
                email: email.to_owned(),
                uuid: "00000000-0000-4000-8000-000000000001".to_owned(),
                flow: None,
                level: 0,
            }],
        }
    }

    #[test]
    fn config_candidate_grants_do_not_fall_back_to_formal_authorization() {
        let dir = temp_dir("candidate-grants-source");
        crate::fsutil::atomic_write_private(
            &dir.join("grants.desired.json"),
            &serde_json::to_vec(&vec![grants_fixture("old-inbound", "old-user")]).unwrap(),
        )
        .unwrap();
        let runtime = config_grants_test_runtime();
        assert!(candidate_grants_for_recovery(&dir, &runtime).is_err());
        crate::fsutil::atomic_write_private(&dir.join(CONFIG_GRANTS_FILE), b"null").unwrap();
        assert!(candidate_grants_for_recovery(&dir, &runtime).is_err());
        crate::fsutil::atomic_write_private(&dir.join(CONFIG_GRANTS_FILE), b"[]").unwrap();
        assert_eq!(
            candidate_grants_for_recovery(&dir, &runtime).unwrap(),
            Some(vec![])
        );
        let wanted = vec![grants_fixture("new-inbound", "new-user")];
        crate::fsutil::atomic_write_private(
            &dir.join(CONFIG_GRANTS_FILE),
            &serde_json::to_vec(&wanted).unwrap(),
        )
        .unwrap();
        assert_eq!(
            candidate_grants_for_recovery(&dir, &runtime).unwrap(),
            Some(wanted)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn binary_candidate_grants_keep_the_existing_recovery_source() {
        let dir = temp_dir("binary-candidate-grants");
        crate::fsutil::atomic_write_private(&dir.join(CONFIG_GRANTS_FILE), b"invalid snapshot")
            .unwrap();
        let runtime = retirement_test_runtime(
            XrayUpgradeSource::BinaryRelease,
            XrayReleaseStrategy::Handoff,
        );
        assert_eq!(candidate_grants_for_recovery(&dir, &runtime).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_candidate_grants_reject_unsafe_files_without_logging_credentials() {
        let dir = temp_dir("candidate-grants-private");
        let path = dir.join(CONFIG_GRANTS_FILE);
        let malformed = br#"[{"tag":"new","clients":"sensitive-fixture-value"}]"#;
        crate::fsutil::atomic_write_private(&path, malformed).unwrap();
        let error = read_config_handoff_grants(&dir).unwrap_err();
        assert!(!error.contains("sensitive-fixture-value"));
        crate::fsutil::atomic_write_private(&path, b"[]").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_config_handoff_grants(&dir).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = dir.join("alias");
        fs::hard_link(&path, &alias).unwrap();
        assert!(read_config_handoff_grants(&dir).is_err());
        fs::remove_file(&alias).unwrap();
        fs::rename(&path, &alias).unwrap();
        std::os::unix::fs::symlink(&alias, &path).unwrap();
        assert!(read_config_handoff_grants(&dir).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&alias, &path).unwrap();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(crate::MAX_CONFIG_CONVERGENCE_BYTES + 1)
            .unwrap();
        assert!(read_config_handoff_grants(&dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_candidate_grants_freeze_unmanaged_authorization_before_pause() {
        let dir = temp_dir("candidate-grants-freeze");
        let config = r#"{"inbounds":[{"tag":"kept"}]}"#;
        let kept = grants_fixture("kept", "kept-user");
        let previous = vec![kept.clone(), grants_fixture("removed", "removed-user")];
        crate::fsutil::atomic_write_private(
            &dir.join("grants.desired.json"),
            &serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        let frozen = freeze_config_handoff_grants(&dir, config, None).unwrap();
        assert_eq!(frozen, vec![kept]);
        assert_eq!(
            freeze_config_handoff_grants(&dir, config, Some(&[])).unwrap(),
            vec![]
        );
        crate::fsutil::atomic_write_private(
            &dir.join(CONFIG_GRANTS_FILE),
            &serde_json::to_vec(&frozen).unwrap(),
        )
        .unwrap();
        crate::fsutil::atomic_write_private(&dir.join("grants.desired.json"), b"[]").unwrap();
        assert_eq!(
            candidate_grants_for_recovery(&dir, &config_grants_test_runtime()).unwrap(),
            Some(frozen)
        );
        let _ = fs::remove_dir_all(dir);
    }

    fn retirement_test_runtime(
        source: XrayUpgradeSource,
        strategy: XrayReleaseStrategy,
    ) -> XrayUpgradeRuntime {
        XrayUpgradeRuntime {
            source,
            release_id: 1,
            attempt: 0,
            strategy,
            phase: XrayUpgradePhase::Blocked,
            old: XrayUpgradeProcess {
                pid: 101,
                pid_start_ticks: 1_001,
                boot_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
                sha256: "a".repeat(64),
                alive: true,
                accepting: Some(false),
                active_connections: Some(3),
            },
            candidate: Some(XrayUpgradeProcess {
                pid: 202,
                pid_start_ticks: 1_002,
                boot_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
                sha256: "b".repeat(64),
                alive: true,
                accepting: Some(true),
                active_connections: Some(1),
            }),
            drain_deadline_unix_secs: Some(1),
            blocked_reason: Some("test".to_owned()),
        }
    }

    #[test]
    fn retired_generation_cleanup_is_bounded_and_preserves_unknown_entries() {
        let dir = temp_dir("retired-generation-cleanup");
        let root = dir.join(HANDOFF_DIR);
        crate::create_private_dir(&root).unwrap();
        crate::create_private_dir(&dir.join("logs")).unwrap();
        for index in 0..6 {
            let name = format!("brocade-{index:032x}");
            let generation = root.join(&name);
            crate::create_private_dir(&generation).unwrap();
            let manifest = HandoffManifestIdentity {
                version: 2,
                scope: "whole-generation".to_owned(),
                generation_id: name.clone(),
                control_path: generation.join("control.sock"),
                listeners: vec![],
            };
            crate::fsutil::atomic_write_private(
                &generation.join("manifest.json"),
                &serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            crate::write_private(&generation.join("xray"), "synthetic retired bytes").unwrap();
            crate::write_private(
                &dir.join("logs").join(format!("xray-{name}.log")),
                "synthetic log",
            )
            .unwrap();
            let log = dir.join("logs").join(format!("xray-{name}.log"));
            crate::write_private(&crate::logcap::archive_path(&log), "synthetic archive").unwrap();
        }
        fs::create_dir(root.join("unrelated")).unwrap();
        let mut active = config_grants_test_runtime();
        active.release_id = -604;
        super::write_upgrade_runtime(&dir, &active).unwrap();
        prune_retired_generations(&dir).unwrap();
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            7,
            "active handoff must fence cleanup"
        );
        fs::remove_file(dir.join(UPGRADE_RUNTIME_FILE)).unwrap();
        prune_retired_generations(&dir).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
        assert!(root.join("unrelated").is_dir());
        assert_eq!(fs::read_dir(dir.join("logs")).unwrap().count(), 4);
        prune_retired_generations(&dir).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configuration_handoffs_force_only_after_the_drain_deadline() {
        let runtime = retirement_test_runtime(
            XrayUpgradeSource::ConfigDeployment {
                deployment_id: 604,
                claim_generation: 7,
            },
            // Protocol-v32 persisted this older strategy. Protocol-v33 must still retire it
            // automatically before it can claim a later configuration deployment.
            XrayReleaseStrategy::Handoff,
        );
        assert!(!should_force_handoff_retirement(&runtime, false, false));
        assert!(should_force_handoff_retirement(&runtime, false, true));
        assert!(should_force_handoff_retirement(&runtime, true, false));
    }

    #[test]
    fn binary_handoff_keeps_its_explicit_retirement_policy() {
        let graceful = retirement_test_runtime(
            XrayUpgradeSource::BinaryRelease,
            XrayReleaseStrategy::Handoff,
        );
        assert!(!should_force_handoff_retirement(&graceful, true, false));
        assert!(should_force_handoff_retirement(&graceful, true, true));

        let forced = retirement_test_runtime(
            XrayUpgradeSource::BinaryRelease,
            XrayReleaseStrategy::HandoffForce,
        );
        assert!(should_force_handoff_retirement(&forced, true, false));
    }

    #[test]
    fn config_handoff_recovery_restores_the_previous_config_checkpoint() {
        let dir = temp_dir("config-handoff-restore");
        crate::fsutil::atomic_write_private(&dir.join("xray.json"), b"desired").unwrap();
        crate::fsutil::atomic_write_private(&dir.join(CONFIG_PREVIOUS_FILE), b"previous").unwrap();
        let runtime = XrayUpgradeRuntime {
            source: XrayUpgradeSource::ConfigDeployment {
                deployment_id: 603,
                claim_generation: 4,
            },
            release_id: -603,
            attempt: 0,
            strategy: XrayReleaseStrategy::Handoff,
            phase: XrayUpgradePhase::RollingBack,
            old: XrayUpgradeProcess {
                pid: 101,
                pid_start_ticks: 8_001,
                boot_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
                sha256: "a".repeat(64),
                alive: true,
                accepting: Some(false),
                active_connections: Some(0),
            },
            candidate: None,
            drain_deadline_unix_secs: None,
            blocked_reason: None,
        };

        restore_old_handoff_state(&dir, &runtime).unwrap();
        assert_eq!(fs::read(dir.join("xray.json")).unwrap(), b"previous");
        cleanup_handoff_backup(&dir, &runtime);
        assert!(!dir.join(CONFIG_PREVIOUS_FILE).exists());
        let _ = fs::remove_dir_all(dir);
    }

    fn bind_test_seqpacket(path: &Path) -> OwnedFd {
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        assert!(bytes.len() < address.sun_path.len());
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        let fd =
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
        assert!(fd >= 0, "socket: {}", io::Error::last_os_error());
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let address_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
            as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    &address as *const _ as *const libc::sockaddr,
                    address_len,
                )
            },
            0,
            "bind: {}",
            io::Error::last_os_error()
        );
        assert_eq!(unsafe { libc::listen(fd.as_raw_fd(), 1) }, 0);
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        fd
    }

    #[test]
    fn exported_listener_descriptors_move_out_of_shell_fd_range() {
        let mut pair = [-1_i32; 2];
        assert_eq!(
            unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "pipe2: {}",
            io::Error::last_os_error()
        );
        let exported = unsafe { OwnedFd::from_raw_fd(pair[0]) };
        let peer = unsafe { OwnedFd::from_raw_fd(pair[1]) };

        let relocated = relocate_handoff_descriptors(&[exported]).unwrap();
        assert_eq!(relocated.len(), 1);
        assert!(relocated[0].as_raw_fd() >= HANDOFF_CHILD_FD_MIN);
        let payload = b"listener-open-file-description";
        assert_eq!(
            unsafe { libc::write(peer.as_raw_fd(), payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let mut received = [0_u8; 30];
        assert_eq!(
            unsafe {
                libc::read(
                    relocated[0].as_raw_fd(),
                    received.as_mut_ptr().cast(),
                    received.len(),
                )
            },
            payload.len() as isize
        );
        assert_eq!(&received[..payload.len()], payload);
    }

    #[test]
    fn handoff_status_rpc_requires_the_exact_private_process_endpoint() {
        let dir = temp_dir("handoff-status-rpc");
        let control_path = dir.join("control.sock");
        let listener = bind_test_seqpacket(&control_path);
        let generation_id = "generation-test-0001".to_owned();
        let server_generation = generation_id.clone();
        // Process identity includes the executable digest. Compute it before accepting the RPC so
        // the fixture itself cannot consume the client's deliberately short control timeout.
        let server_process = current_process();
        let server = thread::spawn(move || {
            let client = unsafe {
                libc::accept4(
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            assert!(client >= 0, "accept: {}", io::Error::last_os_error());
            let client = unsafe { OwnedFd::from_raw_fd(client) };
            let mut request = [0_u8; 4096];
            let received = unsafe {
                libc::recv(
                    client.as_raw_fd(),
                    request.as_mut_ptr().cast(),
                    request.len(),
                    0,
                )
            };
            assert!(received > 0);
            let request: serde_json::Value =
                serde_json::from_slice(&request[..received as usize]).unwrap();
            assert_eq!(request["operation"], "status");
            assert_eq!(request["generation_id"], server_generation);
            let response = serde_json::to_vec(&serde_json::json!({
                "status": {
                    "scope": "whole-generation",
                    "retirement_supported": true,
                    "generation_id": server_generation,
                    "pid": server_process.pid,
                    "boot_id": server_process.boot_id,
                    "pid_start_ticks": server_process.pid_start_ticks.to_string(),
                    "ready": true,
                    "accepting": true
                }
            }))
            .unwrap();
            assert_eq!(
                unsafe {
                    libc::send(
                        client.as_raw_fd(),
                        response.as_ptr().cast(),
                        response.len(),
                        libc::MSG_NOSIGNAL,
                    )
                },
                response.len() as isize
            );
        });

        let manifest = HandoffManifestIdentity {
            version: 2,
            scope: "whole-generation".to_owned(),
            generation_id,
            control_path,
            listeners: Vec::new(),
        };
        let status = handoff_status_rpc(&manifest, std::process::id() as i32).unwrap();
        assert!(status.retirement_supported && status.ready && status.accepting);
        assert_eq!(status.scope, "whole-generation");
        server.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn handoff_control_connect_timeout_includes_a_full_accept_backlog() {
        let dir = temp_dir("handoff-connect-timeout");
        let control_path = dir.join("control.sock");
        let _listener = bind_test_seqpacket(&control_path);
        let mut connections = Vec::new();
        // Linux accepts backlog + 1 queued Unix connections. Never accept any of them.
        for _ in 0..2 {
            let path = control_path.as_os_str().as_encoded_bytes();
            let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (slot, byte) in address.sun_path.iter_mut().zip(path) {
                *slot = *byte as libc::c_char;
            }
            let fd = unsafe {
                libc::socket(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    0,
                )
            };
            assert!(fd >= 0);
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            assert_eq!(
                unsafe {
                    libc::connect(
                        fd.as_raw_fd(),
                        &address as *const _ as *const libc::sockaddr,
                        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1)
                            as libc::socklen_t,
                    )
                },
                0
            );
            connections.push(fd);
        }
        let manifest = HandoffManifestIdentity {
            version: 2,
            scope: "whole-generation".to_owned(),
            generation_id: "generation-backlog-0001".to_owned(),
            control_path,
            listeners: Vec::new(),
        };
        // Release the backlog as a test fail-safe: the pre-fix implementation would otherwise
        // block this test forever. It must finish on its own well before that release.
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_secs(4));
            drop(connections);
        });
        let started = Instant::now();
        let error = handoff_status_any(&manifest).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "connect was not bounded: {error}"
        );
        assert!(
            error.contains("status 连接失败") && error.contains("2 秒"),
            "{error}"
        );
        release.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn handoff_control_eof_does_not_report_a_stale_os_error() {
        let dir = temp_dir("handoff-eof");
        let control_path = dir.join("control.sock");
        let listener = bind_test_seqpacket(&control_path);
        let server = thread::spawn(move || {
            let client = unsafe {
                libc::accept4(
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            assert!(client >= 0);
            let client = unsafe { OwnedFd::from_raw_fd(client) };
            let mut request = [0_u8; 4096];
            assert!(
                unsafe {
                    libc::recv(
                        client.as_raw_fd(),
                        request.as_mut_ptr().cast(),
                        request.len(),
                        0,
                    )
                } > 0
            );
        });
        let manifest = HandoffManifestIdentity {
            version: 2,
            scope: "whole-generation".to_owned(),
            generation_id: "generation-eof-0001".to_owned(),
            control_path,
            listeners: Vec::new(),
        };
        let error = handoff_status_any(&manifest).unwrap_err();
        assert!(error.contains("status 响应提前关闭"), "{error}");
        assert!(!error.contains("os error"), "{error}");
        server.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_health_waits_for_late_business_listeners() {
        let mut attempts = 0;
        wait_for_startup_health(Duration::from_secs(1), || {
            attempts += 1;
            if attempts < 3 {
                WorkloadRuntime::Broken("anytls / hysteria2 尚未监听".to_owned())
            } else {
                WorkloadRuntime::Healthy
            }
        })
        .unwrap();
        assert_eq!(attempts, 3);
    }

    #[test]
    fn bootstrap_manifest_covers_static_dynamic_cover_and_udp_listeners() {
        use brocade_deployment::protocol::{
            ForwardingDesiredState, ForwardingProtocol, ForwardingRuleDesired,
        };

        let dir = std::env::temp_dir().join(format!("bhm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        crate::create_private_dir(&dir).unwrap();
        let forwarding = ForwardingDesiredState {
            generation: 3,
            rules: vec![ForwardingRuleDesired {
                id: 7,
                protocol: ForwardingProtocol::TcpUdp,
                listen_port: 40_000,
                target_host: "127.0.0.1".to_owned(),
                target_port: 443,
                allow_private_targets: Some(false),
            }],
        };
        crate::fsutil::atomic_write_private(
            &dir.join("forwarding-desired.json"),
            &serde_json::to_vec(&forwarding).unwrap(),
        )
        .unwrap();
        let config = serde_json::json!({
            "api": {"tag": "api", "services": ["HandlerService", "StatsService"]},
            "inbounds": [
                {
                    "tag": "api", "listen": "127.0.0.1", "port": 10085,
                    "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1", "network": "tcp"}
                },
                {
                    "tag": "vision", "listen": "0.0.0.0", "port": 8448,
                    "protocol": "vless", "settings": {},
                    "streamSettings": {"network": "tcp", "security": "reality", "realitySettings": {"dest": "127.0.0.1:61101"}}
                },
                {
                    "tag": "cover", "listen": "127.0.0.1", "port": 61101,
                    "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1", "network": "tcp"},
                    "streamSettings": {"network": "tcp", "security": "tls"}
                },
                {
                    "tag": "hy2", "listen": "::", "port": 8443,
                    "protocol": "vless", "settings": {}, "streamSettings": {"network": "hysteria"}
                }
            ]
        });

        let bootstrap =
            prepare_bootstrap_handoff(&dir, &serde_json::to_string(&config).unwrap()).unwrap();
        let endpoints = &bootstrap.manifest.listeners;
        assert_eq!(endpoints.len(), 6);
        assert_eq!(
            endpoints
                .iter()
                .filter(|endpoint| endpoint.role == "control" && endpoint.prepare)
                .count(),
            1
        );
        assert!(endpoints.iter().any(|endpoint| {
            endpoint.address == "0.0.0.0:8448"
                && endpoint.role == "public"
                && endpoint.depends_on == "127.0.0.1:61101"
        }));
        assert!(endpoints
            .iter()
            .any(|endpoint| { endpoint.address == "127.0.0.1:61101" && endpoint.role == "cover" }));
        assert!(endpoints.iter().any(|endpoint| {
            endpoint.address == "[::]:8443"
                && endpoint.network == "udp"
                && endpoint.bind_address == "[::]:8443"
        }));
        for network in ["tcp", "udp"] {
            assert!(endpoints.iter().any(|endpoint| {
                endpoint.address == "0.0.0.0:40000" && endpoint.network == network
            }));
        }
        assert!(bootstrap.manifest_path().is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bootstrap_manifest_rejects_a_dynamic_listener_collision() {
        use brocade_deployment::protocol::{
            ForwardingDesiredState, ForwardingProtocol, ForwardingRuleDesired,
        };

        let dir = temp_dir("bootstrap-collision");
        crate::fsutil::atomic_write_private(
            &dir.join("forwarding-desired.json"),
            &serde_json::to_vec(&ForwardingDesiredState {
                generation: 1,
                rules: vec![ForwardingRuleDesired {
                    id: 1,
                    protocol: ForwardingProtocol::Tcp,
                    listen_port: 8448,
                    target_host: "127.0.0.1".to_owned(),
                    target_port: 443,
                    allow_private_targets: Some(false),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let config = serde_json::json!({
            "api": {"tag": "api"},
            "inbounds": [
                {"tag": "api", "listen": "127.0.0.1", "port": 10085, "protocol": "dokodemo-door", "settings": {"network": "tcp"}},
                {"tag": "public", "listen": "0.0.0.0", "port": 8448, "protocol": "vless", "settings": {}}
            ]
        });
        let error = prepare_bootstrap_handoff(&dir, &config.to_string()).unwrap_err();
        assert!(error.contains("重复 listener"), "{error}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn candidate_manifest_preserves_logical_and_kernel_tcp_addresses() {
        let dir = std::env::temp_dir().join(format!("bca-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        crate::create_private_dir(&dir).unwrap();
        let candidate = dir.join("candidate");
        fs::write(&candidate, b"candidate-bytes").unwrap();
        let descriptor: OwnedFd = File::open("/dev/null").unwrap().into();
        let old = HandoffRuntimeStatus {
            scope: "whole-generation".to_owned(),
            retirement_supported: true,
            generation_id: "source-generation-0001".to_owned(),
            pid: std::process::id() as i32,
            boot_id: "test-boot-id".to_owned(),
            pid_start_ticks: "1".to_owned(),
            ready: true,
            accepting: false,
            paused: true,
            epoch: 1,
            retire_ready: false,
            active: 0,
            public_active: 0,
            cover_active: 0,
            udp_dropped: false,
            listeners: vec![HandoffListenerStatus {
                address: "0.0.0.0:8448".to_owned(),
                bind_address: "[::]:8448".to_owned(),
                network: "tcp".to_owned(),
                role: "public".to_owned(),
                depends_on: String::new(),
                socket_inode: 1,
                socket_cookie: 2,
                protocol: libc::IPPROTO_MPTCP,
                socket_config_sha256: "a".repeat(64),
                active: 0,
                _in_accept: false,
                closed: false,
            }],
            unsupported_reasons: Vec::new(),
        };

        let descriptors = [descriptor];
        let prepared = prepare_candidate_handoff(&dir, &candidate, &old, &descriptors).unwrap();
        assert_eq!(prepared.manifest.listeners[0].address, "0.0.0.0:8448");
        assert_eq!(prepared.manifest.listeners[0].bind_address, "[::]:8448");

        let desired = vec![
            new_bootstrap_endpoint(
                "tcp",
                "public",
                false,
                "0.0.0.0:8448".to_owned(),
                "127.0.0.1:61104".to_owned(),
            )
            .unwrap(),
            new_bootstrap_endpoint(
                "tcp",
                "cover",
                false,
                "127.0.0.1:61104".to_owned(),
                String::new(),
            )
            .unwrap(),
            new_bootstrap_endpoint(
                "udp",
                "public",
                false,
                "[::]:8449".to_owned(),
                String::new(),
            )
            .unwrap(),
        ];
        let prepared = prepare_candidate_handoff_for_listeners(
            &dir,
            &candidate,
            &old,
            &descriptors,
            Some(desired),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(prepared.manifest.listeners.len(), 3);
        assert_eq!(
            prepared.manifest.listeners[0].fd,
            descriptors[0].as_raw_fd()
        );
        assert_eq!(prepared.manifest.listeners[0].bind_address, "[::]:8448");
        assert_eq!(prepared.manifest.listeners[0].depends_on, "127.0.0.1:61104");
        assert_eq!(prepared.manifest.listeners[1].fd, 0);
        assert_eq!(prepared.manifest.listeners[2].fd, 0);
        let removed = vec![new_bootstrap_endpoint(
            "tcp",
            "control",
            true,
            "127.0.0.1:10086".to_owned(),
            String::new(),
        )
        .unwrap()];
        let prepared = prepare_candidate_handoff_for_listeners(
            &dir,
            &candidate,
            &old,
            &descriptors,
            Some(removed),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(prepared.manifest.listeners.len(), 1);
        assert_eq!(prepared.manifest.listeners[0].address, "127.0.0.1:10086");
        assert_eq!(prepared.manifest.listeners[0].fd, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn udp_listener_waiting_in_accept_is_serving_until_closed() {
        let mut listener = HandoffListenerStatus {
            address: "0.0.0.0:30000".to_owned(),
            bind_address: "[::]:30000".to_owned(),
            network: "udp".to_owned(),
            role: "public".to_owned(),
            depends_on: String::new(),
            socket_inode: 1,
            socket_cookie: 2,
            protocol: libc::IPPROTO_UDP,
            socket_config_sha256: "a".repeat(64),
            active: 0,
            _in_accept: true,
            closed: false,
        };

        assert!(handoff_listener_is_serving_udp(&listener, "0.0.0.0:30000"));
        listener.closed = true;
        assert!(!handoff_listener_is_serving_udp(&listener, "0.0.0.0:30000"));
    }

    #[test]
    fn udp_health_uses_the_accepting_generation_during_handoff() {
        let listener = HandoffListenerStatus {
            address: "0.0.0.0:18000".to_owned(),
            bind_address: "[::]:18000".to_owned(),
            network: "udp".to_owned(),
            role: "public".to_owned(),
            depends_on: String::new(),
            socket_inode: 1,
            socket_cookie: 2,
            protocol: libc::IPPROTO_UDP,
            socket_config_sha256: "a".repeat(64),
            active: 0,
            _in_accept: true,
            closed: false,
        };
        let status =
            |generation_id: &str, pid: i32, accepting: bool, closed: bool| HandoffRuntimeStatus {
                scope: "whole-generation".to_owned(),
                retirement_supported: true,
                generation_id: generation_id.to_owned(),
                pid,
                boot_id: "test-boot-id".to_owned(),
                pid_start_ticks: pid.to_string(),
                ready: true,
                accepting,
                paused: !accepting,
                epoch: 1,
                retire_ready: false,
                active: 0,
                public_active: 0,
                cover_active: 0,
                udp_dropped: closed,
                listeners: vec![HandoffListenerStatus {
                    closed,
                    ..listener.clone()
                }],
                unsupported_reasons: Vec::new(),
            };
        let old = status("old-generation-0001", 10, false, true);
        let candidate = status("candidate-generation-0001", 11, true, false);

        assert!(any_handoff_generation_serves_udp(
            &[old, candidate],
            "0.0.0.0:18000"
        ));
    }

    #[test]
    fn startup_health_does_not_delay_an_already_healthy_process() {
        wait_for_startup_health(Duration::ZERO, || WorkloadRuntime::Healthy).unwrap();
    }

    #[test]
    fn startup_health_timeout_keeps_broken_and_unknown_states_as_failures() {
        for state in [
            WorkloadRuntime::Broken("业务端口没有监听".to_owned()),
            WorkloadRuntime::Unknown("无法检查业务端口".to_owned()),
        ] {
            let detail = match &state {
                WorkloadRuntime::Broken(error) | WorkloadRuntime::Unknown(error) => error,
                WorkloadRuntime::Healthy => unreachable!(),
            };
            let expected = format!("Xray 启动后健康检查失败：{detail}");
            let mut state = Some(state);
            let error =
                wait_for_startup_health(Duration::ZERO, || state.take().unwrap()).unwrap_err();
            assert_eq!(error, expected);
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("brocade-xray-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn handoff_identity_accepts_private_read_only_files_and_directories() {
        let dir = temp_dir("read-only-manifest");
        let path = dir.join("manifest.json");
        crate::fsutil::atomic_write_private(
            &path,
            br#"{
            "version":2,"scope":"whole-generation","generation_id":"generation-test-0001",
            "control_path":"/private/control.sock","listeners":[]
        }"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let path_result = validate_handoff_manifest_path(&path);
        let read_result = read_handoff_manifest_identity(&path);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        path_result.unwrap();
        assert_eq!(read_result.unwrap().generation_id, "generation-test-0001");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(read_handoff_manifest_identity(&path).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(validate_handoff_manifest_path(&path).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn handoff_identity_reads_large_manifests_in_batches() {
        let dir = temp_dir("manifest-batch-read");
        let path = dir.join("manifest.json");
        let manifest = serde_json::json!({
            "version": 2,
            "scope": "whole-generation",
            "generation_id": "generation-batch-read-0001",
            "control_path": "/private/control.sock",
            "listeners": [],
            "future_field": "a".repeat(128 * 1024),
        });
        crate::fsutil::atomic_write_private(&path, &serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        let before = crate::tests::thread_io_counter("syscr");

        let observed = read_handoff_manifest_identity(&path).unwrap();

        let reads = crate::tests::thread_io_counter("syscr") - before;
        assert_eq!(observed.generation_id, "generation-batch-read-0001");
        assert!(reads < 16, "manifest used {reads} read calls");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn handoff_identity_batch_read_preserves_size_and_json_validation() {
        let dir = temp_dir("manifest-batch-validation");
        let path = dir.join("manifest.json");
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "generation_id": "generation-batch-validation-0001",
            "control_path": "/private/control.sock",
        }))
        .unwrap();
        bytes.resize(MAX_HANDOFF_MESSAGE_BYTES, b' ');
        crate::fsutil::atomic_write_private(&path, &bytes).unwrap();
        read_handoff_manifest_identity(&path).unwrap();

        bytes.push(b' ');
        crate::fsutil::atomic_write_private(&path, &bytes).unwrap();
        assert!(read_handoff_manifest_identity(&path).is_err());
        crate::fsutil::atomic_write_private(&path, b"{}").unwrap();
        assert!(read_handoff_manifest_identity(&path).is_err());
        crate::fsutil::atomic_write_private(&path, b"").unwrap();
        assert!(read_handoff_manifest_identity(&path).is_err());

        let alias = dir.join("symlink.json");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(read_handoff_manifest_identity(&alias).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    fn current_process() -> XrayUpgradeProcess {
        let pid = std::process::id() as i32;
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        XrayUpgradeProcess {
            pid,
            pid_start_ticks: fields[19].parse().unwrap(),
            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap()
                .trim()
                .to_owned(),
            sha256: file_sha256_hex(&PathBuf::from(format!("/proc/{pid}/exe"))).unwrap(),
            alive: false,
            accepting: Some(false),
            active_connections: Some(7),
        }
    }

    fn write_upgrade_runtime(dir: &Path, runtime: &XrayUpgradeRuntime, mode: u32) {
        let path = dir.join(UPGRADE_RUNTIME_FILE);
        fs::write(&path, serde_json::to_vec(runtime).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn upgrade_runtime_reports_only_a_private_exact_process_identity() {
        let dir = temp_dir("upgrade-runtime");
        let process = current_process();
        let runtime = XrayUpgradeRuntime {
            source: XrayUpgradeSource::BinaryRelease,
            release_id: 41,
            attempt: 2,
            strategy: XrayReleaseStrategy::Handoff,
            phase: brocade_deployment::protocol::XrayUpgradePhase::Draining,
            old: process.clone(),
            candidate: None,
            drain_deadline_unix_secs: Some(1_800_000_000),
            blocked_reason: None,
        };
        write_upgrade_runtime(&dir, &runtime, 0o600);

        let observed = observe_upgrade_runtime(&dir).unwrap();
        assert!(observed.old.alive);
        assert_eq!(observed.old.sha256, process.sha256);
        let expected = crate::process_ref(process.pid).unwrap();
        for _ in 0..2 {
            assert!(observe_upgrade_runtime(&dir).unwrap().old.alive);
            assert_eq!(exact_upgrade_process(&process), Some(expected));
        }
        assert_eq!(
            observe_handoff_capability(
                &dir,
                true,
                Some("Brocade-Capabilities: generation-retirement-v1 listener-handoff-v1"),
            )
            .state,
            XrayHandoffState::Busy
        );

        let mut config_runtime = runtime.clone();
        config_runtime.source = XrayUpgradeSource::ConfigDeployment {
            deployment_id: 603,
            claim_generation: 0,
        };
        config_runtime.release_id = -603;
        config_runtime.attempt = 0;
        write_upgrade_runtime(&dir, &config_runtime, 0o600);
        assert_eq!(
            read_upgrade_runtime(&dir).unwrap().unwrap().source,
            config_runtime.source
        );

        let mut wrong_digest = runtime.clone();
        wrong_digest.old.sha256 = "f".repeat(64);
        write_upgrade_runtime(&dir, &wrong_digest, 0o600);
        let observed = observe_upgrade_runtime(&dir).unwrap();
        assert!(!observed.old.alive);
        assert_eq!(observed.old.sha256, "f".repeat(64));
        assert_eq!(exact_upgrade_process(&wrong_digest.old), None);

        let mut wrong_start = runtime.clone();
        wrong_start.old.pid_start_ticks += 1;
        write_upgrade_runtime(&dir, &wrong_start, 0o600);
        assert!(!observe_upgrade_runtime(&dir).unwrap().old.alive);
        assert_eq!(exact_upgrade_process(&wrong_start.old), None);

        write_upgrade_runtime(&dir, &runtime, 0o644);
        assert_eq!(observe_upgrade_runtime(&dir), None);
        assert!(read_upgrade_runtime(&dir).is_err());

        let path = dir.join(UPGRADE_RUNTIME_FILE);
        fs::remove_file(&path).unwrap();
        let linked = dir.join("linked-upgrade.json");
        fs::write(&linked, serde_json::to_vec(&runtime).unwrap()).unwrap();
        fs::set_permissions(&linked, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&linked, &path).unwrap();
        assert_eq!(observe_upgrade_runtime(&dir), None);
        assert!(read_upgrade_runtime(&dir).is_err());

        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&linked, &path).unwrap();
        assert_eq!(observe_upgrade_runtime(&dir), None);
        assert!(read_upgrade_runtime(&dir).is_err());

        fs::remove_file(&path).unwrap();
        fs::write(&path, b"{not-json").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_upgrade_runtime(&dir).is_err());

        fs::remove_file(&path).unwrap();
        assert_eq!(read_upgrade_runtime(&dir).unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn handoff_capability_is_never_inferred_from_the_binary_marker_alone() {
        let dir = temp_dir("handoff-capability");
        let unsupported = observe_handoff_capability(&dir, false, Some("Xray without marker"));
        assert_eq!(unsupported.state, XrayHandoffState::Unsupported);
        let legacy = observe_handoff_capability(
            &dir,
            false,
            Some("Brocade-Capabilities: generation-retirement-v1 listener-handoff-v1"),
        );
        assert_eq!(legacy.state, XrayHandoffState::Unsupported);
        assert!(legacy.reason.unwrap().contains("tcp-handoff-udp-reset-v1"));
        let blocked = observe_handoff_capability(
            &dir,
            false,
            Some(
                "Brocade-Capabilities: generation-retirement-v2 listener-handoff-v1 tcp-handoff-udp-reset-v1",
            ),
        );
        assert_eq!(blocked.state, XrayHandoffState::Blocked);
        assert!(blocked.reason.unwrap().contains("尚未完成代际接管"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_executable_replacement_keeps_complete_bytes_and_mode() {
        let dir = temp_dir("replace");
        let source = dir.join("candidate");
        let target = dir.join("xray");
        fs::write(&source, b"new-complete-binary").unwrap();
        fs::write(&target, b"old").unwrap();

        replace_executable(&source, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new-complete-binary");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains("brocade-new"))
                .count(),
            0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rollout_interval_has_a_stable_bounded_per_node_jitter() {
        let first = update_interval("broc_node_first");
        assert_eq!(first, update_interval("broc_node_first"));
        assert!(first >= UPDATE_INTERVAL);
        assert!(first <= UPDATE_INTERVAL + Duration::from_secs(UPDATE_JITTER_MAX_SECS));
    }

    #[test]
    fn downloaded_candidate_must_be_nonempty_and_bounded() {
        let dir = temp_dir("download-size");
        let path = dir.join("candidate");
        fs::write(&path, []).unwrap();
        assert!(validate_download_size(&path).is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_DOWNLOAD_BYTES + 1)
            .unwrap();
        assert!(validate_download_size(&path).is_err());
        fs::write(&path, b"x").unwrap();
        assert!(validate_download_size(&path).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_rollout_replaces_the_managed_binary_end_to_end() {
        let dir = temp_dir("disabled-rollout");
        let target = dir.join("xray");
        let candidate = dir.join("candidate");
        let old = b"#!/bin/sh\necho 'Xray 26.4.25 old'\n";
        let next = b"#!/bin/sh\necho 'Xray 26.9.1 candidate'\n";
        fs::write(&target, old).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&candidate, next).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
        let previous_sha256 = file_sha256_hex(&target).unwrap();
        let sha256 = file_sha256_hex(&candidate).unwrap();
        let offer = XrayReleaseOffer {
            release_id: 7,
            attempt: 1,
            version: "26.9.1".to_owned(),
            url: "https://console.invalid/brocade-xray/x86_64".to_owned(),
            sha256: sha256.clone(),
            previous_sha256,
            strategy: XrayReleaseStrategy::Restart,
            drain_timeout_secs: None,
        };
        let options = Options {
            command: "run".to_owned(),
            server: "https://console.invalid".to_owned(),
            token: "broc_node_test".to_owned(),
            state_dir: dir.clone(),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: std::time::Duration::from_secs(900),
        };

        let report = perform_update_at_target(
            &options,
            &Arc::new(Mutex::new(())),
            &offer,
            &candidate,
            &target,
        );

        assert_eq!(report.outcome, XrayReleaseOutcome::Succeeded);
        assert!(report.performed_update);
        assert!(!report.xray_enabled);
        assert_eq!(report.installed_sha256.as_deref(), Some(sha256.as_str()));
        assert_eq!(fs::read(&target).unwrap(), next);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let no_op = perform_update_at_target(
            &options,
            &Arc::new(Mutex::new(())),
            &offer,
            &candidate,
            &target,
        );
        assert_eq!(no_op.outcome, XrayReleaseOutcome::Succeeded);
        assert!(!no_op.performed_update);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_handoff_keeps_old_and_new_tcp_flows_and_promotes_accounting() {
        native_agent_handoff_case(NativeInterruption::None);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local IPv4/IPv6/Unix sockets"]
    fn native_config_candidate_restores_target_grants_after_old_process_loss() {
        native_config_candidate_grants_case(false, false, false);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local IPv4/IPv6/Unix sockets"]
    fn native_config_candidate_guard_rejects_missing_sealed_grants_checkpoint() {
        native_config_candidate_grants_case(true, false, false);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local IPv4/IPv6/Unix sockets"]
    fn native_config_candidate_recovers_an_explicit_empty_grants_snapshot() {
        native_config_candidate_grants_case(false, true, false);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local IPv4/IPv6/Unix sockets"]
    fn native_config_candidate_crash_restores_frozen_target_grants() {
        native_config_candidate_grants_case(false, false, true);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local IPv4/IPv6/Unix sockets"]
    fn native_config_candidate_crash_restores_explicit_empty_grants() {
        native_config_candidate_grants_case(false, true, true);
    }

    fn native_config_candidate_grants_case(sealed: bool, empty: bool, crashed: bool) {
        use std::{ffi::OsString, net::TcpListener};

        struct Fixture {
            root: PathBuf,
            environment: Vec<(&'static str, Option<OsString>)>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = crate::terminate_processes(
                    &crate::serving_xray_processes_in(&self.root),
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                );
                for (name, value) in &self.environment {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let source = PathBuf::from(
            std::env::var_os("BROCADE_TEST_XRAY").expect("BROCADE_TEST_XRAY is required"),
        );
        // Keep the Unix control path under sockaddr_un's limit even with a custom TMPDIR.
        let root = std::env::temp_dir().join(format!("gc-{}", std::process::id()));
        assert!(!root.exists(), "isolated test directory already exists");
        crate::create_private_dir(&root).unwrap();
        let _fixture = Fixture {
            root: root.clone(),
            environment: [
                "BROCADE_XRAY_BIN",
                "BROCADE_TEST_LOG_SINK",
                "BROCADE_TEST_CONFIG_HANDOFF_INTERRUPT_BEFORE_RECOVERY",
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
        };
        let binary = root.join("xray");
        copy_atomic(&source, &binary, 0o755).unwrap();
        std::env::set_var("BROCADE_XRAY_BIN", &binary);
        std::env::set_var("BROCADE_TEST_LOG_SINK", "cat >/dev/null");
        let free_address = |listen: &str| TcpListener::bind(listen).unwrap().local_addr().unwrap();
        let old_api = free_address("127.0.0.1:0");
        let mut inbounds = vec![serde_json::json!({
            "tag": "api", "listen": "127.0.0.1", "port": old_api.port(),
            "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1"}
        })];
        for tag in ["kept", "cleared", "removed"] {
            inbounds.push(serde_json::json!({
                "tag": tag, "listen": "127.0.0.1", "port": free_address("127.0.0.1:0").port(),
                "protocol": "vless", "settings": {"decryption": "none", "clients": []}
            }));
        }
        let previous = serde_json::json!({
            "log": {"loglevel": "error"}, "stats": {},
            "api": {"tag": "api", "services": ["HandlerService", "StatsService"]},
            "policy": {"levels": {"0": {"statsUserUplink": true, "statsUserDownlink": true}}},
            "inbounds": inbounds,
            "outbounds": [{"tag": "direct", "protocol": "freedom"}],
            "routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
        })
        .to_string();
        let old_grants = vec![
            grants_fixture("kept", "same-label"),
            grants_fixture("cleared", "revoked-user"),
            grants_fixture("removed", "removed-user"),
        ];
        let formal = root.join("grants.desired.json");
        let formal_before = serde_json::to_vec(&old_grants).unwrap();
        crate::fsutil::atomic_write_private(&formal, &formal_before).unwrap();
        let config = root.join("xray.json");
        crate::fsutil::atomic_write_private(&config, previous.as_bytes()).unwrap();
        crate::fsutil::atomic_write_private(&root.join("usage-generation"), b"1").unwrap();
        crate::apply_xray(&config, old_api).unwrap();
        let old = crate::serving_xray_process_in(&root).unwrap();
        assert!(crate::grants_match_runtime(&old_grants, old_api).unwrap());

        let api = free_address("[::1]:0");
        let mut desired: serde_json::Value = serde_json::from_str(&previous).unwrap();
        desired["api"]["listen"] = api.to_string().into();
        desired["routing"]["rules"] = serde_json::json!([]);
        desired["inbounds"]
            .as_array_mut()
            .unwrap()
            .retain(|inbound| {
                !empty && matches!(inbound["tag"].as_str(), Some("kept" | "cleared"))
            });
        let mut wanted = Vec::new();
        if !empty {
            desired["inbounds"].as_array_mut().unwrap().push(serde_json::json!({
                "tag": "added", "listen": "127.0.0.1", "port": free_address("127.0.0.1:0").port(),
                "protocol": "vless", "settings": {"decryption": "none", "clients": []}
            }));
            let mut kept = grants_fixture("kept", "same-label");
            kept.clients[0].uuid = "00000000-0000-4000-8000-000000000002".to_owned();
            wanted = vec![
                kept,
                grants_fixture("added", "added-user"),
                GrantInbound {
                    tag: "cleared".to_owned(),
                    clients: vec![],
                },
            ];
        }
        let desired = desired.to_string();
        let next = root.join("xray.next.json");
        crate::fsutil::atomic_write_private(&next, desired.as_bytes()).unwrap();
        std::env::set_var("BROCADE_TEST_CONFIG_HANDOFF_INTERRUPT_BEFORE_RECOVERY", "1");
        let error =
            begin_config_handoff(&root, 604, 7, &previous, &next, Some(&wanted)).unwrap_err();
        std::env::remove_var("BROCADE_TEST_CONFIG_HANDOFF_INTERRUPT_BEFORE_RECOVERY");
        assert!(error.contains("test interruption"), "{error}");
        let runtime = read_upgrade_runtime(&root).unwrap().unwrap();
        assert_eq!(runtime.phase, XrayUpgradePhase::CandidateReady);
        let candidate = exact_upgrade_process(runtime.candidate.as_ref().unwrap()).unwrap();
        let (manifest, mut status) = observe_handoff_process(candidate).unwrap();
        wait_for_candidate_api(api, &manifest, candidate).unwrap();
        assert!(!status.ready && !status.accepting);

        // Reproduce a partially restored candidate carrying revoked credentials. Its next
        // recovery must replace these from the frozen target, not reapply the formal old file.
        let partial = crate::recovery_grants_for_config(&desired, old_grants).unwrap();
        crate::sync_grants(&desired, api, &partial).unwrap();
        if crashed {
            let recovered =
                crate::restore_xray_runtime_with_grants(&root, &desired, api, Some(&wanted));
            assert!(recovered.is_complete(), "{:?}", recovered.error());
            let bootstrap = BootstrapHandoff {
                manifest_path: manifest
                    .control_path
                    .parent()
                    .unwrap()
                    .join("manifest.json"),
                manifest: manifest.clone(),
            };
            activate_bootstrap_handoff(&bootstrap, candidate.pid).unwrap();
            crate::guard_xray_once(&root, false).unwrap();
            let old_status = observe_handoff_process(old).unwrap().1;
            assert!(old_status.udp_dropped);
            assert!(old_status.listeners.iter().all(|listener| listener.closed));
            crate::fsutil::atomic_write_private(&formal, &formal_before).unwrap();
            let epochs = crate::accounting::handoff_journal_epochs(&root).unwrap();
            crash_test_process(candidate);
            let snapshot = root.join(CONFIG_GRANTS_FILE);
            let saved = fs::read(&snapshot).unwrap();
            fs::remove_file(&snapshot).unwrap();
            assert!(crate::guard_xray_once(&root, true).is_err());
            assert!(
                crate::process_is_live(old),
                "invalid grants retired the residual generation"
            );
            crate::fsutil::atomic_write_private(&snapshot, &saved).unwrap();
            crate::guard_xray_once(&root, true).unwrap();
            assert!(!crate::process_is_live(old));
            let replacement = crate::serving_xray_process_in(&root).unwrap();
            assert_ne!(replacement, candidate);
            assert!(crate::grants_match_runtime(&wanted, api).unwrap());
            assert_eq!(crate::desired_grants_on_disk(&root).unwrap(), Some(wanted));
            assert_eq!(
                crate::accounting::handoff_journal_epochs(&root).unwrap(),
                epochs
            );
            assert!(read_upgrade_runtime(&root).unwrap().is_none());
            assert!(!root.join(REPORT_FILE).exists());
            assert!(!root.join("xray-crash-report.json").exists());
            return;
        }
        if sealed {
            let restored =
                crate::restore_xray_runtime_with_grants(&root, &desired, api, Some(&wanted));
            assert!(restored.is_complete(), "{:?}", restored.error());
            status = handoff_rpc(
                &manifest,
                candidate.pid,
                "seal",
                status.epoch + 1,
                "test-config-grants-seal",
            )
            .unwrap();
            assert!(status.ready && !status.accepting);
        }
        crate::terminate_processes(&[old], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE).unwrap();
        let options = Options {
            command: "run".to_owned(),
            server: "https://console.invalid".to_owned(),
            token: "broc_node_grants_fixture".to_owned(),
            state_dir: root.clone(),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let meter = Arc::new(Mutex::new(()));
        let snapshot = root.join(CONFIG_GRANTS_FILE);
        let saved = fs::read(&snapshot).unwrap();
        assert_eq!(
            read_config_handoff_grants(&root).unwrap(),
            Some(wanted.clone())
        );
        fs::remove_file(&snapshot).unwrap();
        for bytes in [
            None,
            Some(b"null".as_slice()),
            Some(b"malformed-fixture".as_slice()),
        ] {
            if let Some(bytes) = bytes {
                crate::fsutil::atomic_write_private(&snapshot, bytes).unwrap();
            }
            assert!(recover_interrupted_handoff(&options, &meter).is_err());
            assert!(guard_interrupted_handoff(&root).is_err());
            let status = handoff_status_rpc(&manifest, candidate.pid).unwrap();
            assert!(!status.accepting, "invalid checkpoint reopened candidate");
            assert_eq!(status.ready, sealed);
            assert_eq!(fs::read(&formal).unwrap(), formal_before);
        }
        if !sealed {
            let mut invalid = wanted.clone();
            invalid.push(GrantInbound {
                tag: "absent-inbound".to_owned(),
                clients: vec![],
            });
            crate::fsutil::atomic_write_private(&snapshot, &serde_json::to_vec(&invalid).unwrap())
                .unwrap();
            let error = recover_interrupted_handoff(&options, &meter).unwrap_err();
            assert!(error.contains("动态状态无法恢复"), "{error}");
            let status = handoff_status_rpc(&manifest, candidate.pid).unwrap();
            assert!(!status.ready && !status.accepting);
            assert_eq!(fs::read(&formal).unwrap(), formal_before);
        }
        crate::fsutil::atomic_write_private(&snapshot, &saved).unwrap();
        if sealed {
            assert!(guard_interrupted_handoff(&root).unwrap());
            assert!(
                handoff_status_rpc(&manifest, candidate.pid)
                    .unwrap()
                    .accepting
            );
        }
        assert!(recover_interrupted_handoff(&options, &meter).unwrap());
        assert!(crate::grants_match_runtime(&wanted, api).unwrap());
        assert_eq!(crate::desired_grants_on_disk(&root).unwrap(), Some(wanted));
        assert!(crate::xray_grpc::query_stats(api, "").is_ok());
        assert!(!crate::xray_grpc::inbound_tags(api)
            .unwrap()
            .contains(&"removed".to_owned()));
        assert!(read_upgrade_runtime(&root).unwrap().is_none());
        assert!(!snapshot.exists());
        assert!(
            !root.join(REPORT_FILE).exists(),
            "config recovery manufactured a binary receipt"
        );
        assert!(crate::process_is_live(candidate));
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_recovers_an_interrupted_switched_handoff() {
        native_agent_handoff_case(NativeInterruption::AfterSwitch);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_restarts_a_crashed_candidate_after_udp_handoff() {
        native_agent_handoff_case(NativeInterruption::CandidateLost);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_restarts_when_both_handoff_generations_crash() {
        native_agent_handoff_case(NativeInterruption::BothLost);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_rolls_back_an_interrupted_pre_switch_handoff() {
        native_agent_handoff_case(NativeInterruption::BeforeSwitch);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_completes_an_orphaned_pre_switch_candidate() {
        native_agent_handoff_case(NativeInterruption::BeforeSwitchOldLost);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and local TCP/Unix sockets"]
    fn native_agent_graceful_timeout_yields_until_the_old_flow_drains() {
        native_agent_handoff_case(NativeInterruption::GracefulTimeout);
    }

    #[test]
    #[ignore = "requires BROCADE_TEST_XRAY and isolated local TCP/Unix sockets; run serially"]
    fn native_unresponsive_old_without_a_registered_candidate_recovers_previous_state() {
        use std::{ffi::OsString, net::TcpListener};
        struct Fixture {
            root: PathBuf,
            previous: Vec<(&'static str, Option<OsString>)>,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = crate::terminate_processes(
                    &crate::serving_xray_processes_in(&self.root),
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                );
                for (name, value) in &self.previous {
                    if let Some(value) = value {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let source =
            PathBuf::from(std::env::var_os("BROCADE_TEST_XRAY").expect("explicit Xray required"));
        for config_source in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "br-{}-{}",
                std::process::id(),
                u8::from(config_source)
            ));
            assert!(!root.exists());
            crate::create_private_dir(&root).unwrap();
            let _fixture = Fixture {
                root: root.clone(),
                previous: ["BROCADE_XRAY_BIN", "BROCADE_TEST_LOG_SINK"]
                    .into_iter()
                    .map(|name| (name, std::env::var_os(name)))
                    .collect(),
            };
            let binary = root.join("xray");
            copy_atomic(&source, &binary, 0o755).unwrap();
            std::env::set_var("BROCADE_XRAY_BIN", &binary);
            std::env::set_var("BROCADE_TEST_LOG_SINK", "cat >/dev/null");
            let api = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap();
            let content = serde_json::json!({
                "log": {"loglevel": "error"}, "stats": {},
                "api": {"tag": "api", "listen": api.to_string(), "services": ["HandlerService", "StatsService"]},
                "inbounds": [], "outbounds": [{"tag": "out:forward", "protocol": "freedom"}]
            }).to_string();
            crate::write_private(&root.join("xray.json"), &content).unwrap();
            crate::write_private(&root.join(CONFIG_PREVIOUS_FILE), &content).unwrap();
            crate::write_private(&root.join(CONFIG_GRANTS_FILE), "[]").unwrap();
            crate::write_private(&root.join("grants.desired.json"), "[]").unwrap();
            crate::write_private(&root.join("usage-generation"), "1").unwrap();
            crate::apply_xray(&root.join("xray.json"), api).unwrap();
            let (old, manifest, status) = observe_serving_handoff_generation(&root).unwrap();
            let paused = handoff_rpc(
                &manifest,
                old.pid,
                "pause",
                status.epoch + 1,
                "test-pre-candidate-pause",
            )
            .unwrap();
            let runtime = XrayUpgradeRuntime {
                source: if config_source {
                    XrayUpgradeSource::ConfigDeployment {
                        deployment_id: 77,
                        claim_generation: 1,
                    }
                } else {
                    XrayUpgradeSource::BinaryRelease
                },
                release_id: if config_source { -77 } else { 91 },
                attempt: if config_source { 0 } else { 1 },
                strategy: XrayReleaseStrategy::HandoffForce,
                phase: XrayUpgradePhase::Switching,
                old: upgrade_process(&paused, file_sha256_hex(&binary).unwrap()).unwrap(),
                candidate: None,
                drain_deadline_unix_secs: None,
                blocked_reason: None,
            };
            super::write_upgrade_runtime(&root, &runtime).unwrap();
            crate::signal_processes(&[old], libc::SIGSTOP).unwrap();
            let started = Instant::now();
            assert!(guard_interrupted_handoff(&root).unwrap());
            assert!(started.elapsed() < Duration::from_secs(12));
            let replacement = crate::serving_xray_process_in(&root).unwrap();
            assert_ne!(replacement, old);
            assert_eq!(crate::serving_xray_processes_in(&root), vec![replacement]);
            assert_eq!(fs::read_to_string(root.join("xray.json")).unwrap(), content);
            assert!(crate::xray_grpc::query_stats(api, "").is_ok());
            assert!(read_upgrade_runtime(&root).unwrap().is_none());
            if config_source {
                assert!(
                    !root.join(crash_recovery::RECOVERED_REPORT_FILE).exists(),
                    "config manufactured binary receipt"
                );
            } else {
                let stored: StoredReport = serde_json::from_slice(
                    &fs::read(root.join(crash_recovery::RECOVERED_REPORT_FILE)).unwrap(),
                )
                .unwrap();
                assert_eq!(stored.report.outcome, XrayReleaseOutcome::FailedRecovered);
                assert!(!stored.report.performed_update);
                assert_eq!(stored.report.running_sha256, Some(runtime.old.sha256));
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum NativeInterruption {
        None,
        BeforeSwitch,
        BeforeSwitchOldLost,
        AfterSwitch,
        CandidateLost,
        BothLost,
        GracefulTimeout,
    }

    fn crash_test_process(process: crate::ProcessRef) {
        crate::signal_processes(&[process], libc::SIGKILL).unwrap();
        assert!(crate::wait_for_process_exit(&[process], Duration::from_secs(3)).is_empty());
    }

    fn native_agent_handoff_case(interruption: NativeInterruption) {
        use std::{
            io::{Read, Write},
            net::{Shutdown, TcpListener, TcpStream, UdpSocket},
            sync::atomic::{AtomicBool, Ordering},
        };

        struct RuntimeGuard {
            config: PathBuf,
        }
        impl Drop for RuntimeGuard {
            fn drop(&mut self) {
                let processes = crate::live_processes_named("xray")
                    .into_iter()
                    .filter(|process| {
                        fs::read(format!("/proc/{}/cmdline", process.pid))
                            .ok()
                            .and_then(|cmdline| {
                                crate::xray_config_argument(&cmdline).map(PathBuf::from)
                            })
                            .is_some_and(|path| path == self.config)
                    })
                    .collect::<Vec<_>>();
                let _ = crate::terminate_processes(
                    &processes,
                    crate::XRAY_TERM_GRACE,
                    crate::XRAY_KILL_GRACE,
                );
            }
        }

        struct TestEnvironmentGuard;
        impl Drop for TestEnvironmentGuard {
            fn drop(&mut self) {
                for name in [
                    "BROCADE_XRAY_BIN",
                    "BROCADE_TEST_LOG_SINK",
                    "BROCADE_TEST_HANDOFF_INTERRUPT_BEFORE_SWITCH",
                    "BROCADE_TEST_HANDOFF_INTERRUPT_AFTER_SWITCH",
                    "BROCADE_TEST_CRASH_RECOVERY_INTERRUPT_AFTER_LAUNCH",
                ] {
                    std::env::remove_var(name);
                }
            }
        }
        let _environment = TestEnvironmentGuard;

        let source = PathBuf::from(
            std::env::var_os("BROCADE_TEST_XRAY").expect("BROCADE_TEST_XRAY is required"),
        );
        let root = std::env::temp_dir().join(format!("bha-{}", std::process::id()));
        let binaries = std::env::temp_dir().join(format!("bhb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&binaries);
        crate::create_private_dir(&root).unwrap();
        crate::create_private_dir(&binaries).unwrap();
        let target = binaries.join("xray");
        copy_atomic(&source, &target, 0o755).unwrap();
        let candidate = binaries.join("candidate-xray");
        copy_atomic(&source, &candidate, 0o755).unwrap();
        let port = || {
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        };
        let api_address = std::net::SocketAddr::from(([127, 0, 0, 1], port()));
        let public_port = port();
        let public_udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let public_udp_port = public_udp.local_addr().unwrap().port();
        drop(public_udp);
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let upstream_connections = if interruption == NativeInterruption::None {
            5
        } else if matches!(
            interruption,
            NativeInterruption::CandidateLost | NativeInterruption::BothLost
        ) {
            4
        } else {
            2
        };
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..upstream_connections {
                let (mut stream, _) = upstream.accept().unwrap();
                workers.push(thread::spawn(move || {
                    let mut buffer = [0_u8; 64];
                    loop {
                        let count = stream.read(&mut buffer).unwrap();
                        if count == 0 {
                            break;
                        }
                        stream.write_all(&buffer[..count]).unwrap();
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let upstream_udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        upstream_udp
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let upstream_udp_port = upstream_udp.local_addr().unwrap().port();
        let stop_udp = Arc::new(AtomicBool::new(false));
        let udp_stopped = Arc::clone(&stop_udp);
        let udp_server = thread::spawn(move || {
            let mut buffer = [0_u8; 64];
            while !udp_stopped.load(Ordering::Acquire) {
                match upstream_udp.recv_from(&mut buffer) {
                    Ok((count, peer)) => {
                        upstream_udp.send_to(&buffer[..count], peer).unwrap();
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => panic!("UDP echo failed: {error}"),
                };
            }
        });
        let config = root.join("xray.json");
        let public_tag = if matches!(
            interruption,
            NativeInterruption::CandidateLost | NativeInterruption::BothLost
        ) {
            "fwd:18"
        } else {
            "public"
        };
        crate::fsutil::atomic_write_private(
            &config,
            &serde_json::to_vec(&serde_json::json!({
                "log": {"loglevel": "warning"}, "stats": {},
                "api": {"tag": "api", "services": ["HandlerService", "StatsService"]},
                "policy": {"system": {"statsInboundUplink": true, "statsInboundDownlink": true}},
                "inbounds": [
                    {"tag": "api", "listen": "127.0.0.1", "port": api_address.port(), "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1", "network": "tcp"}},
                    {"tag": public_tag, "listen": "127.0.0.1", "port": public_port, "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1", "port": upstream_port, "network": "tcp"}},
                    {"tag": "public-udp", "listen": "127.0.0.1", "port": public_udp_port, "protocol": "dokodemo-door", "settings": {"address": "127.0.0.1", "port": upstream_udp_port, "network": "udp"}}
                ],
                "outbounds": [{"tag": "direct", "protocol": "freedom"}],
                "routing": {"rules": [{"type": "field", "inboundTag": ["api"], "outboundTag": "api"}]}
            }))
            .unwrap(),
        )
        .unwrap();
        crate::fsutil::atomic_write_private(&root.join("usage-generation"), b"1").unwrap();
        let _guard = RuntimeGuard {
            config: config.clone(),
        };
        std::env::set_var("BROCADE_XRAY_BIN", &target);
        std::env::set_var("BROCADE_TEST_LOG_SINK", "cat >/dev/null");
        crate::apply_xray(&config, api_address).unwrap();
        let old = crate::serving_xray_process_in(&root).unwrap();

        let mut old_client = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
        old_client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        old_client.write_all(b"before").unwrap();
        let mut echoed = [0_u8; 6];
        old_client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"before");

        let sha = file_sha256_hex(&target).unwrap();
        let offer = XrayReleaseOffer {
            release_id: 91,
            attempt: 1,
            version: "26.4.25".to_owned(),
            url: "https://console.invalid/xray".to_owned(),
            sha256: sha.clone(),
            previous_sha256: sha,
            strategy: XrayReleaseStrategy::Handoff,
            // Directly exercising the executor bypasses offer validation so this case can prove
            // the timeout transition without making the native suite sleep for ten seconds.
            drain_timeout_secs: Some(if interruption == NativeInterruption::GracefulTimeout {
                0
            } else {
                10
            }),
        };
        let options = Options {
            command: "run".to_owned(),
            server: "https://console.invalid".to_owned(),
            token: "broc_node_handoff".to_owned(),
            state_dir: root.clone(),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let meter = Arc::new(Mutex::new(()));
        match interruption {
            NativeInterruption::BeforeSwitch | NativeInterruption::BeforeSwitchOldLost => {
                std::env::set_var("BROCADE_TEST_HANDOFF_INTERRUPT_BEFORE_SWITCH", "1")
            }
            NativeInterruption::AfterSwitch
            | NativeInterruption::CandidateLost
            | NativeInterruption::BothLost => {
                std::env::set_var("BROCADE_TEST_HANDOFF_INTERRUPT_AFTER_SWITCH", "1")
            }
            NativeInterruption::None | NativeInterruption::GracefulTimeout => {}
        }
        let worker = {
            let options = options.clone();
            let meter = Arc::clone(&meter);
            let candidate = candidate.clone();
            let target = target.clone();
            thread::spawn(move || {
                perform_handoff_update_at_target(&options, &meter, &offer, &candidate, &target)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if observe_upgrade_runtime(&root).is_some_and(|runtime| {
                matches!(
                    runtime.phase,
                    XrayUpgradePhase::Draining | XrayUpgradePhase::Blocked
                )
            }) {
                break;
            }
            if worker.is_finished() {
                // The worker persists Draining before returning `None`, but this observer may have
                // sampled just before the atomic rename. Recheck once after completion before
                // treating the absence as a terminal pre-admission failure.
                thread::sleep(Duration::from_millis(20));
                if observe_upgrade_runtime(&root).is_some() {
                    break;
                }
                if interruption != NativeInterruption::None {
                    let report = worker.join().unwrap();
                    panic!("interrupted handoff lost its durable runtime: {report:?}");
                }
                let report = worker.join().unwrap();
                panic!("handoff stopped before candidate admission: {report:?}");
            }
            assert!(
                Instant::now() < deadline,
                "candidate did not take admission"
            );
            thread::sleep(Duration::from_millis(20));
        }
        if interruption != NativeInterruption::None {
            assert!(guard_interrupted_handoff(&root).unwrap());
        }
        if matches!(
            interruption,
            NativeInterruption::None
                | NativeInterruption::AfterSwitch
                | NativeInterruption::GracefulTimeout
        ) {
            let (_, old_status) = observe_handoff_process(old).unwrap();
            assert!(
                old_status.udp_dropped,
                "the old UDP socket must be closed before the draining phase is visible"
            );
            let udp_client = UdpSocket::bind("127.0.0.1:0").unwrap();
            udp_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            udp_client
                .send_to(b"new-udp", ("127.0.0.1", public_udp_port))
                .unwrap();
            let mut echoed = [0_u8; 7];
            let (count, _) = udp_client.recv_from(&mut echoed).unwrap();
            assert_eq!(&echoed[..count], b"new-udp");
        }
        if matches!(
            interruption,
            NativeInterruption::CandidateLost | NativeInterruption::BothLost
        ) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(10));
            }
            let runtime = read_upgrade_runtime(&root).unwrap().unwrap();
            let candidate = exact_upgrade_process(runtime.candidate.as_ref().unwrap()).unwrap();
            assert!(observe_handoff_process(old).unwrap().1.udp_dropped);
            let mut completed = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
            completed
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            completed.write_all(b"accounted").unwrap();
            let mut echoed = [0_u8; 9];
            completed.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"accounted");
            completed.shutdown(Shutdown::Both).unwrap();
            drop(completed);
            let before = crate::read_usage_report(&root).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let before_stats = loop {
                let stats =
                    crate::xray_grpc::query_stats(api_address, "inbound>>>fwd:18>>>traffic>>>")
                        .unwrap();
                if stats.iter().any(|stat| stat.value > 0) {
                    break stats;
                }
                assert!(
                    Instant::now() < deadline,
                    "completed flow was not counted: {stats:?}"
                );
                thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(before_stats.len(), 2);
            assert!(
                before_stats.iter().any(|stat| stat.value > 0),
                "{before_stats:?}"
            );
            let epochs = crate::accounting::handoff_journal_epochs(&root).unwrap();
            crash_test_process(candidate);
            if interruption == NativeInterruption::BothLost {
                crash_test_process(old);
            }
            std::env::set_var("BROCADE_TEST_CRASH_RECOVERY_INTERRUPT_AFTER_LAUNCH", "1");
            let error = crate::guard_xray_once(&root, true).unwrap_err();
            assert!(
                error.contains("test interruption after crash recovery launch"),
                "{error}"
            );
            std::env::remove_var("BROCADE_TEST_CRASH_RECOVERY_INTERRUPT_AFTER_LAUNCH");
            let first_replacement = crate::serving_xray_process_in(&root).unwrap();
            let checkpoint = root.join("xray-crash-recovery.json");
            let mut saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&checkpoint).unwrap()).unwrap();
            assert_eq!(saved["failures"], 1);
            assert!(
                saved["retry_after_unix_secs"].as_i64().unwrap()
                    > crate::current_unix_secs().unwrap()
            );
            crate::guard_xray_once(&root, true).unwrap();
            assert_eq!(
                crate::serving_xray_process_in(&root),
                Some(first_replacement)
            );
            if interruption == NativeInterruption::BothLost {
                crash_test_process(first_replacement);
            }
            saved["retry_after_unix_secs"] = 0.into();
            crate::fsutil::atomic_write_private(&checkpoint, &serde_json::to_vec(&saved).unwrap())
                .unwrap();
            crate::guard_xray_once(&root, true).unwrap();
            assert!(read_upgrade_runtime(&root).unwrap().is_none());
            assert!(!checkpoint.exists());
            let replacement = crate::serving_xray_process_in(&root).unwrap();
            if interruption == NativeInterruption::CandidateLost {
                assert_eq!(
                    replacement, first_replacement,
                    "recovery must adopt its already running process"
                );
            } else {
                assert_ne!(replacement, first_replacement);
            }
            assert_ne!(replacement, candidate);
            assert_ne!(replacement, old);
            let (_, status) = observe_handoff_process(replacement).unwrap();
            assert!(status.ready && status.accepting && status.retirement_supported);
            assert_eq!(
                crate::accounting::handoff_journal_epochs(&root).unwrap(),
                epochs
            );
            let after = crate::read_usage_report(&root).unwrap();
            assert_eq!(after.xray_epoch, before.xray_epoch);
            assert!(after.sequence > before.sequence);
            assert!(
                after.zero_windows.is_empty(),
                "crash downtime must remain unknown"
            );
            let after_stats =
                crate::xray_grpc::query_stats(api_address, "inbound>>>fwd:18>>>traffic>>>")
                    .unwrap();
            for counter in &before_stats {
                let current = after_stats
                    .iter()
                    .find(|current| current.name == counter.name);
                assert!(
                    current.is_some_and(|current| current.value >= counter.value),
                    "before {counter:?}, after {after_stats:?}"
                );
            }
            let failure: StoredReport =
                serde_json::from_slice(&fs::read(root.join("xray-crash-report.json")).unwrap())
                    .unwrap();
            assert_eq!(failure.report.outcome, XrayReleaseOutcome::FailedDirty);
            assert_eq!(failure.report.release_id, 91);
            let success: StoredReport = serde_json::from_slice(
                &fs::read(root.join(crash_recovery::RECOVERED_REPORT_FILE)).unwrap(),
            )
            .unwrap();
            assert_eq!(success.report.outcome, XrayReleaseOutcome::Succeeded);
            assert_eq!(success.report.attempt, runtime.attempt);
            assert_eq!(
                success.report.running_sha256,
                Some(runtime.candidate.unwrap().sha256)
            );
            crate::guard_xray_once(&root, true).unwrap();
            assert_eq!(crate::serving_xray_process_in(&root), Some(replacement));
            let udp_client = UdpSocket::bind("127.0.0.1:0").unwrap();
            udp_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            udp_client
                .send_to(b"recovered-udp", ("127.0.0.1", public_udp_port))
                .unwrap();
            let mut echoed = [0_u8; 13];
            let (count, _) = udp_client.recv_from(&mut echoed).unwrap();
            assert_eq!(&echoed[..count], b"recovered-udp");
            // Continue below with the recovered generation as the echo/UDP target. The old TCP
            // flow is intentionally lost by crash recovery rather than a graceful drain.
            old_client = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
            old_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
        }
        old_client.write_all(b"during").unwrap();
        let mut echoed = [0_u8; 6];
        old_client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"during");
        if interruption == NativeInterruption::AfterSwitch {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "interruption hook did not return"
                );
                thread::sleep(Duration::from_millis(10));
            }
            old_client.write_all(b"recovered").unwrap();
            let mut echoed = [0_u8; 9];
            old_client.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"recovered");
        }
        if interruption == NativeInterruption::GracefulTimeout {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "graceful timeout did not return control to convergence"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let runtime = observe_upgrade_runtime(&root).expect("blocked two-generation runtime");
            assert_eq!(runtime.phase, XrayUpgradePhase::Blocked);
            let recovery_started = Instant::now();
            assert!(recover_interrupted_handoff(&options, &meter).unwrap());
            assert!(
                recovery_started.elapsed() < Duration::from_secs(2),
                "one blocked recovery step must stay bounded: {:?}",
                recovery_started.elapsed()
            );
            assert_eq!(
                observe_upgrade_runtime(&root)
                    .expect("old flow is still draining")
                    .phase,
                XrayUpgradePhase::Blocked
            );
        }
        if interruption == NativeInterruption::BeforeSwitch {
            let deadline = Instant::now() + Duration::from_secs(20);
            while observe_upgrade_runtime(&root).is_some() {
                recover_interrupted_handoff(&options, &meter).unwrap();
                if observe_upgrade_runtime(&root).is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "interrupted pre-switch handoff did not roll back: {:?}",
                    observe_upgrade_runtime(&root)
                );
                thread::sleep(Duration::from_millis(20));
            }
        }
        if interruption == NativeInterruption::BeforeSwitchOldLost {
            let _ = old_client.shutdown(Shutdown::Both);
            crate::terminate_processes(&[old], crate::XRAY_TERM_GRACE, crate::XRAY_KILL_GRACE)
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            while observe_upgrade_runtime(&root).is_some() {
                recover_interrupted_handoff(&options, &meter).unwrap();
                if observe_upgrade_runtime(&root).is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "orphaned candidate did not recover: {:?}",
                    observe_upgrade_runtime(&root)
                );
                thread::sleep(Duration::from_millis(20));
            }
        }

        let mut new_client = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
        new_client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        new_client.write_all(b"newgen").unwrap();
        let mut echoed = [0_u8; 6];
        new_client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"newgen");
        let _ = old_client.shutdown(Shutdown::Both);
        new_client.shutdown(Shutdown::Both).unwrap();
        drop(old_client);
        drop(new_client);

        let report = worker.join().unwrap();
        let yielded = report.is_none();
        if interruption == NativeInterruption::GracefulTimeout
            || (interruption == NativeInterruption::None && yielded)
        {
            assert!(
                yielded,
                "a handoff that yielded to bounded convergence must not report a terminal result"
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            while observe_upgrade_runtime(&root).is_some() {
                recover_interrupted_handoff(&options, &meter).unwrap();
                if observe_upgrade_runtime(&root).is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "gracefully blocked handoff did not finish after the old flow drained"
                );
                thread::sleep(Duration::from_millis(20));
            }
        } else if interruption != NativeInterruption::None {
            let report = report.expect("interruption hook must return a terminal test report");
            std::env::remove_var("BROCADE_TEST_HANDOFF_INTERRUPT_BEFORE_SWITCH");
            std::env::remove_var("BROCADE_TEST_HANDOFF_INTERRUPT_AFTER_SWITCH");
            assert_eq!(
                report.outcome,
                XrayReleaseOutcome::FailedDirty,
                "{report:?}"
            );
            if interruption == NativeInterruption::AfterSwitch {
                let deadline = Instant::now() + Duration::from_secs(20);
                while observe_upgrade_runtime(&root).is_some() {
                    recover_interrupted_handoff(&options, &meter).unwrap();
                    if observe_upgrade_runtime(&root).is_none() {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "interrupted handoff did not retire"
                    );
                    thread::sleep(Duration::from_millis(20));
                }
            }
        } else {
            let report = report.expect("completed handoff report");
            assert_eq!(report.outcome, XrayReleaseOutcome::Succeeded, "{report:?}");
            assert!(!report.forced_retirement);
        }
        let mut current = crate::serving_xray_process_in(&root).unwrap();
        if interruption == NativeInterruption::BeforeSwitch {
            assert_eq!(current, old);
            assert!(crate::process_is_live(old));
        } else {
            assert_ne!(current, old);
            assert!(!crate::process_is_live(old));
        }
        if matches!(
            interruption,
            NativeInterruption::AfterSwitch
                | NativeInterruption::CandidateLost
                | NativeInterruption::BothLost
                | NativeInterruption::BeforeSwitchOldLost
                | NativeInterruption::GracefulTimeout
        ) || yielded
        {
            let name = if matches!(
                interruption,
                NativeInterruption::CandidateLost | NativeInterruption::BothLost
            ) {
                crash_recovery::RECOVERED_REPORT_FILE
            } else {
                REPORT_FILE
            };
            let stored: StoredReport = serde_json::from_slice(
                &fs::read(root.join(name)).expect("recovered success report"),
            )
            .unwrap();
            assert_eq!(stored.report.outcome, XrayReleaseOutcome::Succeeded);
            assert!(stored.report.performed_update);
            assert_eq!(stored.report.release_id, 91);
        } else {
            assert!(!root.join(REPORT_FILE).exists());
        }
        if interruption == NativeInterruption::None {
            let release_report_before_config = fs::read(root.join(REPORT_FILE)).ok();
            let mut old_config_client = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
            old_config_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            old_config_client.write_all(b"config-before").unwrap();
            let mut echoed = [0_u8; 13];
            old_config_client.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"config-before");

            let previous = fs::read_to_string(&config).unwrap();
            let mut desired: serde_json::Value = serde_json::from_str(&previous).unwrap();
            desired["log"]["loglevel"] = "error".into();
            desired["outbounds"][0]["settings"] =
                serde_json::json!({"domainStrategy": "UseIPv4v6"});
            let next_api_address = TcpListener::bind("[::1]:0").unwrap().local_addr().unwrap();
            let next_api_port = next_api_address.port();
            let next_public_port = public_port;
            let metrics_port = port();
            let added_vless_port = port();
            desired["api"]["tag"] = "control-v2".into();
            desired["inbounds"][0]["tag"] = "control-v2".into();
            desired["inbounds"][0]["listen"] = "::1".into();
            desired["inbounds"][0]["port"] = next_api_port.into();
            desired["inbounds"][1]["port"] = next_public_port.into();
            desired["inbounds"][1]["streamSettings"] =
                serde_json::json!({"sockopt": {"tcpKeepAliveIdle": 37}});
            desired["routing"]["rules"][0]["inboundTag"] = serde_json::json!(["control-v2"]);
            desired["routing"]["rules"][0]["outboundTag"] = "control-v2".into();
            desired["metrics"] = serde_json::json!({"tag": "metrics", "listen": format!("127.0.0.1:{metrics_port}")});
            desired["inbounds"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "tag": "added-vless", "listen": "127.0.0.1", "port": added_vless_port,
                    "protocol": "vless", "settings": {"decryption": "none", "clients": []},
                }));
            let added_grants = vec![brocade_deployment::plan::GrantInbound {
                tag: "added-vless".to_owned(),
                clients: vec![brocade_deployment::plan::GrantClient {
                    email: "test-handoff-user".to_owned(),
                    uuid: "00000000-0000-4000-8000-000000000001".to_owned(),
                    flow: None,
                    level: 0,
                }],
            }];
            let desired = serde_json::to_string(&desired).unwrap();
            {
                let _guard = meter
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                crate::converge_linux_xray(
                    &root,
                    &brocade_deployment::plan::DesiredArtifact::Present {
                        content: desired.clone(),
                        sha256: "ignored-by-agent".to_owned(),
                    },
                    false,
                    Some((603, 0)),
                    brocade_deployment::protocol::XrayConfigUpdatePolicy::Handoff,
                    Some(&added_grants),
                )
                .unwrap();
            }
            let runtime = observe_upgrade_runtime(&root).expect("config handoff runtime");
            assert_eq!(
                runtime.source,
                XrayUpgradeSource::ConfigDeployment {
                    deployment_id: 603,
                    claim_generation: 0,
                }
            );
            assert_eq!(runtime.strategy, XrayReleaseStrategy::HandoffForce);
            assert_eq!(runtime.phase, XrayUpgradePhase::Draining);
            assert_eq!(runtime.old.pid, current.pid);

            old_config_client.write_all(b"config-during").unwrap();
            let mut echoed = [0_u8; 13];
            old_config_client.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"config-during");
            assert!(TcpStream::connect(api_address).is_err());
            let (_, old_status) = observe_handoff_process(current).unwrap();
            let old_listener = old_status
                .listeners
                .iter()
                .find(|listener| listener.address == format!("127.0.0.1:{public_port}"))
                .unwrap();
            assert!(old_listener.closed);
            let candidate_pid = runtime.candidate.as_ref().unwrap().pid;
            let (_, new_status) =
                observe_handoff_process(crate::process_ref(candidate_pid).unwrap()).unwrap();
            let new_listener = new_status
                .listeners
                .iter()
                .find(|listener| listener.address == format!("127.0.0.1:{public_port}"))
                .unwrap();
            assert_ne!(old_listener.socket_cookie, new_listener.socket_cookie, "changed socket options must use a fresh kernel socket, not merely bypass the digest check");
            assert_ne!(
                old_listener.socket_config_sha256,
                new_listener.socket_config_sha256
            );
            let mut new_config_client =
                TcpStream::connect(("127.0.0.1", next_public_port)).unwrap();
            new_config_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            new_config_client.write_all(b"config-new").unwrap();
            let mut echoed = [0_u8; 10];
            new_config_client.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"config-new");
            assert!(crate::xray_grpc::inbound_tags(next_api_address)
                .unwrap()
                .contains(&"control-v2".to_owned()));
            assert!(crate::grants_match_runtime(&added_grants, next_api_address).unwrap());
            assert!(crate::xray_grpc::query_stats(next_api_address, "").is_ok());
            // Simulate an Agent exit immediately after resume, before its grants checkpoint was
            // committed. The guarded recovery must complete the staged full snapshot.
            crate::fsutil::atomic_write_private(&root.join("grants.desired.json"), b"[]").unwrap();
            assert!(guard_interrupted_handoff(&root).unwrap());
            assert_eq!(
                crate::desired_grants_on_disk(&root).unwrap().unwrap(),
                added_grants
            );
            let mut metrics = TcpStream::connect(("127.0.0.1", metrics_port)).unwrap();
            metrics
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            metrics
                .write_all(
                    b"GET /debug/pprof/ HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut response = String::new();
            metrics.read_to_string(&mut response).unwrap();
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "metrics response: {response:?}"
            );

            let udp_client = UdpSocket::bind("127.0.0.1:0").unwrap();
            udp_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            udp_client
                .send_to(b"config-udp", ("127.0.0.1", public_udp_port))
                .unwrap();
            let mut echoed = [0_u8; 10];
            let (count, _) = udp_client.recv_from(&mut echoed).unwrap();
            assert_eq!(&echoed[..count], b"config-udp");

            old_config_client.shutdown(Shutdown::Both).unwrap();
            new_config_client.shutdown(Shutdown::Both).unwrap();
            drop(old_config_client);
            drop(new_config_client);
            let deadline = Instant::now() + Duration::from_secs(20);
            while observe_upgrade_runtime(&root).is_some() {
                recover_interrupted_handoff(&options, &meter).unwrap();
                if observe_upgrade_runtime(&root).is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "configuration handoff did not retire after the old flow drained"
                );
                thread::sleep(Duration::from_millis(20));
            }
            let config_candidate = crate::serving_xray_process_in(&root).unwrap();
            assert_ne!(config_candidate, current);
            assert!(!crate::process_is_live(current));
            assert_eq!(fs::read_to_string(&config).unwrap(), desired);
            assert!(!root.join(CONFIG_PREVIOUS_FILE).exists());
            assert_eq!(
                fs::read(root.join(REPORT_FILE)).ok(),
                release_report_before_config,
                "configuration handoff must not manufacture a binary release receipt"
            );
            current = config_candidate;

            // A real prepared candidate must roll back cleanly without any rmi having damaged
            // the old generation. Then remove every business listener while keeping its accepted
            // TCP connection alive until natural retirement.
            let mut retained = TcpStream::connect(("127.0.0.1", next_public_port)).unwrap();
            retained
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            retained.write_all(b"rollback-before").unwrap();
            let mut echoed = [0_u8; 15];
            retained.read_exact(&mut echoed).unwrap();
            let mut failed: serde_json::Value = serde_json::from_str(&desired).unwrap();
            failed["log"]["loglevel"] = "warning".into();
            let failed = failed.to_string();
            let absent_grant = brocade_deployment::plan::GrantInbound {
                tag: "absent-inbound".to_owned(),
                clients: Vec::new(),
            };
            let error = crate::converge_linux_xray(
                &root,
                &brocade_deployment::plan::DesiredArtifact::Present {
                    content: failed,
                    sha256: "unused".to_owned(),
                },
                false,
                Some((604, 0)),
                brocade_deployment::protocol::XrayConfigUpdatePolicy::Handoff,
                Some(&[absent_grant]),
            )
            .unwrap_err();
            assert!(error.contains("旧代已恢复"), "{error}");
            assert_eq!(fs::read_to_string(&config).unwrap(), desired);
            assert!(observe_upgrade_runtime(&root).is_none());
            assert!(crate::process_is_live(current));
            assert!(!root.join(crate::XRAY_RECOVERY_MARKER).exists());
            assert!(crate::grants_match_runtime(&added_grants, next_api_address).unwrap());
            retained.write_all(b"rollback-after!").unwrap();
            retained.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"rollback-after!");

            let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut collision: serde_json::Value = serde_json::from_str(&desired).unwrap();
            let mut conflicting = collision["inbounds"][1].clone();
            conflicting["tag"] = "occupied-listener".into();
            conflicting["port"] = occupied.local_addr().unwrap().port().into();
            collision["inbounds"]
                .as_array_mut()
                .unwrap()
                .push(conflicting);
            let conflict_started = Instant::now();
            let error = crate::converge_linux_xray(
                &root,
                &brocade_deployment::plan::DesiredArtifact::Present {
                    content: collision.to_string(),
                    sha256: "unused".to_owned(),
                },
                false,
                Some((604, 1)),
                brocade_deployment::protocol::XrayConfigUpdatePolicy::Handoff,
                Some(&added_grants),
            )
            .unwrap_err();
            assert!(error.contains("旧代已恢复"), "{error}");
            assert!(error.contains("EADDRINUSE"), "{error}");
            assert!(
                error.contains(&occupied.local_addr().unwrap().to_string()),
                "{error}"
            );
            assert!(!error.contains("日志末尾"), "{error}");
            assert!(!error.contains("No such file"), "{error}");
            assert!(
                conflict_started.elapsed() < HANDOFF_CANDIDATE_START_TIMEOUT,
                "typed failure waited for the control timeout"
            );
            assert_eq!(fs::read_to_string(&config).unwrap(), desired);
            assert!(observe_upgrade_runtime(&root).is_none());
            assert_eq!(crate::serving_xray_processes_in(&root), vec![current]);
            retained.write_all(b"rollback-after!").unwrap();
            retained.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"rollback-after!");
            drop(occupied);

            let mut removed: serde_json::Value = serde_json::from_str(&desired).unwrap();
            let direct_api_port = port();
            let direct_api_address = SocketAddr::from(([127, 0, 0, 1], direct_api_port));
            removed["api"]["listen"] = format!("127.0.0.1:{direct_api_port}").into();
            removed["inbounds"] = serde_json::json!([]);
            removed["routing"]["rules"] = serde_json::json!([]);
            removed.as_object_mut().unwrap().remove("metrics");
            let removed = removed.to_string();
            crate::converge_linux_xray(
                &root,
                &brocade_deployment::plan::DesiredArtifact::Present {
                    content: removed.clone(),
                    sha256: "unused".to_owned(),
                },
                false,
                Some((605, 0)),
                brocade_deployment::protocol::XrayConfigUpdatePolicy::Handoff,
                Some(&[]),
            )
            .unwrap();
            assert!(crate::xray_grpc::inbound_tags(direct_api_address)
                .unwrap()
                .is_empty());
            let runtime = observe_upgrade_runtime(&root).unwrap();
            let (manifest, _) = observe_handoff_process(
                crate::process_ref(runtime.candidate.as_ref().unwrap().pid).unwrap(),
            )
            .unwrap();
            assert_eq!(
                manifest.listeners.len(),
                1,
                "only the direct API should remain"
            );
            retained.write_all(b"removed-still!!").unwrap();
            retained.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"removed-still!!");
            assert!(crate::process_is_live(current));
            assert!(
                TcpStream::connect(("127.0.0.1", next_public_port)).is_err(),
                "old listener must not remain bound until the accepted connection drains"
            );
            retained.shutdown(Shutdown::Both).unwrap();
            drop(retained);
            let deadline = Instant::now() + Duration::from_secs(20);
            while observe_upgrade_runtime(&root).is_some() {
                recover_interrupted_handoff(&options, &meter).unwrap();
                assert!(
                    Instant::now() < deadline,
                    "removed listener generation did not retire"
                );
                thread::sleep(Duration::from_millis(20));
            }
            assert!(!crate::process_is_live(current));
            assert!(TcpStream::connect(("127.0.0.1", public_port)).is_err());
            assert!(TcpStream::connect(("127.0.0.1", next_public_port)).is_err());
            assert!(TcpStream::connect(next_api_address).is_err());
            assert!(TcpStream::connect(("127.0.0.1", added_vless_port)).is_err());
            assert!(TcpStream::connect(("127.0.0.1", metrics_port)).is_err());
            assert!(!root.join(CONFIG_GRANTS_FILE).exists());
            assert_eq!(fs::read_to_string(&config).unwrap(), removed);
            assert_eq!(
                fs::read(root.join(REPORT_FILE)).ok(),
                release_report_before_config
            );
            current = crate::serving_xray_process_in(&root).unwrap();
        }
        let journal = fs::read(crate::accounting::journal_path(&root)).unwrap();
        assert_eq!(
            u32::from_le_bytes(journal[28..32].try_into().unwrap()),
            current.pid as u32
        );
        server.join().unwrap();
        stop_udp.store(true, Ordering::Release);
        udp_server.join().unwrap();
        std::env::remove_var("BROCADE_XRAY_BIN");
        std::env::remove_var("BROCADE_TEST_LOG_SINK");
        std::env::remove_var("BROCADE_TEST_HANDOFF_INTERRUPT_BEFORE_SWITCH");
        std::env::remove_var("BROCADE_TEST_HANDOFF_INTERRUPT_AFTER_SWITCH");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(binaries);
    }

    #[test]
    fn offer_requires_lowercase_full_digests() {
        let mut offer = XrayReleaseOffer {
            release_id: 1,
            attempt: 1,
            version: "v1".to_owned(),
            url: "https://console/xray".to_owned(),
            sha256: "a".repeat(64),
            previous_sha256: "b".repeat(64),
            strategy: XrayReleaseStrategy::Restart,
            drain_timeout_secs: None,
        };
        assert!(validate_offer(&offer).is_ok());
        offer.sha256 = "A".repeat(64);
        assert!(validate_offer(&offer).is_err());
        offer.sha256 = "a".repeat(64);
        offer.url = "file:///tmp/xray".to_owned();
        assert!(validate_offer(&offer).is_err());
    }

    #[test]
    fn handoff_drain_deadline_stays_independent_from_the_one_hour_execution_timeout() {
        let mut offer = XrayReleaseOffer {
            release_id: 1,
            attempt: 1,
            version: "v1".to_owned(),
            url: "https://console/xray".to_owned(),
            sha256: "a".repeat(64),
            previous_sha256: "b".repeat(64),
            strategy: XrayReleaseStrategy::Handoff,
            drain_timeout_secs: Some(XRAY_HANDOFF_DEFAULT_DRAIN_SECS),
        };
        assert!(validate_offer(&offer).is_ok());
        offer.drain_timeout_secs = Some(XRAY_HANDOFF_MAX_DRAIN_SECS);
        assert!(validate_offer(&offer).is_ok());
        offer.drain_timeout_secs = Some(XRAY_HANDOFF_MAX_DRAIN_SECS + 1);
        assert!(validate_offer(&offer).is_err());
        offer.drain_timeout_secs = Some(XRAY_HANDOFF_MIN_DRAIN_SECS - 1);
        assert!(validate_offer(&offer).is_err());
    }

    #[test]
    fn recovered_means_the_managed_path_is_back_on_the_frozen_digest() {
        let previous = "b".repeat(64);
        assert_eq!(
            classify_failure(true, Some(&previous), Some(&previous), &previous),
            XrayReleaseOutcome::FailedRecovered
        );
        assert_eq!(
            classify_failure(true, Some(&"c".repeat(64)), Some(&previous), &previous),
            XrayReleaseOutcome::FailedDirty
        );
        assert_eq!(
            classify_failure(true, Some(&previous), None, &previous),
            XrayReleaseOutcome::FailedDirty
        );
        assert_eq!(
            classify_failure(false, Some(&previous), None, &previous),
            XrayReleaseOutcome::FailedRecovered
        );
        assert_eq!(
            classify_failure(false, None, None, &previous),
            XrayReleaseOutcome::FailedDirty
        );
    }

    #[test]
    fn version_preflight_compares_the_exact_xray_version_field() {
        assert!(reports_expected_version(
            "Xray 26.9.1 (Xray, Penetrates Everything.) baseline (go1.26 linux/amd64)",
            "26.9.1"
        ));
        assert!(!reports_expected_version(
            "Xray 126.9.10 anything",
            "26.9.1"
        ));
        assert!(!reports_expected_version("not-xray 26.9.1", "26.9.1"));
    }

    #[test]
    fn managed_target_rejects_a_group_writable_executable() {
        let dir = temp_dir("unsafe-mode");
        let target = dir.join("xray");
        fs::write(&target, b"binary").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o775)).unwrap();

        assert!(validate_managed_target(&target).is_err());

        let _ = fs::remove_dir_all(dir);
    }
}
