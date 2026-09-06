mod certfile;
mod command;
mod conntrack;
use brocade_probe as e2e;
mod fsutil;
mod http;
mod hy2_port_hop;
mod icmp;
mod identity;
mod inetdiag;
mod load;
mod logcap;
mod options;
mod phantun;
mod probe;
mod realtime;
mod selfupdate;
mod spool;
mod wg;
mod xray_grpc;
pub(crate) use command::{command_success, run_command, run_shell, run_shell_with_timeout};
use http::{HttpClient, HttpResponse};
use options::{ApplyMode, Options};
use phantun::{
    apply_phantun, converge_linux_phantun, observe_linux_phantun, phantun_instances,
    phantun_nat_intact, phantun_running, phantun_runtime_probes, phantun_wanted,
    prepare_phantun_binaries,
};
use probe::{
    collect_e2e_probe_report, collect_link_probe_report, collect_ping_probe_report, e2e_once,
    fetch_e2e_probe_targets, fetch_ping_probe_settings, fetch_probe_targets, judge_hops,
    ping_probe_once, probe_once, read_hop_downlinks, send_e2e_probe_report, send_link_probe_report,
    send_ping_probe_report, xray_listen_ports, XrayListenProtocol,
};
use spool::{
    collect_runtime_report, collect_spool_backlog, record_local_reconcile, runtime_cycle,
    send_runtime_report, spool_drain, spool_push, Spool, OBSERVATION_SPOOL, USAGE_SPOOL,
};

use wg::{
    backbone_lock, check_wg_peers, drift_is_fatal, guard_wireguard, handshake_age_text,
    peer_overlay_ips, ping_all, reset_stale_phantun_passive_peers, set_wireguard_disabled,
    wireguard_conf_mtu, PeerState, WG_GUARD_INTERVAL,
};

use std::{
    collections::BTreeMap,
    env, fs,
    net::IpAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use brocade_core::hash::sha256_hex;
use brocade_deployment::{
    hotswap::{hot_swap, HotSwap},
    plan::{
        AppliedArtifactState, AppliedGrantsState, DesiredArtifact, DesiredGrants, GrantClient,
        GrantInbound, ObservedClient, ObservedInbound,
    },
    protocol::{
        AgentObservationRequest, DesiredStateResponse, E2eProbeRequest, E2eProbeTargetList,
        GeodataFileState, GeodataObservation, LinkHealthRequest, LinkProbeRequest,
        LoadReportRequest, NodeDesiredDeployment, NodeRuntimeReport, PingProbeReportRequest,
        PingProbeSettings, ProbeTargetList, ReportedNodeState, RouteIpReport, SpoolBacklog,
        TargetApplyResult, UsageCounter, UsageReportRequest,
    },
};

const ROUTE_IPV4_HEADER: &str = "X-Brocade-Route-IPv4";
const ROUTE_IPV6_HEADER: &str = "X-Brocade-Route-IPv6";
/// Which architecture this binary was built for. The control plane carries one agent per
/// architecture and has no other source for this value: enrolment records no architecture, and
/// an incorrect guess would hand a machine a binary that downloads, verifies, and cannot run.
const ARCH_HEADER: &str = "X-Brocade-Arch";
const USAGE_CURSOR_FILE: &str = "usage-cursor.json";
const USAGE_GENERATION_FILE: &str = "usage-generation";
/// Presence means the running Xray was launched through the bounded sink. It deliberately sits
/// outside xray.json: logging is agent runtime state, not part of the compiled Xray artifact.
const XRAY_BOUNDED_LOG_MARKER: &str = "xray.bounded-log-v3";
const XRAY_SHARED_POLICY_LOG_MARKER: &str = "xray.bounded-log-v2";
const XRAY_OLD_BOUNDED_LOG_MARKER: &str = "xray.bounded-log-v1";
const AGENT_LOG_MAX_MIB_HEADER: &str = "x-brocade-log-max-mib";
const AGENT_JOURNAL_MAX_MIB_HEADER: &str = "x-brocade-agent-journal-max-mib";
const XRAY_LOG_MAX_MIB_HEADER: &str = "x-brocade-xray-log-max-mib";
const PHANTUN_LOG_MAX_MIB_HEADER: &str = "x-brocade-phantun-log-max-mib";

#[derive(Debug, Serialize, Deserialize)]
struct UsageCursor {
    agent_instance_id: String,
    last_sequence: u64,
}

fn reserve_usage_sequence(state_dir: &Path) -> Result<(String, u64), String> {
    let path = state_dir.join(USAGE_CURSOR_FILE);
    let mut cursor = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<UsageCursor>(&text)
            .map_err(|error| format!("failed to decode usage cursor: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
            UsageCursor {
                agent_instance_id: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                last_sequence: 0,
            }
        }
        Err(error) => return Err(format!("failed to read usage cursor: {error}")),
    };
    cursor.last_sequence = cursor
        .last_sequence
        .checked_add(1)
        .ok_or("usage sequence exhausted")?;
    let text = serde_json::to_string(&cursor).map_err(|error| error.to_string())?;
    fsutil::atomic_write_private(&path, text.as_bytes())?;
    Ok((cursor.agent_instance_id, cursor.last_sequence))
}

fn read_usage_generation(state_dir: &Path) -> Result<Option<i64>, String> {
    match fs::read_to_string(state_dir.join(USAGE_GENERATION_FILE)) {
        Ok(text) => text
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|error| format!("failed to decode usage generation: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read usage generation: {error}")),
    }
}

fn write_usage_generation(state_dir: &Path, generation_id: i64) -> Result<(), String> {
    fsutil::atomic_write_private(
        &state_dir.join(USAGE_GENERATION_FILE),
        generation_id.to_string().as_bytes(),
    )
}

/// Self-healing runs without operator involvement, so every occurrence has to emit a
/// line whose level appears at the start. journald collects logs on these machines and
/// operators filter with `grep`, which cannot match a level embedded in the wording.
fn warn(message: impl AsRef<str>) {
    eprintln!("warn: {}", message.as_ref());
}

// Wrap each round in catch_unwind. By default a panic on a thread terminates only that
// thread, while the process continues and the apply loop keeps running, so the control
// plane observes a healthy machine. A stopped usage thread understates bills, and a
// stopped watchdog leaves drifted links unrepaired; both are harder to detect than an
// error.
// systemd's Restart=always does not cover this: a panic off the main thread does not
// terminate the process, so the unit never restarts.
// AssertUnwindSafe adds no new assumption: the locks here are already handled uniformly
// as usable even when poisoned.
fn each_round(name: &str, body: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
        warn(format!("{name} 线程这一轮 panic 了，下一轮继续"));
    }
}

/// Keep periodic work anchored to its previous tick instead of adding the work duration to every
/// interval. An overrun advances to the first future point on the same grid: immediately replaying
/// missed probes would create a burst without recovering the observations that were missed.
fn next_periodic_tick(previous_tick: Instant, interval: Duration, now: Instant) -> Instant {
    let next_tick = previous_tick + interval;
    if next_tick > now {
        return next_tick;
    }
    let elapsed = now.duration_since(previous_tick);
    let remainder_nanos = elapsed.as_nanos() % interval.as_nanos();
    let remainder = Duration::from_nanos(
        u64::try_from(remainder_nanos).expect("PING probe interval fits into u64 nanoseconds"),
    );
    now + if remainder.is_zero() {
        interval
    } else {
        interval - remainder
    }
}

struct SettingsCache<T> {
    current: Mutex<Option<T>>,
    changed: Condvar,
}

impl<T> Default for SettingsCache<T> {
    fn default() -> Self {
        Self {
            current: Mutex::new(None),
            changed: Condvar::new(),
        }
    }
}

impl<T: Clone + PartialEq> SettingsCache<T> {
    fn publish(&self, settings: T) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.as_ref() != Some(&settings) {
            *current = Some(settings);
            self.changed.notify_all();
        }
    }

    fn wait_for_initial(&self) -> T {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(settings) = current.as_ref() {
                return settings.clone();
            }
            current = self
                .changed
                .wait(current)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn latest(&self) -> Option<T> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Wait until either a different settings snapshot arrives or the sampling deadline is due.
    fn wait_for_change_until(&self, previous: &T, deadline: Instant) -> Option<T> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(settings) = current.as_ref().filter(|settings| *settings != previous) {
                return Some(settings.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            (current, _) = self
                .changed
                .wait_timeout(current, deadline.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

type PingProbeSettingsCache = SettingsCache<PingProbeSettings>;

struct PendingLatestReport<T> {
    report: Option<T>,
    dropped: u64,
}

struct LatestReport<T> {
    pending: Mutex<PendingLatestReport<T>>,
    ready: Condvar,
}

impl<T> Default for LatestReport<T> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(PendingLatestReport {
                report: None,
                dropped: 0,
            }),
            ready: Condvar::new(),
        }
    }
}

impl<T> LatestReport<T> {
    /// Replace an unsent report instead of extending a FIFO of observations that are already stale.
    fn publish(&self, report: T) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.report.replace(report).is_some() {
            pending.dropped = pending.dropped.saturating_add(1);
        }
        self.ready.notify_one();
    }

    fn take(&self) -> (T, u64) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(report) = pending.report.take() {
                return (report, std::mem::take(&mut pending.dropped));
            }
            pending = self
                .ready
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn try_take(&self) -> Option<(T, u64)> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending
            .report
            .take()
            .map(|report| (report, std::mem::take(&mut pending.dropped)))
    }
}

/// Runtime reports are deliberately infrequent, but a healthy spool can be created and drained
/// in one second. Remember the last backlog snapshot queued by this process and publish a fresh
/// runtime report when a drain changes it. Collection and comparison share one lock so a periodic
/// sample cannot publish a stale non-zero value after a concurrent drain has already cleared it.
struct RuntimeReports {
    reports: LatestReport<NodeRuntimeReport>,
    last_spool: Mutex<Option<SpoolBacklog>>,
}

impl Default for RuntimeReports {
    fn default() -> Self {
        Self {
            reports: LatestReport::default(),
            last_spool: Mutex::new(None),
        }
    }
}

impl RuntimeReports {
    fn publish_current(&self, state_dir: &Path) -> Result<(), String> {
        let mut last_spool = self
            .last_spool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = collect_runtime_report(state_dir)?;
        *last_spool = Some(report.spool.clone());
        self.reports.publish(report);
        Ok(())
    }

    fn publish_if_spool_changed(&self, state_dir: &Path) -> Result<(), String> {
        let mut last_spool = self
            .last_spool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = collect_spool_backlog(state_dir)?;
        if last_spool.as_ref() == Some(&current) {
            return Ok(());
        }
        let report = collect_runtime_report(state_dir)?;
        *last_spool = Some(report.spool.clone());
        self.reports.publish(report);
        Ok(())
    }
}

static RUNTIME_REPORTS: OnceLock<Arc<RuntimeReports>> = OnceLock::new();

/// Queue an immediate runtime snapshot in the daemon. One-shot commands do not start the shared
/// reporter, so they retain the synchronous fallback.
pub(crate) fn publish_runtime_now(options: &Options) -> Result<(), String> {
    match RUNTIME_REPORTS.get() {
        Some(reports) => reports.publish_current(&options.state_dir),
        None => runtime_cycle(options),
    }
}

/// Deliver observations without coupling their producer to the network.
///
/// A failed current value is retried with a short bounded backoff. If a newer value arrives while
/// waiting, it replaces the failed one: these reports describe current state, so replaying a FIFO
/// of obsolete samples after an outage would be actively misleading.
fn report_latest_forever<T>(
    name: &'static str,
    reports: &LatestReport<T>,
    send: impl Fn(&T) -> Result<(), String>,
) -> ! {
    loop {
        let (mut report, dropped) = reports.take();
        if dropped > 0 {
            eprintln!("{name}: sender busy; dropped {dropped} stale reports");
        }
        let mut retry = LATEST_REPORT_RETRY_MIN;
        loop {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| send(&report))) {
                Ok(Ok(())) => break,
                Ok(Err(error)) => eprintln!("{name}: {error}; retrying in {retry:?}"),
                Err(_) => warn(format!("{name} 线程这一轮 panic 了，{retry:?} 后重试")),
            }
            thread::sleep(retry);
            retry = retry.saturating_mul(2).min(LATEST_REPORT_RETRY_MAX);
            if let Some((newer, dropped)) = reports.try_take() {
                eprintln!(
                    "{name}: replaced failed report and {} stale pending reports with the latest",
                    dropped
                );
                report = newer;
            }
        }
    }
}

type LatestPingProbeReport = LatestReport<PingProbeReportRequest>;

fn main() {
    if let Err(error) = run() {
        eprintln!("brocade-agent: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    // health and repair do not contact the control plane and therefore need no
    // --server / token, so they branch off before the full parse
    match args.first().map(String::as_str) {
        Some("health") => return health(&health_state_dir(&args)),
        Some("repair") => return reconcile_local(&health_state_dir(&args), Drift::Always),
        // Internal stdin consumer used by the Xray/Phantun launch pipelines. It must not require
        // a control-plane URL or token: doing so would put a secret on every child command line.
        Some("log-sink") => return logcap::run_args(&args[1..]),
        _ => {}
    }

    let options = Options::parse(args)?;
    match options.command.as_str() {
        "desired" => {
            let client = HttpClient::new(&options.server)?;
            let response = desired_request(&client, &options)?;
            if response.status == 204 {
                println!("no desired state");
            } else if response.status == 200 {
                println!("{}", response.body);
            } else {
                return Err(format!("desired request failed: HTTP {}", response.status));
            }
            Ok(())
        }
        "apply-once" => apply_once(options),
        "usage-once" => usage_once(options),
        "load-once" => load_once(options),
        // One manual probe. The cycle is half an hour, which is too long to wait
        // while diagnosing an MTU problem.
        "probe-once" => probe_once(options),
        // One manual end-to-end probe. The cycle is a minute, which is still a wait
        // after editing a chain's rule table.
        "e2e-once" => e2e_once(options),
        "ping-probe-once" => ping_probe_once(options),
        "run" => run_forever(options),
        value => Err(format!(
            "unknown command {value}; expected run, desired, apply-once, usage-once, probe-once, e2e-once, ping-probe-once, health, or repair"
        )),
    }
}

fn health_state_dir(args: &[String]) -> PathBuf {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--state-dir" {
            if let Some(value) = args.get(i + 1) {
                return PathBuf::from(value);
            }
        }
        i += 1;
    }
    env::var("BROCADE_AGENT_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./brocade-agent-state"))
}

/// The local reconcile taken by `repair` and by every idle round.
///
/// The control plane sends artifacts only when they differ from what was last
/// reported, returning 204 otherwise, and an observation has to be attached to a
/// deployment. State that drifts on its own is therefore neither visible to the
/// release flow nor repairable by it: a deleted interface, a terminated xray, an MTU
/// that was not applied, or an allow-list emptied by a restart. This path does not
/// contact the control plane; it replays the artifacts already on disk in state_dir.
///
/// Replay only, never rewrite: nothing here produces new desired state, so it cannot
/// contend with the control plane's source of truth.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drift {
    /// For idle rounds: act only when something is confirmed broken, and run no
    /// commands otherwise.
    OnlyWhenBroken,
    /// For `repair`: replay everything without checking state first.
    Always,
}

#[derive(Debug, PartialEq, Eq)]
enum WorkloadRuntime {
    Healthy,
    Broken(String),
    Unknown(String),
}

/// Combine the process, port and auxiliary checks under one ordering rule: a confirmed
/// failure takes precedence over an unavailable probe. Otherwise a machine with one
/// missing inbound and one unprobeable inbound would be classified as unknown, and
/// self-healing would skip the failure that was established.
fn classify_workload_runtime(
    process_running: bool,
    auxiliary_failure: Option<&str>,
    probes: impl IntoIterator<Item = (String, String, Option<bool>)>,
) -> WorkloadRuntime {
    if !process_running {
        return WorkloadRuntime::Broken("进程没在运行".to_owned());
    }

    let mut broken = Vec::new();
    let mut unknown = Vec::new();
    for (failure, unavailable, status) in probes {
        match status {
            Some(true) => {}
            Some(false) => broken.push(failure),
            None => unknown.push(unavailable),
        }
    }
    if let Some(detail) = auxiliary_failure {
        broken.push(detail.to_owned());
    }
    if !broken.is_empty() {
        WorkloadRuntime::Broken(broken.join("；"))
    } else if !unknown.is_empty() {
        WorkloadRuntime::Unknown(unknown.join("；"))
    } else {
        WorkloadRuntime::Healthy
    }
}

fn phantun_runtime(path: &Path) -> WorkloadRuntime {
    let instances = match phantun_instances(path) {
        Ok(instances) => instances,
        Err(error) => return WorkloadRuntime::Unknown(format!("读不了计划：{error}")),
    };
    let probes = instances
        .iter()
        .flat_map(phantun_runtime_probes)
        .collect::<Vec<_>>();
    classify_workload_runtime(
        true,
        (!phantun_nat_intact()).then_some("NAT/转发规则不完整"),
        probes,
    )
}

fn xray_runtime(content: &str) -> WorkloadRuntime {
    let running = xray_running();
    if !running {
        return classify_workload_runtime(false, None, []);
    }
    let probes = xray_listen_ports(content)
        .into_iter()
        .map(|(tag, listen, port, protocol)| {
            (
                format!("{tag} {listen}:{port} 没在监听"),
                format!("{tag} {listen}:{port} 无法检查"),
                xray_port_listening(port, protocol),
            )
        })
        .collect::<Vec<_>>();
    classify_workload_runtime(true, None, probes)
}

fn reconcile_local(state_dir: &Path, mode: Drift) -> Result<(), String> {
    let mut acted = Vec::new();
    let result = reconcile_local_inner(state_dir, mode, &mut acted);
    let recorded = create_private_dir(state_dir).and_then(|()| {
        record_local_reconcile(state_dir, &acted, result.as_ref().err().map(String::as_str))
    });
    match (result, recorded) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(record)) => Err(format!("本地对账完成，但记录结果失败：{record}")),
        (Err(error), Err(record)) => Err(format!("{error}；记录本地对账失败：{record}")),
    }
}

/// Keep the action body separate so the outer function has one exit path that
/// records both success and failure.  A `?` below may stop the replay, but may
/// never again skip the only runtime record the control plane can see.
fn reconcile_local_inner(
    state_dir: &Path,
    mode: Drift,
    acted: &mut Vec<String>,
) -> Result<(), String> {
    let force = mode == Drift::Always;

    // The two backbone components, phantun and wg0, contend with the watchdog thread
    // over the same interface and have to be serialized. xray and the allow-list stay
    // outside this lock: they do not touch the watchdog's resources, and holding the
    // lock longer would only block the watchdog.
    let backbone = backbone_lock();

    // Before wireguard: wg's Endpoint may point at the phantun client's local port,
    // so starting phantun first gives wg a reachable target.
    let phantun_conf = state_dir.join("phantun.json");
    if phantun_conf.exists()
        && !state_dir.join("phantun.disabled").exists()
        && (force
            || (phantun_wanted(&phantun_conf)
                && matches!(phantun_runtime(&phantun_conf), WorkloadRuntime::Broken(_))))
    {
        let content = fs::read_to_string(&phantun_conf).map_err(|error| error.to_string())?;
        // Local reconcile does not contact the control plane and therefore has no
        // distribution source. The binary was installed by the last release, so its
        // absence is an error; deriving a download URL is outside this path's scope.
        apply_phantun(state_dir, &content, None)?;
        acted.push("phantun".to_owned());
    }

    let wg_conf = state_dir.join("wireguard.conf");
    if wg_conf.exists() && !state_dir.join("wireguard.disabled").exists() {
        let want_mtu = wireguard_conf_mtu(&wg_conf);
        let broken = !command_success("ip", &["link", "show", "wg0"])
            || want_mtu.is_some_and(|want| live_wg_mtu() != Some(want));
        if force || broken {
            apply_wireguard(&wg_conf)?;
            // A newly created interface waits for outgoing data before
            // handshaking, so without a poke the first user eats the handshake
            // latency and the health check misreports idle as down.
            let _ = ping_all(&peer_overlay_ips());
            acted.push("wireguard".to_owned());
        }
        // These two tests only establish that the config was applied, and both can
        // pass while the link carries no traffic. The test for the link itself runs
        // on the watchdog thread (`guard_wireguard`) rather than here, because it
        // needs second-level granularity and this path runs at `APPLY_INTERVAL`.
    }

    drop(backbone);

    let hop_conf = state_dir.join("hy2_port_hop.json");
    if hop_conf.exists() && !state_dir.join("hy2_port_hop.disabled").exists() {
        let content = fs::read_to_string(&hop_conf).map_err(|error| error.to_string())?;
        if force || !hy2_port_hop::matches_content(&content) {
            hy2_port_hop::apply_content(&content)?;
            acted.push("port-hop".to_owned());
        }
    }

    let xray_conf = state_dir.join("xray.json");
    if xray_conf.exists() && !state_dir.join("xray.disabled").exists() {
        let content = fs::read_to_string(&xray_conf).map_err(|error| error.to_string())?;
        let api_port = xray_api_port(&content).unwrap_or(10085);
        let restarted = if force || matches!(xray_runtime(&content), WorkloadRuntime::Broken(_)) {
            // No sampling here: reaching this point means xray is already gone (or
            // repair forced a replay), the counters vanished with it, and sampling
            // would only read a freshly started, empty process.
            apply_xray(&xray_conf, api_port)?;
            acted.push("xray".to_owned());
            true
        } else {
            false
        };

        // Ahead of the grants sync deliberately: that step talks gRPC to a process that may have
        // just restarted, and a `?` out of it would leave the exemption unchecked on every idle
        // round until a real deployment ran. This one is a table comparison and cannot fail that
        // way, so it costs the grants sync nothing to go second.
        //
        // Compared against what the machine reports rather than against what was last written —
        // the failure this catches is somebody else's `nft flush ruleset` or a firewalld reload
        // taking our table with it, which no artifact and no deployment would ever notice.
        if force || !conntrack::matches_content(&content) {
            conntrack::apply_content(&content)?;
            acted.push("conntrack".to_owned());
        }

        // The allow-list is added to the running xray over gRPC and exists only in
        // process memory, because the inbounds in xray.json carry no clients and xray
        // has no config hot reload. One xray restart therefore drops every user, and
        // the symptom is a subscription that connects and then reports an invalid
        // request user id. This goes through sync_grants rather than calling adu
        // directly, because the difference has to be taken against the list currently
        // read back from the machine, not against the last version the control plane
        // recorded.
        if let Some(want) = desired_grants_on_disk(state_dir)? {
            if force || restarted || grants_drifted(&want, api_port) {
                sync_grants(state_dir, &content, api_port, &want)?;
                acted.push("grants".to_owned());
            }
        }
    }

    if acted.is_empty() {
        if force {
            let message = format!(
                "{} 里没有任何制品，这台机器还没接收过发布",
                state_dir.display()
            );
            return Err(message);
        }
    } else {
        println!("本地对账：重放了 {}", acted.join("、"));
    }
    Ok(())
}

