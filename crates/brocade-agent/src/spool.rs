//! On-disk retry spools, and the runtime observation reporting that hangs off
//! them.
//!
//! Usage, convergence results and recent host-load windows share one mechanism: write to disk,
//! then send. Their overflow policies differ: accounting never evicts a checkpoint, while
//! operational observations keep a bounded recent history.
use std::{
    borrow::Cow,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

use brocade_deployment::protocol::{
    LocalReconcileReport, NodeRuntimeReport, NodeVersions, SpoolBacklog, UserOnlineSources,
};

use crate::{
    current_unix_secs, fsutil::atomic_write_private, http::HttpClient, observe_geodata,
    options::Options, run_command, wg::wireguard_health_snapshot,
};

/// One on-disk retry spool.
///
/// All users share the property that a locally established fact must survive at least a normal
/// control-plane outage. They differ in endpoint, depth and overflow policy, hence one descriptor
/// rather than several copies which eventually get fixed on only one side.
#[derive(Clone, Copy)]
pub(crate) struct Spool {
    pub(crate) file: &'static str,
    pub(crate) endpoint: &'static str,
    pub(crate) max: usize,
    /// Log prefix, and the unit noun in the over-limit message
    pub(crate) what: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) dropped_kind: DroppedKind,
    /// Statuses which prove this exact body can never become valid. Authentication failures,
    /// throttling, timeouts and conflicts are deliberately absent: all can recover unchanged.
    pub(crate) terminal_statuses: &'static [u16],
}

/// Bounded accounting spool. Never evict an old epoch/generation's final checkpoint: a newer
/// process or namespace cannot recover its ownership or bytes. Backpressure stops destructive
/// changes; continuous data-plane counters can catch up once reporting recovers.
pub(crate) const USAGE_SPOOL: Spool = Spool {
    file: "usage.spool.jsonl",
    endpoint: "/agent/v1/usage",
    max: 720,
    what: "usage",
    unit: "读数",
    dropped_kind: DroppedKind::Usage,
    terminal_statuses: &[],
};

/// Forwarding counters use a separate endpoint and ownership ledger but the same cumulative-
/// counter durability rule as subscription usage.
pub(crate) const FORWARDING_USAGE_SPOOL: Spool = Spool {
    file: "forwarding-usage.spool.jsonl",
    endpoint: "/agent/v1/forwarding/usage",
    max: 720,
    what: "forwarding usage",
    unit: "读数",
    dropped_kind: DroppedKind::Usage,
    terminal_statuses: &[],
};

// Optional detail checkpoints are durable before Xray may reuse their resident slots.
// They have independent file/network locks and never evict an unacknowledged checkpoint.
pub(crate) const DOMAIN_DETAIL_SPOOL: Spool = Spool {
    file: "other-domain.spool.jsonl",
    endpoint: "/agent/v1/other-domain-usage",
    max: 720,
    what: "other-domain",
    unit: "明细检查点",
    dropped_kind: DroppedKind::Detail,
    terminal_statuses: &[],
};
pub(crate) const FAILED_DETAIL_SPOOL: Spool = Spool {
    file: "sniffing-failed.spool.jsonl",
    endpoint: "/agent/v1/sniffing-failed-usage",
    what: "sniffing-failed",
    ..DOMAIN_DETAIL_SPOOL
};
const DETAIL_SPOOL_MAX_BYTES: usize = 32 * 1024 * 1024;

/// The observation spool is far shorter than the usage one because it cannot
/// naturally accumulate: an entry appears only after desired was fetched and one
/// convergence ran, and when the control plane is unreachable `desired_request`
/// fails first, so that point is never reached. Tens of entries can only mean the
/// control plane lost just its reporting endpoint, and old entries are worthless
/// then anyway.
pub(crate) const OBSERVATION_SPOOL: Spool = Spool {
    file: "observation.spool.jsonl",
    endpoint: "/agent/v1/observation",
    max: 64,
    what: "observation",
    unit: "条收敛结果",
    dropped_kind: DroppedKind::Observation,
    terminal_statuses: &[400, 404, 409, 410, 422],
};

/// Thirty-second host windows retained across a Console maintenance window. Two hours of old CPU
/// state would be misleading and would race the control plane's one-hour raw-history compactor, so
/// this deliberately keeps only the most recent hour (120 complete windows).
pub(crate) const LOAD_SPOOL: Spool = Spool {
    file: "load.spool.jsonl",
    endpoint: "/agent/v1/load",
    max: 120,
    what: "load",
    unit: "个指标窗口",
    dropped_kind: DroppedKind::Load,
    terminal_statuses: &[400, 404, 410, 422],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DroppedKind {
    Usage,
    Observation,
    Load,
    Detail,
}

impl DroppedKind {
    const fn file(self) -> &'static str {
        match self {
            Self::Usage => "spool-dropped-usage",
            Self::Observation => "spool-dropped-observation",
            Self::Load => "spool-dropped-load",
            Self::Detail => "spool-dropped-detail",
        }
    }
}

/// Pre-v23 cumulative count. It cannot be attributed after the fact, so new agents preserve it as
/// the unclassified remainder of the all-kinds total instead of presenting it as lost usage.
const LEGACY_DROPPED_FILE: &str = "spool-dropped";
/// Last round's local reconcile result. In a file rather than memory for the same
/// reason: `reconcile_local` may have been run on its own by the `repair`
/// subcommand, which is a different process.
const LOCAL_RECONCILE_FILE: &str = "local-reconcile.json";

// Sampling may append while a reporter is waiting on the network. Keep file mutation and network
// serialization as two different locks: holding one lock across both would make the durable queue
// exist on paper while still letting a slow POST move the sampling clock.
static USAGE_FILE_LOCK: Mutex<()> = Mutex::new(());
static OBSERVATION_FILE_LOCK: Mutex<()> = Mutex::new(());
static LOAD_FILE_LOCK: Mutex<()> = Mutex::new(());
static OTHER_FILE_LOCK: Mutex<()> = Mutex::new(());
static USAGE_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static OBSERVATION_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static LOAD_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static OTHER_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static DROPPED_LOCK: Mutex<()> = Mutex::new(());
static DOMAIN_FILE_LOCK: Mutex<()> = Mutex::new(());
static DOMAIN_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static FAILED_FILE_LOCK: Mutex<()> = Mutex::new(());
static FAILED_DRAIN_LOCK: Mutex<()> = Mutex::new(());

fn file_lock(spool: Spool) -> &'static Mutex<()> {
    match spool.file {
        file if file == USAGE_SPOOL.file => &USAGE_FILE_LOCK,
        file if file == OBSERVATION_SPOOL.file => &OBSERVATION_FILE_LOCK,
        file if file == LOAD_SPOOL.file => &LOAD_FILE_LOCK,
        file if file == DOMAIN_DETAIL_SPOOL.file => &DOMAIN_FILE_LOCK,
        file if file == FAILED_DETAIL_SPOOL.file => &FAILED_FILE_LOCK,
        _ => &OTHER_FILE_LOCK,
    }
}

fn drain_lock(spool: Spool) -> &'static Mutex<()> {
    match spool.file {
        file if file == USAGE_SPOOL.file => &USAGE_DRAIN_LOCK,
        file if file == OBSERVATION_SPOOL.file => &OBSERVATION_DRAIN_LOCK,
        file if file == LOAD_SPOOL.file => &LOAD_DRAIN_LOCK,
        file if file == DOMAIN_DETAIL_SPOOL.file => &DOMAIN_DRAIN_LOCK,
        file if file == FAILED_DETAIL_SPOOL.file => &FAILED_DRAIN_LOCK,
        _ => &OTHER_DRAIN_LOCK,
    }
}

fn bump_dropped(kind: DroppedKind, state_dir: &Path, by: u64) {
    let _guard = DROPPED_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = read_dropped_unlocked(kind, state_dir).saturating_add(by);
    let _ = fs::write(state_dir.join(kind.file()), now.to_string());
}

fn read_dropped(kind: DroppedKind, state_dir: &Path) -> u64 {
    let _guard = DROPPED_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read_dropped_unlocked(kind, state_dir)
}

fn read_dropped_unlocked(kind: DroppedKind, state_dir: &Path) -> u64 {
    read_counter_file(&state_dir.join(kind.file()))
}

fn read_legacy_dropped(state_dir: &Path) -> u64 {
    read_counter_file(&state_dir.join(LEGACY_DROPPED_FILE))
}