fn live_wg_mtu() -> Option<u16> {
    fs::read_to_string("/sys/class/net/wg0/mtu")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The last full set of desired grants this agent accepted. An older agent never wrote
/// this file, so the result is None, and taking no action is preferable to deriving the
/// full set from an incremental batch.
fn desired_grants_on_disk(state_dir: &Path) -> Result<Option<Vec<GrantInbound>>, String> {
    let path = state_dir.join("grants.desired.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| format!("{} 解不开: {error}", path.display()))
}

/// Whether the runtime list agrees with the local full set of desired grants. Both
/// directions must be checked. An unreadable list counts as nothing missing —
/// pushing once more is idempotent, but pushing every round because of a failed read
/// is not.
///
/// Checking only for missing entries is insufficient, because the revoking side drifts
/// as well. An `rmu` that did not complete, whether from a network failure, an xray
/// restart at that moment, or a control plane that stopped before reporting, leaves an
/// account in the runtime that should have been removed. Checking only for absences
/// concludes that nothing is missing, and that account stays connected past its quota
/// while the control plane records the revocation as complete.
///
/// The surplus side is compared by email, which is the grant label and unique per
/// account. The same email with a new uuid counts as a missing version, which the other
/// side detects.
fn grants_drifted(want: &[GrantInbound], api_port: u16) -> bool {
    want.iter().any(|inbound| {
        let Ok(live) = read_users(api_port, &inbound.tag) else {
            return false;
        };
        grants_drifted_in(&inbound.clients, &live)
    })
}

/// Pure comparison, so that the layer above only has to fetch.
fn grants_drifted_in(want: &[GrantClient], live: &[ObservedClient]) -> bool {
    let missing = want.iter().any(|client| {
        !live.iter().any(|seen| {
            seen.email == client.email && seen.uuid == client.uuid && seen.flow == client.flow
        })
    });
    let extra = live
        .iter()
        .any(|seen| !want.iter().any(|client| client.email == seen.email));
    missing || extra
}

/// Written when the control plane answers 204, and removed as soon as a deployment is
/// claimed. It exists for `health` alone, to distinguish two situations that leave an
/// identically empty state directory:
///   - the machine has just enrolled and no released plan includes it yet, so it has
///     nothing to converge to
///   - the agent received no answer at all, from a wrong `--server`, a revoked token,
///     or a blocked network
///
/// The first is not a fault; the second is what the self-check exists to detect.
const NO_DESIRED_FILE: &str = "no-desired";

fn mark_no_desired(state_dir: &Path) {
    // On a machine that has never converged, nothing has created the state directory,
    // because the artifacts that normally create it never arrived. Without this call
    // the marker fails to be written in exactly the case it exists for.
    let _ = create_private_dir(state_dir);
    let at = current_unix_secs().unwrap_or(0);
    let _ = fs::write(state_dir.join(NO_DESIRED_FILE), at.to_string());
}

fn clear_no_desired(state_dir: &Path) {
    let _ = fs::remove_file(state_dir.join(NO_DESIRED_FILE));
}

/// Whether this machine has ever taken a release. Determined from the artifacts alone
/// rather than from applied-state.json, because that file is written only after a
/// successful convergence, so a machine whose first release failed would otherwise be
/// read as one that was never assigned work.
fn never_converged(state_dir: &Path) -> bool {
    [
        "wireguard.conf",
        "wireguard.disabled",
        "xray.json",
        "xray.disabled",
        "phantun.json",
        "phantun.disabled",
        "applied-state.json",
    ]
    .iter()
    .all(|name| !state_dir.join(name).exists())
}

/// Reads actual system state rather than the applied-state.json the agent wrote itself.
/// In state-dir mode that file only records that a file was written, which is what
/// leaves the console showing a component as present while nothing runs on the machine.
/// The test comes from the artifacts in the state directory: a config present with no
/// .disabled marker means the corresponding component should be running.
fn health(state_dir: &Path) -> Result<(), String> {
    let mut failures = 0_usize;
    let report = |name: &str, status: &str, detail: &str| {
        println!("  {name:<10} {status:<9} {detail}");
    };

    // A newly enrolled machine has an empty state directory, and that is not a fault:
    // the control plane sends nothing until a released plan includes the machine.
    // Counted as a failure, it fails the install script's self-check every time, so
    // *every* first install ended in 90 seconds of waiting, a failed report and 50
    // lines of journal output on a machine that was functioning and waiting for work.
    // The self-check exists to detect a broken installation, and a node with no
    // assigned plan is not one.
    //
    // The distinction is carried by NO_DESIRED_FILE, which is written only after the
    // control plane answered 204. An empty state directory without it means the agent
    // received no answer, which remains a failure.
    if never_converged(state_dir) {
        if state_dir.join(NO_DESIRED_FILE).exists() {
            report(
                "deploy",
                "pending",
                "控制面还没给这台机器派活——去控制台把它加进计划，发布一版",
            );
            println!("健康：agent 已就位并在轮询，等控制面派活（还没收敛过，没有别的可查）");
            return Ok(());
        }
        // Still a failure, but say which one. Two lines of "there is no
        // wireguard.conf in the state directory" describe the symptom and name no
        // cause; what has actually happened here is that the agent has not
        // completed a single successful round, and the two usual reasons are worth
        // naming outright.
        report(
            "deploy",
            "FAIL",
            "agent 还没从控制面拿到过任何回答（一轮都没跑完）",
        );
        return Err(format!(
            "{} 是空的，agent 连 204 都没收到过。\n\
             多半是这两种：--server 指到了 admin 端口（默认 8080，那上面没有 /agent/v1/*），\
             或者 token 不对/被吊销了。\n\
             journalctl -u brocade-agent -n 50 里有具体那条错。",
            state_dir.display()
        ));
    }

    let wg_conf = state_dir.join("wireguard.conf");
    if wg_conf.exists() && !state_dir.join("wireguard.disabled").exists() {
        if command_success("ip", &["link", "show", "wg0"]) {
            report("wireguard", "ok", "wg0 已就位");
            // An existing interface does not imply a working tunnel. With the
            // backbone down, xray's forwarding outbound cannot reach the next hop,
            // which presents as a correct configuration that does not connect, so
            // each peer's handshake time is checked here.
            // MTU is a wg-quick-only key that wg syncconf ignores. With 1200 in the
            // config and the interface still at the default 1420, small packets pass
            // and large ones are dropped, and the symptom is a connection on which
            // no page loads.
            if let Some(want) = wireguard_conf_mtu(&wg_conf) {
                match fs::read_to_string("/sys/class/net/wg0/mtu") {
                    Ok(actual) => {
                        let actual = actual.trim().parse::<u16>().unwrap_or(0);
                        if actual == want {
                            report("mtu", "ok", &format!("wg0 MTU {actual}"));
                        } else {
                            failures += 1;
                            report(
                                "mtu",
                                "FAIL",
                                &format!("配置要求 {want}，接口实际是 {actual}——大包会被丢"),
                            );
                        }
                    }
                    Err(error) => report("mtu", "?", &format!("读不到 wg0 的 MTU: {error}")),
                }
            }

            // The same `check_wg_peers` the watchdog uses. With a separate test on
            // each side, health could report a working link while the watchdog
            // repaired it every minute.
            match check_wg_peers(&wg_conf) {
                Ok(checks) => {
                    for check in &checks {
                        if let Some((want, live)) = &check.endpoint_drift {
                            // A non-fatal drift on a working link is reported as a
                            // note only: a dual-stack peer dialing in from the other
                            // family is ordinary roaming, and reporting it as FAIL
                            // would make the report unreliable.
                            let fatal = drift_is_fatal(check);
                            if fatal {
                                failures += 1;
                            }
                            report(
                                "peer",
                                if fatal { "FAIL" } else { "warn" },
                                &format!(
                                    "{} 的 Endpoint 是 {live}，配置里写的是 {want}——\
                                     漫游顶掉的，wg 不会自己改回来",
                                    check.name
                                ),
                            );
                        }
                        match &check.state {
                            PeerState::Fresh => {
                                report("peer", "ok", &format!("{} 握手正常", check.name))
                            }
                            PeerState::Alive => report(
                                "peer",
                                "ok",
                                &format!(
                                    "{} 通（{}，隧道闲着而已）",
                                    check.name,
                                    handshake_age_text(check.handshake_age)
                                ),
                            ),
                            // An unprobeable peer reports as `?` and does not count
                            // as a failure, because it describes this machine
                            // rather than the link. Counting it would block the
                            // install self-check on a link that may be working.
                            PeerState::Unprobed { reason } => report(
                                "peer",
                                "?",
                                &format!(
                                    "{} {}，而这台机器上探不了：{reason}",
                                    check.name,
                                    handshake_age_text(check.handshake_age)
                                ),
                            ),
                            // `check_wg_peers` always settles before returning
                            PeerState::Suspect => {}
                            PeerState::Down { detail } => {
                                failures += 1;
                                report("peer", "FAIL", &format!("{} {detail}", check.name));
                            }
                        }
                    }
                }
                Err(error) => report("peer", "?", &format!("查不到握手状态: {error}")),
            }
        } else {
            failures += 1;
            report("wireguard", "FAIL", "有 wireguard.conf，但 wg0 不存在");
        }
    } else if state_dir.join("wireguard.disabled").exists() {
        report("wireguard", "disabled", "这台不在 overlay 里");
    } else {
        failures += 1;
        report(
            "wireguard",
            "MISSING",
            "状态目录里没有 wireguard.conf，还没收敛过",
        );
    }

    let phantun_conf = state_dir.join("phantun.json");
    if phantun_conf.exists() && !state_dir.join("phantun.disabled").exists() {
        if !phantun_wanted(&phantun_conf) {
            report("phantun", "ok", "这一版不需要实例");
        } else {
            match phantun_runtime(&phantun_conf) {
                WorkloadRuntime::Healthy => {
                    report("phantun", "ok", "计划实例、TUN 和 NAT/转发规则正常")
                }
                WorkloadRuntime::Broken(detail) => {
                    failures += 1;
                    report("phantun", "FAIL", &detail);
                }
                WorkloadRuntime::Unknown(detail) => {
                    report("phantun", "?", &detail);
                }
            }
        }
    } else if state_dir.join("phantun.disabled").exists() && phantun_running() {
        failures += 1;
        report("phantun", "FAIL", "已停用，但进程还在跑");
    }

    let xray_conf = state_dir.join("xray.json");
    if xray_conf.exists() && !state_dir.join("xray.disabled").exists() {
        let content = fs::read_to_string(&xray_conf)
            .map_err(|error| format!("读不了 {}: {error}", xray_conf.display()))?;
        match xray_runtime(&content) {
            WorkloadRuntime::Healthy => report("xray", "ok", "进程和所有计划端口正常"),
            WorkloadRuntime::Broken(detail) => {
                failures += 1;
                report("xray", "FAIL", &detail);
            }
            WorkloadRuntime::Unknown(detail) => report("xray", "?", &detail),
        }

        // A listening port does not mean anyone can connect: the allow-list is added
        // to process memory over gRPC and empties on any xray restart, presenting on
        // the subscription side as invalid request user id.
        if let Some(want) = desired_grants_on_disk(state_dir)? {
            let api_port = xray_api_port(&content).unwrap_or(10085);
            if grants_drifted(&want, api_port) {
                failures += 1;
                report(
                    "grants",
                    "FAIL",
                    "放行名单跟期望对不上：少了人订阅会被拒，多了人是该断没断——跑 `brocade-agent repair`",
                );
            } else {
                report("grants", "ok", "放行名单齐");
            }
        }
    } else if state_dir.join("xray.disabled").exists() {
        report("xray", "disabled", "这台没有 xray 工作负载");
    } else {
        failures += 1;
        report("xray", "MISSING", "状态目录里没有 xray.json，还没收敛过");
    }

    if failures == 0 {
        println!("健康：全部检查通过");
        Ok(())
    } else {
        Err(format!("健康检查有 {failures} 项没通过"))
    }
}

// The convergence and sampling cycles are independent: the convergence cycle sets how
// quickly drift is detected, and the sampling cycle bounds how much traffic an
// unexpected restart can lose. They therefore run on two threads rather than one loop
// alternating between two tasks.
const APPLY_INTERVAL: Duration = Duration::from_secs(15);
const USAGE_INTERVAL: Duration = Duration::from_secs(30);
/// A round sends about a dozen pings per peer, while path MTU is a property of the
/// upstream link and typically holds for hours. A shorter interval only adds traffic on
/// both ends.
const PROBE_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Runtime state is also a service re-entry gate for isolated nodes. Keep it fresh enough that an
/// administrator does not have to wait for the path-MTU probe cadence after debt has converged.
const RUNTIME_INTERVAL: Duration = Duration::from_secs(30);
const PROBE_TARGETS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Host sampling. Shorter than every other cycle by necessity: CPU is differenced over this
/// interval, and a 30-second difference averages a spike away, while spikes are the dominant
/// component of forwarding load. The report is still sent every 30 seconds, carrying the peak
/// of three sub-samples, so the shorter tick costs /proc reads rather than requests.
const LOAD_INTERVAL: Duration = Duration::from_secs(load::SUB_INTERVAL_SECS);
/// How long end-to-end probing waits before retrying when the control plane is
/// unreachable. Longer than the normal cycle, because a machine that cannot reach the
/// control plane usually has nothing measurable either, and a longer interval reduces
/// load in that state.
const E2E_TARGETS_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
/// Settings change much less often than samples. Refresh independently so even a day-long sampling
/// interval learns a policy change promptly, without putting a GET in front of every observation.
const PING_PROBE_SETTINGS_INTERVAL: Duration = Duration::from_secs(15);
const SPOOL_IDLE_POLL: Duration = Duration::from_secs(1);
const SPOOL_MAX_RETRY: Duration = Duration::from_secs(60);
const LATEST_REPORT_RETRY_MIN: Duration = Duration::from_secs(1);
const LATEST_REPORT_RETRY_MAX: Duration = Duration::from_secs(15);

fn spawn_spool_reporter(
    name: &'static str,
    spool: Spool,
    options: Options,
    runtime_reports: Arc<RuntimeReports>,
) -> Result<(), String> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut retry = SPOOL_IDLE_POLL;
            loop {
                match spool_drain(spool, &options) {
                    Ok(changed) => {
                        if changed {
                            if let Err(error) =
                                runtime_reports.publish_if_spool_changed(&options.state_dir)
                            {
                                eprintln!("{name}: backlog 即时快照没采集成：{error}");
                            }
                        }
                        retry = SPOOL_IDLE_POLL;
                    }
                    Err(error) => {
                        eprintln!("{name}: {error}");
                        retry = retry.saturating_mul(2).min(SPOOL_MAX_RETRY);
                    }
                }
                thread::sleep(retry);
            }
        })
        .map(|_| ())
        .map_err(|error| format!("cannot spawn {name} thread: {error}"))
}