fn read_counter_file(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

pub(crate) fn record_local_reconcile(
    state_dir: &Path,
    actions: &[String],
    error: Option<&str>,
) -> Result<(), String> {
    let report = LocalReconcileReport {
        at: current_unix_secs().unwrap_or(0),
        actions: actions.to_vec(),
        error: error.map(str::to_owned),
    };
    let text = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    atomic_write_private(&state_dir.join(LOCAL_RECONCILE_FILE), text.as_bytes())
}

/// What is actually installed on this machine.
///
/// Every field may be `None` — unreadable does not mean absent, it may just not
/// be on PATH. Report `None` rather than guess: an invented version number is
/// worse than none.
fn observe_wg_backend(state_dir: &Path) -> Option<String> {
    // An absent config or the explicit disable marker means there is no WireGuard backend to
    // classify. Looking only at `/sys/module/wireguard` used to call every WG-off machine
    // "userspace", even though wg-quick had not selected or started any implementation.
    if !state_dir.join("wireguard.conf").exists() || state_dir.join("wireguard.disabled").exists() {
        return None;
    }

    Some(
        if Path::new("/sys/module/wireguard").exists() {
            "kernel"
        } else {
            "userspace"
        }
        .to_owned(),
    )
}

fn observe_versions(
    state_dir: &Path,
    xray_upgrade_in_progress: bool,
    xray_upgrade_error: Option<&str>,
) -> NodeVersions {
    let xray = crate::options::xray_binary_path();
    let xray_version_output = run_command(&xray.to_string_lossy(), &["version"]).ok();
    let xray_handoff = xray_upgrade_error.map_or_else(
        || {
            crate::xrayupdate::observe_handoff_capability(
                state_dir,
                xray_upgrade_in_progress,
                xray_version_output.as_deref(),
            )
        },
        |error| brocade_deployment::protocol::XrayHandoffCapability {
            state: brocade_deployment::protocol::XrayHandoffState::Blocked,
            reason: Some(format!("Xray 双进程状态损坏：{error}")),
        },
    );
    NodeVersions {
        // The sha256 of the running binary rather than `CARGO_PKG_VERSION`: nobody bumps a
        // workspace version on the way to a node, so that number claimed every build since it was
        // last touched was the same thing. See `identity.rs`.
        agent: crate::identity::self_identity().to_owned(),
        xray: first_line(xray_version_output.clone()),
        xray_installed_sha256: crate::installed_xray_sha256(),
        xray_running_sha256: crate::running_xray_sha256(),
        xray_handoff: Some(xray_handoff),
        kernel_tls_module: Some(crate::kernel_tls_module_loaded()),
        phantun: first_line(run_command("phantun-client", &["--version"]).ok()),
        // OpenVPN alone is not a usable capability: without an openable TUN clone device every
        // provider profile reaches the same local failure and can poison the fleet-wide candidate
        // failure streak. Withhold the capability so Console never assigns VPN Gate work until the
        // host prerequisite is actually usable.
        openvpn: crate::vpngate::tun_device_available()
            .then(|| first_line(run_command("openvpn", &["--version"]).ok()))
            .flatten(),
        vpngate_catalog_probe_workers: Some(crate::vpngate::CATALOG_PROBE_WORKERS),
        wg_tools: first_line(run_command("wg", &["--version"]).ok()),
        // Whether the kernel module is present decides between the in-kernel
        // path and a fallback to wireguard-go / boringtun. Their `wg show`
        // output is identical and throughput differs by an order of magnitude —
        // without asking explicitly this is never discovered.
        wg_backend: observe_wg_backend(state_dir),
    }
}

fn first_line(output: Option<String>) -> Option<String> {
    let text = output?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// Read a current online-source snapshot only when the applied Xray policy enabled the matching
/// counters. A successful empty response is meaningful; a missing/disabled configuration stays
/// `None` so the control plane never mistakes unsupported collection for zero online sources.
fn observe_online_sources(state_dir: &Path) -> Result<Option<Vec<UserOnlineSources>>, String> {
    let path = state_dir.join("xray.json");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let config: serde_json::Value = serde_json::from_str(&content)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let enabled = config
        .pointer("/policy/levels/0/statsUserOnline")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let api_address = crate::xray_api_address(&content)
        .ok_or_else(|| "xray.json does not contain an API address".to_owned())?;
    crate::xray_grpc::online_sources(api_address).map(Some)
}

pub(crate) fn collect_runtime_report(
    state_dir: &Path,
    traffic: Option<brocade_deployment::protocol::NodeTrafficReading>,
) -> Result<NodeRuntimeReport, String> {
    // Inspect and hash each release generation once per snapshot. Capability projection consumes
    // the resulting presence bit instead of rereading and rehashing both process executables.
    let (xray_upgrade, xray_upgrade_error) =
        match crate::xrayupdate::read_upgrade_runtime(state_dir) {
            Ok(runtime) => (runtime, None),
            Err(error) => {
                eprintln!("xray-upgrade: {error}");
                (None, Some(error))
            }
        };
    let versions = observe_versions(
        state_dir,
        xray_upgrade.is_some(),
        xray_upgrade_error.as_deref(),
    );
    let certificate = crate::certfile::observe(state_dir);
    let geodata = observe_geodata();
    let local_reconcile = fs::read_to_string(state_dir.join(LOCAL_RECONCILE_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let spool = collect_spool_backlog(state_dir)?;
    // Presence is ephemeral and must not prevent versions, backlog or geodata from reporting.
    // On failure, `None` lets the preceding snapshot age out naturally instead of inventing a
    // known-zero observation.
    let online_sources = match observe_online_sources(state_dir) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("online sources: {error}");
            None
        }
    };
    Ok(NodeRuntimeReport {
        observed_at_unix_secs: current_unix_secs()?,
        versions,
        certificate,
        geodata,
        local_reconcile,
        wireguard_health: wireguard_health_snapshot(state_dir),
        traffic,
        online_sources,
        xray_upgrade,
        spool,
    })
}

pub(crate) fn collect_spool_backlog(state_dir: &Path) -> Result<SpoolBacklog, String> {
    let usage_dropped = read_dropped(DroppedKind::Usage, state_dir);
    let observation_dropped = read_dropped(DroppedKind::Observation, state_dir);
    let load_dropped = read_dropped(DroppedKind::Load, state_dir);
    Ok(SpoolBacklog {
        observation: spool_read(OBSERVATION_SPOOL, state_dir)?.len() as u32,
        load: spool_read(LOAD_SPOOL, state_dir)?.len() as u32,
        usage: spool_read(USAGE_SPOOL, state_dir)?
            .len()
            .saturating_add(spool_read(FORWARDING_USAGE_SPOOL, state_dir)?.len())
            .try_into()
            .unwrap_or(u32::MAX),
        dropped: read_legacy_dropped(state_dir)
            .saturating_add(usage_dropped)
            .saturating_add(observation_dropped)
            .saturating_add(load_dropped),
        usage_dropped: Some(usage_dropped),
        observation_dropped: Some(observation_dropped),
        load_dropped: Some(load_dropped),
    })
}

pub(crate) fn send_runtime_report(
    options: &Options,
    report: &NodeRuntimeReport,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/runtime", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "runtime report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

pub(crate) fn runtime_cycle(options: &Options) -> Result<(), String> {
    // One-shot commands do not start the daemon's dedicated meter. Preserve their durable
    // sample, but keep the report assembler itself read-only in every execution mode.
    let traffic = match crate::traffic::sample(&options.state_dir) {
        Ok(reading) => Some(reading),
        Err(error) => {
            eprintln!("traffic: {error}");
            None
        }
    };
    let report = collect_runtime_report(&options.state_dir, traffic)?;
    send_runtime_report(options, &report)
}

fn spool_path(spool: Spool, state_dir: &Path) -> PathBuf {
    state_dir.join(spool.file)
}

pub(crate) fn spool_push<T: serde::Serialize>(
    spool: Spool,
    state_dir: &Path,
    item: &T,
) -> Result<(), String> {
    spool_push_inner(spool, state_dir, item, false)
}

/// Append a fact unless the exact serialized fact is already durable. Configuration convergence
/// recovery uses this after an Agent crash: the crash may have happened after the append but
/// before its private recovery journal was removed, and replay must not manufacture a second
/// observation while the first one is still queued.
pub(crate) fn spool_push_unique<T: serde::Serialize>(
    spool: Spool,
    state_dir: &Path,
    item: &T,
) -> Result<(), String> {
    spool_push_inner(spool, state_dir, item, true)
}

fn spool_push_inner<T: serde::Serialize>(
    spool: Spool,
    state_dir: &Path,
    item: &T,
    unique: bool,
) -> Result<(), String> {
    let _guard = file_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut lines = spool_read_unlocked(spool, state_dir)?;
    let line = serde_json::to_string(item).map_err(|error| error.to_string())?;
    if unique && lines.iter().any(|existing| existing == &line) {
        return Ok(());
    }
    let merged = if spool.dropped_kind == DroppedKind::Detail {
        lines
            .last()
            .map(|previous| merge_detail_checkpoint(spool, previous, &line))
            .transpose()?
            .flatten()
    } else {
        None
    };
    if let Some(merged) = merged {
        *lines
            .last_mut()
            .expect("a merged checkpoint has an existing tail") = merged;
    } else {
        lines.push(line);
    }
    if spool.dropped_kind == DroppedKind::Detail
        && lines.iter().map(|line| line.len() + 1).sum::<usize>() > DETAIL_SPOOL_MAX_BYTES
    {
        return Err(format!(
            "{}: detail queue byte limit reached; preserved all checkpoints",
            spool.what
        ));
    }
    if lines.len() > spool.max {
        if matches!(spool.dropped_kind, DroppedKind::Usage | DroppedKind::Detail) {
            return Err(format!(
                "{}: accounting queue is full ({}); preserved all checkpoints",
                spool.what, spool.max
            ));
        }
        let dropped = lines.len() - spool.max;
        eprintln!(
            "{}: 队列超过 {} 条，丢掉最旧的 {dropped} {}",
            spool.what, spool.max, spool.unit
        );
        lines.drain(..dropped);
        // A drop must leave a durable, correctly classified trace. Lost usage makes accounting
        // incomplete; observations distinguish release evidence from host-history windows.
        bump_dropped(spool.dropped_kind, state_dir, dropped as u64);
    }
    spool_write_unlocked(spool, state_dir, &lines)
}

// Only the pending tail may be coalesced. Each segment stays cumulative and retains its epoch,
// ownership generation and hour, so replays after an in-flight response are still idempotent.
fn merge_detail_checkpoint(
    spool: Spool,
    previous: &str,
    incoming: &str,
) -> Result<Option<String>, String> {
    use brocade_deployment::{
        other_domain::{
            OtherDomainCounter, OtherDomainReport, DETAIL_CHECKPOINT_VERSION, OTHER_DOMAIN_MAX_ROWS,
        },
        sniffing_failed::{SniffingFailedCounter, SniffingFailedReport},
    };
    let decode = |line: &str| -> Result<OtherDomainReport, String> {
        if spool.file == FAILED_DETAIL_SPOOL.file {
            serde_json::from_str::<SniffingFailedReport>(line)
                .map(|report| {
                    report.map_counters(|r| OtherDomainCounter {
                        counter_id: r.counter_id,
                        bucket: r.bucket,
                        label: r.label,
                        domain: r.target,
                        uplink_bytes: r.uplink_bytes,
                        downlink_bytes: r.downlink_bytes,
                    })
                })
                .map_err(|_| "cannot decode failed-target checkpoint".into())
        } else {
            serde_json::from_str(line).map_err(|_| "cannot decode domain checkpoint".into())
        }
    };
    let old = decode(previous)?;
    let mut next = decode(incoming)?;
    if old.version != DETAIL_CHECKPOINT_VERSION
        || next.version != DETAIL_CHECKPOINT_VERSION
        || old.epoch != next.epoch
        || old.activation_unix_secs != next.activation_unix_secs
        || old.usage_generation_id != next.usage_generation_id
        || old.read_at_unix_secs / 3600 != next.read_at_unix_secs / 3600
    {
        return Ok(None);
    }
    let mut counters = std::collections::BTreeMap::new();
    for row in old.counters.into_iter().chain(next.counters) {
        let key = (
            row.bucket,
            row.label.clone(),
            row.domain.clone(),
            row.counter_id,
        );
        let previous = counters.entry(key).or_insert((0, 0));
        previous.0 = previous.0.max(row.uplink_bytes);
        previous.1 = previous.1.max(row.downlink_bytes);
    }
    if counters.len() > OTHER_DOMAIN_MAX_ROWS {
        return Ok(None);
    }
    next.read_at_unix_secs = next.read_at_unix_secs.max(old.read_at_unix_secs);
    next.limited |= old.limited;
    next.counters = counters
        .into_iter()
        .map(
            |((bucket, label, domain, counter_id), (uplink_bytes, downlink_bytes))| {
                OtherDomainCounter {
                    counter_id,
                    bucket,
                    label,
                    domain,
                    uplink_bytes,
                    downlink_bytes,
                }
            },
        )
        .collect();
    if spool.file == FAILED_DETAIL_SPOOL.file {
        serde_json::to_string(&next.map_counters(|r| SniffingFailedCounter {
            counter_id: r.counter_id,
            bucket: r.bucket,
            label: r.label,
            target: r.domain,
            uplink_bytes: r.uplink_bytes,
            downlink_bytes: r.downlink_bytes,
        }))
    } else {
        serde_json::to_string(&next)
    }
    .map(Some)
    .map_err(|_| "cannot encode merged detail checkpoint".into())
}

pub(crate) fn spool_read(spool: Spool, state_dir: &Path) -> Result<Vec<String>, String> {
    let _guard = file_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    spool_read_unlocked(spool, state_dir)
}

fn spool_read_unlocked(spool: Spool, state_dir: &Path) -> Result<Vec<String>, String> {
    let path = spool_path(spool, state_dir);
    let contents = if spool.dropped_kind == DroppedKind::Detail {
        fs::File::open(path).and_then(|file| {
            let mut text = String::new();
            file.take(DETAIL_SPOOL_MAX_BYTES as u64 + 1)
                .read_to_string(&mut text)?;
            if text.len() > DETAIL_SPOOL_MAX_BYTES {
                return Err(std::io::Error::other("detail spool exceeds byte limit"));
            }
            Ok(text)
        })
    } else {
        fs::read_to_string(path)
    };
    match contents {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

// Only after fsync and atomic rename does it count as on disk. The sample taken
// before restarting xray depends on this most: the very next step is the restart,
// the data inside xray is about to vanish, and a truncated spool is no copy at all.
fn spool_write_unlocked(spool: Spool, state_dir: &Path, lines: &[String]) -> Result<(), String> {
    let path = spool_path(spool, state_dir);
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    atomic_write_private(&path, body.as_bytes())
}

/// Send the spool's contents in order, removing each one that lands. Stop on
/// failure — later entries are newer, and sending them first makes the control
/// plane compute deltas in the wrong order. The same holds for the observation
/// spool: one machine's convergence results within a release are ordered, and
/// out-of-order delivery overwrites new with old.
pub(crate) fn spool_drain(spool: Spool, options: &Options) -> Result<bool, String> {
    // Only reporters serialize here. Producers use the separate file lock and can append while
    // any request below is blocked in DNS, connect, TLS or response IO.
    let _drain_guard = drain_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = spool_read(spool, &options.state_dir)?;
    if lines.is_empty() {
        return Ok(false);
    }
    let client = HttpClient::new(&options.server)?;
    let mut sent = 0;
    let mut failure = None;
    for line in &lines {
        let body = body_for_attempt(spool, line)?;
        match client.request("POST", spool.endpoint, &options.token, Some(&body)) {
            Ok(response) if (200..300).contains(&response.status) => {
                sent += 1;
                println!("{}", response.body);
            }
            // The control plane says outright that this payload is wrong (4xx);
            // resending changes nothing, and discarding is what lets the queue
            // advance. The observation spool depends on this most: a finished or
            // cancelled deployment, or a vanished target row, all answer 4xx
            // (409/404), and that observation will never land — filing it as 5xx
            // and retrying blocks the head of the queue forever.
            Ok(response) if spool.terminal_statuses.contains(&response.status) => {
                sent += 1;
                bump_dropped(spool.dropped_kind, &options.state_dir, 1);
                eprintln!(
                    "{}: 控制面拒绝了一条，丢弃：HTTP {} {}",
                    spool.what, response.status, response.body
                );
            }
            Ok(response) => {
                failure = Some(format!(
                    "{} request failed: HTTP {}",
                    spool.what, response.status
                ));
                break;
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if sent > 0 {
        let _file_guard = file_lock(spool)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current = spool_read_unlocked(spool, &options.state_dir)?;
        if current.len() < sent || current[..sent] != lines[..sent] {
            return Err(format!(
                "{} spool changed at its head while reporting",
                spool.what
            ));
        }
        current.drain(..sent);
        spool_write_unlocked(spool, &options.state_dir, &current)?;
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(sent > 0),
    }
}

/// Load queue age is not clock skew. Stamp the actual attempt time without rewriting the durable
/// line: a retry gets a fresh value, while the original sampling time and deduplication key stay
/// byte-for-byte stable. Other spool protocols retain their exact serialized body.
fn body_for_attempt<'a>(spool: Spool, line: &'a str) -> Result<Cow<'a, str>, String> {
    if spool.file != LOAD_SPOOL.file {
        return Ok(Cow::Borrowed(line));
    }
    let mut value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        // Let the control plane make the existing terminal-4xx decision for a corrupt line. A
        // local parse failure must not pin every valid report behind it forever.
        Err(_) => return Ok(Cow::Borrowed(line)),
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(Cow::Borrowed(line));
    };
    object.insert(
        "sent_at_unix_secs".to_owned(),
        serde_json::Value::from(current_unix_secs()?),
    );
    serde_json::to_string(&value)
        .map(Cow::Owned)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        io::{Read, Write},
        net::TcpListener,
        path::{Path, PathBuf},
        sync::mpsc,
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use crate::options::{ApplyMode, Options};

    use super::{
        body_for_attempt, collect_runtime_report, collect_spool_backlog, first_line,
        observe_wg_backend, read_dropped, spool_drain, spool_push, spool_read, DroppedKind, Spool,
        LOAD_SPOOL, OBSERVATION_SPOOL,
    };

    const TEST_SPOOL: Spool = Spool {
        file: "test.spool.jsonl",
        endpoint: "/agent/v1/test",
        max: 16,
        what: "test",
        unit: "条",
        dropped_kind: DroppedKind::Observation,
        terminal_statuses: &[400, 404, 410, 422],
    };

    #[test]
    fn wg_backend_exists_only_while_wireguard_is_enabled() {
        let dir = state_dir("wg-backend-state");
        assert_eq!(observe_wg_backend(&dir), None);

        fs::write(dir.join("wireguard.conf"), "[Interface]\n").unwrap();
        assert!(matches!(
            observe_wg_backend(&dir).as_deref(),
            Some("kernel" | "userspace")
        ));

        fs::write(dir.join("wireguard.disabled"), "").unwrap();
        assert_eq!(observe_wg_backend(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_upgrade_runtime_is_reported_as_a_blocked_capability() {
        let dir = state_dir("corrupt-upgrade-runtime");
        fs::write(dir.join("xray-upgrade.json"), b"{not-json").unwrap();

        let report = collect_runtime_report(&dir, None).unwrap();
        assert!(report.xray_upgrade.is_none());
        let capability = report.versions.xray_handoff.unwrap();
        assert_eq!(
            capability.state,
            brocade_deployment::protocol::XrayHandoffState::Blocked
        );
        assert!(capability.reason.unwrap().contains("双进程状态损坏"));
        fs::remove_dir_all(dir).unwrap();
    }

    fn state_dir(name: &str) -> PathBuf {
        let dir =
            env::temp_dir().join(format!("brocade-agent-spool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A control plane that answers with scripted status codes and nothing else.
    /// Returns its address plus a handle yielding the bodies it actually
    /// received — half of the spool's semantics is what got sent, which cannot be
    /// seen from what remains.
    fn fake_control_plane(script: Vec<u16>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            for status in script {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                received.push(read_request_body(&mut stream));
                let body = format!("{{\"status\":{status}}}");
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            received
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    fn read_request_body(stream: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 1024];
        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..read]);
            let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= head_end + 4 + length {
                return String::from_utf8_lossy(&raw[head_end + 4..head_end + 4 + length])
                    .into_owned();
            }
        }
        String::new()
    }

    fn options(server: &str, state_dir: &Path) -> Options {
        Options {
            command: "apply-once".to_owned(),
            server: server.to_owned(),
            token: "t".to_owned(),
            state_dir: state_dir.to_path_buf(),
            apply_mode: ApplyMode::StateDir,
            vpngate_stats_window: std::time::Duration::from_secs(900),
        }
    }

    /// When the control plane says outright that an entry is malformed, drop it
    /// and move on. Keeping it for retry blocks the head of the queue: a finished
    /// or cancelled deployment, or a vanished target row, can answer 400/404, that
    /// observation never lands, and everything useful behind it is stuck too.
    #[test]
    fn a_rejected_item_is_dropped_so_the_queue_keeps_moving() {
        let dir = state_dir("drain-4xx");
        for n in 0..3 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let (server, handle) = fake_control_plane(vec![400, 200, 200]);
        assert!(spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap());

        let received = handle.join().unwrap();
        assert_eq!(received.len(), 3, "被拒的那条之后还要继续发后面的");
        assert!(received[0].contains("\"n\":0"));
        assert!(received[2].contains("\"n\":2"));
        assert!(
            spool_read(TEST_SPOOL, &dir).unwrap().is_empty(),
            "三条都处理完了，队列该空"
        );
        assert_eq!(
            read_dropped(DroppedKind::Observation, &dir),
            1,
            "服务端永久拒绝也必须进入对应的丢失计数"
        );
        assert_eq!(read_dropped(DroppedKind::Usage, &dir), 0);
        assert_eq!(read_dropped(DroppedKind::Load, &dir), 0);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_conflict_is_retryable_and_stays_at_the_head() {
        let dir = state_dir("drain-conflict");
        for n in 0..2 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }
        let (server, handle) = fake_control_plane(vec![409]);
        let error = spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap_err();
        assert!(error.contains("409"));
        assert_eq!(handle.join().unwrap().len(), 1);
        assert_eq!(spool_read(TEST_SPOOL, &dir).unwrap().len(), 2);
        assert_eq!(read_dropped(DroppedKind::Observation, &dir), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_never_discards_auth_throttle_timeout_or_conflict_responses() {
        for status in [400, 401, 403, 404, 408, 409, 410, 422, 425, 429] {
            assert!(
                !super::USAGE_SPOOL.terminal_statuses.contains(&status),
                "HTTP {status} can recover with the same queued body"
            );
        }
    }

    /// A 5xx means "cannot deliver right now": stop where you are and leave the
    /// unsent entries as they were. Pressing on scrambles the order the control
    /// plane sees — usage is cumulative and read as deltas, observations are
    /// ordered within a release, and out-of-order delivery overwrites new with
    /// old.
    #[test]
    fn a_server_failure_stops_the_drain_and_keeps_the_rest_in_order() {
        let dir = state_dir("drain-5xx");
        for n in 0..3 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let (server, handle) = fake_control_plane(vec![200, 503]);
        let error = spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap_err();
        assert!(error.contains("503"), "错误里要带上状态码，实际 {error}");

        let received = handle.join().unwrap();
        assert_eq!(received.len(), 2, "撞上 5xx 就不该再发第三条");

        let left = spool_read(TEST_SPOOL, &dir).unwrap();
        assert_eq!(left.len(), 2, "发成功的那条去掉，剩下两条留着");
        assert!(left[0].contains("\"n\":1"), "留下的要保持原顺序");
        assert!(left[1].contains("\"n\":2"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_blocked_reporter_does_not_block_a_new_durable_append() {
        let dir = state_dir("append-during-report");
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 0 })).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request_body(&mut stream);
            accepted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });

        let drain_options = options(&server, &dir);
        let drain = thread::spawn(move || spool_drain(TEST_SPOOL, &drain_options));
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 1 })).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "append waited for network IO"
        );
        release_tx.send(()).unwrap();
        drain.join().unwrap().unwrap();
        server_thread.join().unwrap();

        let left = spool_read(TEST_SPOOL, &dir).unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].contains("\"n\":1"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detail_coalescing_during_delivery_preserves_the_newer_checkpoint() {
        use brocade_deployment::other_domain::{OtherDomainCounter, OtherDomainReport};
        let spool = super::DOMAIN_DETAIL_SPOOL;
        let dir = state_dir("detail-merge-in-flight");
        let report = |up| OtherDomainReport {
            version: 2,
            epoch: "a".repeat(32),
            activation_unix_secs: 7200,
            read_at_unix_secs: 7300,
            usage_generation_id: 1,
            limited: false,
            counters: vec![OtherDomainCounter {
                counter_id: 1,
                bucket: 7200,
                label: "a@t#i".into(),
                domain: "a.example".into(),
                uplink_bytes: up,
                downlink_bytes: 0,
            }],
        };
        spool_push(spool, &dir, &report(10)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = read_request_body(&mut stream);
            accepted_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            body
        });
        let drain_options = options(&server, &dir);
        let drain = thread::spawn(move || spool_drain(spool, &drain_options));
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        spool_push(spool, &dir, &report(15)).unwrap();
        release_tx.send(()).unwrap();
        assert!(
            drain.join().unwrap().is_err(),
            "a changed in-flight body must remain queued"
        );
        assert_eq!(
            serde_json::from_str::<OtherDomainReport>(&worker.join().unwrap())
                .unwrap()
                .counters[0]
                .uplink_bytes,
            10
        );
        let remaining = spool_read(spool, &dir).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            serde_json::from_str::<OtherDomainReport>(&remaining[0])
                .unwrap()
                .counters[0]
                .uplink_bytes,
            15
        );
        let (server, worker) = fake_control_plane(vec![200]);
        assert!(spool_drain(spool, &options(&server, &dir)).unwrap());
        assert_eq!(worker.join().unwrap().len(), 1);
        assert!(spool_read(spool, &dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_detail_coalescing_keeps_target_identity_and_ownership_boundaries() {
        use brocade_deployment::sniffing_failed::{SniffingFailedCounter, SniffingFailedReport};
        let dir = state_dir("failed-detail-coalescing");
        let spool = super::FAILED_DETAIL_SPOOL;
        let mut report = SniffingFailedReport {
            version: 2,
            epoch: "a".repeat(32),
            activation_unix_secs: 7200,
            read_at_unix_secs: 7300,
            usage_generation_id: 1,
            limited: false,
            counters: vec![SniffingFailedCounter {
                counter_id: 1,
                bucket: 7200,
                label: "a@t#i".into(),
                target: "tcp://192.0.2.1:443".into(),
                uplink_bytes: 10,
                downlink_bytes: 0,
            }],
        };
        spool_push(spool, &dir, &report).unwrap();
        report.counters[0].uplink_bytes = 15;
        spool_push(spool, &dir, &report).unwrap();
        let queued = spool_read(spool, &dir).unwrap();
        assert_eq!(queued.len(), 1);
        let checkpoint: SniffingFailedReport = serde_json::from_str(&queued[0]).unwrap();
        assert_eq!(checkpoint.counters[0].uplink_bytes, 15);
        assert_eq!(checkpoint.counters[0].target, "tcp://192.0.2.1:443");
        report.usage_generation_id = 2;
        spool_push(spool, &dir, &report).unwrap();
        report.epoch = "b".repeat(32);
        spool_push(spool, &dir, &report).unwrap();
        assert_eq!(
            spool_read(spool, &dir).unwrap().len(),
            3,
            "ownership and counter lifetimes cannot be merged"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// With the control plane unreachable, nothing may be lost. This is the whole
    /// reason the spool exists: convergence already happened on this machine, and
    /// that result is its only record.
    #[test]
    fn an_unreachable_control_plane_loses_nothing() {
        let dir = state_dir("drain-unreachable");
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 0 })).unwrap();

        // Bind then drop, to obtain a port almost certainly nobody listens on.
        let port = {
            let probe = TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let options = options(&format!("http://127.0.0.1:{port}"), &dir);
        assert!(spool_drain(TEST_SPOOL, &options).is_err());
        assert_eq!(spool_read(TEST_SPOOL, &dir).unwrap().len(), 1);

        let _ = fs::remove_dir_all(dir);
    }

    /// An empty spool must not dial out. This path runs every cycle, and opening a
    /// connection for something that is not there adds load to the control plane
    /// for nothing.
    #[test]
    fn an_empty_spool_does_not_dial_out() {
        let dir = state_dir("drain-empty");
        // An address nobody listens on: actually dialing returns Err here.
        let options = options("http://127.0.0.1:1", &dir);
        assert!(!spool_drain(TEST_SPOOL, &options).unwrap());
        let _ = fs::remove_dir_all(dir);
    }

    /// Each dropped counter must survive restarts without making a lost convergence result look
    /// like lost accounting. The all-kinds total remains available to old consoles.
    #[test]
    fn dropped_counters_survive_and_keep_report_kinds_separate() {
        let dir = state_dir("dropped");
        assert_eq!(read_dropped(DroppedKind::Usage, &dir), 0);
        assert_eq!(read_dropped(DroppedKind::Observation, &dir), 0);
        assert_eq!(read_dropped(DroppedKind::Load, &dir), 0);

        // Historical/permanently rejected losses remain durable even though new accounting
        // queues now apply backpressure instead of evicting an epoch's final checkpoint.
        super::bump_dropped(DroppedKind::Usage, &dir, 3);
        assert_eq!(read_dropped(DroppedKind::Usage, &dir), 3);
        assert_eq!(read_dropped(DroppedKind::Observation, &dir), 0);

        let other = Spool {
            file: "other.spool.jsonl",
            max: 1,
            dropped_kind: DroppedKind::Observation,
            ..TEST_SPOOL
        };
        for n in 0..3 {
            spool_push(other, &dir, &serde_json::json!({ "n": n })).unwrap();
        }
        assert_eq!(read_dropped(DroppedKind::Usage, &dir), 3);
        assert_eq!(read_dropped(DroppedKind::Observation, &dir), 2);

        let backlog = collect_spool_backlog(&dir).unwrap();
        assert_eq!(backlog.dropped, 5, "旧控制面仍能看到全部丢弃");
        assert_eq!(backlog.usage_dropped, Some(3));
        assert_eq!(backlog.observation_dropped, Some(2));
        assert_eq!(backlog.load_dropped, Some(0));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn full_accounting_queue_keeps_every_old_checkpoint() {
        let dir = state_dir("usage-full");
        let tiny = Spool {
            file: "tiny.spool.jsonl",
            max: 2,
            dropped_kind: DroppedKind::Usage,
            ..TEST_SPOOL
        };
        spool_push(
            tiny,
            &dir,
            &serde_json::json!({"epoch": "old", "counter": 100}),
        )
        .unwrap();
        spool_push(
            tiny,
            &dir,
            &serde_json::json!({"epoch": "new", "counter": 20}),
        )
        .unwrap();
        let before = spool_read(tiny, &dir).unwrap();
        assert!(spool_push(
            tiny,
            &dir,
            &serde_json::json!({"epoch": "new", "counter": 30})
        )
        .is_err());
        assert_eq!(spool_read(tiny, &dir).unwrap(), before);
        assert_eq!(read_dropped(DroppedKind::Usage, &dir), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_queue_keeps_only_the_latest_one_hundred_twenty_windows() {
        let dir = state_dir("load-bounded");
        for n in 0..125 {
            spool_push(LOAD_SPOOL, &dir, &serde_json::json!({ "window": n })).unwrap();
        }

        let queued = spool_read(LOAD_SPOOL, &dir).unwrap();
        assert_eq!(queued.len(), 120);
        assert!(queued.first().unwrap().contains("\"window\":5"));
        assert!(queued.last().unwrap().contains("\"window\":124"));
        let backlog = collect_spool_backlog(&dir).unwrap();
        assert_eq!(backlog.load, 120);
        assert_eq!(backlog.load_dropped, Some(5));
        assert_eq!(backlog.dropped, 5);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn each_load_delivery_attempt_gets_a_fresh_clock_sample_without_mutating_the_queue() {
        let queued = r#"{"read_at_unix_secs":1,"sent_at_unix_secs":1}"#;
        let before = crate::current_unix_secs().unwrap();
        let outgoing = body_for_attempt(LOAD_SPOOL, queued).unwrap();
        let value: serde_json::Value = serde_json::from_str(&outgoing).unwrap();
        assert!(value["sent_at_unix_secs"].as_i64().unwrap() >= before);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(queued).unwrap()["sent_at_unix_secs"],
            1,
            "the durable line keeps the original sampling payload"
        );
    }

    #[test]
    fn legacy_dropped_count_stays_unclassified() {
        let dir = state_dir("legacy-dropped");
        fs::write(dir.join(super::LEGACY_DROPPED_FILE), "7").unwrap();

        let backlog = collect_spool_backlog(&dir).unwrap();
        assert_eq!(backlog.dropped, 7);
        assert_eq!(backlog.usage_dropped, Some(0));
        assert_eq!(backlog.observation_dropped, Some(0));
        assert_eq!(backlog.load_dropped, Some(0));

        let _ = fs::remove_dir_all(dir);
    }

    /// The spool file is read back on the next replay, so one over-limit round
    /// must not deform the on-disk format.
    #[test]
    fn the_spool_file_stays_one_json_object_per_line() {
        let dir = state_dir("format");
        for n in 0..3 {
            spool_push(OBSERVATION_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let text = fs::read_to_string(dir.join(OBSERVATION_SPOOL.file)).unwrap();
        assert!(text.ends_with('\n'), "每条以换行收尾，追加才不会粘在一起");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }

        let _ = fs::remove_dir_all(dir);
    }

    /// An unreadable version reports None. Inventing one is worse than none — the
    /// console would show a plausible-looking version unrelated to what is
    /// actually installed.
    #[test]
    fn a_blank_version_line_reads_as_none_not_as_empty_string() {
        assert_eq!(first_line(None), None);
        assert_eq!(first_line(Some(String::new())), None);
        assert_eq!(first_line(Some("   \n".to_owned())), None);
        assert_eq!(
            first_line(Some(
                "Xray 1.8.4 (Xray, Penetrates)\nA unified platform\n".to_owned()
            )),
            Some("Xray 1.8.4 (Xray, Penetrates)".to_owned())
        );
    }
    // An HTTP timeout is an unknown delivery outcome. The queue must retain the exact body
    // until the complete ACK is read, so a retry keeps the Console deduplication identity.
    fn unconfirmed_ack_keeps_the_exact_durable_body(stall: bool) {
        let dir = state_dir(if stall {
            "review-timeout-ack"
        } else {
            "review-lost-ack"
        });
        let payload = serde_json::json!({"agent_instance_id":"review-fixture","sequence":1,"n":7});
        spool_push(super::USAGE_SPOOL, &dir, &payload).unwrap();
        let original = spool_read(super::USAGE_SPOOL, &dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let body1 = read_request_body(&mut first);
            // Model server commit followed by a lost or incomplete ACK.
            if stall {
                first
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{")
                    .unwrap();
                first
                    .set_read_timeout(Some(Duration::from_secs(33)))
                    .unwrap();
                let mut byte = [0];
                assert_eq!(
                    first.read(&mut byte).unwrap(),
                    0,
                    "timeout must close connection"
                );
            }
            drop(first);
            let (mut second, _) = listener.accept().unwrap();
            let body2 = read_request_body(&mut second);
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
            assert_eq!(
                body1, body2,
                "the retry must preserve deduplication identity and payload"
            );
            body2
        });
        let opts = options(&server, &dir);
        let started = Instant::now();
        assert!(spool_drain(super::USAGE_SPOOL, &opts).is_err());
        if stall {
            assert!(started.elapsed() >= Duration::from_secs(29));
            assert!(started.elapsed() < Duration::from_secs(32));
        }
        assert_eq!(
            spool_read(super::USAGE_SPOOL, &dir).unwrap(),
            original,
            "unconfirmed ACK must not delete accounting"
        );
        assert!(spool_drain(super::USAGE_SPOOL, &opts).unwrap());
        assert_eq!(worker.join().unwrap(), original[0]);
        assert!(spool_read(super::USAGE_SPOOL, &dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn committed_usage_with_dropped_ack_is_retained_and_retried_exactly() {
        unconfirmed_ack_keeps_the_exact_durable_body(false);
    }
    #[test]
    fn committed_usage_with_timed_out_body_is_retained_and_retried_exactly() {
        unconfirmed_ack_keeps_the_exact_durable_body(true);
    }
}