/// One process, two loops.
///
/// A version split into two systemd units existed and was wrong:
/// - step 4 requires sampling the counters before restarting xray, and that step sits
///   in the middle of the convergence sequence where another process cannot insert
///   itself
/// - instant snapshots and periodic sampling must be serialized inside the agent (one
///   in-process lock), and across processes there is no such lock
///
/// Reading xray's counters is serialized by `meter`. The convergence path holds it
/// across the xray restart, so that the sampling thread does not read a freshly
/// started, empty process and mistake the reset for users sending no traffic.
fn run_forever(options: Options) -> Result<(), String> {
    certfile::ensure_layout(&options.state_dir)?;
    match logcap::ensure_agent_journal_namespace(&options.state_dir) {
        Ok(true) => {
            println!("agent 日志已切到独立 journal，重启一次使配置生效");
            return Ok(());
        }
        Ok(false) => {}
        // Log policy is operational hygiene, not permission to stop convergence or accounting.
        // Keep serving and leave a searchable warning for the operator.
        Err(error) => warn(format!("agent 日志上限未能落地：{error}")),
    }
    let meter = Arc::new(Mutex::new(()));
    // Set by the self-update thread once a new binary is in place, and read by the loop at the
    // bottom of this function. The replacement itself is safe at any moment, because the running
    // process keeps its own inode, but exiting is not: an exit during `apply_once` would leave a
    // convergence incomplete. The thread that swaps the file therefore does not terminate the
    // process; it sets this flag, and the main loop exits between rounds.
    let wants_exit = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let runtime_reports = Arc::new(RuntimeReports::default());
    let _ = RUNTIME_REPORTS.set(Arc::clone(&runtime_reports));
    {
        let report_options = options.clone();
        let reports_in = Arc::clone(&runtime_reports);
        thread::Builder::new()
            .name("runtime-report".to_owned())
            .spawn(move || {
                report_latest_forever("runtime", &reports_in.reports, |report| {
                    send_runtime_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn runtime-report thread: {error}"))?;
    }

    // Convergence results are durable and ordered, but their network delivery is not part of the
    // convergence transaction. The control plane already withholds new work while an observation
    // is outstanding, so a dedicated drainer preserves ordering without delaying heartbeats.
    spawn_spool_reporter(
        "observation-report",
        OBSERVATION_SPOOL,
        options.clone(),
        Arc::clone(&runtime_reports),
    )?;

    {
        // One outbound connection, idle until the control plane says somebody is watching. It is
        // independent of load and usage on purpose: neither their 30-second cadence nor their
        // persistence/accounting semantics changes when this stream is enabled.
        let options = options.clone();
        thread::Builder::new()
            .name("realtime".to_owned())
            .spawn(move || realtime::run(&options))
            .map_err(|error| format!("cannot spawn realtime telemetry thread: {error}"))?;
    }

    {
        spawn_spool_reporter(
            "usage-report",
            USAGE_SPOOL,
            options.clone(),
            Arc::clone(&runtime_reports),
        )?;
        let health_reports = Arc::new(LatestReport::<LinkHealthRequest>::default());

        let report_options = options.clone();
        let reports_in = Arc::clone(&health_reports);
        thread::Builder::new()
            .name("link-health-report".to_owned())
            .spawn(move || {
                report_latest_forever("link-health", &reports_in, |report| {
                    send_health_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn link-health-report thread: {error}"))?;

        let options = options.clone();
        let meter = Arc::clone(&meter);
        let reports_out = Arc::clone(&health_reports);
        thread::Builder::new()
            .name("usage".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    each_round("usage", || {
                        let _guard = meter
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if let Err(error) = collect_usage_report(&options) {
                            eprintln!("usage: {error}");
                        }
                        match collect_health_report(&options) {
                            Ok(Some(report)) => reports_out.publish(report),
                            Ok(None) => {}
                            Err(error) => eprintln!("link-health: {error}"),
                        }
                    });
                    let now = Instant::now();
                    tick = next_periodic_tick(tick, USAGE_INTERVAL, now);
                    thread::sleep(tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn usage thread: {error}"))?;
    }

    {
        let targets = Arc::new(SettingsCache::<ProbeTargetList>::default());
        let reports = Arc::new(LatestReport::<LinkProbeRequest>::default());

        let settings_options = options.clone();
        let settings_out = Arc::clone(&targets);
        thread::Builder::new()
            .name("probe-targets".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    each_round("probe-targets", || {
                        match fetch_probe_targets(&settings_options) {
                            Ok(next) => settings_out.publish(next),
                            Err(error) => eprintln!("probe targets: {error}"),
                        }
                    });
                    let now = Instant::now();
                    tick = next_periodic_tick(tick, PROBE_TARGETS_REFRESH_INTERVAL, now);
                    thread::sleep(tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn probe-targets thread: {error}"))?;

        let report_options = options.clone();
        let reports_in = Arc::clone(&reports);
        thread::Builder::new()
            .name("probe-report".to_owned())
            .spawn(move || {
                report_latest_forever("probe", &reports_in, |report| {
                    send_link_probe_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn probe-report thread: {error}"))?;

        let targets_in = Arc::clone(&targets);
        let reports_out = Arc::clone(&reports);
        thread::Builder::new()
            .name("probe".to_owned())
            .spawn(move || {
                let mut targets = targets_in.wait_for_initial();
                let mut tick = Instant::now();
                loop {
                    each_round("probe", || match collect_link_probe_report(&targets) {
                        Ok(Some(report)) => reports_out.publish(report),
                        Ok(None) => {}
                        Err(error) => eprintln!("probe: {error}"),
                    });
                    let next_tick = next_periodic_tick(tick, PROBE_INTERVAL, Instant::now());
                    while let Some(next) = targets_in.wait_for_change_until(&targets, next_tick) {
                        targets = next;
                    }
                    tick = next_tick;
                }
            })
            .map_err(|error| format!("cannot spawn probe thread: {error}"))?;
    }

    {
        let state_dir = options.state_dir.clone();
        let reports_out = Arc::clone(&runtime_reports);
        thread::Builder::new()
            .name("runtime".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    each_round("runtime", || {
                        if let Err(error) = reports_out.publish_current(&state_dir) {
                            eprintln!("runtime: {error}");
                        }
                    });
                    let now = Instant::now();
                    tick = next_periodic_tick(tick, RUNTIME_INTERVAL, now);
                    thread::sleep(tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn runtime thread: {error}"))?;
    }

    {
        // The link watchdog gets its own thread too. On the apply loop its floor
        // would be `APPLY_INTERVAL` (15 seconds), while the two failures it hunts (an
        // Endpoint overwritten by roaming, a peer dropped from the runtime) are
        // visible at a glance in `wg show wg0 dump` and wg never repairs either, so
        // noticing sooner fixes sooner.
        // It contends with convergence over wg0, so both share `BACKBONE` (taken
        // inside `guard_wireguard`).
        let state_dir = options.state_dir.clone();
        let reports_out = Arc::clone(&runtime_reports);
        thread::Builder::new()
            .name("wg-guard".to_owned())
            .spawn(move || loop {
                each_round("wg-guard", || {
                    let conf = state_dir.join("wireguard.conf");
                    // No config, or an explicit disable marker, means no check runs:
                    // that state is not a failure, it means the machine is not in
                    // the backbone.
                    let changed = if conf.exists() && !state_dir.join("wireguard.disabled").exists()
                    {
                        guard_wireguard(&state_dir, &conf)
                    } else {
                        set_wireguard_disabled()
                    };
                    if changed {
                        if let Err(error) = reports_out.publish_current(&state_dir) {
                            eprintln!("wg-guard runtime: {error}");
                        }
                    }
                });
                thread::sleep(WG_GUARD_INTERVAL);
            })
            .map_err(|error| format!("cannot spawn wg-guard thread: {error}"))?;
    }

    {
        // Its own thread rather than a passenger on usage, which is where semantically-close work
        // normally goes. Two reasons it does not fit there: it ticks at 10 seconds against usage's
        // 30 (see LOAD_INTERVAL), and it never touches xray's counters, so taking `meter` would
        // make host sampling wait behind a convergence that has nothing to do with it.
        //
        // It also must not delay usage. A netlink dump on a machine with thousands of
        // connections has a measurable cost, and usage reporting drives billing, so anything
        // that can slow it runs on a separate thread.
        // Sampling must never wait for the control plane. A load request may legally spend up to
        // 30 seconds in an HTTP read, which used to stretch the next nominal 10-second sample and
        // then label the mixed interval as 30 seconds. A one-slot best-effort handoff preserves
        // the existing "do not spool stale telemetry" rule while keeping the sampling clock free.
        let reports = Arc::new(LatestReport::<LoadReportRequest>::default());
        let report_options = options.clone();
        let reports_in = Arc::clone(&reports);
        thread::Builder::new()
            .name("load-report".to_owned())
            .spawn(move || {
                report_latest_forever("load", &reports_in, |report| {
                    send_load_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn load-report thread: {error}"))?;

        let options = options.clone();
        let reports_out = Arc::clone(&reports);
        thread::Builder::new()
            .name("load".to_owned())
            .spawn(move || {
                let mut next_tick = Instant::now();
                loop {
                    each_round("load", || match build_load_report(&options) {
                        Ok(Some(report)) => reports_out.publish(report),
                        Ok(None) => {}
                        Err(error) => eprintln!("load: {error}"),
                    });

                    let now = Instant::now();
                    next_tick = next_periodic_tick(next_tick, LOAD_INTERVAL, now);
                    thread::sleep(next_tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn load thread: {error}"))?;
    }

    {
        // Settings, sampling, and reporting have separate clocks. A slow control plane must not
        // move a five-second observation tick, and recovering from an outage must not replay a
        // queue of stale diagnostics. The cache wakes the sampler on policy changes; the report
        // slot keeps only the newest observation that is not already in flight.
        let settings = Arc::new(PingProbeSettingsCache::default());
        let reports = Arc::new(LatestPingProbeReport::default());

        let settings_options = options.clone();
        let settings_out = Arc::clone(&settings);
        thread::Builder::new()
            .name("ping-settings".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    each_round("ping-settings", || {
                        match fetch_ping_probe_settings(&settings_options) {
                            Ok(next) => settings_out.publish(next),
                            Err(error) => eprintln!("ping-probe settings: {error}"),
                        }
                    });
                    let now = Instant::now();
                    tick = next_periodic_tick(tick, PING_PROBE_SETTINGS_INTERVAL, now);
                    thread::sleep(tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn PING settings thread: {error}"))?;

        let report_options = options.clone();
        let report_in = Arc::clone(&reports);
        thread::Builder::new()
            .name("ping-report".to_owned())
            .spawn(move || {
                report_latest_forever("ping-probe", &report_in, |report| {
                    send_ping_probe_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn PING report thread: {error}"))?;

        let settings_in = Arc::clone(&settings);
        let report_out = Arc::clone(&reports);
        thread::Builder::new()
            .name("ping-probe".to_owned())
            .spawn(move || {
                let mut settings = settings_in.wait_for_initial();
                let mut tick = Instant::now();
                loop {
                    // A refresh can land on the deadline after the timed wait returned. Read once
                    // more before sampling so that race costs no extra observation interval.
                    settings = settings_in.latest().unwrap_or(settings);
                    each_round("ping-probe", || {
                        match collect_ping_probe_report(&settings) {
                            Ok(Some(report)) => report_out.publish(report),
                            Ok(None) => {}
                            Err(error) => eprintln!("ping-probe: {error}"),
                        }
                    });

                    let interval = Duration::from_secs(u64::from(settings.interval_secs));
                    let mut next_tick = next_periodic_tick(tick, interval, Instant::now());
                    while let Some(next) = settings_in.wait_for_change_until(&settings, next_tick) {
                        settings = next;
                        let interval = Duration::from_secs(u64::from(settings.interval_secs));
                        next_tick = next_periodic_tick(tick, interval, Instant::now());
                    }
                    tick = next_tick;
                }
            })
            .map_err(|error| format!("cannot spawn PING probe thread: {error}"))?;
    }

    {
        let settings = Arc::new(SettingsCache::<E2eProbeTargetList>::default());
        let reports = Arc::new(LatestReport::<E2eProbeRequest>::default());

        let settings_options = options.clone();
        let settings_out = Arc::clone(&settings);
        thread::Builder::new()
            .name("e2e-targets".to_owned())
            .spawn(move || {
                let mut tick = Instant::now();
                loop {
                    each_round("e2e-targets", || {
                        match fetch_e2e_probe_targets(&settings_options) {
                            Ok(next) => settings_out.publish(next),
                            Err(error) => eprintln!("e2e targets: {error}"),
                        }
                    });
                    let now = Instant::now();
                    tick = next_periodic_tick(tick, E2E_TARGETS_REFRESH_INTERVAL, now);
                    thread::sleep(tick.saturating_duration_since(now));
                }
            })
            .map_err(|error| format!("cannot spawn e2e-targets thread: {error}"))?;

        let report_options = options.clone();
        let reports_in = Arc::clone(&reports);
        thread::Builder::new()
            .name("e2e-report".to_owned())
            .spawn(move || {
                report_latest_forever("e2e", &reports_in, |report| {
                    send_e2e_probe_report(&report_options, report)
                })
            })
            .map_err(|error| format!("cannot spawn e2e-report thread: {error}"))?;

        let settings_in = Arc::clone(&settings);
        let reports_out = Arc::clone(&reports);
        thread::Builder::new()
            .name("e2e".to_owned())
            .spawn(move || {
                let mut settings = settings_in.wait_for_initial();
                let mut tick = Instant::now();
                loop {
                    settings = settings_in.latest().unwrap_or(settings);
                    each_round("e2e", || match collect_e2e_probe_report(&settings) {
                        Ok(Some(report)) => reports_out.publish(report),
                        Ok(None) => {}
                        Err(error) => eprintln!("e2e: {error}"),
                    });
                    let mut next_tick =
                        next_periodic_tick(tick, settings.interval(), Instant::now());
                    while let Some(next) = settings_in.wait_for_change_until(&settings, next_tick) {
                        settings = next;
                        next_tick = next_periodic_tick(tick, settings.interval(), Instant::now());
                    }
                    tick = next_tick;
                }
            })
            .map_err(|error| format!("cannot spawn e2e thread: {error}"))?;
    }

    selfupdate::spawn_selfupdate(&options, &wants_exit);

    let mut apply_tick = Instant::now();
    loop {
        if let Err(error) = apply_once_locked(&options, &meter) {
            eprintln!("apply: {error}");
        }
        // Checked after the round rather than before the sleep, so that the wait for systemd to
        // restart does not also include a full apply interval.
        //
        // Exit 0, and systemd's `Restart=always` starts the replacement after `RestartSec` (5s).
        // Deliberately not `systemctl restart`: that command kills the process group it was
        // issued from, which is this process, and systemd's behavior for a unit whose restart
        // command exited mid-restart is not specified. Exiting is unambiguous.
        //
        // Four properties keep this from interrupting traffic: `KillMode=process` in the unit, so
        // xray, which this agent started with nohup in the same cgroup, is not killed alongside;
        // wg0 is a kernel interface and is unaffected; convergence is idempotent and re-runs every
        // 15 seconds; and anything owed to the control plane is already on disk in the spool.
        if wants_exit.load(std::sync::atomic::Ordering::SeqCst) {
            println!("selfupdate: 这一轮收敛做完了，退出让 systemd 用新二进制拉起来");
            std::process::exit(0);
        }
        let now = Instant::now();
        apply_tick = next_periodic_tick(apply_tick, APPLY_INTERVAL, now);
        thread::sleep(apply_tick.saturating_duration_since(now));
    }
}

fn apply_once_locked(options: &Options, meter: &Arc<Mutex<()>>) -> Result<(), String> {
    apply_once_inner(options.clone(), Some(meter), false)
}

fn apply_once(options: Options) -> Result<(), String> {
    apply_once_inner(options, None, true)
}

fn log_limit_header(response: &HttpResponse, name: &str) -> Result<Option<u32>, String> {
    response
        .header(name)
        .map(|raw| {
            raw.parse::<u32>()
                .map_err(|error| format!("{name}={raw:?}: {error}"))
        })
        .transpose()
}

/// New control planes send one ceiling per workload class. The legacy scalar remains a fallback
/// in both directions: a new Agent can still poll an old console, and a rolling console update can
/// serve an Agent binary that has not restarted into the new release yet.
fn log_policy_from_response(response: &HttpResponse) -> Result<Option<logcap::LogPolicy>, String> {
    let legacy = log_limit_header(response, AGENT_LOG_MAX_MIB_HEADER)?;
    let agent = log_limit_header(response, AGENT_JOURNAL_MAX_MIB_HEADER)?;
    let xray = log_limit_header(response, XRAY_LOG_MAX_MIB_HEADER)?;
    let phantun = log_limit_header(response, PHANTUN_LOG_MAX_MIB_HEADER)?;
    if legacy.is_none() && agent.is_none() && xray.is_none() && phantun.is_none() {
        return Ok(None);
    }
    let missing = |name: &str| format!("{name} 缺失且没有旧版统一上限可回退");
    Ok(Some(logcap::LogPolicy {
        agent_journal_mib: agent
            .or(legacy)
            .ok_or_else(|| missing(AGENT_JOURNAL_MAX_MIB_HEADER))?,
        xray_mib: xray
            .or(legacy)
            .ok_or_else(|| missing(XRAY_LOG_MAX_MIB_HEADER))?,
        phantun_mib: phantun
            .or(legacy)
            .ok_or_else(|| missing(PHANTUN_LOG_MAX_MIB_HEADER))?,
    }))
}

fn upgrade_dynamic_log_sinks(
    options: &Options,
    meter: Option<&Arc<Mutex<()>>>,
) -> Result<(), String> {
    let state_dir = &options.state_dir;
    let phantun_conf = state_dir.join("phantun.json");
    if phantun_conf.exists()
        && !state_dir.join("phantun.disabled").exists()
        && !state_dir.join(phantun::PHANTUN_BOUNDED_LOG_MARKER).exists()
    {
        let content = fs::read_to_string(&phantun_conf)
            .map_err(|error| format!("读取 phantun 日志迁移配置失败：{error}"))?;
        apply_phantun(state_dir, &content, None)?;
        println!("phantun 日志已切到动态上限");
    }

    let xray_conf = state_dir.join("xray.json");
    if xray_conf.exists()
        && !state_dir.join("xray.disabled").exists()
        && !state_dir.join(XRAY_BOUNDED_LOG_MARKER).exists()
    {
        let content = fs::read_to_string(&xray_conf)
            .map_err(|error| format!("读取 xray 日志迁移配置失败：{error}"))?;
        let api_port = xray_api_port(&content).unwrap_or(10085);
        let _guard = meter.map(|meter| {
            meter
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        });
        if let Err(error) = collect_usage_report(options) {
            // The migration remains necessary for the disk bound. Preserve the same release
            // policy as a normal restart: log a failed pre-sample, then continue rather than
            // leaving an unbounded/fixed child forever.
            eprintln!("usage: 切换动态日志前的采集没成功：{error}");
        }
        apply_xray(&xray_conf, api_port)?;
        println!("xray 日志已切到动态上限");
    }
    Ok(())
}

fn apply_once_inner(
    options: Options,
    meter: Option<&Arc<Mutex<()>>>,
    report_inline: bool,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    // Flush the convergence results still owed from earlier rounds before asking for
    // new work. It runs before fetching desired state, because while an observation is
    // outstanding the control plane answers 204 — the row is still at `dispatched`, its
    // lease has not expired, and the claim query cannot select it — so a flush placed
    // after that early 204 return would never run.
    // Failure does not stop this round: a failed flush usually means the control plane
    // is not reachable, which is the situation the local reconcile below covers.
    if report_inline {
        if let Err(error) = spool_drain(OBSERVATION_SPOOL, &options) {
            warn(format!("补发上一轮的收敛结果没成功：{error}"));
        }
    }
    let response = match desired_request(&client, &options) {
        Ok(response) => response,
        Err(error) => {
            // When desired state cannot be fetched, the machine has to check itself more
            // rather than less: an unreachable control plane and a dead backbone link
            // frequently share one cause. Local reconcile requires no control plane,
            // because the last desired state is already in state_dir. This previously
            // returned immediately, which made self-healing depend on the control plane
            // being reachable, and it is unreachable in exactly this case.
            if let Err(drift) = reconcile_local(&options.state_dir, Drift::OnlyWhenBroken) {
                warn(format!("拉不到期望状态，本地对账也没跑成：{drift}"));
            }
            return Err(error);
        }
    };
    match log_policy_from_response(&response) {
        Ok(Some(policy)) => match logcap::apply_policy(&options.state_dir, policy) {
            Ok(true) => println!(
                "日志上限已更新：Agent {} MiB，XRAY {} MiB，Phantun 每实例 {} MiB",
                policy.agent_journal_mib, policy.xray_mib, policy.phantun_mib
            ),
            Ok(false) => {}
            Err(error) => warn(format!("日志上限未能落地：{error}")),
        },
        Ok(None) => {}
        Err(error) => warn(format!("控制面返回的日志上限无效：{error}")),
    }
    if response.status == 204 {
        println!("no desired state");
        // Record it on disk: `health` runs in another process and cannot contact the
        // control plane, yet it has to distinguish a machine not included in any plan
        // from one that cannot reach the control plane.
        mark_no_desired(&options.state_dir);
        // Version 3 changes the shared policy file into one file per workload class. It needs one
        // workload restart to replace the old pipe, but that restart must sample Xray's volatile
        // counters first. General local reconcile cannot do that safely because it has no meter;
        // perform the migration here, where the accounting lock is available.
        upgrade_dynamic_log_sinks(&options, meter)?;
        // 204 means the control plane judged both dimensions converged: no deployment
        // owed, and the reported certificate sha matches the serving certificate.
        // The control plane compares artifacts only against what was last reported and
        // cannot observe the machine drifting on its own, because an observation has to
        // be attached to a deployment and without one there is no reporting channel.
        // Every idle round therefore reconciles locally, replaying the artifacts already
        // received.
        return reconcile_local(&options.state_dir, Drift::OnlyWhenBroken);
    }
    if response.status != 200 {
        return Err(format!("desired request failed: HTTP {}", response.status));
    }
    clear_no_desired(&options.state_dir);

    let response: DesiredStateResponse =
        serde_json::from_str(&response.body).map_err(|error| error.to_string())?;
    let (desired, certificates) = match response {
        DesiredStateResponse::Converged => {
            // The control plane sends this state as HTTP 204, which the branch above already
            // handled. A serialized one means the two sides disagree about the protocol.
            return Err("控制面回了 Converged 而不是 204".to_owned());
        }
        DesiredStateResponse::Deployment {
            deployment,
            certificates,
        } => (deployment, certificates),
        DesiredStateResponse::Certificates(materials) => {
            for material in &materials {
                crate::certfile::apply_material(&options, material)?;
            }
            // This variant means the control plane still sees the certificate dimension as stale.
            // Reload even when the files match: a previous reload may have failed after the write.
            if !materials.is_empty() {
                wait_for_xray_certificate_reload(&options)?;
            }
            crate::certfile::report_applied(&options);
            return Ok(());
        }
    };
    // Put the pair on disk before convergence. A failed write fails the round; a Present xray is
    // forced through its restart path below, while an Unmanaged xray is reloaded explicitly.
    let certificate_received = !certificates.is_empty();
    for material in &certificates {
        crate::certfile::apply_material(&options, material)?;
    }
    let before = observe_state(&options.state_dir, &desired, options.apply_mode)?;
    // Hold this lock across the whole restart: a sampling thread running between the
    // pkill and the new process would read freshly zeroed counters and record the reset
    // as an idle window.
    let mut held = None;
    let mut usage_activated_at_unix_secs = None;
    let applied = {
        let mut collect = || {
            if let Some(meter) = meter {
                held = Some(
                    meter
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
            // A failed sample must not block the release: updating xray's config is what
            // the operator requested, and a failed sample costs at most one window. It is
            // still reported rather than dropped.
            if let Err(error) = collect_usage_report(&options) {
                eprintln!("usage: 重启 xray 前的采集没成功：{error}");
            }
            // This is deliberately after the pre-change sample and before converge_linux mutates
            // Xray or its runtime grants. A newly introduced counter has value zero at this boundary,
            // even though Xray does not expose the counter until that user first sends traffic.
            usage_activated_at_unix_secs = current_unix_secs().ok();
        };
        match options.apply_mode {
            ApplyMode::StateDir => converge_to_state_dir(&options.state_dir, &desired),
            ApplyMode::Linux => converge_linux(&options.state_dir, &desired, &mut collect)
                .map(|_| observe_linux_state(&options.state_dir, &desired)),
        }
    };
    let (result, after, error) = match applied {
        Ok(after) => {
            let outcome = successful_apply_outcome(&options.state_dir, after);
            persist_converged_usage_generation(
                &options.state_dir,
                desired.usage_generation_id,
                outcome,
            )
        }
        Err(error) => {
            let after = observe_state(&options.state_dir, &desired, options.apply_mode)
                .unwrap_or_else(|_| unknown_state());
            (
                TargetApplyResult::FailedDirty,
                after,
                Some(error.to_string()),
            )
        }
    };

    let report = AgentObservationRequest {
        deployment_id: desired.deployment_id,
        claim_generation: desired.claim_generation,
        result,
        observed_before: before,
        observed_after: after,
        error: error.clone(),
        route: Some(route_ip_report()),
        usage_activated_at_unix_secs,
    };
    // Write to disk before sending. Convergence has already happened on this machine and
    // this result is its only record. If it is not delivered, the control plane stays at
    // `dispatched` and waits out a full 15-minute distribution lease before sending
    // again, while the work of this round is already complete. This previously returned
    // on a failed send, discarding the record of a completed convergence. The usage spool
    // has always worked this way, and both share the same persistence and retry
    // machinery (`Spool`).
    spool_push(OBSERVATION_SPOOL, &options.state_dir, &report)?;
    if report_inline {
        spool_drain(OBSERVATION_SPOOL, &options)?;
    }
    if let Some(error) = error {
        return Err(error);
    }
    if certificate_received {
        wait_for_xray_certificate_reload(&options)?;
        crate::certfile::report_applied(&options);
    }
    Ok(())
}

/// Validate the complete slot pair and give the already-running Xray watcher one bounded interval
/// to ingest an atomic replacement. Ordinary rotation must not restart Xray: doing so discards the
/// very sessions the fixed dual slots are designed to preserve.
fn wait_for_xray_certificate_reload(options: &Options) -> Result<(), String> {
    if options.apply_mode == ApplyMode::StateDir {
        return Ok(());
    }
    let path = options.state_dir.join("xray.json");
    if !path.exists() || options.state_dir.join("xray.disabled").exists() {
        return Ok(());
    }
    run_command("xray", &["-test", "-config", &path.display().to_string()])?;
    if !xray_running() {
        return Err("证书文件已写入，但 xray 当前没有运行；拒绝把磁盘状态报告成已加载".to_owned());
    }
    std::thread::sleep(std::time::Duration::from_secs(6));
    if !xray_running() {
        return Err("等待证书热更新时 xray 退出了".to_owned());
    }
    println!("xray 证书槽已通过预检并完成热更新等待");
    Ok(())
}

fn persist_converged_usage_generation(
    state_dir: &Path,
    generation_id: Option<i64>,
    outcome: (TargetApplyResult, ReportedNodeState, Option<String>),
) -> (TargetApplyResult, ReportedNodeState, Option<String>) {
    let Some(generation_id) = generation_id else {
        return outcome;
    };
    match write_usage_generation(state_dir, generation_id) {
        Ok(()) => outcome,
        Err(error) => (
            TargetApplyResult::FailedDirty,
            outcome.1,
            Some(match outcome.2 {
                Some(existing) => {
                    format!("{existing}；运行态已经应用，但 usage generation 写入失败：{error}")
                }
                None => format!("运行态已经应用，但 usage generation 写入失败：{error}"),
            }),
        ),
    }
}

/// Persisting the state-dir cache is part of a clean convergence, but its
/// failure must not return before the observation below is built and spooled.
/// The machine has already changed at this point; FailedDirty is the truthful
/// result and, crucially, gives the control plane a record of that fact.
fn successful_apply_outcome(
    state_dir: &Path,
    after: ReportedNodeState,
) -> (TargetApplyResult, ReportedNodeState, Option<String>) {
    match write_applied_state(state_dir, &after) {
        Ok(()) => (TargetApplyResult::Applied, after, None),
        Err(error) => (
            TargetApplyResult::FailedDirty,
            after,
            Some(format!(
                "运行态已经应用，但 applied-state.json 写入失败：{error}"
            )),
        ),
    }
}

/// One accounting sample: read the counters and durably append them.
///
/// Separating sampling from reporting is a requirement. Reading the counters is local
/// gRPC and costs little; only reporting goes over the network. The write in between is
/// not a cache but the only copy of this data: xray's counters are in memory and reset on
/// restart, so with the network down, skipping the write loses the traffic record.
fn collect_usage_report(options: &Options) -> Result<(), String> {
    // A wg-only node has no xray workload to sample. That is not an error; the independent
    // spool reporter still delivers readings left from an earlier role or outage.
    if options.state_dir.join("xray.json").exists()
        && !options.state_dir.join("xray.disabled").exists()
    {
        read_usage_report(&options.state_dir)
            .and_then(|report| spool_push(USAGE_SPOOL, &options.state_dir, &report))
    } else {
        Ok(())
    }
}

/// Synchronous composition retained for `usage-once`; the daemon uses independent sampler and
/// reporter workers so neither HTTP nor backlog recovery moves the accounting clock.
fn usage_cycle(options: &Options) -> Result<(), String> {
    let sampled = collect_usage_report(options);
    let drained = spool_drain(USAGE_SPOOL, options).map(|_| ());
    if let Err(error) = health_cycle(options) {
        eprintln!("link-health: {error}");
    }
    match (sampled, drained) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(sample), Err(drain)) => Err(format!(
            "本轮 usage 采集失败：{sample}；历史 usage spool 发送也失败：{drain}"
        )),
    }
}

/// Read the outbound counters once, compare against the previous round, and report
/// each hop's liveness.
fn collect_health_report(options: &Options) -> Result<Option<LinkHealthRequest>, String> {
    let xray_content = match fs::read_to_string(options.state_dir.join("xray.json")) {
        Ok(content) => content,
        // No xray on this machine (impossible for a pure backbone relay, but real for
        // a wg-only node)
        Err(_) => return Ok(None),
    };
    let Some(api_port) = xray_api_port(&xray_content) else {
        return Ok(None);
    };
    let now = read_hop_downlinks(api_port)?;
    Ok(judge_hops(now, current_unix_secs()? as u64))
}

fn send_health_report(options: &Options, report: &LinkHealthRequest) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/link-health", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "link health report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

fn health_cycle(options: &Options) -> Result<(), String> {
    if let Some(report) = collect_health_report(options)? {
        send_health_report(options, &report)?;
    }
    Ok(())
}

/// One host-sampling tick. Most ticks accumulate and produce nothing.
///
/// Nothing here is spooled, unlike usage and observations. The spool exists so a locally
/// established fact survives an outage: usage drives billing, and a convergence result is the
/// only record that convergence happened. A CPU reading from five minutes ago is neither, so
/// spooling it would add a file that can fill a disk in exchange for readings that are no longer
/// used. A failed send drops the window, and the next one is sent 30 seconds later.
fn build_load_report(options: &Options) -> Result<Option<LoadReportRequest>, String> {
    let now = current_unix_secs()? as u64;
    let Some((mut sample, processes)) = load::tick(&options.state_dir, now) else {
        // Still filling the window, or this was the first tick and there is no baseline to
        // difference against yet.
        return Ok(None);
    };

    // One all-state inet_diag dump serves two consumers: established connections feed per-hop
    // quality, while every non-listening state feeds anonymous local-port pressure. Keeping it
    // here makes the O(number of sockets) walk happen once per finished window, not per 10-second
    // sub-sample and not twice for the two views.
    let hops = match crate::inetdiag::dump() {
        Ok(conns) => {
            if let Some(range) = load::read_ephemeral_port_range() {
                crate::inetdiag::apply_port_pressure(
                    sample.network_detail.get_or_insert_default(),
                    &conns,
                    range,
                );
            }
            collect_hops(
                options,
                &conns,
                sample.window_start_unix_secs,
                sample.window_end_unix_secs,
            )
        }
        Err(error) => {
            // Insufficient privilege and a kernel without inet_diag are both legitimate legacy
            // environments. Lose these optional details, never the host sample around them.
            eprintln!("load: inet_diag unavailable: {error}");
            Vec::new()
        }
    };
    Ok(Some(LoadReportRequest {
        read_at_unix_secs: now as i64,
        btime_unix_secs: load::current_btime(),
        host: load::host_facts(&options.state_dir),
        samples: vec![sample],
        processes,
        hops,
    }))
}

fn send_load_report(options: &Options, report: &LoadReportRequest) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/load", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "load report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    // Print the response, as the usage spool does with its own. It carries the two counters to
    // watch: a non-zero `skipped_samples` means windows are colliding, and a non-zero
    // `rejected_hops` means this machine is reporting hops it does not carry. Neither is
    // observable from this side otherwise.
    println!("{}", response.body);
    Ok(())
}

fn load_cycle(options: &Options) -> Result<(), String> {
    if let Some(report) = build_load_report(options)? {
        send_load_report(options, &report)?;
    }
    Ok(())
}

/// Per-hop link quality for this window. Empty is a normal answer on a machine with no xray, or
/// one whose hops have no connections right now.
fn collect_hops(
    options: &Options,
    conns: &[crate::inetdiag::Conn],
    window_start: i64,
    window_end: i64,
) -> Vec<brocade_deployment::protocol::HopLinkSample> {
    let Ok(content) = fs::read_to_string(options.state_dir.join("xray.json")) else {
        return Vec::new();
    };
    let targets = crate::probe::hop_targets(&content);
    if targets.is_empty() {
        // No forwarding outbound: a pure wg backbone relay, or an ingress-only machine. Dumping
        // every socket to attribute none of them is work for nothing.
        return Vec::new();
    }
    crate::inetdiag::aggregate(conns, &targets, window_start, window_end)
}

fn usage_once(options: Options) -> Result<(), String> {
    usage_cycle(&options)
}

/// Sample a full window and report it once, then stop.
///
/// Unlike the other one-shot commands this one takes time, about
/// `SUB_INTERVAL × (SUBS_PER_WINDOW + 1)`, so roughly 40 seconds. The duration is inherent rather
/// than an implementation detail: a rate needs two readings and a window needs three rates, so a
/// single instantaneous call could report only zeroes. `usage-once` can be instant because
/// counters are cumulative and one read is a complete value; a CPU percentage is not.
fn load_once(options: Options) -> Result<(), String> {
    let rounds = load::SUBS_PER_WINDOW + 1;
    for round in 0..rounds {
        if round > 0 {
            thread::sleep(Duration::from_secs(load::SUB_INTERVAL_SECS));
        }
        eprintln!(
            "load-once: 采样 {}/{rounds}（每次间隔 {} 秒）",
            round + 1,
            load::SUB_INTERVAL_SECS
        );
        load_cycle(&options)?;
    }
    Ok(())
}

fn read_usage_report(state_dir: &Path) -> Result<UsageReportRequest, String> {
    let xray_content = fs::read_to_string(state_dir.join("xray.json"))
        .map_err(|error| format!("failed to read xray.json from state dir: {error}"))?;
    let api_port = xray_api_port(&xray_content).ok_or("xray.json does not contain api port")?;
    // Reserve and persist before touching the volatile counters. A crash may leave a harmless
    // sequence gap; reserving after the read could reuse the same id for different traffic.
    let (agent_instance_id, sequence) = reserve_usage_sequence(state_dir)?;
    let process = xray_process_identity()?;
    Ok(UsageReportRequest {
        agent_instance_id: Some(agent_instance_id),
        sequence: Some(sequence),
        usage_generation_id: read_usage_generation(state_dir)?,
        read_at_unix_secs: current_unix_secs()?,
        xray_started_at_unix_secs: process.started_at_unix_secs,
        xray_epoch: Some(process.epoch),
        route: Some(route_ip_report()),
        counters: read_xray_usage_counters(api_port)?,
    })
}

const ROUTE_REPORT_TTL: Duration = Duration::from_secs(30);
const ROUTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
static ROUTE_REPORT_CACHE: OnceLock<Mutex<Option<(Instant, RouteIpReport)>>> = OnceLock::new();

fn route_ip_report() -> RouteIpReport {
    let cache = ROUTE_REPORT_CACHE.get_or_init(|| Mutex::new(None));
    let mut cached = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((observed_at, report)) = cached.as_ref() {
        if observed_at.elapsed() < ROUTE_REPORT_TTL {
            return report.clone();
        }
    }
    let report = RouteIpReport {
        ipv4: route_source_ip(&["-4", "route", "get", "1.1.1.1"], RouteFamily::V4),
        ipv6: route_source_ip(
            &["-6", "route", "get", "2606:4700:4700::1111"],
            RouteFamily::V6,
        ),
    };
    *cached = Some((Instant::now(), report.clone()));
    report
}

fn desired_request(client: &HttpClient, options: &Options) -> Result<HttpResponse, String> {
    let route = route_ip_report();
    let headers = route_headers(&route);
    client.request_with_headers("GET", "/agent/v1/desired", &options.token, None, &headers)
}

fn route_headers(route: &RouteIpReport) -> Vec<(&'static str, String)> {
    let mut headers = Vec::new();
    if let Some(ipv4) = &route.ipv4 {
        headers.push((ROUTE_IPV4_HEADER, ipv4.clone()));
    }
    if let Some(ipv6) = &route.ipv6 {
        headers.push((ROUTE_IPV6_HEADER, ipv6.clone()));
    }
    headers
}

#[derive(Debug, Clone, Copy)]
enum RouteFamily {
    V4,
    V6,
}

fn route_source_ip(args: &[&str], family: RouteFamily) -> Option<String> {
    let output = command::run_command_with_timeout("ip", args, ROUTE_COMMAND_TIMEOUT).ok()?;
    let mut parts = output.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "src" {
            let value = parts.next()?;
            let ip = value.parse::<IpAddr>().ok()?;
            return match (family, ip) {
                (RouteFamily::V4, IpAddr::V4(_)) | (RouteFamily::V6, IpAddr::V6(_)) => {
                    Some(value.to_owned())
                }
                _ => None,
            };
        }
    }
    None
}

fn converge_to_state_dir(
    state_dir: &Path,
    desired: &NodeDesiredDeployment,
) -> Result<ReportedNodeState, String> {
    create_private_dir(state_dir)?;
    let phantun = converge_artifact(
        state_dir,
        "phantun.json",
        "phantun.disabled",
        &desired.desired.phantun,
    )?;
    let wireguard = converge_artifact(
        state_dir,
        "wireguard.conf",
        "wireguard.disabled",
        &desired.desired.wireguard,
    )?;
    let xray = converge_artifact(
        state_dir,
        "xray.json",
        "xray.disabled",
        &desired.desired.xray,
    )?;
    let hy2_port_hop = converge_artifact(
        state_dir,
        "hy2_port_hop.json",
        "hy2_port_hop.disabled",
        &desired.desired.hy2_port_hop,
    )?;
    hy2_port_hop::apply(&desired.desired.hy2_port_hop)?;
    // Derived from the xray artifact rather than one of its own: the ports to exempt are exactly
    // the inbounds xray already declares, and a second artifact naming them again would be a
    // second thing to keep in step with the first.
    conntrack::apply(&desired.desired.xray)?;
    let grants = converge_grants(state_dir, &desired.desired.grants)?;

    Ok(ReportedNodeState {
        phantun,
        wireguard,
        xray,
        hy2_port_hop,
        grants,
    })
}

fn converge_linux(
    state_dir: &Path,
    desired: &NodeDesiredDeployment,
    // The hook for step 4. A parameter rather than a direct call, because state-dir
    // mode and a lone apply-once have no reporting channel and must skip this step.
    before_xray_restart: &mut dyn FnMut(),
) -> Result<(), String> {
    create_private_dir(state_dir)?;

    // A missing phantun binary may require a network download.  Finish that
    // before taking the lock shared with wg's watchdog; only short local process
    // and interface operations belong inside the critical section.
    if let DesiredArtifact::Present { content, .. } = &desired.desired.phantun {
        prepare_phantun_binaries(content, desired.phantun_binary.as_ref())?;
    }

    {
        // The two backbone pieces contend with the watchdog thread over the same
        // interface. The lock covers only these two steps: the xray ones do not
        // intersect the watchdog, and including them would only make it wait out an
        // xray restart for nothing.
        let _backbone = backbone_lock();

        // phantun has to start before wg is touched. For the fake-TCP peers in
        // `wg0.conf`, `Endpoint` names the phantun client's local loopback port, so
        // with phantun not running the handshake packets reach a port with no
        // listener, and the symptom is a correct configuration whose handshake never
        // completes.
        converge_linux_phantun(
            state_dir,
            &desired.desired.phantun,
            desired.phantun_binary.as_ref(),
        )?;
        converge_linux_wireguard(state_dir, &desired.desired.wireguard)?;
    }
    // xray's counters are in memory and reset on restart. Without a sample here,
    // everything since the last one is lost, and the loss is not reported anywhere; it
    // appears only as an understated bill.
    if desired.usage_generation_id.is_some() {
        before_xray_restart();
    }
    converge_linux_xray(state_dir, &desired.desired.xray, false)?;
    // After xray, because the ports come out of its config: applying first would exempt the
    // previous release's ports and leave the new ones tracked until the next convergence.
    conntrack::apply(&desired.desired.xray)?;
    converge_linux_hy2_port_hop(state_dir, &desired.desired.hy2_port_hop)?;
    converge_linux_grants(state_dir, &desired.desired.xray, &desired.desired.grants)?;

    Ok(())
}

fn converge_linux_hy2_port_hop(state_dir: &Path, desired: &DesiredArtifact) -> Result<(), String> {
    converge_artifact(
        state_dir,
        "hy2_port_hop.json",
        "hy2_port_hop.disabled",
        desired,
    )?;
    hy2_port_hop::apply(desired)
}

fn converge_linux_wireguard(state_dir: &Path, desired: &DesiredArtifact) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => {
            let path = state_dir.join("wireguard.conf");
            write_private(&path, content)?;
            let _ = fs::remove_file(state_dir.join("wireguard.disabled"));
            apply_wireguard(&path)?;
            Ok(())
        }
        DesiredArtifact::Disabled { reason } => {
            let _ = run_shell("ip link del wg0 2>/dev/null || true")?;
            let _ = fs::remove_file(state_dir.join("wireguard.conf"));
            fs::write(state_dir.join("wireguard.disabled"), reason)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

fn apply_wireguard(path: &Path) -> Result<(), String> {
    let conf = shell_quote(&path.display().to_string());
    let stripped_path = Path::new("/tmp/brocade-agent-wg0.stripped");
    let existed = command_success("ip", &["link", "show", "wg0"]);
    let configure = if existed {
        "wg syncconf wg0 /tmp/brocade-agent-wg0.stripped"
    } else {
        "ip link add wg0 type wireguard\n\
         wg setconf wg0 /tmp/brocade-agent-wg0.stripped"
    };
    run_shell(&format!(
        "set -eu\n\
         CONF={conf}\n\
         wg-quick strip \"$CONF\" > /tmp/brocade-agent-wg0.stripped\n\
         {configure}"
    ))?;
    if existed {
        let reset = reset_stale_phantun_passive_peers(path, stripped_path)?;
        if reset > 0 {
            println!("wireguard: 已清理 {reset} 个 phantun 被动 peer 的旧运行态");
        }
    }
    run_shell(&format!(
        "set -eu\n\
         CONF={conf}\n\
         ADDR=$(awk -F'= *' '/^Address/{{print $2; exit}}' \"$CONF\" | tr -d ' ')\n\
         MTU=$(awk -F'= *' '/^MTU/{{print $2; exit}}' \"$CONF\" | tr -d ' ')\n\
         if [ -n \"$MTU\" ]; then ip link set wg0 mtu \"$MTU\"; fi\n\
         if [ -n \"$ADDR\" ] && ! ip addr show dev wg0 | grep -q \"${{ADDR%/*}}\"; then\n\
           ip addr add \"$ADDR\" dev wg0 2>/dev/null || true\n\
         fi\n\
         ip link set wg0 up\n\
         wg show wg0 allowed-ips | awk '{{print $2}}' | while read -r net; do\n\
           [ -n \"$net\" ] && ip route replace \"$net\" dev wg0 || true\n\
         done"
    ))?;
    Ok(())
}

fn converge_linux_xray(
    state_dir: &Path,
    desired: &DesiredArtifact,
    force_restart: bool,
) -> Result<(), String> {
    match desired {
        DesiredArtifact::Present { content, .. } => {
            let path = state_dir.join("xray.json");
            // Read before writing: the file about to be overwritten is the only record of
            // what the running process was given, and the swap below is expressed as the
            // difference between the two.
            let previous = fs::read_to_string(&path).ok();
            let desired_splice_disabled = xray_needs_splice_disabled(content)?;
            let splice_mode_changed = previous
                .as_deref()
                .map(xray_needs_splice_disabled)
                .transpose()?
                != Some(desired_splice_disabled);
            write_private(&path, content)?;
            let _ = fs::remove_file(state_dir.join("xray.disabled"));
            let api_port = xray_api_port(content).unwrap_or(10085);
            let bounded_log = state_dir.join(XRAY_BOUNDED_LOG_MARKER).exists();

            // The file is already on disk, so a cold start uses the new config whichever
            // branch runs. That is what makes the swap an optimization rather than a
            // second source of truth: it removes a restart, and a failed swap costs only
            // the restart it was avoiding.
            if !force_restart && xray_running() && !splice_mode_changed && bounded_log {
                if let Some(swap) = previous
                    .as_deref()
                    .and_then(|previous| hot_swap(previous, content))
                {
                    match apply_hot_swap(state_dir, api_port, content, &swap) {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            // Not fatal, and deliberately not silent: a swap that keeps
                            // failing is a machine restarting on every deployment while
                            // appearing to have gained the ability not to.
                            eprintln!("热切换失败，回退到重启 xray：{error}");
                        }
                    }
                }
            }
            apply_xray(&path, api_port)?;
            Ok(())
        }
        DesiredArtifact::Disabled { reason } => {
            terminate_xray()?;
            let _ = fs::remove_file("/tmp/brocade-agent-xray.log");
            let _ = fs::remove_file(state_dir.join("xray.json"));
            let _ = fs::remove_file(state_dir.join(XRAY_BOUNDED_LOG_MARKER));
            let _ = fs::remove_file(state_dir.join(XRAY_SHARED_POLICY_LOG_MARKER));
            let _ = fs::remove_file(state_dir.join(XRAY_OLD_BOUNDED_LOG_MARKER));
            fs::write(state_dir.join("xray.disabled"), reason)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        DesiredArtifact::Unmanaged { .. } => Ok(()),
    }
}

/// Installs a `HotSwap`, in the one order that leaves no gap.
///
/// Appending the new table while the old one is still installed keeps the old rules in
/// effect until they are removed, because they remain ahead of the new ones and routing
/// takes the first match. There is no instant at which the table is empty or partially
/// built.
///
/// The simpler alternative is incorrect: `adrules` without `-append` replaces the table, and
/// a replacement removes the balancers with it. The liveness balancer cannot be rebuilt
/// afterwards, so hop health would stop with nothing reporting that it had.
fn apply_hot_swap(
    state_dir: &Path,
    api_port: u16,
    xray_content: &str,
    swap: &HotSwap,
) -> Result<(), String> {
    if swap.add_outbounds.is_empty()
        && swap.remove_outbounds.is_empty()
        && swap.add_rules.is_empty()
        && swap.remove_rule_tags.is_empty()
        && swap.add_inbounds.is_empty()
        && swap.remove_inbounds.is_empty()
    {
        return Ok(());
    }
    let server = format!("--server=127.0.0.1:{api_port}");
    // `--server` goes directly after the verb. Go's flag parsing stops at the first
    // non-flag argument, so placed after a file name it is read as another file, and the
    // call dials the default port with no error.
    let call = |verb: &str, rest: &[&str]| -> Result<String, String> {
        let mut args = vec!["api", verb, server.as_str()];
        args.extend_from_slice(rest);
        run_command("xray", &args)
    };
    let stage = |name: &str, value: Value| -> Result<PathBuf, String> {
        let path = state_dir.join(name);
        write_private(
            &path,
            &serde_json::to_string(&value).map_err(|error| error.to_string())?,
        )?;
        Ok(path)
    };

    if !swap.add_outbounds.is_empty() {
        let path = stage(
            "hotswap.outbounds.json",
            json!({ "outbounds": swap.add_outbounds }),
        )?;
        call("ado", &[&path.display().to_string()])?;
    }

    // Inbounds leave before the rule table does. A rule table is swapped by adding the new
    // one and then removing the old, so for a moment both are installed. If an inbound
    // being removed were still listening once its rule had gone, traffic arriving on it
    // would match nothing and leave by whichever rule matched next. Traffic taking an
    // unrelated exit with no report is the failure a relay port without an account
    // produced in the preview cluster, and it is not visible in the artifacts.
    if !swap.remove_inbounds.is_empty() {
        let remove = swap
            .remove_inbounds
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        call("rmi", &remove)?;
    }

    // Skipped when the table did not change: an outbound can be added or removed without
    // any rule moving, and reinstalling an identical table would collide with itself.
    if !swap.add_rules.is_empty() {
        let path = stage(
            "hotswap.rules.json",
            json!({ "routing": { "rules": swap.add_rules } }),
        )?;
        // Balancers are declared once at startup and only referenced here. Sending them
        // again is rejected as a duplicate tag, which is why the staged payload carries
        // `rules` alone.
        call("adrules", &["-append", &path.display().to_string()])?;

        let remove = swap
            .remove_rule_tags
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        call("rmrules", &remove)?;
    }

    // Inbounds arrive after the rule table, for the mirror of the reason they left before
    // it: an inbound accepting traffic that no rule yet describes falls through the same
    // way. A rule naming an inbound that does not exist yet, by contrast, costs nothing —
    // nothing can arrive on it.
    if !swap.add_inbounds.is_empty() {
        let path = stage(
            "hotswap.inbounds.json",
            json!({ "inbounds": swap.add_inbounds }),
        )?;
        call("adi", &[&path.display().to_string()])?;

        // A newly added inbound has no accounts: the compiled config carries none, and
        // they exist only in the running process. Nothing downstream corrects this,
        // because a configuration deployment leaves grants `Unmanaged`, which performs no
        // action, so an inbound left in this state would listen and reject every
        // subscription as an unknown user. A failure here is reported, and the caller
        // then restarts, which reloads the config and takes the path that re-pushes the
        // accounts.
        if let Some(want) = desired_grants_on_disk(state_dir)? {
            sync_grants(state_dir, xray_content, api_port, &want)?;
        }
    }

    // Last, and only at this point: until the old rules were removed, a rule could still
    // reference these outbounds.
    if !swap.remove_outbounds.is_empty() {
        let remove = swap
            .remove_outbounds
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        call("rmo", &remove)?;
    }
    Ok(())
}

fn apply_xray(path: &Path, api_port: u16) -> Result<(), String> {
    let conf = shell_quote(&path.display().to_string());
    let content =
        fs::read_to_string(path).map_err(|error| format!("读取待启动的 xray 配置失败：{error}"))?;
    let disable_splice = xray_needs_splice_disabled(&content)?;
    let launch = if disable_splice {
        format!("env 'xray.buf.splice=disable' xray run -config {conf}")
    } else {
        format!("xray run -config {conf}")
    };
    let state_dir = path.parent().ok_or("xray config has no state directory")?;
    let log_dir = state_dir.join("logs");
    create_private_dir(&log_dir)?;
    let log_path = log_dir.join("xray.log");
    let sink = logcap::command(&log_path, state_dir, logcap::WorkloadLog::Xray)?;
    let pipeline = shell_quote(&format!("{launch} 2>&1 | {sink}"));
    let log = shell_quote(&log_path.display().to_string());
    run_command("xray", &["-test", "-config", &path.display().to_string()])?;
    // Xray closes its listeners as soon as SIGTERM starts the synchronous feature teardown.
    // Waiting for that teardown before launching the replacement makes the entire wait a service
    // outage. Bound the graceful phase tightly, then kill a stuck old process; a configuration
    // restart cannot preserve its sessions in either case.
    terminate_xray()?;
    run_shell(&format!(
        "set -eu\n\
         nohup sh -c {pipeline} >/dev/null 2>&1 &\n\
         launcher=$!\n\
         port_up() {{\n\
           if command -v ss >/dev/null 2>&1; then ss -ltn 2>/dev/null | grep -q ':{api_port} ';\n\
           else netstat -ltn 2>/dev/null | grep -q ':{api_port} '; fi\n\
         }}\n\
         for _ in $(seq 1 400); do\n\
           port_up && exit 0\n\
           kill -0 \"$launcher\" 2>/dev/null || break\n\
           sleep 0.05\n\
         done\n\
         cat {log} 2>/dev/null || true\n\
         exit 1"
    ))?;
    fs::write(state_dir.join(XRAY_BOUNDED_LOG_MARKER), b"dynamic\n")
        .map_err(|error| format!("failed to record bounded xray logging: {error}"))?;
    let _ = fs::remove_file(state_dir.join(XRAY_SHARED_POLICY_LOG_MARKER));
    let _ = fs::remove_file(state_dir.join(XRAY_OLD_BOUNDED_LOG_MARKER));
    // The legacy file is no longer held open once the old Xray has exited. Removing it here, not
    // during installation, guarantees its blocks are actually released immediately.
    let _ = fs::remove_file("/tmp/brocade-agent-xray.log");
    Ok(())
}

fn xray_needs_splice_disabled(content: &str) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_str(content)
        .map_err(|error| format!("解析待启动的 xray 配置失败：{error}"))?;
    Ok(value["inbounds"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|inbound| {
            inbound["protocol"] == "dokodemo-door"
                && inbound["settings"]["port"].is_number()
                && matches!(
                    inbound["streamSettings"]["security"].as_str(),
                    Some("reality" | "tls")
                )
        }))
}

fn converge_linux_grants(
    state_dir: &Path,
    desired_xray: &DesiredArtifact,
    desired: &DesiredGrants,
) -> Result<(), String> {
    match desired {
        DesiredGrants::Present { inbounds } => {
            // The list goes into the running xray, so both the inbounds and the api
            // port come from it. A grants deployment marks xray Unmanaged and does not
            // restart it, so this reads the local `xray.json`, which is the config the
            // last convergence wrote and the process is currently using. The same
            // applies to a machine whose config deployment left xray unchanged. This is
            // more accurate than reading the desired xray, because the difference has to
            // be taken against the inbounds present on the machine rather than against a
            // desired state not yet applied.
            let xray_content = match desired_xray {
                DesiredArtifact::Present { content, .. } => content.clone(),
                DesiredArtifact::Unmanaged { .. } => {
                    fs::read_to_string(state_dir.join("xray.json")).map_err(|error| {
                        format!("这一单不带 xray，本地也读不到 xray.json：{error}")
                    })?
                }
                DesiredArtifact::Disabled { .. } => {
                    return Err("cannot sync grants while xray is disabled".to_owned())
                }
            };
            let xray_content = xray_content.as_str();
            let api_port = xray_api_port(xray_content).unwrap_or(10085);
            // Persist the full desired set so that local reconvergence works without
            // a deployment. grants.adu.json cannot stand in: it is the last
            // incremental batch and holds only the people added that time.
            write_private(
                &state_dir.join("grants.desired.json"),
                &serde_json::to_string_pretty(inbounds).map_err(|error| error.to_string())?,
            )?;
            sync_grants(state_dir, xray_content, api_port, inbounds)?;
            Ok(())
        }
        DesiredGrants::Disabled { .. } => Ok(()),
        DesiredGrants::Unmanaged { .. } => Ok(()),
    }
}

fn observe_state(
    state_dir: &Path,
    desired: &NodeDesiredDeployment,
    apply_mode: ApplyMode,
) -> Result<ReportedNodeState, String> {
    match apply_mode {
        ApplyMode::StateDir => read_applied_state(state_dir),
        ApplyMode::Linux => Ok(observe_linux_state(state_dir, desired)),
    }
}

fn observe_linux_state(state_dir: &Path, desired: &NodeDesiredDeployment) -> ReportedNodeState {
    let phantun = observe_linux_phantun(state_dir, &desired.desired.phantun);
    let wireguard = observe_linux_wireguard(state_dir, &desired.desired.wireguard);
    let xray = observe_linux_xray(state_dir, &desired.desired.xray);
    let grants = observe_linux_grants(
        state_dir,
        &desired.desired.xray,
        &desired.desired.grants,
        &xray,
    );

    ReportedNodeState {
        phantun,
        wireguard,
        xray,
        hy2_port_hop: if hy2_port_hop::matches(&desired.desired.hy2_port_hop) {
            match &desired.desired.hy2_port_hop {
                DesiredArtifact::Present { sha256, .. } => AppliedArtifactState::Present {
                    sha256: sha256.clone(),
                },
                DesiredArtifact::Disabled { .. } => AppliedArtifactState::Disabled,
                DesiredArtifact::Unmanaged { .. } => AppliedArtifactState::Unmanaged,
            }
        } else {
            AppliedArtifactState::Dirty {
                reason: "机器上的端口跳转规则跟产物对不上".to_owned(),
            }
        },
        grants,
    }
}

fn observe_linux_wireguard(state_dir: &Path, desired: &DesiredArtifact) -> AppliedArtifactState {
    if matches!(desired, DesiredArtifact::Unmanaged { .. }) {
        return AppliedArtifactState::Unmanaged;
    }

    let path = state_dir.join("wireguard.conf");
    let disabled_path = state_dir.join("wireguard.disabled");
    let has_config = path.exists();
    let has_disabled_marker = disabled_path.exists();
    let has_wg0 = command_success("ip", &["link", "show", "wg0"]);

    match (has_config, has_disabled_marker, has_wg0) {
        (true, true, _) => artifact_dirty("wireguard.conf and wireguard.disabled both exist"),
        (true, false, true) => present_file_state(&path, "wireguard.conf"),
        (true, false, false) => artifact_dirty("wireguard.conf exists but wg0 is not present"),
        (false, true, false) => AppliedArtifactState::Disabled,
        (false, true, true) => artifact_dirty("wg0 is present while wireguard is disabled"),
        (false, false, true) => artifact_dirty("wg0 is present without wireguard.conf"),
        (false, false, false) => AppliedArtifactState::Unknown,
    }
}

fn observe_linux_xray(state_dir: &Path, desired: &DesiredArtifact) -> AppliedArtifactState {
    if matches!(desired, DesiredArtifact::Unmanaged { .. }) {
        return AppliedArtifactState::Unmanaged;
    }

    let path = state_dir.join("xray.json");
    let disabled_path = state_dir.join("xray.disabled");
    let has_config = path.exists();
    let has_disabled_marker = disabled_path.exists();
    let has_xray = xray_running();

    match (has_config, has_disabled_marker, has_xray) {
        (true, true, _) => artifact_dirty("xray.json and xray.disabled both exist"),
        (true, false, false) => artifact_dirty("xray.json exists but xray is not running"),
        (true, false, true) => {
            let content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => return artifact_dirty(format!("failed to read xray.json: {error}")),
            };
            let Some(api_port) = xray_api_port(&content) else {
                return artifact_dirty("xray.json does not contain api inbound port");
            };
            if !tcp_port_listening(api_port) {
                return artifact_dirty(format!("xray api port {api_port} is not listening"));
            }
            if !state_dir.join(XRAY_BOUNDED_LOG_MARKER).exists() {
                return artifact_dirty("xray is still using the legacy unbounded log");
            }
            AppliedArtifactState::Present {
                sha256: sha256_hex(content.as_bytes()),
            }
        }
        (false, true, false) => AppliedArtifactState::Disabled,
        (false, true, true) => artifact_dirty("xray is running while xray is disabled"),
        (false, false, true) => artifact_dirty("xray is running without xray.json"),
        (false, false, false) => AppliedArtifactState::Unknown,
    }
}

fn observe_linux_grants(
    state_dir: &Path,
    desired_xray: &DesiredArtifact,
    desired: &DesiredGrants,
    observed_xray: &AppliedArtifactState,
) -> AppliedGrantsState {
    match desired {
        DesiredGrants::Disabled { .. } => AppliedGrantsState::Disabled,
        DesiredGrants::Unmanaged { .. } => AppliedGrantsState::Unmanaged,
        // `Unmanaged` takes the same path as `Present`: it means this deployment did
        // not manage xray, not that no xray runs on the machine. A grants deployment
        // always marks xray Unmanaged, and taking the Unmanaged branch below would
        // leave the list's state unreported, so the control plane could not determine
        // whether the deployment succeeded.
        // The actual test is further down: an unreadable local xray.json, or an api
        // port that does not answer, is what identifies a stopped process.
        DesiredGrants::Present { inbounds } => match observed_xray {
            AppliedArtifactState::Present { .. } | AppliedArtifactState::Unmanaged => {
                let path = state_dir.join("xray.json");
                let content = match fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(error) => {
                        return grants_dirty(format!(
                            "failed to read xray.json for grants: {error}"
                        ))
                    }
                };
                let api_port = xray_api_port(&content).or_else(|| match desired_xray {
                    DesiredArtifact::Present { content, .. } => xray_api_port(content),
                    _ => None,
                });
                let Some(api_port) = api_port else {
                    return grants_dirty("cannot observe grants without xray api port");
                };
                match read_observed_grants(api_port, inbounds) {
                    Ok(inbounds) => AppliedGrantsState::Present { inbounds },
                    Err(error) => grants_dirty(error),
                }
            }
            AppliedArtifactState::Disabled => AppliedGrantsState::Disabled,
            AppliedArtifactState::Unknown => AppliedGrantsState::Unknown,
            AppliedArtifactState::Dirty { reason } => grants_dirty(format!(
                "cannot observe grants because xray is dirty: {reason}"
            )),
        },
    }
}

fn present_file_state(path: &Path, label: &str) -> AppliedArtifactState {
    match file_sha256_hex(path) {
        Ok(sha256) => AppliedArtifactState::Present { sha256 },
        Err(error) => artifact_dirty(format!("failed to hash {label}: {error}")),
    }
}

fn artifact_dirty(reason: impl Into<String>) -> AppliedArtifactState {
    AppliedArtifactState::Dirty {
        reason: reason.into(),
    }
}

fn grants_dirty(reason: impl Into<String>) -> AppliedGrantsState {
    AppliedGrantsState::Dirty {
        reason: reason.into(),
    }
}

fn sync_grants(
    state_dir: &Path,
    xray_content: &str,
    api_port: u16,
    desired: &[GrantInbound],
) -> Result<(), String> {
    let backend = configured_grant_write_backend()?;
    sync_grants_with_backend(state_dir, xray_content, api_port, desired, backend)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GrantAccount {
    Vless { id: String, flow: Option<String> },
    Hysteria2 { auth: String },
    AnyTls { password: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GrantAddition {
    tag: String,
    email: String,
    level: u8,
    account: GrantAccount,
}

trait GrantWriteBackend {
    fn validate_protocol(&self, protocol: GrantProtocol) -> Result<(), String>;

    fn remove_user(&self, api_port: u16, tag: &str, email: &str) -> Result<(), String>;

    fn add_users(
        &self,
        state_dir: &Path,
        api_port: u16,
        additions: &[GrantAddition],
        cli_inbounds: &[serde_json::Value],
    ) -> Result<(), String>;
}

/// Native gRPC is the only default. A protocol or transport failure is not retried through a
/// subprocess with different behavior.
struct GrpcGrantWriteBackend;

impl GrantWriteBackend for GrpcGrantWriteBackend {
    fn validate_protocol(&self, _protocol: GrantProtocol) -> Result<(), String> {
        Ok(())
    }

    fn remove_user(&self, api_port: u16, tag: &str, email: &str) -> Result<(), String> {
        xray_grpc::remove_user(api_port, tag, email)
    }

    fn add_users(
        &self,
        _state_dir: &Path,
        api_port: u16,
        additions: &[GrantAddition],
        _cli_inbounds: &[serde_json::Value],
    ) -> Result<(), String> {
        for addition in additions {
            let account = match &addition.account {
                GrantAccount::Vless { id, flow } => xray_grpc::XrayAccount::Vless {
                    id,
                    flow: flow.as_deref(),
                },
                GrantAccount::Hysteria2 { auth } => xray_grpc::XrayAccount::Hysteria2 { auth },
                GrantAccount::AnyTls { password } => xray_grpc::XrayAccount::AnyTls { password },
            };
            xray_grpc::add_user(
                api_port,
                &addition.tag,
                &addition.email,
                u32::from(addition.level),
                account,
            )?;
        }
        Ok(())
    }
}

/// Compatibility implementation for old deployments and manual rollback only.
///
/// New reconciliation must use [`GrpcGrantWriteBackend`]. This backend is intentionally never an
/// automatic fallback: falling back would hide a broken native encoder or API contract.
#[deprecated(note = "use GrpcGrantWriteBackend; the xray CLI grant writer is compatibility-only")]
struct XrayCliGrantWriteBackend;

#[allow(deprecated)]
impl GrantWriteBackend for XrayCliGrantWriteBackend {
    fn validate_protocol(&self, protocol: GrantProtocol) -> Result<(), String> {
        match protocol {
            GrantProtocol::Vless => Ok(()),
            GrantProtocol::Hysteria2 => Err(
                "deprecated xray CLI grant backend cannot add Hysteria 2 users; use native gRPC"
                    .to_owned(),
            ),
            GrantProtocol::AnyTls => Err(
                "deprecated xray CLI grant backend cannot add AnyTLS users; use native gRPC"
                    .to_owned(),
            ),
        }
    }

    fn remove_user(&self, api_port: u16, tag: &str, email: &str) -> Result<(), String> {
        run_command(
            "xray",
            &[
                "api",
                "rmu",
                &format!("--server=127.0.0.1:{api_port}"),
                &format!("-tag={tag}"),
                email,
            ],
        )?;
        Ok(())
    }

    /// The file exists only as a command-line argument.
    ///
    /// It was previously written by the shared path and left in place, which made it resemble a
    /// stored record. It is not one: it holds only the accounts added in that one round, so it
    /// substitutes for neither the desired set (`grants.desired.json`, which local
    /// reconvergence reads) nor the compiled grants artifact. It does hold credentials in the
    /// clear, so it is written by the code that needs it and removed as soon as that call
    /// returns.
    fn add_users(
        &self,
        state_dir: &Path,
        api_port: u16,
        _additions: &[GrantAddition],
        cli_inbounds: &[serde_json::Value],
    ) -> Result<(), String> {
        let path = state_dir.join("grants.adu.json");
        let payload = serde_json::json!({ "inbounds": cli_inbounds });
        let text = serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?;
        write_private(&path, &text)?;
        let result = run_command(
            "xray",
            &[
                "api",
                "adu",
                &format!("--server=127.0.0.1:{api_port}"),
                &path.display().to_string(),
            ],
        );
        // Unconditionally: a failed call leaves its detail in the error, not on the disk.
        let _ = fs::remove_file(&path);
        result?;
        Ok(())
    }
}

static GRPC_GRANT_WRITE_BACKEND: GrpcGrantWriteBackend = GrpcGrantWriteBackend;
#[allow(deprecated)]
static CLI_GRANT_WRITE_BACKEND: XrayCliGrantWriteBackend = XrayCliGrantWriteBackend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantWriteBackendChoice {
    Grpc,
    Cli,
}

fn grant_write_backend_choice(value: Option<&str>) -> Result<GrantWriteBackendChoice, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("grpc") => Ok(GrantWriteBackendChoice::Grpc),
        Some("cli") => Ok(GrantWriteBackendChoice::Cli),
        Some(value) => Err(format!(
            "unknown BROCADE_XRAY_GRANT_BACKEND {value}; expected grpc or cli"
        )),
    }
}

fn configured_grant_write_backend() -> Result<&'static dyn GrantWriteBackend, String> {
    match grant_write_backend_choice(env::var("BROCADE_XRAY_GRANT_BACKEND").ok().as_deref())? {
        GrantWriteBackendChoice::Grpc => Ok(&GRPC_GRANT_WRITE_BACKEND),
        GrantWriteBackendChoice::Cli => {
            warn("BROCADE_XRAY_GRANT_BACKEND=cli 已 deprecated，仅用于 VLESS 兼容/回滚");
            Ok(&CLI_GRANT_WRITE_BACKEND)
        }
    }
}

fn sync_grants_with_backend(
    state_dir: &Path,
    xray_content: &str,
    api_port: u16,
    desired: &[GrantInbound],
    backend: &dyn GrantWriteBackend,
) -> Result<(), String> {
    let xray: serde_json::Value =
        serde_json::from_str(xray_content).map_err(|error| error.to_string())?;

    let mut add_inbounds = Vec::new();
    let mut additions = Vec::new();
    for inbound in desired {
        let mut xray_inbound = xray_inbound_by_tag(&xray, &inbound.tag)?;
        let protocol = grant_protocol(&xray_inbound, &inbound.tag)?;
        // Checked before observing or removing any account. The deprecated CLI backend cannot
        // add Hysteria accounts, and detecting that after the removals would turn an
        // unsupported compatibility setting into an outage.
        backend.validate_protocol(protocol)?;
        let desired_clients = inbound
            .clients
            .iter()
            .map(|client| {
                let flow = match protocol {
                    GrantProtocol::Vless => client.flow.clone(),
                    GrantProtocol::Hysteria2 | GrantProtocol::AnyTls => None,
                };
                (
                    client.email.clone(),
                    (client.uuid.clone(), flow, client.level),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let actual = read_users(api_port, &inbound.tag)?;
        let actual_by_email = actual
            .into_iter()
            .map(|client| (client.email, (client.uuid, client.flow)))
            .collect::<BTreeMap<_, _>>();

        for (email, (uuid, flow)) in &actual_by_email {
            if desired_clients
                .get(email)
                .is_none_or(|(desired_uuid, desired_flow, _)| {
                    desired_uuid != uuid || desired_flow != flow
                })
            {
                backend.remove_user(api_port, &inbound.tag, email)?;
            }
        }

        let missing = inbound
            .clients
            .iter()
            .filter(|client| {
                let flow = match protocol {
                    GrantProtocol::Vless => client.flow.clone(),
                    GrantProtocol::Hysteria2 | GrantProtocol::AnyTls => None,
                };
                actual_by_email.get(&client.email) != Some(&(client.uuid.clone(), flow))
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            continue;
        }

        let clients = missing
            .into_iter()
            .map(|client| {
                let (json, addition) = grant_addition(protocol, &inbound.tag, client);
                additions.push(addition);
                json
            })
            .collect::<Vec<_>>();
        xray_inbound["settings"]["clients"] = serde_json::Value::Array(clients);
        add_inbounds.push(xray_inbound);
    }

    if add_inbounds.is_empty() {
        return Ok(());
    }

    backend.add_users(state_dir, api_port, &additions, &add_inbounds)?;
    Ok(())
}

fn grant_addition(
    protocol: GrantProtocol,
    tag: &str,
    client: &GrantClient,
) -> (serde_json::Value, GrantAddition) {
    let mut object = serde_json::Map::new();
    object.insert("email".to_owned(), serde_json::json!(client.email));
    object.insert("level".to_owned(), serde_json::json!(client.level));
    let account = match protocol {
        GrantProtocol::Vless => {
            object.insert("id".to_owned(), serde_json::json!(client.uuid));
            if let Some(flow) = &client.flow {
                object.insert("flow".to_owned(), serde_json::json!(flow));
            }
            GrantAccount::Vless {
                id: client.uuid.clone(),
                flow: client.flow.clone(),
            }
        }
        GrantProtocol::Hysteria2 => {
            object.insert("auth".to_owned(), serde_json::json!(client.uuid));
            GrantAccount::Hysteria2 {
                auth: client.uuid.clone(),
            }
        }
        GrantProtocol::AnyTls => {
            object.insert("password".to_owned(), serde_json::json!(client.uuid));
            GrantAccount::AnyTls {
                password: client.uuid.clone(),
            }
        }
    };
    (
        serde_json::Value::Object(object),
        GrantAddition {
            tag: tag.to_owned(),
            email: client.email.clone(),
            level: client.level,
            account,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantProtocol {
    Vless,
    Hysteria2,
    AnyTls,
}

fn grant_protocol(inbound: &serde_json::Value, tag: &str) -> Result<GrantProtocol, String> {
    match inbound.get("protocol").and_then(serde_json::Value::as_str) {
        Some("vless") => Ok(GrantProtocol::Vless),
        Some("hysteria") => Ok(GrantProtocol::Hysteria2),
        Some("anytls") => Ok(GrantProtocol::AnyTls),
        Some(protocol) => Err(format!(
            "xray inbound {tag} protocol {protocol} does not support dynamic grants"
        )),
        None => Err(format!("xray inbound {tag} is missing protocol")),
    }
}

fn read_observed_grants(
    api_port: u16,
    desired: &[GrantInbound],
) -> Result<Vec<ObservedInbound>, String> {
    let mut observed = desired
        .iter()
        .map(|inbound| {
            Ok(ObservedInbound {
                tag: inbound.tag.clone(),
                clients: read_users(api_port, &inbound.tag)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    for inbound in &mut observed {
        inbound.clients.sort_by(|left, right| {
            left.email
                .cmp(&right.email)
                .then_with(|| left.uuid.cmp(&right.uuid))
                .then_with(|| left.flow.cmp(&right.flow))
        });
    }
    observed.sort_by(|left, right| left.tag.cmp(&right.tag));
    Ok(observed)
}

fn read_users(api_port: u16, tag: &str) -> Result<Vec<ObservedClient>, String> {
    Ok(xray_grpc::inbound_users(api_port, tag)?
        .into_iter()
        .map(|user| ObservedClient {
            email: user.email,
            // One field for two protocols: a VLESS id and a Hysteria 2 auth string are both "the
            // secret this account is known by", and drift is judged the same way for either.
            uuid: user.credential,
            flow: user.flow,
        })
        .collect())
}

fn xray_inbound_by_tag(value: &serde_json::Value, tag: &str) -> Result<serde_json::Value, String> {
    value
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .and_then(|inbounds| {
            inbounds
                .iter()
                .find(|inbound| inbound.get("tag").and_then(serde_json::Value::as_str) == Some(tag))
        })
        .cloned()
        .ok_or_else(|| format!("xray config does not contain inbound tag {tag}"))
}

fn xray_api_port(content: &str) -> Option<u16> {
    let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    value
        .get("inbounds")?
        .as_array()?
        .iter()
        .find(|inbound| inbound.get("tag").and_then(serde_json::Value::as_str) == Some("api"))?
        .get("port")?
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
}

fn read_xray_usage_counters(api_port: u16) -> Result<Vec<UsageCounter>, String> {
    Ok(usage_counters_from_stats(&xray_grpc::query_stats(
        api_port, "user>>>",
    )?))
}

fn usage_counters_from_stats(stats: &[xray_grpc::XrayStat]) -> Vec<UsageCounter> {
    let mut counters = BTreeMap::<String, (u64, u64)>::new();

    for stat in stats {
        // A negative counter is not a number this can bill; xray never emits one, and taking it
        // as a huge unsigned value would be the worst possible reading of it.
        let Ok(value) = u64::try_from(stat.value) else {
            continue;
        };
        let Some((label, direction)) = parse_xray_stat_name(&stat.name) else {
            continue;
        };
        let entry = counters.entry(label.to_owned()).or_default();
        match direction {
            "uplink" => entry.0 = value,
            "downlink" => entry.1 = value,
            _ => {}
        }
    }

    counters
        .into_iter()
        .map(|(label, (uplink_bytes, downlink_bytes))| UsageCounter {
            label,
            uplink_bytes,
            downlink_bytes,
        })
        .collect()
}

fn parse_xray_stat_name(name: &str) -> Option<(&str, &str)> {
    let parts = name.split(">>>").collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "user" || parts[2] != "traffic" {
        return None;
    }
    let direction = parts[3];
    matches!(direction, "uplink" | "downlink").then_some((parts[1], direction))
}

/// When the xray behind those counters came up. The control plane needs it to tell "the counter
/// went backwards because the process restarted" from "the counter went backwards because
/// something is wrong".
///
/// The oldest process is selected, not the first PID `pgrep` prints. Several processes on this
/// machine are named `xray`: the server this agent started with nohup, the `xray api statsquery`
/// this round invokes, and one short-lived `xray -config` child per chain each time the e2e
/// prober runs. `pgrep -x xray | head -n 1` returned whichever held the lowest PID, so an ingress
/// serving for hours reported a start time two seconds old every few minutes, and the control
/// plane recorded each of those as a restart. The serving instance is the long-lived one by
/// construction: probe children last seconds and api calls last milliseconds.
#[derive(Debug, Clone)]
struct XrayProcessIdentity {
    started_at_unix_secs: i64,
    epoch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRef {
    pid: libc::pid_t,
    start_ticks: u64,
}

const XRAY_TERM_GRACE: Duration = Duration::from_millis(250);
const XRAY_KILL_GRACE: Duration = Duration::from_millis(250);
const PROCESS_EXIT_POLL: Duration = Duration::from_millis(10);

/// Every live process whose kernel name is exactly `name`.
///
/// `pgrep` is deliberately not used here. It includes zombies, and Xray processes orphaned by
/// the nohup pipeline can remain zombies after all sockets and memory have already gone away. A
/// restart that waits for `pgrep` to become empty therefore waits for the parent to reap a process,
/// not for the old listener to release its port.
fn live_processes_named(name: &str) -> Vec<ProcessRef> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
            let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            if comm.trim() != name {
                return None;
            }
            process_ref(pid)
        })
        .collect()
}

fn process_ref(pid: libc::pid_t) -> Option<ProcessRef> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (state, start_ticks) = parse_proc_stat(&stat).ok()?;
    (state != 'Z').then_some(ProcessRef { pid, start_ticks })
}

fn process_is_live(process: ProcessRef) -> bool {
    process_ref(process.pid).is_some_and(|current| current.start_ticks == process.start_ticks)
}

fn xray_running() -> bool {
    !live_processes_named("xray").is_empty()
}

fn signal_processes(processes: &[ProcessRef], signal: libc::c_int) -> Result<(), String> {
    for process in processes {
        // Do not signal a PID that has been recycled since the snapshot. The second check cannot
        // make kill and /proc atomic, but it narrows that race to the few instructions between
        // them; starttime is the kernel identity available without retaining a pidfd.
        if !process_is_live(*process) {
            continue;
        }
        // SAFETY: `pid` came from /proc, is positive, and refers to one process rather than a
        // process group. ESRCH only means it exited between the identity check and this call.
        if unsafe { libc::kill(process.pid, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!(
                    "signal xray pid {} with {signal}: {error}",
                    process.pid
                ));
            }
        }
    }
    Ok(())
}

fn wait_for_process_exit(processes: &[ProcessRef], timeout: Duration) -> Vec<ProcessRef> {
    let deadline = Instant::now() + timeout;
    loop {
        let survivors = processes
            .iter()
            .copied()
            .filter(|process| process_is_live(*process))
            .collect::<Vec<_>>();
        if survivors.is_empty() || Instant::now() >= deadline {
            return survivors;
        }
        thread::sleep(PROCESS_EXIT_POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Stop the old Xray without allowing graceful teardown to become prolonged downtime.
fn terminate_processes(
    processes: &[ProcessRef],
    term_grace: Duration,
    kill_grace: Duration,
) -> Result<(), String> {
    if processes.is_empty() {
        return Ok(());
    }
    signal_processes(processes, libc::SIGTERM)?;
    let survivors = wait_for_process_exit(processes, term_grace);
    if survivors.is_empty() {
        return Ok(());
    }
    signal_processes(&survivors, libc::SIGKILL)?;
    let survivors = wait_for_process_exit(&survivors, kill_grace);
    if survivors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "xray 进程在 SIGKILL 后仍未退出：{}",
            survivors
                .iter()
                .map(|process| process.pid.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

fn terminate_xray() -> Result<(), String> {
    terminate_processes(
        &live_processes_named("xray"),
        XRAY_TERM_GRACE,
        XRAY_KILL_GRACE,
    )
}

fn xray_process_identity() -> Result<XrayProcessIdentity, String> {
    // The serving instance is the oldest live Xray by construction. Excluding zombies before
    // choosing the oldest matters: the leftover zombie is normally older than its replacement.
    let process = live_processes_named("xray")
        .into_iter()
        .min_by_key(|process| process.start_ticks)
        .ok_or("xray process is not running")?;
    let btime = fs::read_to_string("/proc/stat")
        .map_err(|error| format!("failed to read /proc/stat: {error}"))?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == "btime")
                .then(|| value.trim().parse::<u64>().ok())
                .flatten()
        })
        .ok_or("failed to read btime from /proc/stat")?;
    let clock_ticks = run_command("getconf", &["CLK_TCK"])
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100);
    let started = btime
        .checked_add(process.start_ticks / clock_ticks)
        .ok_or("xray start time overflow")?;
    Ok(XrayProcessIdentity {
        started_at_unix_secs: i64::try_from(started)
            .map_err(|_| "xray start time is out of range".to_owned())?,
        epoch: format!("{btime}:{}", process.start_ticks),
    })
}

fn parse_proc_stat(stat: &str) -> Result<(char, u64), String> {
    let after_comm = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .ok_or("proc stat is missing process comm")?;
    let fields = after_comm.split_whitespace().collect::<Vec<_>>();
    let state = fields
        .first()
        .and_then(|value| value.chars().next())
        .ok_or("proc stat is missing process state")?;
    let start_ticks = fields
        .get(19)
        .ok_or("proc stat is missing starttime")?
        .parse::<u64>()
        .map_err(|error| format!("failed to parse proc stat starttime: {error}"))?;
    Ok((state, start_ticks))
}

fn current_unix_secs() -> Result<i64, String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    i64::try_from(secs).map_err(|_| "current unix time is out of range".to_owned())
}

/// The order xray searches for asset files, mirroring `GetAssetLocation` in
/// `common/platform`: environment variable, then the executable's own directory, then
/// three fixed fallback directories, taking the first where the file exists.
///
/// Reproduced rather than hardcoding `/usr/local/share/xray`. The install script places
/// the files there, but an operator can change the location with `XRAY_LOCATION_ASSET`,
/// and the report has to name the copy xray reads rather than an assumed one.
fn xray_asset_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(value) = env::var("XRAY_LOCATION_ASSET") {
        if !value.trim().is_empty() {
            dirs.push(PathBuf::from(value));
        }
    }
    if let Ok(exe) = which_xray() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs.push(PathBuf::from("/usr/local/share/xray"));
    dirs.push(PathBuf::from("/usr/share/xray"));
    dirs.push(PathBuf::from("/opt/share/xray"));
    dirs
}

fn which_xray() -> Result<PathBuf, String> {
    let out = run_shell("command -v xray")?;
    let path = out.trim();
    if path.is_empty() {
        return Err("xray 不在 PATH 里".to_owned());
    }
    Ok(PathBuf::from(path))
}

/// Rule-database reconcile: size, mtime, and sha256 of the two .dat files.
///
/// An unreadable file is neither an error nor retried, because this is an observation
/// rather than an action. The agent loop must not stall over a missing .dat file; the
/// control plane receives `None` and decides what it means.
fn observe_geodata() -> Option<GeodataObservation> {
    let dir = xray_asset_dirs()
        .into_iter()
        .find(|dir| dir.join("geoip.dat").exists() || dir.join("geosite.dat").exists())?;
    Some(GeodataObservation {
        geoip: geodata_file_state(&dir.join("geoip.dat")),
        geosite: geodata_file_state(&dir.join("geosite.dat")),
        asset_dir: dir.to_string_lossy().into_owned(),
    })
}

fn geodata_file_state(path: &Path) -> Option<GeodataFileState> {
    let meta = fs::metadata(path).ok()?;
    // sha256 reads the whole file, and geosite.dat is around ten megabytes with geoip.dat
    // around twenty. This runs only on the observation round, since the 15-second default
    // belongs to APPLY, so the cost is acceptable. Reporting only mtime would make it
    // impossible to determine whether two machines hold the same copy.
    let sha256 = file_sha256_hex(path).ok()?;
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(GeodataFileState {
        sha256,
        bytes: meta.len(),
        modified_at,
    })
}

fn file_sha256_hex(path: &Path) -> Result<String, String> {
    let content = fs::read(path).map_err(|error| error.to_string())?;
    Ok(sha256_hex(&content))
}

/// The artifacts hold WireGuard private keys, REALITY private keys, and user UUIDs, so
/// on disk they must be readable by root alone. The shared writer creates a 0600
/// temporary file, fsyncs it, and atomically replaces the destination; no reader can
/// observe a permissive, truncated, or partially-written artifact.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    fsutil::atomic_write_private(path, contents.as_bytes())
}

/// The same for the state directory: every artifact inside it is a secret.
fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

// ss comes from iproute2, the same package as the ip the agent already depends on;
// netstat belongs to net-tools, which many slim images lack. Prefer ss, and only
// declare the probe impossible when neither exists.
fn tcp_port_listening(port: u16) -> bool {
    xray_port_listening(port, XrayListenProtocol::Tcp) == Some(true)
}

fn xray_port_listening(port: u16, protocol: XrayListenProtocol) -> Option<bool> {
    let flag = match protocol {
        XrayListenProtocol::Tcp => "t",
        XrayListenProtocol::Udp => "u",
    };
    let output = run_shell(&format!(
        "if command -v ss >/dev/null 2>&1; then \
             ss -ln{flag} 2>/dev/null | grep -q ':{port} ' && echo yes || echo no; \
         elif command -v netstat >/dev/null 2>&1; then \
             netstat -ln{flag} 2>/dev/null | grep -q ':{port} ' && echo yes || echo no; \
         else echo unknown; \
         fi"
    ))
    .ok()?;
    match output.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn converge_artifact(
    state_dir: &Path,
    present_name: &str,
    disabled_name: &str,
    desired: &DesiredArtifact,
) -> Result<AppliedArtifactState, String> {
    match desired {
        DesiredArtifact::Present { content, .. } => {
            let path = state_dir.join(present_name);
            write_private(&path, content)?;
            let _ = fs::remove_file(state_dir.join(disabled_name));
            Ok(AppliedArtifactState::Present {
                sha256: file_sha256_hex(&path)?,
            })
        }
        DesiredArtifact::Disabled { reason } => {
            let _ = fs::remove_file(state_dir.join(present_name));
            fs::write(state_dir.join(disabled_name), reason).map_err(|error| error.to_string())?;
            Ok(AppliedArtifactState::Disabled)
        }
        DesiredArtifact::Unmanaged { .. } => Ok(AppliedArtifactState::Unmanaged),
    }
}

fn converge_grants(
    state_dir: &Path,
    desired: &DesiredGrants,
) -> Result<AppliedGrantsState, String> {
    match desired {
        DesiredGrants::Present { inbounds } => {
            let text = serde_json::to_string_pretty(inbounds).map_err(|error| error.to_string())?;
            write_private(&state_dir.join("grants.json"), &text)?;
            Ok(AppliedGrantsState::Present {
                inbounds: observed_inbounds(inbounds),
            })
        }
        DesiredGrants::Disabled { .. } => {
            let _ = fs::remove_file(state_dir.join("grants.json"));
            Ok(AppliedGrantsState::Disabled)
        }
        DesiredGrants::Unmanaged { .. } => Ok(AppliedGrantsState::Unmanaged),
    }
}

fn observed_inbounds(inbounds: &[GrantInbound]) -> Vec<ObservedInbound> {
    inbounds
        .iter()
        .map(|inbound| ObservedInbound {
            tag: inbound.tag.clone(),
            clients: inbound
                .clients
                .iter()
                .map(|client| ObservedClient {
                    email: client.email.clone(),
                    uuid: client.uuid.clone(),
                    flow: client.flow.clone(),
                })
                .collect(),
        })
        .collect()
}

fn read_applied_state(state_dir: &Path) -> Result<ReportedNodeState, String> {
    let path = state_dir.join("applied-state.json");
    if !path.exists() {
        return Ok(unknown_state());
    }
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn write_applied_state(state_dir: &Path, state: &ReportedNodeState) -> Result<(), String> {
    create_private_dir(state_dir)?;
    let text = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    write_private(&state_dir.join("applied-state.json"), &text)
}

fn unknown_state() -> ReportedNodeState {
    ReportedNodeState {
        phantun: AppliedArtifactState::Unknown,
        wireguard: AppliedArtifactState::Unknown,
        xray: AppliedArtifactState::Unknown,
        hy2_port_hop: AppliedArtifactState::Unknown,
        grants: AppliedGrantsState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_policy_prefers_specific_headers_and_falls_back_to_the_legacy_scalar() {
        let specific = crate::http::parse_http_response(
            b"HTTP/1.1 204 No Content\r\nX-Brocade-Log-Max-MiB: 64\r\nX-Brocade-Agent-Journal-Max-MiB: 96\r\nX-Brocade-Xray-Log-Max-MiB: 80\r\nX-Brocade-Phantun-Log-Max-MiB: 72\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            super::log_policy_from_response(&specific).unwrap(),
            Some(crate::logcap::LogPolicy {
                agent_journal_mib: 96,
                xray_mib: 80,
                phantun_mib: 72,
            })
        );

        let legacy = crate::http::parse_http_response(
            b"HTTP/1.1 204 No Content\r\nX-Brocade-Log-Max-MiB: 64\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(
            super::log_policy_from_response(&legacy).unwrap(),
            Some(crate::logcap::LogPolicy {
                agent_journal_mib: 64,
                xray_mib: 64,
                phantun_mib: 64,
            })
        );
    }

    // By default a panic on a thread terminates only that thread while the process
    // continues, so the control plane observes a healthy machine. This test verifies
    // that `each_round` catches the panic.
    #[test]
    fn a_panicking_round_does_not_kill_the_thread() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ROUNDS: AtomicUsize = AtomicUsize::new(0);

        let worker = std::thread::spawn(|| {
            for i in 0..3 {
                super::each_round("test", || {
                    ROUNDS.fetch_add(1, Ordering::SeqCst);
                    if i == 1 {
                        panic!("这一轮故意炸");
                    }
                });
            }
        });

        // A third round ran after the panicking one, so the thread survived.
        assert!(worker.join().is_ok(), "线程被 panic 带走了");
        assert_eq!(ROUNDS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_periodic_tick_does_not_add_the_round_duration() {
        let started = std::time::Instant::now();
        let interval = std::time::Duration::from_secs(5);

        assert_eq!(
            super::next_periodic_tick(
                started,
                interval,
                started + std::time::Duration::from_secs(2),
            ),
            started + interval,
        );
    }

    #[test]
    fn an_overdue_periodic_tick_skips_catch_up_work() {
        let started = std::time::Instant::now();
        let interval = std::time::Duration::from_secs(5);
        let finished = started + std::time::Duration::from_secs(7);

        assert_eq!(
            super::next_periodic_tick(started, interval, finished),
            started + std::time::Duration::from_secs(10),
        );
    }

    #[test]
    fn a_busy_ping_sender_keeps_only_the_latest_pending_report() {
        let reports = super::LatestPingProbeReport::default();
        let report = |probed_at_unix_secs| brocade_deployment::protocol::PingProbeReportRequest {
            probed_at_unix_secs,
            samples: Vec::new(),
        };

        reports.publish(report(1));
        reports.publish(report(2));
        reports.publish(report(3));

        let (latest, dropped) = reports.take();
        assert_eq!(latest.probed_at_unix_secs, 3);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn a_drained_spool_replaces_a_stale_runtime_backlog_snapshot() {
        let dir = test_state_dir("runtime-backlog");
        super::spool_push(
            super::USAGE_SPOOL,
            &dir,
            &serde_json::json!({ "sequence": 1 }),
        )
        .unwrap();

        let reports = super::RuntimeReports::default();
        reports.publish_current(&dir).unwrap();
        assert_eq!(reports.reports.try_take().unwrap().0.spool.usage, 1);

        fs::write(dir.join(super::USAGE_SPOOL.file), b"").unwrap();
        reports.publish_if_spool_changed(&dir).unwrap();
        assert_eq!(reports.reports.try_take().unwrap().0.spool.usage, 0);
        reports.publish_if_spool_changed(&dir).unwrap();
        assert!(reports.reports.try_take().is_none());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_ping_settings_refresh_replaces_the_cached_snapshot() {
        let cache = super::PingProbeSettingsCache::default();
        let settings = |interval_secs| brocade_deployment::protocol::PingProbeSettings {
            targets: Vec::new(),
            interval_secs,
            timeout_ms: 420,
        };

        cache.publish(settings(60));
        let previous = cache.wait_for_initial();
        cache.publish(settings(5));

        let updated = cache
            .wait_for_change_until(
                &previous,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .expect("changed settings should wake the sampler");
        assert_eq!(updated.interval_secs, 5);
    }

    #[test]
    fn a_failed_local_reconcile_is_recorded_before_returning() {
        let dir = test_state_dir("reconcile-failure-recorded");
        let error = super::reconcile_local(&dir, super::Drift::Always).unwrap_err();

        let report: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("local-reconcile.json")).unwrap())
                .unwrap();
        assert_eq!(report["actions"], serde_json::json!([]));
        assert_eq!(report["error"], error);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn definite_workload_breakage_wins_over_an_unknown_probe() {
        let status = super::classify_workload_runtime(
            true,
            None,
            [
                (
                    "in:a :443 坏了".to_owned(),
                    "in:a :443 查不了".to_owned(),
                    None,
                ),
                (
                    "in:b :8443 坏了".to_owned(),
                    "in:b :8443 查不了".to_owned(),
                    Some(false),
                ),
            ],
        );
        assert!(matches!(status, super::WorkloadRuntime::Broken(_)));
    }

    #[test]
    fn unavailable_port_tools_do_not_trigger_a_restart() {
        let status = super::classify_workload_runtime(
            true,
            None,
            [(
                "in:a :443 坏了".to_owned(),
                "in:a :443 查不了".to_owned(),
                None,
            )],
        );
        assert!(matches!(status, super::WorkloadRuntime::Unknown(_)));
    }

    use std::{
        collections::BTreeMap,
        env, fs,
        io::{ErrorKind, Read, Write},
        net::{TcpListener, TcpStream},
        os::unix::process::ExitStatusExt,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::{
        converge_artifact, grants_drifted_in, observed_inbounds, route_headers, sha256_hex,
        usage_counters_from_stats,
    };
    use crate::http::{
        decode_chunked_body, parse_http_response, HttpClient, MAX_HTTP_RESPONSE_BYTES,
    };
    use crate::options::{ApplyMode, Options};
    use crate::probe::{xray_listen_ports, XrayListenProtocol};
    use crate::spool::{spool_push, spool_read, Spool, OBSERVATION_SPOOL, USAGE_SPOOL};
    use crate::wg::{judge_wg_peers, parse_wg_dump, parse_wireguard_conf_peers, PeerState};
    use crate::xray_grpc::XrayStat;
    use brocade_deployment::plan::{
        AppliedArtifactState, AppliedGrantsState, DesiredArtifact, GrantClient, GrantInbound,
        ObservedClient,
    };
    use brocade_deployment::protocol::RouteIpReport;
    use brocade_deployment::protocol::{LinkProbeStatus, ProbeTransport, ReportedNodeState};

    fn want(emails: &[(&str, &str)]) -> Vec<GrantClient> {
        emails
            .iter()
            .map(|(email, uuid)| GrantClient {
                email: (*email).to_owned(),
                uuid: (*uuid).to_owned(),
                flow: None,
                level: 0,
            })
            .collect()
    }

    fn live(emails: &[(&str, &str)]) -> Vec<ObservedClient> {
        emails
            .iter()
            .map(|(email, uuid)| ObservedClient {
                email: (*email).to_owned(),
                uuid: (*uuid).to_owned(),
                flow: None,
            })
            .collect()
    }

    // Local reconcile's trigger has to compare in both directions. Checking only for
    // missing entries leaves a failed `rmu` uncorrected: the account should have been
    // removed but stays connected past its quota, while the control plane records the
    // revocation as complete.
    #[test]
    fn grant_drift_is_detected_in_both_directions() {
        let a = ("alice@t#i", "uuid-a");
        let b = ("bob@t#i", "uuid-b");

        // Identical sets: no action
        assert!(!grants_drifted_in(&want(&[a, b]), &live(&[a, b])));
        // Order is not significant
        assert!(!grants_drifted_in(&want(&[a, b]), &live(&[b, a])));

        // One missing: the subscription is rejected
        assert!(grants_drifted_in(&want(&[a, b]), &live(&[a])));
        // One extra: an account that should have been removed, which a
        // missing-only check does not detect
        assert!(grants_drifted_in(&want(&[a]), &live(&[a, b])));
        // Revoked down to none, yet the runtime still holds people
        assert!(grants_drifted_in(&[], &live(&[a])));
        // Desired empty, runtime empty: agrees
        assert!(!grants_drifted_in(&[], &live(&[])));

        // New UUID: same email but not the same version of the credential, so
        // it must be pushed again
        assert!(grants_drifted_in(
            &want(&[("alice@t#i", "uuid-new")]),
            &live(&[a])
        ));

        // Flow is part of a VLESS account. Keeping the same label and UUID with yesterday's flow
        // is still drift: without this comparison a runtime-only mutation survives every
        // 15-second local reconcile even though a control-plane deployment would reject it.
        let mut vision = want(&[a]);
        vision[0].flow = Some("xtls-rprx-vision".to_owned());
        assert!(grants_drifted_in(&vision, &live(&[a])));
        let mut live_vision = live(&[a]);
        live_vision[0].flow = Some("xtls-rprx-vision".to_owned());
        assert!(!grants_drifted_in(&vision, &live_vision));
    }

    /// A newly enrolled machine belongs to no released plan, so `/agent/v1/desired`
    /// answers 204 and its state directory stays empty. That must not be classified as
    /// a broken installation. It previously was, so the install script's self-check
    /// failed on *every* first install, producing 90 seconds of waiting, a failed
    /// report and a journal dump on a machine that was functioning and waiting for
    /// work.
    ///
    /// The other two directions are equally important. An empty state directory with no
    /// answer from the control plane, caused by an admin port instead of the agent port
    /// or by a revoked token, is a genuine failed installation. The marker is also
    /// written on every idle round, including on a machine that has been serving for
    /// months, so it must never by itself suppress a failure.
    #[test]
    fn a_node_nobody_deployed_to_is_pending_not_broken() {
        let dir = test_state_dir("health-pending");

        // Nothing on disk and no answer received: a failed installation
        assert!(super::health(&dir).is_err());

        // The control plane answered and has no work for this machine
        super::mark_no_desired(&dir);
        assert!(super::health(&dir).is_ok());

        // A release arrives: the marker is removed and the artifacts decide again
        super::clear_no_desired(&dir);
        assert!(super::health(&dir).is_err());

        // Idle rounds on a long-converged machine write the marker too, so
        // anything at all on disk means the real checks apply
        super::mark_no_desired(&dir);
        fs::write(dir.join("xray.disabled"), "").unwrap();
        assert!(!super::never_converged(&dir));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn secret_artifacts_land_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_state_dir("secret-artifact-permissions");
        let state = converge_artifact(
            &dir,
            "wireguard.conf",
            "wireguard.disabled",
            &DesiredArtifact::Present {
                content: "[Interface]\nPrivateKey = secret\n".to_owned(),
                sha256: "ignored".to_owned(),
            },
        )
        .unwrap();
        assert!(matches!(state, AppliedArtifactState::Present { .. }));

        let mode = fs::metadata(dir.join("wireguard.conf"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "wg 配置里有私钥，不能让本机其他用户读到");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_hysteria_addition_carries_auth_and_never_a_vless_id() {
        let dir = test_state_dir("hysteria2-grants-adu");
        let client = GrantClient {
            email: "alice@platform#hy2".to_owned(),
            uuid: "plain-hysteria-auth".to_owned(),
            // A stale VLESS-only field must not cross the account-type boundary.
            flow: Some("xtls-rprx-vision".to_owned()),
            level: 0,
        };
        let (client_json, addition) =
            super::grant_addition(super::GrantProtocol::Hysteria2, "in:app/hy2", &client);
        assert_eq!(client_json["auth"], "plain-hysteria-auth");
        assert!(client_json.get("id").is_none());
        assert!(client_json.get("flow").is_none());
        assert!(matches!(
            addition.account,
            super::GrantAccount::Hysteria2 { ref auth } if auth == "plain-hysteria-auth"
        ));

        let _ = fs::remove_dir_all(dir);
    }

    /// The default path must leave no credential file behind.
    ///
    /// `grants.adu.json` is not a stored record: it holds only the accounts added in one round,
    /// so it substitutes for neither the desired set that local reconvergence reads
    /// (`grants.desired.json`) nor the compiled grants artifact. Under gRPC nothing reads it,
    /// and an unread plaintext copy of credentials only adds exposure.
    #[test]
    fn the_grpc_backend_writes_no_credentials_to_disk() {
        let dir = test_state_dir("grants-no-spill");
        let inbound = serde_json::json!({
            "tag": "in:app/hy2",
            "protocol": "hysteria",
            "settings": { "clients": [{ "email": "u@t#i", "auth": "plain-hysteria-auth" }] }
        });
        let addition = super::GrantAddition {
            tag: "in:app/hy2".to_owned(),
            email: "u@t#i".to_owned(),
            level: 0,
            account: super::GrantAccount::Hysteria2 {
                auth: "plain-hysteria-auth".to_owned(),
            },
        };

        // No xray is listening, so the call fails. The assertion is about what it wrote to
        // disk before failing.
        let _ = super::GrantWriteBackend::add_users(
            &super::GrpcGrantWriteBackend,
            &dir,
            1,
            &[addition],
            &[inbound],
        );

        assert!(
            !dir.join("grants.adu.json").exists(),
            "gRPC 那条路不该往盘上落任何凭据"
        );
        let spilled = fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        fs::read_to_string(entry.path())
                            .map(|text| text.contains("plain-hysteria-auth"))
                            .unwrap_or(false)
                    })
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(spilled.is_empty(), "凭据落到了这些文件里：{spilled:?}");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[allow(deprecated)]
    fn grant_backend_defaults_to_grpc_and_cli_refuses_hysteria_before_writing() {
        assert_eq!(
            super::grant_write_backend_choice(None).unwrap(),
            super::GrantWriteBackendChoice::Grpc
        );
        assert_eq!(
            super::grant_write_backend_choice(Some("grpc")).unwrap(),
            super::GrantWriteBackendChoice::Grpc
        );
        assert_eq!(
            super::grant_write_backend_choice(Some("cli")).unwrap(),
            super::GrantWriteBackendChoice::Cli
        );
        assert!(super::grant_write_backend_choice(Some("automatic-fallback")).is_err());
        assert!(super::GrantWriteBackend::validate_protocol(
            &super::XrayCliGrantWriteBackend,
            super::GrantProtocol::Hysteria2,
        )
        .is_err());
    }

    #[test]
    fn desired_grants_on_disk_is_the_full_set_not_the_add_batch() {
        let dir = test_state_dir("grants-desired");
        // Never written means None: the grants.adu.json an older agent left is an
        // incremental batch, and taking it as the full set would treat every user
        // outside that batch as one that should not exist.
        fs::write(dir.join("grants.adu.json"), "{\"inbounds\":[]}").unwrap();
        assert_eq!(super::desired_grants_on_disk(&dir).unwrap(), None);

        fs::write(
            dir.join("grants.desired.json"),
            "[{\"tag\":\"in:a\",\"clients\":[{\"email\":\"u@t#a\",\"uuid\":\"x\",\"flow\":null,\"level\":0}]}]",
        )
        .unwrap();
        let want = super::desired_grants_on_disk(&dir).unwrap().unwrap();
        assert_eq!(want.len(), 1);
        assert_eq!(want[0].tag, "in:a");
        assert_eq!(want[0].clients[0].email, "u@t#a");
        let _ = fs::remove_dir_all(dir);
    }

    /// For every real MTU in 1000..=1500 the bisection must return exactly that
    /// number.
    ///
    /// Exhaustive rather than sampled: off-by-one is the most common error in this
    /// kind of search, and its consequence is a network-wide MTU one byte short —
    /// large packets dropped, tunnel up, ping fine, pages not loading, the hardest
    /// class of symptom to chase.
    #[test]
    fn the_binary_search_lands_exactly_on_every_reachable_mtu() {
        for truth in crate::probe::PROBE_MTU_MIN..=crate::probe::PROBE_MTU_MAX {
            let (status, found) = crate::probe::probe_path_mtu_with(|| true, |mtu| mtu <= truth);
            assert_eq!(status, LinkProbeStatus::Ok, "真实 MTU {truth}");
            assert_eq!(found, Some(truth), "真实 MTU {truth} 探成了 {found:?}");
        }
    }

    /// The two undeterminable outcomes have to stay distinct from a measured small
    /// value. Recording blocked ICMP as a small number would lead an operator to
    /// follow the suggestion and lower the MTU to the floor on a working link.
    #[test]
    fn unreachable_and_blocked_carry_no_mtu() {
        let (status, found) = crate::probe::probe_path_mtu_with(|| false, |_| true);
        assert_eq!(status, LinkProbeStatus::Unreachable);
        assert_eq!(found, None, "不可达不该带出一个数来拉低建议值");

        // The ping is answered, but no packet with DF set gets through
        let (status, found) = crate::probe::probe_path_mtu_with(|| true, |_| false);
        assert_eq!(status, LinkProbeStatus::Blocked);
        assert_eq!(found, None);
    }

    /// The tests above use a stub probe function. This one uses a real ICMP socket to
    /// verify that the packet is assembled correctly, the echo is recognized, and
    /// EMSGSIZE is interpreted correctly. Loopback's MTU is 65536, so every probe in
    /// the search interval passes and the result is the ceiling.
    ///
    /// Environments that cannot open an ICMP socket, lacking CAP_NET_RAW with
    /// `ping_group_range` disallowing it, skip the test rather than report a failure.
    #[test]
    fn real_icmp_against_loopback_reports_the_ceiling() {
        if super::icmp::Pinger::open("127.0.0.1").is_err() {
            eprintln!("跳过：这个环境开不了 ICMP socket");
            return;
        }
        // Loopback is v4-only, so the overhead is 60: 1500 - 60 = 1440
        assert_eq!(
            crate::probe::probe_path_mtu("127.0.0.1", ProbeTransport::Udp),
            (
                LinkProbeStatus::Ok,
                Some(crate::probe::PROBE_MTU_MAX),
                Some(crate::probe::PROBE_MTU_MAX - 60)
            )
        );

        // TEST-NET-1, guaranteed unroutable by RFC 5737
        assert_eq!(
            crate::probe::probe_path_mtu("192.0.2.1", ProbeTransport::Udp),
            (LinkProbeStatus::Unreachable, None, None)
        );
    }

    /// Liveness is judged by the delta, not the absolute value.
    ///
    /// With no baseline, the first round has to return unknown rather than dead, so a
    /// newly restarted agent does not mark every link as failed. A counter that
    /// decreased means xray restarted, since the counters are cumulative and held in
    /// process memory, and that round has no usable delta either.
    #[test]
    fn hop_health_needs_a_baseline_and_survives_counter_resets() {
        let tag = "out:relay/c-relay>sg-01".to_owned();

        // First round: a baseline only, with nothing to report
        let first = crate::probe::judge_hops(BTreeMap::from([(tag.clone(), 1000)]), 100);
        assert!(first.is_none(), "第一轮没有上一轮可比，不该报");

        // Second round grew → alive
        let second = crate::probe::judge_hops(BTreeMap::from([(tag.clone(), 1600)]), 130).unwrap();
        assert_eq!(second.window_secs, 30);
        assert_eq!(second.hops[0].chain_id, "relay/c-relay");
        assert_eq!(second.hops[0].peer_node_id, "sg-01");
        assert!(second.hops[0].alive);
        assert_eq!(second.hops[0].downlink_bytes, 600);

        // Third round did not grow → dead (the observatory is running, so no delta
        // means this hop is down)
        let third = crate::probe::judge_hops(BTreeMap::from([(tag.clone(), 1600)]), 160).unwrap();
        assert!(!third.hops[0].alive);
        assert_eq!(third.hops[0].downlink_bytes, 0);

        // Fourth round's counter shrank = xray restarted; treat it as no data rather
        // than negative growth
        let fourth = crate::probe::judge_hops(BTreeMap::from([(tag, 40)]), 190).unwrap();
        assert!(!fourth.hops[0].alive);
        assert_eq!(fourth.hops[0].downlink_bytes, 0, "不能算成负数或回绕");
    }

    /// Forwarding outbounds only. `out:egress` always carries traffic (measuring no
    /// relay liveness) and `out:block` never does.
    #[test]
    fn hop_downlinks_ignore_egress_and_block() {
        let stat = |name: &str, value: i64| XrayStat {
            name: name.to_owned(),
            value,
        };
        let parsed = crate::probe::hop_downlinks_from_stats(&[
            stat(
                "outbound>>>out:relay/c-relay>sg-01>>>traffic>>>downlink",
                900,
            ),
            stat("outbound>>>out:relay/c-relay>sg-01>>>traffic>>>uplink", 120),
            stat("outbound>>>out:egress>>>traffic>>>downlink", 999_999),
            stat("outbound>>>out:block>>>traffic>>>downlink", 0),
        ]);
        assert_eq!(
            parsed.keys().collect::<Vec<_>>(),
            ["out:relay/c-relay>sg-01"]
        );
        assert_eq!(
            parsed["out:relay/c-relay>sg-01"], 900,
            "只取 downlink，不要 uplink"
        );
    }

    /// The suggestion is the path MTU minus wg's encapsulation overhead, and the
    /// overhead depends on whether the endpoint is dual-stack (see
    /// `icmp::Pinger::wireguard_overhead`). This pins down only the arithmetic; the
    /// two constants themselves are covered by icmp.rs's tests.
    #[test]
    fn suggested_wg_mtu_subtracts_the_wireguard_overhead() {
        // A 1500-byte Ethernet path with a v4-only endpoint → 1440
        assert_eq!(1500 - 60, 1440);
        // The same path with a dual-stack endpoint stays at 1420, which is
        // wg-quick's default
        assert_eq!(1500 - 80, 1420);
        // The ~1258 cross-border link described in deployment.md
        assert_eq!(1258 - 60, 1198);
    }

    #[test]
    fn wireguard_conf_mtu_is_read_back_from_the_config() {
        let dir = test_state_dir("wg-conf-mtu");
        let conf = dir.join("wireguard.conf");
        fs::write(
            &conf,
            "[Interface]\nPrivateKey = x\nAddress    = 10.66.0.1/32\nMTU        = 1200\n",
        )
        .unwrap();
        assert_eq!(crate::wg::wireguard_conf_mtu(&conf), Some(1200));

        fs::write(&conf, "[Interface]\nPrivateKey = x\n").unwrap();
        assert_eq!(crate::wg::wireguard_conf_mtu(&conf), None);
        let _ = fs::remove_dir_all(dir);
    }

    /// A rendered wireguard.conf (`format/ini.rs`), used by several tests below.
    const CONF: &str = "\
[Interface]
PrivateKey = SECRET=
Address    = 10.66.0.1/32
ListenPort = 51820
MTU        = 1200

[Peer]
# sg-01
PublicKey  = SG=
AllowedIPs = 10.66.0.2/32
Endpoint   = 127.0.0.1:29000
PersistentKeepalive = 25

[Peer]
# jp-01
PublicKey  = JP=
AllowedIPs = 10.66.0.3/32
";

    #[test]
    fn conf_peers_carry_the_name_endpoint_and_overlay_address() {
        let peers = parse_wireguard_conf_peers(CONF);

        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "sg-01");
        assert_eq!(peers[0].public_key, "SG=");
        assert_eq!(peers[0].endpoint.as_deref(), Some("127.0.0.1:29000"));
        assert_eq!(peers[0].overlay_ip.as_deref(), Some("10.66.0.2"));
        // A peer only the far side can dial has no Endpoint in the config, which must
        // not be read as a removed Endpoint
        assert_eq!(peers[1].name, "jp-01");
        assert_eq!(peers[1].endpoint, None);
        // The PrivateKey in [Interface] must not be parsed as a peer
        assert!(peers.iter().all(|peer| peer.public_key != "SECRET="));
    }

    /// `wg show wg0 dump`: the first line is the interface itself, then one line per
    /// peer.
    const DUMP: &str = "\
SECRET=\tPUB=\t51820\toff
SG=\t(none)\t127.0.0.1:29000\t10.66.0.2/32\t1754200000\t1024\t2048\t25
JP=\t(none)\t(none)\t10.66.0.3/32\t0\t0\t0\toff
";

    #[test]
    fn wg_dump_skips_the_interface_line_and_reads_none_markers() {
        let peers = parse_wg_dump(DUMP);

        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].public_key, "SG=");
        assert_eq!(peers[0].endpoint.as_deref(), Some("127.0.0.1:29000"));
        assert_eq!(peers[0].overlay_ip.as_deref(), Some("10.66.0.2"));
        assert_eq!(peers[0].latest_handshake, 1_754_200_000);
        assert_eq!(peers[0].persistent_keepalive, 25);
        // A peer that never handshook reports 0 and no Endpoint reports (none);
        // neither may be read as a measured value
        assert_eq!(peers[1].latest_handshake, 0);
        assert_eq!(peers[1].endpoint, None);
        assert_eq!(peers[1].persistent_keepalive, 0);
    }

    /// An old handshake is not a failure, so the state stops at Suspect and the ping
    /// decides.
    #[test]
    fn a_stale_handshake_is_only_suspect_until_the_ping_answers() {
        let conf = parse_wireguard_conf_peers(CONF);
        let now = 1_754_200_000_i64;

        let live = parse_wg_dump(DUMP);
        let checks = judge_wg_peers(&conf, &live, now + 10, &Default::default());
        assert_eq!(checks[0].state, PeerState::Fresh);
        assert_eq!(checks[0].handshake_age, Some(10));
        // The peer that never handshook is also only suspect: a newly created
        // interface has not handshook yet
        assert_eq!(checks[1].state, PeerState::Suspect);
        assert_eq!(checks[1].handshake_age, None);

        let checks = judge_wg_peers(
            &conf,
            &live,
            now + crate::wg::HANDSHAKE_STALE_SECS + 1,
            &Default::default(),
        );
        assert_eq!(checks[0].state, PeerState::Suspect);
    }

    /// Present in the config and absent from the runtime. `wg show` alone does not
    /// reveal it, because the corresponding line is missing.
    #[test]
    fn a_peer_missing_from_the_running_interface_is_down_without_a_ping() {
        let conf = parse_wireguard_conf_peers(CONF);
        let live = parse_wg_dump("SECRET=\tPUB=\t51820\toff\n");

        let checks = judge_wg_peers(&conf, &live, 1_754_200_000, &Default::default());

        assert_eq!(checks.len(), 2);
        assert!(checks
            .iter()
            .all(|check| matches!(check.state, PeerState::Down { .. })));
        // The overlay address must be recoverable from the config, or the report
        // cannot even say who it was
        assert_eq!(checks[0].overlay_ip.as_deref(), Some("10.66.0.2"));
    }

    /// The Endpoint roamed off phantun's loopback port back to a public UDP address
    /// while the handshake time still looks fresh.
    #[test]
    fn a_roamed_endpoint_is_caught_while_the_handshake_still_looks_fine() {
        let conf = parse_wireguard_conf_peers(CONF);
        let now = 1_754_200_000_i64;
        let roamed = "\
SECRET=\tPUB=\t51820\toff
SG=\t(none)\t198.51.100.133:51820\t10.66.0.2/32\t1754200000\t1024\t2048\t25
JP=\t(none)\t203.0.113.7:51820\t10.66.0.3/32\t1754200000\t1\t1\toff
";

        let checks = judge_wg_peers(&conf, &parse_wg_dump(roamed), now + 10, &Default::default());

        // The handshake is fresh; the handshake time alone reveals nothing
        assert_eq!(checks[0].state, PeerState::Fresh);
        assert_eq!(
            checks[0].endpoint_drift,
            Some((
                "127.0.0.1:29000".to_owned(),
                "198.51.100.133:51820".to_owned()
            ))
        );
        // It also has to be corrected immediately: packets bypass phantun and reach a
        // port whose inbound UDP is blocked, so the link stops carrying traffic
        assert!(crate::wg::drift_is_fatal(&checks[0]));

        // A learned endpoint on the peer with no configured Endpoint is not drift,
        // because that peer's endpoint is learned by design
        assert_eq!(checks[1].endpoint_drift, None);
    }

    /// The passive side drifting to a public address. With no configured Endpoint to
    /// compare against, the only signal is that the peer arrives through this machine's
    /// phantun server.
    ///
    /// After the drift, every packet reaches the far side's blocked UDP port, and
    /// roaming moves the far side's Endpoint as well, so each machine keeps the other on
    /// the unusable path. `endpoint_drift` does not cover this: it compares only peers
    /// with a configured Endpoint, and for these it is always None.
    #[test]
    fn a_passive_peer_that_roamed_to_a_public_address_is_caught() {
        let conf = parse_wireguard_conf_peers(CONF);
        let now = 1_754_200_000_i64;
        // JP has no configured Endpoint, because it dials this machine, yet at runtime
        // it carries a public address
        let dump = "\
SECRET=\tPUB=\t51820\toff
SG=\t(none)\t127.0.0.1:29000\t10.66.0.2/32\t1754200000\t1024\t2048\t25
JP=\t(none)\t203.0.113.7:51820\t10.66.0.3/32\t1754200000\t1\t1\toff
";
        let mut via = std::collections::BTreeSet::new();
        via.insert("jp-01".to_owned());

        // Without the knowledge that the peer arrives through phantun, nothing is
        // detected, which is how the incident presented
        let blind = judge_wg_peers(&conf, &parse_wg_dump(dump), now + 10, &Default::default());
        assert_eq!(blind[1].endpoint_drift, None);
        assert_eq!(blind[1].stray_endpoint, None);

        // With that set supplied by the artifact, it is detected
        let checks = judge_wg_peers(&conf, &parse_wg_dump(dump), now + 10, &via);
        assert_eq!(
            checks[1].stray_endpoint.as_deref(),
            Some("203.0.113.7:51820")
        );
        // The peer behind phantun is on loopback, which is not drift
        assert_eq!(checks[0].stray_endpoint, None);
    }

    #[test]
    fn only_loopback_counts_as_arriving_through_phantun() {
        assert!(crate::wg::endpoint_is_local("127.0.0.1:29000"));
        assert!(crate::wg::endpoint_is_local("[::1]:29000"));
        assert!(!crate::wg::endpoint_is_local("192.168.200.6:29000"));
        assert!(!crate::wg::endpoint_is_local("203.0.113.7:51820"));
    }

    /// The converse case: ordinary roaming on a working link is left in place. Correcting
    /// it would loop, because the agent rewrites the endpoint and the peer roams again,
    /// and the endpoint being rewritten is often the reason it roamed.
    ///
    /// A working link includes one that is idle but answers pings (`Alive`). Writing the
    /// test as not-Fresh fails here: since `HANDSHAKE_STALE_SECS` was reduced to 120,
    /// healthy idle links reach `Alive` routinely.
    #[test]
    fn a_healthy_public_peer_that_roamed_is_left_alone() {
        let conf = parse_wireguard_conf_peers(
            "[Peer]\n# sg-01\nPublicKey  = SG=\nAllowedIPs = 10.66.0.2/32\nEndpoint   = 198.51.100.133:51820\n",
        );
        let now = 1_754_200_000_i64;
        let live = parse_wg_dump(
            "SG=\t(none)\t[2001:db8::1]:51820\t10.66.0.2/32\t1754200000\t1\t1\toff\n",
        );

        // The handshake is still fresh
        let checks = judge_wg_peers(&conf, &live, now + 10, &Default::default());
        assert!(checks[0].endpoint_drift.is_some());
        assert!(!crate::wg::drift_is_fatal(&checks[0]));

        // Long idle but answering pings, which is also left in place
        let mut checks = judge_wg_peers(&conf, &live, now + 3600, &Default::default());
        assert_eq!(checks[0].state, PeerState::Suspect);
        checks[0].state = PeerState::Alive;
        assert!(!crate::wg::drift_is_fatal(&checks[0]));

        // Only a confirmed failure is corrected
        checks[0].state = PeerState::Down {
            detail: "ping 不通".to_owned(),
        };
        assert!(crate::wg::drift_is_fatal(&checks[0]));
    }

    /// wg prints IPv6 as `[addr]:port`, which need not match the config's text form.
    #[test]
    fn endpoints_compare_by_address_not_by_spelling() {
        assert!(crate::wg::endpoint_matches(
            "[2001:db8::1]:51820",
            "[2001:0db8:0000::1]:51820"
        ));
        assert!(crate::wg::endpoint_matches(
            "127.0.0.1:29000",
            "127.0.0.1:29000"
        ));
        assert!(!crate::wg::endpoint_matches(
            "127.0.0.1:29000",
            "127.0.0.1:29001"
        ));
        // Unparseable on both sides counts as equal: an unequal result triggers
        // `wg set endpoint`, and writing a string the agent cannot parse itself
        // repeats every round at best
        assert!(crate::wg::endpoint_matches("不是地址", "不是地址"));
        assert!(crate::wg::endpoint_matches("不是地址", "另一个不是地址"));
    }

    /// Unprobeable must never become unreachable.
    ///
    /// The watchdog responds to `Down` by rewriting Endpoints, restarting the interface,
    /// and finally deleting and rebuilding it. Treating a property of this machine, that
    /// it cannot open an ICMP socket, as a property of the link would therefore rebuild
    /// wg0 on a healthy machine every two minutes. This previously shelled out to
    /// `/bin/ping`, where `command_success` mapped both a missing ping binary and an
    /// unanswered ping to false, which fails on a minimal image and reports no cause.
    #[test]
    fn a_peer_we_could_not_probe_never_counts_as_down() {
        let conf = parse_wireguard_conf_peers(CONF);
        let live = parse_wg_dump(DUMP);
        let now = 1_754_200_000_i64 + crate::wg::HANDSHAKE_STALE_SECS + 1;
        let mut checks = judge_wg_peers(&conf, &live, now, &Default::default());
        assert!(checks.iter().all(|c| c.state == PeerState::Suspect));

        let mut reachable = BTreeMap::new();
        reachable.insert("10.66.0.2".to_owned(), Err("开不了 ICMP socket".to_owned()));
        reachable.insert("10.66.0.3".to_owned(), Ok(false));
        crate::wg::apply_reachability(&mut checks, &reachable);

        // Unprobeable → Unprobed, not Down
        assert!(matches!(checks[0].state, PeerState::Unprobed { .. }));
        // Probed and genuinely unreachable → this verdict can be reached
        assert!(matches!(checks[1].state, PeerState::Down { .. }));
        // The ladder only counts Down, so Unprobed drives no remedy at all
        assert_eq!(
            checks
                .iter()
                .filter(|c| matches!(c.state, PeerState::Down { .. }))
                .count(),
            1
        );
    }

    /// Which conclusions survive when probing is impossible, divided by where the
    /// evidence comes from.
    ///
    /// Evidence from the dump needs no packets: an Endpoint disagreeing with the config
    /// is readable directly. Whether the link carries traffic can only be probed, and an
    /// unprobeable link is unknown. Among equally Unprobed peers, loopback drift is
    /// therefore repaired as usual, because it always breaks the link and the repair is
    /// one `wg set`, while public drift is left in place, because it may be ordinary
    /// roaming and there is no evidence of a failure.
    #[test]
    fn what_survives_being_unprobed_is_whatever_the_dump_alone_proves() {
        let mut phantun_peer = judge_wg_peers(
            &parse_wireguard_conf_peers(CONF),
            &parse_wg_dump(
                "SG=\t(none)\t198.51.100.133:51820\t10.66.0.2/32\t1754200000\t1\t1\t25\n",
            ),
            1_754_200_000,
            &Default::default(),
        )
        .remove(0);
        phantun_peer.state = PeerState::Unprobed {
            reason: "开不了 ICMP socket".to_owned(),
        };
        assert!(phantun_peer.endpoint_drift.is_some());
        assert!(crate::wg::drift_is_fatal(&phantun_peer));

        let mut public_peer = judge_wg_peers(
            &parse_wireguard_conf_peers(
                "[Peer]\n# sg-01\nPublicKey  = SG=\nAllowedIPs = 10.66.0.2/32\nEndpoint   = 198.51.100.133:51820\n",
            ),
            &parse_wg_dump("SG=\t(none)\t[2001:db8::1]:51820\t10.66.0.2/32\t1754200000\t1\t1\toff\n"),
            1_754_200_000,
            &Default::default(),
        )
        .remove(0);
        public_peer.state = PeerState::Unprobed {
            reason: "开不了 ICMP socket".to_owned(),
        };
        assert!(public_peer.endpoint_drift.is_some());
        assert!(!crate::wg::drift_is_fatal(&public_peer));
    }

    /// The ladder goes by how long the fault has persisted, not by round number —
    /// moving detection to every 5 seconds must not bring the louder rungs forward with
    /// it. These thresholds are worth pinning down: lower them a little and a machine
    /// whose peer is powered off starts tearing down its own overlay address every
    /// minute.
    #[test]
    fn the_ladder_is_paced_by_wall_clock_not_by_round_count() {
        use crate::wg::{pick_rung, Rung};

        // Just broken: start with the cheap idempotent remedy, which does not
        // interrupt live sessions
        assert_eq!(pick_rung(0, 2, 2), Rung::Syncconf);
        assert_eq!(pick_rung(44, 2, 2), Rung::Syncconf);
        // Only after 45 seconds without recovery may sessions be cut
        assert_eq!(pick_rung(45, 2, 2), Rung::Bounce);
        assert_eq!(pick_rung(119, 2, 2), Rung::Bounce);
        // 120 seconds, and everything unreachable, before touching the interface
        assert_eq!(pick_rung(120, 2, 2), Rung::Rebuild);
        // One peer still working blocks the rebuild: deleting the interface removes the
        // overlay address, and the relay inbound is bound to it
        assert_eq!(pick_rung(120, 1, 2), Rung::SpareTheLive);
        assert_eq!(pick_rung(86_400, 1, 2), Rung::SpareTheLive);
        // Only the two top rungs back off; the earlier two recheck at the shortest
        // interval after every action
        assert!(Rung::Rebuild.is_top() && Rung::SpareTheLive.is_top());
        assert!(!Rung::Syncconf.is_top() && !Rung::Bounce.is_top());
    }

    /// The backoff replaces a fixed 30-minute cooldown after a rebuild. What restores a
    /// link is the ping each round rather than these remedies, and once all of them have
    /// been tried, repeating them unchanged rarely helps. The interval therefore grows
    /// without ever stopping, so a later change to this machine's egress address is still
    /// acted on.
    #[test]
    fn the_backoff_is_aggressive_early_and_quiet_later() {
        let mut dwell = crate::wg::WG_REMEDY_DWELL;
        let mut at = 120_u64;
        let mut retries = vec![at];
        for _ in 0..6 {
            dwell = (dwell * 2).clamp(60, crate::wg::WG_BACKOFF_MAX);
            at += dwell;
            retries.push(at);
        }

        // 2, 3, 5, 9, 17, and 32 minutes, then every 15
        assert_eq!(retries, vec![120, 180, 300, 540, 1020, 1920, 2820]);
        // Three retries within the first five minutes, where the 30-minute cooldown had
        // none
        assert_eq!(retries.iter().filter(|at| **at <= 300).count(), 3);
        // And fewer actions per day than that cooldown produced
        assert!(dwell >= 30 * 60 / 2);
    }

    /// The config says to dial out and the runtime has no endpoint at all, which is a
    /// failure to dial.
    #[test]
    fn a_configured_peer_without_a_live_endpoint_counts_as_drift() {
        let conf = parse_wireguard_conf_peers(CONF);
        let live = parse_wg_dump("SG=\t(none)\t(none)\t10.66.0.2/32\t1754200000\t1024\t2048\t25\n");

        let checks = judge_wg_peers(&conf, &live, 1_754_200_000 + 10, &Default::default());

        assert_eq!(
            checks[0].endpoint_drift,
            Some(("127.0.0.1:29000".to_owned(), "(none)".to_owned()))
        );
    }

    #[test]
    fn xray_listen_ports_lists_every_inbound_for_the_health_check() {
        let ports = xray_listen_ports(
            r#"{"inbounds":[
                 {"tag":"api","listen":"127.0.0.1","port":10085,"protocol":"dokodemo-door"},
                 {"tag":"in:brc/i-hk-01","listen":"0.0.0.0","port":444,"protocol":"vless"},
                 {"tag":"in:brc/i-hy2","listen":"0.0.0.0","port":443,"protocol":"hysteria"}
               ]}"#,
        );

        assert_eq!(
            ports,
            vec![
                (
                    "api".to_owned(),
                    "127.0.0.1".to_owned(),
                    10085_u16,
                    XrayListenProtocol::Tcp,
                ),
                (
                    "in:brc/i-hk-01".to_owned(),
                    "0.0.0.0".to_owned(),
                    444_u16,
                    XrayListenProtocol::Tcp,
                ),
                (
                    "in:brc/i-hy2".to_owned(),
                    "0.0.0.0".to_owned(),
                    443_u16,
                    XrayListenProtocol::Udp,
                ),
            ]
        );
        assert!(xray_listen_ports("not json").is_empty());
    }

    #[test]
    fn splice_is_disabled_only_for_security_fronts() {
        let ordinary = r#"{"inbounds":[
            {"protocol":"dokodemo-door","settings":{"address":"127.0.0.1"}},
            {"protocol":"vless","settings":{"clients":[]},"streamSettings":{"security":"reality"}}
        ]}"#;
        assert!(!crate::xray_needs_splice_disabled(ordinary).unwrap());

        for security in ["reality", "tls"] {
            let split = format!(
                r#"{{"inbounds":[{{
                    "protocol":"dokodemo-door",
                    "settings":{{"address":"127.0.0.1","port":20000}},
                    "streamSettings":{{"security":"{security}"}}
                }}]}}"#
            );
            assert!(
                crate::xray_needs_splice_disabled(&split).unwrap(),
                "{security}"
            );
        }
        assert!(crate::xray_needs_splice_disabled("not json").is_err());
    }

    #[test]
    fn parses_basic_http_response() {
        let response =
            parse_http_response(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
        assert_eq!(response.status, 204);
        assert_eq!(response.body, "");
    }

    #[test]
    fn route_headers_only_include_observed_addresses() {
        let headers = route_headers(&RouteIpReport {
            ipv4: Some("198.51.100.10".to_owned()),
            ipv6: None,
        });

        assert_eq!(
            headers,
            vec![("X-Brocade-Route-IPv4", "198.51.100.10".to_owned())]
        );
    }

    #[test]
    fn parses_chunked_http_response() {
        let response = parse_http_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "hello world");
    }

    #[test]
    fn parses_xray_user_stats_into_usage_counters() {
        let stat = |name: &str, value: i64| XrayStat {
            name: name.to_owned(),
            value,
        };
        let counters = usage_counters_from_stats(&[
            stat("user>>>alice@platform.acme#i-main>>>traffic>>>uplink", 12),
            stat("user>>>alice@platform.acme#i-main>>>traffic>>>downlink", 34),
            stat("inbound>>>in:brc/i-main>>>traffic>>>uplink", 999),
        ]);

        assert_eq!(counters.len(), 1);
        assert_eq!(counters[0].label, "alice@platform.acme#i-main");
        assert_eq!(counters[0].uplink_bytes, 12);
        assert_eq!(counters[0].downlink_bytes, 34);
    }

    #[test]
    fn parses_proc_stat_state_and_start_ticks() {
        let live = "267 (xray) S 1 190 190 0 -1 4194304 4733 0 1 0 11 2 0 0 20 0 22 0 6594102 1331986432 9415";
        let zombie = "481 (xray) Z 1 481 481 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 29695008";

        assert_eq!(super::parse_proc_stat(live).unwrap(), ('S', 6594102));
        assert_eq!(super::parse_proc_stat(zombie).unwrap(), ('Z', 29695008));
    }

    #[test]
    fn a_zombie_does_not_count_as_a_live_process() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn short-lived child");
        let pid = child.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(2);
        let became_zombie = loop {
            let state = fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| super::parse_proc_stat(&stat).ok())
                .map(|(state, _)| state);
            if state == Some('Z') {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let considered_live = super::process_ref(pid);
        let _ = child.wait();

        assert!(
            became_zombie,
            "short-lived child was not observed as a zombie"
        );
        assert_eq!(considered_live, None);
    }

    #[test]
    fn a_stuck_process_is_killed_after_the_short_grace_period() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; printf r; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn TERM-ignoring child");
        let mut ready = [0_u8; 1];
        child
            .stdout
            .take()
            .expect("child stdout")
            .read_exact(&mut ready)
            .expect("wait until TERM is ignored");
        let process = super::process_ref(child.id() as libc::pid_t).expect("live child");
        let started = Instant::now();

        super::terminate_processes(
            &[process],
            Duration::from_millis(20),
            Duration::from_millis(250),
        )
        .unwrap();
        let status = child.wait().expect("reap killed child");

        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stuck shutdown exceeded its bound"
        );
    }

    #[test]
    #[ignore = "requires the repository's pinned Xray binary"]
    fn native_xray_restart_restores_listening_without_waiting_for_its_zombie() {
        fn start_xray(binary: &Path, config: &Path) -> Result<std::process::Child, String> {
            Command::new(binary)
                .args([
                    "run",
                    "-config",
                    config.to_str().ok_or("non-UTF-8 config path")?,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("start native xray: {error}"))
        }

        fn wait_for_xray(child: &mut std::process::Child, port: u16) -> Result<TcpStream, String> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
                    return Ok(stream);
                }
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("poll native xray: {error}"))?
                {
                    return Err(format!("native xray exited before listening: {status}"));
                }
                if Instant::now() >= deadline {
                    return Err("native xray did not listen within 5 seconds".to_owned());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }

        fn closed_connection_shape(mut connection: TcpStream) -> Result<&'static str, String> {
            connection
                .set_read_timeout(Some(Duration::from_millis(500)))
                .map_err(|error| error.to_string())?;
            let mut bytes = [0_u8; 1024];
            loop {
                match connection.read(&mut bytes) {
                    Ok(0) => return Ok("EOF/FIN"),
                    Ok(_) => continue,
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                        ) =>
                    {
                        return Ok("reset")
                    }
                    Err(error) => return Err(format!("old connection stayed open: {error}")),
                }
            }
        }

        fn wait_for_forwarded_connection(listener: &TcpListener) -> Result<TcpStream, String> {
            listener
                .set_nonblocking(true)
                .map_err(|error| error.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => return Ok(stream),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => return Err(format!("accept xray forwarding: {error}")),
                }
                if Instant::now() >= deadline {
                    return Err("xray did not forward the test connection within 5 seconds".into());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }

        let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/xray");
        assert!(binary.exists(), "{} is missing", binary.display());
        let api_port = TcpListener::bind(("127.0.0.1", 0))
            .expect("reserve API port")
            .local_addr()
            .expect("read API port")
            .port();
        let inbound_port = TcpListener::bind(("127.0.0.1", 0))
            .expect("reserve inbound port")
            .local_addr()
            .expect("read inbound port")
            .port();
        let target_listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind forwarding target");
        let target_port = target_listener
            .local_addr()
            .expect("read forwarding target port")
            .port();
        let directory = test_state_dir("native-xray-restart");
        let config = directory.join("xray.json");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "log": { "loglevel": "warning" },
                "api": { "tag": "api", "services": ["HandlerService", "StatsService"] },
                "stats": {},
                "inbounds": [
                    {
                        "tag": "api",
                        "listen": "127.0.0.1",
                        "port": api_port,
                        "protocol": "dokodemo-door",
                        "settings": { "address": "127.0.0.1" }
                    },
                    {
                        "tag": "test-data",
                        "listen": "127.0.0.1",
                        "port": inbound_port,
                        "protocol": "dokodemo-door",
                        "settings": {
                            "address": "127.0.0.1",
                            "port": target_port,
                            "network": "tcp"
                        }
                    }
                ],
                "outbounds": [{ "tag": "direct", "protocol": "freedom" }],
                "routing": { "rules": [{
                    "type": "field",
                    "inboundTag": ["api"],
                    "outboundTag": "api"
                }] }
            }))
            .expect("encode native xray config"),
        )
        .expect("write native xray config");

        let mut old = start_xray(&binary, &config).expect("start old xray");
        let mut replacement = None;
        let result = (|| -> Result<(), String> {
            drop(wait_for_xray(&mut old, api_port)?);
            let mut old_connection = TcpStream::connect(("127.0.0.1", inbound_port))
                .map_err(|error| format!("connect through old xray: {error}"))?;
            old_connection
                .write_all(b"u")
                .map_err(|error| format!("write through old xray: {error}"))?;
            let mut target_connection = wait_for_forwarded_connection(&target_listener)?;
            let mut byte = [0_u8; 1];
            target_connection
                .read_exact(&mut byte)
                .map_err(|error| format!("read xray-forwarded byte: {error}"))?;
            target_connection
                .write_all(b"d")
                .map_err(|error| format!("write xray-forwarded byte: {error}"))?;
            old_connection
                .read_exact(&mut byte)
                .map_err(|error| format!("read through old xray: {error}"))?;
            let process =
                super::process_ref(old.id() as libc::pid_t).ok_or("old native xray is not live")?;
            let outage_started = Instant::now();
            super::terminate_processes(&[process], super::XRAY_TERM_GRACE, super::XRAY_KILL_GRACE)?;
            let stopped_after = outage_started.elapsed();
            let old_connection = closed_connection_shape(old_connection)?;

            replacement = Some(start_xray(&binary, &config)?);
            let replacement_process = replacement.as_mut().expect("replacement assigned");
            drop(wait_for_xray(replacement_process, api_port)?);
            let unavailable_for = outage_started.elapsed();
            eprintln!(
                "native xray restart: stop={stopped_after:?}, listen restored={unavailable_for:?}, old connection={old_connection}"
            );

            if stopped_after > Duration::from_millis(500) {
                return Err(format!(
                    "native xray stop exceeded 500ms: {stopped_after:?}"
                ));
            }
            if unavailable_for > Duration::from_secs(2) {
                return Err(format!(
                    "native xray listening gap exceeded 2s: {unavailable_for:?}"
                ));
            }
            Ok(())
        })();

        let _ = old.kill();
        let _ = old.wait();
        if let Some(child) = replacement.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(directory);
        result.unwrap();
    }

    #[test]
    fn rejects_oversized_chunked_http_response() {
        let body = format!("{:x}\r\n", MAX_HTTP_RESPONSE_BYTES + 1);
        let error = decode_chunked_body(body.as_bytes()).unwrap_err();
        assert!(error.contains("exceeds"));
    }

    #[test]
    fn parses_http_client_authority_with_ipv6_and_port_host_header() {
        let client = HttpClient::new("http://[::1]:8081/base/").unwrap();
        assert_eq!(client.host, "::1");
        assert_eq!(client.port, 8081);
        assert_eq!(client.host_header, "[::1]:8081");
        assert_eq!(client.prefix, "/base");

        let client = HttpClient::new("http://example.test:8081").unwrap();
        assert_eq!(client.host, "example.test");
        assert_eq!(client.port, 8081);
        assert_eq!(client.host_header, "example.test:8081");

        let error = HttpClient::new("http://::1:8081").unwrap_err();
        assert!(error.contains("IPv6 server URLs must use brackets"));
    }

    #[test]
    fn parses_linux_apply_mode() {
        let options = Options::parse(vec![
            "apply-once".to_owned(),
            "--server".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--token".to_owned(),
            "broc_node_test".to_owned(),
            "--apply".to_owned(),
            "linux".to_owned(),
        ])
        .unwrap();

        assert_eq!(options.apply_mode, ApplyMode::Linux);
    }

    #[test]
    fn parses_token_file_and_rejects_missing_option_values() {
        let dir = test_state_dir("parses-token-file");
        let token_file = dir.join("node.token");
        fs::write(&token_file, "  broc_node_file\n").unwrap();

        let options = Options::parse_with_env(
            vec![
                "apply-once".to_owned(),
                "--server".to_owned(),
                "http://127.0.0.1:8080".to_owned(),
                "--token-file".to_owned(),
                token_file.display().to_string(),
            ],
            |_| None,
        )
        .unwrap();
        assert_eq!(options.token, "broc_node_file");

        let error = Options::parse_with_env(
            vec!["apply-once".to_owned(), "--server".to_owned()],
            |name| match name {
                "BROCADE_AGENT_SERVER" => Some("http://127.0.0.1:8080".to_owned()),
                "BROCADE_NODE_TOKEN" => Some("broc_node_env".to_owned()),
                _ => None,
            },
        )
        .unwrap_err();
        assert_eq!(error, "--server requires a value");

        let error = Options::parse_with_env(
            vec!["apply-once".to_owned(), "--token".to_owned()],
            |name| match name {
                "BROCADE_AGENT_SERVER" => Some("http://127.0.0.1:8080".to_owned()),
                "BROCADE_NODE_TOKEN" => Some("broc_node_env".to_owned()),
                _ => None,
            },
        )
        .unwrap_err();
        assert_eq!(error, "--token requires a value");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn observed_grants_keep_flow_and_drop_level() {
        let observed = observed_inbounds(&[GrantInbound {
            tag: "in:brc/i-main".to_owned(),
            clients: vec![GrantClient {
                email: "alice@example#i-main".to_owned(),
                uuid: "2d2304da-f114-4574-8d44-625afdb1db5c".to_owned(),
                flow: Some("xtls-rprx-vision".to_owned()),
                level: 7,
            }],
        }]);

        assert_eq!(observed[0].clients[0].email, "alice@example#i-main");
        assert_eq!(
            observed[0].clients[0].uuid,
            "2d2304da-f114-4574-8d44-625afdb1db5c"
        );
        assert_eq!(
            observed[0].clients[0].flow,
            Some("xtls-rprx-vision".to_owned())
        );
    }

    #[test]
    fn converge_artifact_reports_written_file_hash() {
        let dir = test_state_dir("converge-artifact-reports-written-file-hash");
        let state = converge_artifact(
            &dir,
            "xray.json",
            "xray.disabled",
            &DesiredArtifact::Present {
                content: "{\"ok\":true}\n".to_owned(),
                sha256: "desired-sha-must-not-be-trusted".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(
            state,
            AppliedArtifactState::Present {
                sha256: sha256_hex(b"{\"ok\":true}\n")
            }
        );

        let _ = fs::remove_dir_all(dir);
    }

    fn test_state_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("brocade-agent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn usage_sequence_is_reserved_on_disk_and_survives_process_restart() {
        let dir = test_state_dir("usage-cursor");
        let (instance_a, first) = super::reserve_usage_sequence(&dir).unwrap();
        let (instance_b, second) = super::reserve_usage_sequence(&dir).unwrap();
        assert_eq!(instance_a, instance_b);
        assert_eq!((first, second), (1, 2));
        assert_eq!(instance_a.len(), 32);

        let persisted: super::UsageCursor =
            serde_json::from_str(&fs::read_to_string(dir.join(super::USAGE_CURSOR_FILE)).unwrap())
                .unwrap();
        assert_eq!(persisted.last_sequence, 2);
        assert_eq!(persisted.agent_instance_id, instance_a);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_generation_persistence_is_part_of_clean_convergence() {
        let dir = test_state_dir("usage-generation");
        let after = ReportedNodeState {
            phantun: AppliedArtifactState::Disabled,
            wireguard: AppliedArtifactState::Disabled,
            xray: AppliedArtifactState::Disabled,
            hy2_port_hop: AppliedArtifactState::Disabled,
            grants: AppliedGrantsState::Disabled,
        };
        let outcome = super::persist_converged_usage_generation(
            &dir,
            Some(42),
            (
                brocade_deployment::protocol::TargetApplyResult::Applied,
                after.clone(),
                None,
            ),
        );
        assert_eq!(
            outcome.0,
            brocade_deployment::protocol::TargetApplyResult::Applied
        );
        assert_eq!(super::read_usage_generation(&dir).unwrap(), Some(42));

        fs::remove_file(dir.join(super::USAGE_GENERATION_FILE)).unwrap();
        fs::create_dir(dir.join(super::USAGE_GENERATION_FILE)).unwrap();
        let failed = super::persist_converged_usage_generation(
            &dir,
            Some(43),
            (
                brocade_deployment::protocol::TargetApplyResult::Applied,
                after,
                None,
            ),
        );
        assert_eq!(
            failed.0,
            brocade_deployment::protocol::TargetApplyResult::FailedDirty
        );
        assert!(failed.2.unwrap().contains("usage generation"));
        let _ = fs::remove_dir_all(dir);
    }

    /// The two spools write to separate files. This is the property most likely to break
    /// now that they share code: an incorrect path makes the usage drain post convergence
    /// results to `/agent/v1/usage` as readings, producing wrong data on both sides with
    /// no error.
    #[test]
    fn spools_keep_separate_files() {
        let dir = test_state_dir("spool-separate");

        spool_push(USAGE_SPOOL, &dir, &serde_json::json!({ "kind": "usage" })).unwrap();
        spool_push(
            OBSERVATION_SPOOL,
            &dir,
            &serde_json::json!({ "kind": "observation" }),
        )
        .unwrap();

        assert!(dir.join("usage.spool.jsonl").exists());
        assert!(dir.join("observation.spool.jsonl").exists());

        let usage = spool_read(USAGE_SPOOL, &dir).unwrap();
        let observation = spool_read(OBSERVATION_SPOOL, &dir).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(observation.len(), 1);
        assert!(usage[0].contains("usage"));
        assert!(observation[0].contains("observation"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wireguard_only_node_still_drains_historical_usage() {
        let dir = test_state_dir("wg-only-drains-usage");
        spool_push(
            USAGE_SPOOL,
            &dir,
            &serde_json::json!({ "historical": true }),
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /agent/v1/usage "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let options = Options {
            command: "usage-once".to_owned(),
            server: format!("http://127.0.0.1:{port}"),
            token: "test".to_owned(),
            state_dir: dir.clone(),
            apply_mode: ApplyMode::Linux,
        };

        super::usage_cycle(&options).unwrap();
        server.join().unwrap();
        assert!(spool_read(USAGE_SPOOL, &dir).unwrap().is_empty());
        assert!(!dir.join("xray.json").exists(), "测试节点必须保持纯 WG");

        let _ = fs::remove_dir_all(dir);
    }

    /// Over the limit, the oldest entries go. Dropping the newest freezes the spool in
    /// the past, growing more useless as it fills.
    #[test]
    fn spool_push_drops_the_oldest_over_the_cap() {
        let dir = test_state_dir("spool-cap");
        let spool = Spool {
            file: "tiny.spool.jsonl",
            endpoint: "/agent/v1/nowhere",
            max: 3,
            what: "tiny",
            unit: "条",
            terminal_statuses: &[400],
        };

        for n in 0..5 {
            spool_push(spool, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let lines = spool_read(spool, &dir).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"n\":2"), "留下的该是最新的三条");
        assert!(lines[2].contains("\"n\":4"));

        let _ = fs::remove_dir_all(dir);
    }

    /// Any value interpolated into a shell command has to be quoted first. This guards
    /// phantun's download script, which runs as root: the URL and sha256 both come from
    /// the control plane, and a value containing a quote turns `curl -fsSL <url>` into an
    /// additional command. Inside single quotes the only significant character is the
    /// single quote itself, so the test is that `'` becomes `'\''` rather than passing
    /// through unchanged.
    #[test]
    fn shell_quoting_neutralizes_a_value_that_tries_to_end_the_quote() {
        assert_eq!(super::shell_quote("plain"), "'plain'");
        assert_eq!(super::shell_quote(""), "''");

        // The assertion cannot be on the quoted string's text, because after escaping it
        // still contains those characters. What matters is whether passing it to a real
        // shell returns the original value. The payload is a harmless echo, so a quoting
        // failure prints PWNED rather than deleting anything.
        for payload in [
            "x'; echo PWNED; echo '",
            "$(echo PWNED)",
            "`echo PWNED`",
            r"a\b$c`d",
            "带空格 和中文",
        ] {
            let quoted = super::shell_quote(payload);
            let echoed = super::run_command("sh", &["-c", &format!("printf %s {quoted}")]).unwrap();
            assert_eq!(echoed, payload, "{payload} 引用后过一遍 shell 该原样回来");
        }
    }

    /// A missing tag must be named in the error. A missing inbound in xray's config is
    /// the most common cause of a failed convergence, and without the name one has to
    /// diff two JSON files to learn which one it was.
    #[test]
    fn a_missing_inbound_tag_is_named_in_the_error() {
        let config = serde_json::json!({
            "inbounds": [{ "tag": "in:brc/i-main", "port": 443 }],
        });

        let found = super::xray_inbound_by_tag(&config, "in:brc/i-main").unwrap();
        assert_eq!(found["port"], 443);

        let error = super::xray_inbound_by_tag(&config, "in:brc/i-other").unwrap_err();
        assert!(error.contains("in:brc/i-other"), "实际 {error}");
    }

    /// An unreadable api port yields None, which means usage cannot be sampled. Falling
    /// back to a default port would connect the agent to an unrelated service and report
    /// that service's statistics as its own.
    #[test]
    fn the_api_port_is_none_rather_than_a_guess() {
        let with_api = r#"{"inbounds":[{"tag":"api","port":10085},{"tag":"in:brc","port":443}]}"#;
        assert_eq!(super::xray_api_port(with_api), Some(10085));

        for bad in [
            r#"{"inbounds":[{"tag":"in:brc","port":443}]}"#,
            r#"{"inbounds":[]}"#,
            r#"{"outbounds":[]}"#,
            "{ 这不是 JSON",
            "",
        ] {
            assert_eq!(super::xray_api_port(bad), None, "{bad} 该读不出口");
        }

        // A port that does not fit in u16 is also None; it must not be truncated into
        // some other port.
        let overflow = r#"{"inbounds":[{"tag":"api","port":70000}]}"#;
        assert_eq!(super::xray_api_port(overflow), None);
    }

    /// Disabling removes the artifact file and leaves only a marker recording the reason.
    /// Keeping the old artifact would make the next observation see both files and report
    /// dirty, so a machine that was correctly disabled would stay dirty indefinitely.
    #[test]
    fn disabling_an_artifact_removes_it_and_records_why() {
        let dir = test_state_dir("artifact-disable");
        fs::write(dir.join("xray.json"), "{\"old\":true}").unwrap();

        let state = converge_artifact(
            &dir,
            "xray.json",
            "xray.disabled",
            &DesiredArtifact::Disabled {
                reason: "这台不跑应用层".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(state, AppliedArtifactState::Disabled);
        assert!(!dir.join("xray.json").exists(), "旧产物要删掉");
        assert_eq!(
            fs::read_to_string(dir.join("xray.disabled")).unwrap(),
            "这台不跑应用层"
        );

        let _ = fs::remove_dir_all(dir);
    }

    /// An unmanaged artifact means no file may be written. Writing one would make the
    /// next observation report another component's state as this convergence's result.
    #[test]
    fn an_unmanaged_artifact_leaves_the_state_dir_untouched() {
        let dir = test_state_dir("artifact-unmanaged");

        let state = converge_artifact(
            &dir,
            "xray.json",
            "xray.disabled",
            &DesiredArtifact::Unmanaged {
                reason: "别人在管".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(state, AppliedArtifactState::Unmanaged);
        assert!(!dir.join("xray.json").exists());
        assert!(!dir.join("xray.disabled").exists());

        let _ = fs::remove_dir_all(dir);
    }

    /// Never written means all Unknown, not an error. A freshly installed machine takes
    /// this path on its first round, and returning Err here would leave it never
    /// finishing a first convergence.
    #[test]
    fn a_fresh_machine_reads_back_an_all_unknown_state() {
        let dir = test_state_dir("applied-fresh");
        let state = super::read_applied_state(&dir).unwrap();
        assert_eq!(state.wireguard, AppliedArtifactState::Unknown);
        assert_eq!(state.grants, AppliedGrantsState::Unknown);
        let _ = fs::remove_dir_all(dir);
    }

    /// A round trip through disk must come back identical. This state is what the
    /// control plane judges the machine's shape by, and one field lost in serialization
    /// shows it a machine other than the real one.
    #[test]
    fn the_applied_state_survives_a_round_trip() {
        let dir = test_state_dir("applied-roundtrip");
        let written = ReportedNodeState {
            phantun: AppliedArtifactState::Disabled,
            hy2_port_hop: AppliedArtifactState::Present {
                sha256: "hop-sha".to_owned(),
            },
            wireguard: AppliedArtifactState::Present {
                sha256: "abc123".to_owned(),
            },
            xray: AppliedArtifactState::Dirty {
                reason: "进程没跑".to_owned(),
            },
            grants: AppliedGrantsState::Unmanaged,
        };

        super::write_applied_state(&dir, &written).unwrap();
        assert_eq!(super::read_applied_state(&dir).unwrap(), written);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_applied_state_write_failure_becomes_a_reportable_failed_dirty() {
        let dir = test_state_dir("applied-write-failure");
        // Atomic rename cannot replace a directory.  This deterministically
        // exercises the post-convergence persistence failure without relying on
        // filesystem permissions (tests may run as root).
        fs::create_dir(dir.join("applied-state.json")).unwrap();
        let after = ReportedNodeState {
            phantun: AppliedArtifactState::Disabled,
            wireguard: AppliedArtifactState::Disabled,
            xray: AppliedArtifactState::Disabled,
            hy2_port_hop: AppliedArtifactState::Disabled,
            grants: AppliedGrantsState::Unmanaged,
        };

        let (result, observed, error) = super::successful_apply_outcome(&dir, after.clone());
        assert_eq!(
            result,
            brocade_deployment::protocol::TargetApplyResult::FailedDirty
        );
        assert_eq!(observed, after);
        assert!(error.unwrap().contains("applied-state.json"));

        let _ = fs::remove_dir_all(dir);
    }

    /// Absent geodata files yield None. Inventing a sha would show two machines in the
    /// console as sharing one geodata copy while one of them has no such file at all.
    #[test]
    fn geodata_state_is_none_when_the_file_is_absent() {
        let dir = test_state_dir("geodata");
        assert!(super::geodata_file_state(&dir.join("geoip.dat")).is_none());

        let path = dir.join("geoip.dat");
        fs::write(&path, b"not really geoip").unwrap();
        let state = super::geodata_file_state(&path).unwrap();
        assert_eq!(state.bytes, 16);
        assert_eq!(state.sha256, sha256_hex(b"not really geoip"));

        let _ = fs::remove_dir_all(dir);
    }
}
