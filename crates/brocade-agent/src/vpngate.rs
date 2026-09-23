//! Node-local VPN Gate runtime.
//!
//! Provider profiles run only inside a dedicated Linux network namespace. The node's main Xray
//! reaches a tiny helper Xray over a deterministic veth `/30`; OpenVPN can therefore replace the
//! namespace's default route without touching the host route table. Profiles are validated again
//! here before they reach a privileged process, even though the Console already sanitized them.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use brocade_core::hash::sha256_hex;
use brocade_core::model::{
    vpngate_runtime_peer, VPNGATE_CONNECT_THRESHOLD_MAX_MS, VPNGATE_DOWNLOAD_THRESHOLD_MAX_BPS,
    VPNGATE_MAX_CANDIDATES, VPNGATE_RUNTIME_MAX_POOLS_PER_NODE, VPNGATE_RUNTIME_SOCKS_PORT,
};
use brocade_deployment::protocol::{
    evaluate_vpngate_admission, validate_vpngate_admission_policy, VpngateAdmissionDecision,
    VpngateBackendRole, VpngateBackendState, VpngateCandidate, VpngateCatalogSyncAssignment,
    VpngateCatalogSyncFailure, VpngateDesiredPool, VpngateDesiredState, VpngateFailureReason,
    VpngateIpIntelligenceAssignment, VpngateIpIntelligenceFailure,
    VpngateIpIntelligenceObservation, VpngateIpIntelligenceReport, VpngateIpNetwork,
    VpngateIpProvider, VpngateIpScore, VpngateManualSwitchCommand, VpngateManualSwitchResult,
    VpngateManualSwitchStatus, VpngateNetworkType, VpngatePoolReport, VpngateProbeReport,
    VpngateProbeSample, VpngateProbeStatus, VpngateRealtimeBackend, VpngateRealtimePool,
    VpngateRealtimeReport, VpngateReconcileReport, VpngateRuntimeEvent, VpngateRuntimeEventKind,
    VpngateRuntimeState, VpngateTransport,
};
use flate2::{write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};

use crate::{
    command::capture_command_with_timeout, http::HttpClient, options::ApplyMode, options::Options,
};

const DESIRED_INTERVAL: Duration = Duration::from_secs(60);
const RUNTIME_DESIRED_INTERVAL: Duration = Duration::from_secs(10);
const RUNTIME_INTERVAL: Duration = Duration::from_secs(5);
const CONTINUOUS_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const FULL_PROBE_INTERVAL_SECS: i64 = 15 * 60;
const HEALTH_FAILURE_THRESHOLD: u8 = 2;
const HEALTH_REQUEST_TIMEOUT_SECS: u16 = 4;
const TCP_REACH_TIMEOUT: Duration = Duration::from_secs(2);
const RUNTIME_BACKENDS_PER_POOL: usize = 2;
const MANUAL_SWITCH_COOLDOWN_SECS: u32 = 10 * 60;
const MAX_LOCAL_SAMPLES_PER_CANDIDATE: usize = 128;
const OPENVPN_START_TIMEOUT: Duration =
    Duration::from_millis(VPNGATE_CONNECT_THRESHOLD_MAX_MS as u64);
const HELPER_START_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_CANDIDATES: usize = VPNGATE_MAX_CANDIDATES as usize;
const MAX_PROBE_CANDIDATES: usize = 128;
const MAX_PROBE_ASSIGNMENTS: usize = 128;
const MAX_TOTAL_PROBE_CANDIDATES: usize = 128;
const MAX_CATALOG_FEED_BYTES: usize = 16 * 1024 * 1024;
const MAX_CATALOG_UPLOAD_BYTES: usize = 4 * 1024 * 1024;
const CATALOG_FETCH_TIMEOUT: Duration = Duration::from_secs(50);
pub(crate) const CATALOG_PROBE_WORKERS: u8 = 128;
const CATALOG_SPEED_WORKERS: usize = 2;
const NAMESPACE_RESOLV_CONF: &[u8] =
    b"nameserver 1.1.1.1\nnameserver 8.8.8.8\noptions timeout:2 attempts:2\n";
const MAX_OPENVPN_DNS_VARS_BYTES: u64 = 64 * 1024;
const MAX_RESOLV_CONF_NAMESERVERS: usize = 3;
const MAX_RESOLV_CONF_SEARCH_DOMAINS: usize = 6;
static OPENVPN_HAS_DNS_UPDOWN: OnceLock<Result<bool, String>> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum PendingCatalogueResult {
    Snapshot {
        assignment: VpngateCatalogSyncAssignment,
    },
    Failure {
        report: VpngateCatalogSyncFailure,
    },
}

#[derive(Debug, Deserialize)]
struct VpngateReportReceipt {
    current_state_updated: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ProbeReportDelivery {
    Accepted,
    PermanentlyRejected { status: u16, detail: String },
}

pub(crate) fn run_forever(options: &Options, realtime: crate::realtime::VpngateCache) {
    let client = match HttpClient::new(&options.server) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("vpngate: cannot build control-plane client: {error}");
            return;
        }
    };
    let mut desired = None;
    let mut next_fetch = Instant::now();
    let mut last_report = None;
    let mut telemetry = TelemetrySupervisor::new();
    loop {
        let started = Instant::now();
        if Instant::now() >= next_fetch {
            if let Err(error) = resend_pending_reports(&client, options) {
                eprintln!("vpngate: {error}");
            }
            match fetch_desired(&client, options) {
                Ok(Some(next)) => match validate_desired(&next) {
                    Ok(()) => {
                        let root = vpngate_root(&options.state_dir);
                        let stored = create_private_dir(&root).and_then(|()| {
                            serde_json::to_vec_pretty(&next)
                                .map_err(|error| error.to_string())
                                .and_then(|body| {
                                    write_private_atomic(&root.join("desired.json"), &body)
                                })
                        });
                        if let Err(error) = stored {
                            eprintln!("vpngate: cannot store desired state: {error}");
                        } else {
                            desired = Some(next);
                        }
                    }
                    Err(error) => eprintln!("vpngate: {error}"),
                },
                Ok(None) => {}
                Err(error) => {
                    // A failed poll is not permission to tear down a working egress. Desired state
                    // remains authoritative until a newer complete response arrives.
                    eprintln!("vpngate: {error}");
                }
            }
            next_fetch = Instant::now() + RUNTIME_DESIRED_INTERVAL;
        }
        if options.apply_mode != ApplyMode::StateDir {
            if let Some(desired) = desired.as_ref() {
                let result = (|| {
                    if !desired.pools.is_empty() {
                        require_linux_runtime()?;
                    }
                    let pools =
                        reconcile(&options.state_dir, desired, options.vpngate_stats_window)?;
                    telemetry.publish(&options.state_dir, desired, &pools, &realtime);
                    let report = VpngateReconcileReport {
                        topology_revision: desired.topology_revision,
                        catalog_generation: desired.catalog_generation,
                        pools,
                    };
                    if last_report.as_ref() != Some(&report) {
                        persist_and_send_reconcile_report(&client, options, &report)?;
                        last_report = Some(report);
                    }
                    Ok::<(), String>(())
                })();
                if let Err(error) = result {
                    eprintln!("vpngate: {error}");
                }
            }
        }
        thread::sleep(RUNTIME_INTERVAL.saturating_sub(started.elapsed()));
    }
}

/// Catalogue OpenVPN probes can take minutes and therefore must never delay the five-second
/// runtime health supervisor.
pub(crate) fn run_probe_forever(options: &Options) {
    if options.apply_mode == ApplyMode::StateDir {
        return;
    }
    let client = match HttpClient::new(&options.server) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("vpngate probe: cannot build control-plane client: {error}");
            return;
        }
    };
    // Recover every bounded worker namespace once after an Agent restart. Each actual job also
    // clears its own slot before use; repeating the full 128-slot sweep for every small batch
    // would turn an increased worker ceiling into needless process and iptables churn.
    terminate_catalog_probe_processes(&options.state_dir);
    cleanup_catalog_probe_namespaces();
    loop {
        let outcome: Result<bool, String> = (|| {
            resend_pending_probe_reports(&client, options)?;
            let Some(desired) = fetch_desired(&client, options)? else {
                return Ok(false);
            };
            validate_desired(&desired)?;
            if desired.probe_assignments.is_empty() {
                return Ok(false);
            }
            require_linux_runtime()?;
            let reports = reconcile_probe_assignments(&options.state_dir, &desired)?;
            persist_and_send_probe_reports(&client, options, reports)?;
            Ok(true)
        })();
        let delay = match outcome {
            Ok(completed) => next_cycle_delay(completed),
            Err(error) => {
                eprintln!("vpngate probe: {error}");
                DESIRED_INTERVAL
            }
        };
        thread::sleep(delay);
    }
}

/// Run exit-IP intelligence independently from OpenVPN catalogue work.
///
/// A catalogue batch may spend minutes establishing and measuring several tunnels. Putting this
/// small, network-only job on its own poll loop lets an Agent continuously drain the global
/// exit-IP queue while its sibling thread is still probing profiles.
pub(crate) fn run_intelligence_forever(options: &Options) {
    if options.apply_mode == ApplyMode::StateDir {
        return;
    }
    let client = match HttpClient::new(&options.server) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("vpngate intelligence: cannot build control-plane client: {error}");
            return;
        }
    };
    loop {
        let delay = match intelligence_cycle(&client, options) {
            Ok(queried) => next_cycle_delay(queried),
            Err(error) => {
                eprintln!("vpngate intelligence: {error}");
                DESIRED_INTERVAL
            }
        };
        thread::sleep(delay);
    }
}

/// Fetch the public VPN Gate directory on the selected Agent fleet while leaving all parsing and
/// publication authority on the Console. This is a third loop because neither a slow OpenVPN
/// probe nor an intelligence-provider timeout may delay a scheduled directory refresh.
pub(crate) fn run_catalogue_forever(options: &Options) {
    if options.apply_mode == ApplyMode::StateDir {
        return;
    }
    let client = match HttpClient::new(&options.server) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("vpngate catalogue: cannot build control-plane client: {error}");
            return;
        }
    };
    loop {
        let delay = match catalogue_cycle(&client, options) {
            Ok(collected) => next_cycle_delay(collected),
            Err(error) => {
                eprintln!("vpngate catalogue: {error}");
                DESIRED_INTERVAL
            }
        };
        thread::sleep(delay);
    }
}

fn next_cycle_delay(probed_catalogue: bool) -> Duration {
    if probed_catalogue {
        CONTINUOUS_PROBE_INTERVAL
    } else {
        DESIRED_INTERVAL
    }
}

fn intelligence_cycle(client: &HttpClient, options: &Options) -> Result<bool, String> {
    resend_pending_intelligence_report(client, options)?;
    let Some(assignment) = fetch_intelligence_assignment(client, options)? else {
        return Ok(false);
    };
    let report = query_ip_intelligence(&assignment)?;
    persist_and_send_intelligence_report(client, options, &report)?;
    Ok(true)
}

fn catalogue_cycle(client: &HttpClient, options: &Options) -> Result<bool, String> {
    resend_pending_catalogue_result(client, options)?;
    let Some(assignment) = fetch_catalogue_assignment(client, options)? else {
        return Ok(false);
    };
    match collect_catalogue_snapshot(options, &assignment) {
        Ok(compressed) => {
            persist_and_send_catalogue_snapshot(client, options, assignment, &compressed)?
        }
        Err(failure) => persist_and_send_catalogue_failure(client, options, failure)?,
    }
    Ok(true)
}

fn fetch_desired(
    client: &HttpClient,
    options: &Options,
) -> Result<Option<VpngateDesiredState>, String> {
    let response = client.request("GET", "/agent/v1/vpngate/desired", &options.token, None)?;
    match response.status {
        204 => Ok(None),
        200 => serde_json::from_str(&response.body)
            .map(Some)
            .map_err(|error| format!("invalid VPN Gate desired state: {error}")),
        status => Err(format!("desired request failed: HTTP {status}")),
    }
}

fn fetch_intelligence_assignment(
    client: &HttpClient,
    options: &Options,
) -> Result<Option<VpngateIpIntelligenceAssignment>, String> {
    let response = client.request(
        "GET",
        "/agent/v1/vpngate/intelligence-assignment",
        &options.token,
        None,
    )?;
    let assignment = match response.status {
        204 => return Ok(None),
        200 => serde_json::from_str::<VpngateIpIntelligenceAssignment>(&response.body)
            .map_err(|error| format!("invalid VPN Gate IP intelligence assignment: {error}"))?,
        status => return Err(format!("IP intelligence request failed: HTTP {status}")),
    };
    if assignment.lease_generation == 0
        || assignment.exit_ip.parse::<IpAddr>().is_err()
        || assignment
            .proxycheck_api_key
            .as_deref()
            .is_some_and(|key| !valid_proxycheck_api_key(key))
    {
        return Err("VPN Gate IP intelligence assignment is invalid".to_owned());
    }
    Ok(Some(assignment))
}

fn fetch_catalogue_assignment(
    client: &HttpClient,
    options: &Options,
) -> Result<Option<VpngateCatalogSyncAssignment>, String> {
    let response = client.request(
        "GET",
        "/agent/v1/vpngate/catalogue-assignment",
        &options.token,
        None,
    )?;
    let assignment = match response.status {
        204 => return Ok(None),
        200 => serde_json::from_str::<VpngateCatalogSyncAssignment>(&response.body)
            .map_err(|error| format!("invalid VPN Gate catalogue assignment: {error}"))?,
        status => {
            return Err(format!(
                "catalogue assignment request failed: HTTP {status}"
            ))
        }
    };
    if assignment.run_id == 0
        || assignment.lease_generation == 0
        || assignment.source_url.len() > 2_048
        || !assignment.source_url.starts_with("https://")
        || HttpClient::new(&assignment.source_url).is_err()
    {
        return Err("VPN Gate catalogue assignment is invalid".to_owned());
    }
    Ok(Some(assignment))
}

fn collect_catalogue_snapshot(
    options: &Options,
    assignment: &VpngateCatalogSyncAssignment,
) -> Result<Vec<u8>, VpngateCatalogSyncFailure> {
    let root = vpngate_root(&options.state_dir);
    create_private_dir(&root)
        .map_err(|detail| catalogue_failure(assignment, "local-io", detail))?;
    let download = root.join("catalogue-download.csv");
    let download_text = download.to_string_lossy().into_owned();
    let maximum = MAX_CATALOG_FEED_BYTES.to_string();
    let output = capture_command_with_timeout(
        "curl",
        &[
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "45",
            "--max-filesize",
            &maximum,
            "--output",
            &download_text,
            &assignment.source_url,
        ],
        CATALOG_FETCH_TIMEOUT,
    )
    .map_err(|detail| catalogue_failure(assignment, "fetch-failed", detail))?;
    if !output.status.success() {
        let detail = format!(
            "curl exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Err(catalogue_failure(assignment, "fetch-failed", detail));
    }
    let raw = fs::read(&download)
        .map_err(|detail| catalogue_failure(assignment, "local-io", detail.to_string()))?;
    let _ = fs::remove_file(&download);
    if raw.len() > MAX_CATALOG_FEED_BYTES {
        return Err(catalogue_failure(
            assignment,
            "feed-too-large",
            "VPN Gate feed exceeds the 16 MiB safety limit".to_owned(),
        ));
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&raw)
        .map_err(|detail| catalogue_failure(assignment, "local-io", detail.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|detail| catalogue_failure(assignment, "local-io", detail.to_string()))?;
    if compressed.len() > MAX_CATALOG_UPLOAD_BYTES {
        return Err(catalogue_failure(
            assignment,
            "feed-too-large",
            "compressed VPN Gate feed exceeds the 4 MiB upload limit".to_owned(),
        ));
    }
    Ok(compressed)
}

fn catalogue_failure(
    assignment: &VpngateCatalogSyncAssignment,
    code: &str,
    detail: String,
) -> VpngateCatalogSyncFailure {
    VpngateCatalogSyncFailure {
        run_id: assignment.run_id,
        lease_generation: assignment.lease_generation,
        code: code.to_owned(),
        detail: detail.trim().chars().take(2_000).collect(),
    }
}

fn send_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngatePoolReport,
) -> Result<(), String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/vpngate/report",
        &options.token,
        Some(&body),
    )?;
    if response.status == 200 {
        Ok(())
    } else {
        Err(format!("report request failed: HTTP {}", response.status))
    }
}

fn send_reconcile_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngateReconcileReport,
) -> Result<(), String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/vpngate/reconcile-report",
        &options.token,
        Some(&body),
    )?;
    if response.status != 200 {
        return Err(format!(
            "reconcile report request failed: HTTP {}",
            response.status
        ));
    }
    // A successful response can still mean that the observations were accepted only as
    // history because the topology or catalog generation changed. Keep the pending report in
    // that case so the loop refreshes desired state and publishes a current snapshot again.
    let receipt = serde_json::from_str::<VpngateReportReceipt>(&response.body)
        .map_err(|error| format!("reconcile report acknowledgement is invalid: {error}"))?;
    if !receipt.current_state_updated {
        return Err(
            "reconcile report did not update current state; retry after refreshing desired state"
                .to_owned(),
        );
    }
    Ok(())
}

fn send_probe_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngateProbeReport,
) -> Result<ProbeReportDelivery, String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/vpngate/probe-report",
        &options.token,
        Some(&body),
    )?;
    match response.status {
        200 => Ok(ProbeReportDelivery::Accepted),
        400 | 409 | 410 | 422 => Ok(ProbeReportDelivery::PermanentlyRejected {
            status: response.status,
            detail: summarize_http_error_body(&response.body),
        }),
        status => Err(format!(
            "catalogue probe report request failed: HTTP {status}"
        )),
    }
}

fn summarize_http_error_body(body: &str) -> String {
    const MAX_CHARS: usize = 512;
    let mut summary = String::new();
    let mut written = 0;
    for word in body.split_whitespace() {
        if !summary.is_empty() {
            summary.push(' ');
        }
        for character in word.chars() {
            if written >= MAX_CHARS {
                summary.push('…');
                return summary;
            }
            if character.is_control() {
                continue;
            }
            summary.push(character);
            written += 1;
        }
    }
    summary
}

fn send_intelligence_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngateIpIntelligenceReport,
) -> Result<(), String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/vpngate/intelligence-report",
        &options.token,
        Some(&body),
    )?;
    if response.status == 200 || response.status == 409 {
        // A report is durable only to survive transient delivery failures. Once its lease has
        // expired and been reassigned, retrying the stale generation forever would prevent this
        // Agent from accepting any new intelligence work; the newer lease is now authoritative.
        Ok(())
    } else {
        Err(format!(
            "IP intelligence report request failed: HTTP {}",
            response.status
        ))
    }
}

fn send_catalogue_snapshot(
    client: &HttpClient,
    options: &Options,
    assignment: &VpngateCatalogSyncAssignment,
    compressed: &[u8],
) -> Result<(), String> {
    let path = format!(
        "/agent/v1/vpngate/catalogue-report?run_id={}&lease_generation={}",
        assignment.run_id, assignment.lease_generation
    );
    let response = client.request_bytes_with_headers(
        "POST",
        &path,
        &options.token,
        compressed,
        "application/gzip",
        &[],
    )?;
    if response.status == 200 || response.status == 409 {
        Ok(())
    } else {
        Err(format!(
            "catalogue snapshot report failed: HTTP {}",
            response.status
        ))
    }
}

fn send_catalogue_failure(
    client: &HttpClient,
    options: &Options,
    report: &VpngateCatalogSyncFailure,
) -> Result<(), String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/vpngate/catalogue-failure",
        &options.token,
        Some(&body),
    )?;
    if response.status == 200 || response.status == 409 {
        Ok(())
    } else {
        Err(format!(
            "catalogue failure report failed: HTTP {}",
            response.status
        ))
    }
}

fn pending_reports_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-reports.json")
}

fn pending_reconcile_report_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-reconcile-report.json")
}

fn pending_probe_reports_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-probe-reports.json")
}

fn rejected_probe_reports_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("rejected-probe-reports.json")
}

fn pending_intelligence_report_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-intelligence-report.json")
}

fn pending_catalogue_result_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-catalogue-result.json")
}

fn pending_catalogue_snapshot_path(state_dir: &Path) -> PathBuf {
    vpngate_root(state_dir).join("pending-catalogue-snapshot.gz")
}

fn resend_pending_reports(client: &HttpClient, options: &Options) -> Result<(), String> {
    resend_pending_legacy_reports(client, options)?;
    let path = pending_reconcile_report_path(&options.state_dir);
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot read pending VPN Gate reconcile report: {error}"
            ))
        }
    };
    let report = serde_json::from_slice::<VpngateReconcileReport>(&content)
        .map_err(|error| format!("cannot parse pending VPN Gate reconcile report: {error}"))?;
    send_reconcile_report(client, options, &report)?;
    fs::remove_file(&path)
        .map_err(|error| format!("cannot clear VPN Gate reconcile report: {error}"))
}

/// Upgrade compatibility for an Agent that restarted with reports persisted by the preceding
/// per-pool protocol. Drain those observations before sending the new complete snapshot.
fn resend_pending_legacy_reports(client: &HttpClient, options: &Options) -> Result<(), String> {
    let path = pending_reports_path(&options.state_dir);
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read pending VPN Gate reports: {error}")),
    };
    let reports = serde_json::from_slice::<Vec<VpngatePoolReport>>(&content)
        .map_err(|error| format!("cannot parse pending VPN Gate reports: {error}"))?;
    for report in &reports {
        send_report(client, options, report)?;
    }
    fs::remove_file(&path).map_err(|error| format!("cannot clear VPN Gate reports: {error}"))
}

fn resend_pending_probe_reports(client: &HttpClient, options: &Options) -> Result<(), String> {
    let path = pending_probe_reports_path(&options.state_dir);
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot read pending VPN Gate catalogue probe reports: {error}"
            ))
        }
    };
    let reports = serde_json::from_slice::<Vec<VpngateProbeReport>>(&content).map_err(|error| {
        format!("cannot parse pending VPN Gate catalogue probe reports: {error}")
    })?;
    deliver_persisted_probe_reports(client, options, &path, &reports)
}

fn deliver_persisted_probe_reports(
    client: &HttpClient,
    options: &Options,
    pending_path: &Path,
    reports: &[VpngateProbeReport],
) -> Result<(), String> {
    for report in reports {
        match send_probe_report(client, options, report)? {
            ProbeReportDelivery::Accepted => {}
            ProbeReportDelivery::PermanentlyRejected { status, detail } => {
                let rejected_path = rejected_probe_reports_path(&options.state_dir);
                fs::rename(pending_path, &rejected_path).map_err(|error| {
                    format!(
                        "cannot quarantine permanently rejected VPN Gate catalogue probe reports: {error}"
                    )
                })?;
                let detail = if detail.is_empty() {
                    "response body was empty".to_owned()
                } else {
                    detail
                };
                eprintln!(
                    "vpngate probe: catalogue probe report was permanently rejected with HTTP {status}; quarantined the batch at {}: {detail}",
                    rejected_path.display()
                );
                return Ok(());
            }
        }
    }
    fs::remove_file(pending_path)
        .map_err(|error| format!("cannot clear VPN Gate catalogue probe reports: {error}"))
}

fn resend_pending_intelligence_report(
    client: &HttpClient,
    options: &Options,
) -> Result<(), String> {
    let path = pending_intelligence_report_path(&options.state_dir);
    let content = match fs::read(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot read pending IP intelligence report: {error}"
            ))
        }
    };
    let report = serde_json::from_slice::<VpngateIpIntelligenceReport>(&content)
        .map_err(|error| format!("cannot parse pending IP intelligence report: {error}"))?;
    send_intelligence_report(client, options, &report)?;
    fs::remove_file(&path).map_err(|error| format!("cannot clear IP intelligence report: {error}"))
}

fn resend_pending_catalogue_result(client: &HttpClient, options: &Options) -> Result<(), String> {
    let result_path = pending_catalogue_result_path(&options.state_dir);
    let content = match fs::read(&result_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read pending catalogue result: {error}")),
    };
    let pending = serde_json::from_slice::<PendingCatalogueResult>(&content)
        .map_err(|error| format!("cannot parse pending catalogue result: {error}"))?;
    match &pending {
        PendingCatalogueResult::Snapshot { assignment } => {
            let compressed = fs::read(pending_catalogue_snapshot_path(&options.state_dir))
                .map_err(|error| format!("cannot read pending catalogue snapshot: {error}"))?;
            if compressed.len() > MAX_CATALOG_UPLOAD_BYTES {
                return Err("pending catalogue snapshot exceeds the upload limit".to_owned());
            }
            send_catalogue_snapshot(client, options, assignment, &compressed)?;
        }
        PendingCatalogueResult::Failure { report } => {
            send_catalogue_failure(client, options, report)?;
        }
    }
    clear_pending_catalogue_result(&options.state_dir, &pending)
}

fn clear_pending_catalogue_result(
    state_dir: &Path,
    pending: &PendingCatalogueResult,
) -> Result<(), String> {
    fs::remove_file(pending_catalogue_result_path(state_dir))
        .map_err(|error| format!("cannot clear pending catalogue result: {error}"))?;
    if matches!(pending, PendingCatalogueResult::Snapshot { .. }) {
        match fs::remove_file(pending_catalogue_snapshot_path(state_dir)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot clear pending compressed catalogue snapshot: {error}"
                ))
            }
        }
    }
    Ok(())
}

fn persist_and_send_reconcile_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngateReconcileReport,
) -> Result<(), String> {
    let path = pending_reconcile_report_path(&options.state_dir);
    create_private_dir(path.parent().ok_or("VPN Gate report path has no parent")?)?;
    write_private_atomic(
        &path,
        &serde_json::to_vec(report).map_err(|error| error.to_string())?,
    )?;
    send_reconcile_report(client, options, report)?;
    fs::remove_file(&path)
        .map_err(|error| format!("cannot clear VPN Gate reconcile report: {error}"))
}

fn persist_and_send_probe_reports(
    client: &HttpClient,
    options: &Options,
    reports: Vec<VpngateProbeReport>,
) -> Result<(), String> {
    if reports.is_empty() {
        return Ok(());
    }
    let path = pending_probe_reports_path(&options.state_dir);
    create_private_dir(
        path.parent()
            .ok_or("VPN Gate probe report path has no parent")?,
    )?;
    write_private_atomic(
        &path,
        &serde_json::to_vec(&reports).map_err(|error| error.to_string())?,
    )?;
    deliver_persisted_probe_reports(client, options, &path, &reports)
}

fn persist_and_send_intelligence_report(
    client: &HttpClient,
    options: &Options,
    report: &VpngateIpIntelligenceReport,
) -> Result<(), String> {
    let path = pending_intelligence_report_path(&options.state_dir);
    create_private_dir(
        path.parent()
            .ok_or("VPN Gate intelligence report path has no parent")?,
    )?;
    write_private_atomic(
        &path,
        &serde_json::to_vec(report).map_err(|error| error.to_string())?,
    )?;
    send_intelligence_report(client, options, report)?;
    fs::remove_file(&path).map_err(|error| format!("cannot clear IP intelligence report: {error}"))
}

fn persist_and_send_catalogue_snapshot(
    client: &HttpClient,
    options: &Options,
    assignment: VpngateCatalogSyncAssignment,
    compressed: &[u8],
) -> Result<(), String> {
    let result_path = pending_catalogue_result_path(&options.state_dir);
    create_private_dir(
        result_path
            .parent()
            .ok_or("VPN Gate catalogue result path has no parent")?,
    )?;
    let snapshot_path = pending_catalogue_snapshot_path(&options.state_dir);
    write_private_atomic(&snapshot_path, compressed)?;
    let pending = PendingCatalogueResult::Snapshot { assignment };
    write_private_atomic(
        &result_path,
        &serde_json::to_vec(&pending).map_err(|error| error.to_string())?,
    )?;
    if let PendingCatalogueResult::Snapshot { assignment } = &pending {
        send_catalogue_snapshot(client, options, assignment, compressed)?;
    }
    clear_pending_catalogue_result(&options.state_dir, &pending)
}

fn persist_and_send_catalogue_failure(
    client: &HttpClient,
    options: &Options,
    report: VpngateCatalogSyncFailure,
) -> Result<(), String> {
    let result_path = pending_catalogue_result_path(&options.state_dir);
    create_private_dir(
        result_path
            .parent()
            .ok_or("VPN Gate catalogue result path has no parent")?,
    )?;
    let pending = PendingCatalogueResult::Failure { report };
    write_private_atomic(
        &result_path,
        &serde_json::to_vec(&pending).map_err(|error| error.to_string())?,
    )?;
    if let PendingCatalogueResult::Failure { report } = &pending {
        send_catalogue_failure(client, options, report)?;
    }
    clear_pending_catalogue_result(&options.state_dir, &pending)
}

fn query_ip_intelligence(
    assignment: &brocade_deployment::protocol::VpngateIpIntelligenceAssignment,
) -> Result<VpngateIpIntelligenceReport, String> {
    if assignment.exit_ip.parse::<IpAddr>().is_err() {
        return Err("IP intelligence assignment contains an invalid address".to_owned());
    }
    let exit_ip = assignment.exit_ip.as_str();
    let results = thread::scope(|scope| {
        let proxycheck =
            scope.spawn(|| query_proxycheck(exit_ip, assignment.proxycheck_api_key.as_deref()));
        let ffraud = scope.spawn(|| query_ffraud(exit_ip));
        let iplogs = scope.spawn(|| query_iplogs(exit_ip));
        [
            proxycheck.join().unwrap_or_else(|_| {
                Err(provider_failure(
                    VpngateIpProvider::Proxycheck,
                    "worker-panicked",
                ))
            }),
            ffraud.join().unwrap_or_else(|_| {
                Err(provider_failure(
                    VpngateIpProvider::Ffraud,
                    "worker-panicked",
                ))
            }),
            iplogs.join().unwrap_or_else(|_| {
                Err(provider_failure(
                    VpngateIpProvider::Iplogs,
                    "worker-panicked",
                ))
            }),
        ]
    });
    let mut observations = Vec::with_capacity(3);
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(observation) => observations.push(observation),
            Err(failure) => failures.push(failure),
        }
    }
    observations.sort_by_key(|observation| observation.provider);
    failures.sort_by_key(|failure| failure.provider);
    Ok(VpngateIpIntelligenceReport {
        exit_ip: assignment.exit_ip.clone(),
        lease_generation: assignment.lease_generation,
        observations,
        failures,
    })
}

fn query_proxycheck(
    exit_ip: &str,
    api_key: Option<&str>,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Proxycheck;
    let client = HttpClient::new("https://proxycheck.io")
        .map_err(|_| provider_failure(provider, "client-invalid"))?;
    query_proxycheck_with_client(&client, exit_ip, api_key)
}

fn query_proxycheck_with_client(
    client: &HttpClient,
    exit_ip: &str,
    api_key: Option<&str>,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Proxycheck;
    let path = proxycheck_request_path(exit_ip, api_key);
    let mut response = client
        .request_public("GET", &path, None)
        .map_err(|_| provider_failure(provider, "request-failed"))?;
    // ProxyCheck documents quota exhaustion as a denied 401/403/429 response. Falling back is
    // deliberately narrow: rate limits, disabled keys and malformed requests keep their error
    // instead of silently consuming the smaller unregistered allowance.
    if api_key.is_some() && proxycheck_key_quota_exhausted(response.status, &response.body) {
        response = client
            .request_public("GET", &proxycheck_request_path(exit_ip, None), None)
            .map_err(|_| provider_failure(provider, "request-failed"))?;
    }
    if response.status != 200 {
        return Err(provider_failure(provider, "http-status"));
    }
    let value: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|_| provider_failure(provider, "response-invalid"))?;
    parse_proxycheck_response(exit_ip, &value)
}

fn proxycheck_request_path(exit_ip: &str, api_key: Option<&str>) -> String {
    match api_key {
        Some(key) => format!("/v3/{exit_ip}?key={key}&p=0"),
        None => format!("/v3/{exit_ip}?p=0"),
    }
}

fn proxycheck_key_quota_exhausted(status: u16, body: &str) -> bool {
    if !matches!(status, 401 | 403 | 429) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    if value.get("status").and_then(serde_json::Value::as_str) != Some("denied") {
        return false;
    }
    let Some(message) = value.get("message").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let normalized = message
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect::<String>();
    normalized.contains("1000freequeriesexhausted")
}

fn parse_proxycheck_response(
    exit_ip: &str,
    value: &serde_json::Value,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Proxycheck;
    if !matches!(
        value.get("status").and_then(serde_json::Value::as_str),
        Some("ok" | "warning")
    ) {
        return Err(provider_failure(provider, "provider-rejected"));
    }
    let item = value
        .get(exit_ip)
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let score = json_u8(item.pointer("/detections/risk"))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let country_code = json_country(item.pointer("/location/country_code"))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let isp = json_text(item.pointer("/network/provider"))
        .or_else(|| json_text(item.pointer("/network/organisation")));
    let raw_type = json_text(item.pointer("/network/type"));
    let hosting = item
        .pointer("/detections/hosting")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(VpngateIpIntelligenceObservation {
        provider,
        score,
        country_code,
        isp,
        network_type: if hosting {
            VpngateNetworkType::Datacenter
        } else {
            normalize_network_type(raw_type.as_deref())
        },
    })
}

fn query_ffraud(
    exit_ip: &str,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Ffraud;
    let client = HttpClient::new("https://api.ffraud.com")
        .map_err(|_| provider_failure(provider, "client-invalid"))?;
    let response = client
        .request_public("GET", &format!("/public/ip/{exit_ip}"), None)
        .map_err(|_| provider_failure(provider, "request-failed"))?;
    if response.status != 200 {
        return Err(provider_failure(provider, "http-status"));
    }
    let value: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|_| provider_failure(provider, "response-invalid"))?;
    parse_ffraud_response(&value)
}

fn parse_ffraud_response(
    value: &serde_json::Value,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Ffraud;
    if value.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(provider_failure(provider, "provider-rejected"));
    }
    let score = json_u8(value.get("fraud_score"))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let country_code = json_country(value.pointer("/geo/country"))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let isp = json_text(value.get("ISP"))
        .or_else(|| json_text(value.get("organization")))
        .or_else(|| json_text(value.pointer("/geo/isp")));
    let raw_type = json_text(value.get("connection_type"));
    let hosting = value
        .get("hosting")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mobile = value
        .get("mobile")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(VpngateIpIntelligenceObservation {
        provider,
        score,
        country_code,
        isp,
        network_type: if mobile {
            VpngateNetworkType::Mobile
        } else if hosting {
            VpngateNetworkType::Datacenter
        } else {
            normalize_network_type(raw_type.as_deref())
        },
    })
}

fn query_iplogs(
    exit_ip: &str,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Iplogs;
    let client = HttpClient::new("https://iplogs.com")
        .map_err(|_| provider_failure(provider, "client-invalid"))?;
    let body = serde_json::to_string(&serde_json::json!({ "ip": exit_ip }))
        .map_err(|_| provider_failure(provider, "request-invalid"))?;
    let response = client
        .request_public("POST", "/v1/check", Some(&body))
        .map_err(|_| provider_failure(provider, "request-failed"))?;
    if response.status != 200 {
        return Err(provider_failure(provider, "http-status"));
    }
    let value: serde_json::Value = serde_json::from_str(&response.body)
        .map_err(|_| provider_failure(provider, "response-invalid"))?;
    parse_iplogs_response(&value)
}

fn parse_iplogs_response(
    value: &serde_json::Value,
) -> Result<VpngateIpIntelligenceObservation, VpngateIpIntelligenceFailure> {
    let provider = VpngateIpProvider::Iplogs;
    let raw_score = value
        .get("score")
        .and_then(serde_json::Value::as_f64)
        .filter(|score| (0.0..=1.0).contains(score))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    let score = (raw_score * 100.0).round() as u8;
    let country_code = json_country(value.pointer("/ip_info/country_code"))
        .ok_or_else(|| provider_failure(provider, "response-incomplete"))?;
    Ok(VpngateIpIntelligenceObservation {
        provider,
        score,
        country_code,
        isp: json_text(value.pointer("/ip_info/isp"))
            .or_else(|| json_text(value.pointer("/ip_info/org"))),
        network_type: normalize_network_type(json_text(value.pointer("/ip_info/type")).as_deref()),
    })
}

fn provider_failure(provider: VpngateIpProvider, code: &str) -> VpngateIpIntelligenceFailure {
    VpngateIpIntelligenceFailure {
        provider,
        code: code.to_owned(),
    }
}

fn json_u8(value: Option<&serde_json::Value>) -> Option<u8> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        .filter(|value| *value <= 100)
        .and_then(|value| u8::try_from(value).ok())
}

fn json_country(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?.as_str()?.trim().to_ascii_uppercase();
    valid_country(&value).then_some(value)
}

fn json_text(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()).then(|| value.chars().take(160).collect())
}

fn normalize_network_type(value: Option<&str>) -> VpngateNetworkType {
    let value = value.unwrap_or_default().trim().to_ascii_lowercase();
    if value.contains("data center")
        || value.contains("datacenter")
        || value.contains("hosting")
        || value.contains("cloud")
    {
        VpngateNetworkType::Datacenter
    } else if value.contains("residential") || value.contains("home") {
        VpngateNetworkType::Residential
    } else if value.contains("business") || value.contains("corporate") {
        VpngateNetworkType::Business
    } else if value.contains("mobile") || value.contains("cellular") || value.contains("wireless") {
        VpngateNetworkType::Mobile
    } else if value.contains("relay") || value.contains("proxy") || value.contains("vpn") {
        VpngateNetworkType::Relay
    } else {
        VpngateNetworkType::Unknown
    }
}

fn validate_desired(desired: &VpngateDesiredState) -> Result<(), String> {
    if !validate_vpngate_admission_policy(&desired.admission_policy) {
        return Err("desired VPN Gate admission policy is invalid".to_owned());
    }
    if desired.pools.len() > VPNGATE_RUNTIME_MAX_POOLS_PER_NODE {
        return Err("desired VPN Gate pool count exceeds the runtime limit".to_owned());
    }
    let mut ids = BTreeSet::new();
    for (expected_slot, pool) in desired.pools.iter().enumerate() {
        if !safe_id(&pool.outbound_id) || !ids.insert(pool.outbound_id.as_str()) {
            return Err("desired VPN Gate pool id is unsafe or duplicated".to_owned());
        }
        if usize::from(pool.runtime_slot) != expected_slot
            || pool.prefix_len != 30
            || pool.socks_port != VPNGATE_RUNTIME_SOCKS_PORT
        {
            return Err(format!(
                "VPN Gate pool {} has an invalid runtime slot",
                pool.outbound_id
            ));
        }
        let expected_peer = vpngate_runtime_peer(expected_slot)
            .ok_or("desired VPN Gate runtime slot is outside the compiler range")?;
        let mut host = expected_peer.octets();
        host[3] = host[3]
            .checked_sub(1)
            .ok_or("invalid VPN Gate runtime peer")?;
        if pool.peer_address != expected_peer.to_string()
            || pool.host_address != Ipv4Addr::from(host).to_string()
        {
            return Err(format!(
                "VPN Gate pool {} runtime addresses do not match its slot",
                pool.outbound_id
            ));
        }
        if !valid_country(&pool.country_code)
            || pool.max_connect_ms == 0
            || pool.max_connect_ms > VPNGATE_CONNECT_THRESHOLD_MAX_MS
            || pool.min_download_bps > VPNGATE_DOWNLOAD_THRESHOLD_MAX_BPS
            || pool.max_candidates == 0
            || pool.max_candidates > VPNGATE_MAX_CANDIDATES
            || pool.candidates.len() > usize::from(pool.max_candidates)
            || pool.candidates.len() > MAX_CANDIDATES
        {
            return Err(format!(
                "VPN Gate pool {} selection policy is invalid",
                pool.outbound_id
            ));
        }
        if let Some(command) = &pool.manual_switch {
            if command.request_id == 0
                || !safe_id(&command.previous_server_id)
                || command.cooldown_secs != MANUAL_SWITCH_COOLDOWN_SECS
            {
                return Err(format!(
                    "VPN Gate pool {} manual switch request is invalid",
                    pool.outbound_id
                ));
            }
        }
        let mut candidates = BTreeSet::new();
        for candidate in &pool.candidates {
            if candidate.country_code != pool.country_code
                || !candidates.insert(candidate.server_id.as_str())
            {
                return Err(format!(
                    "VPN Gate pool {} contains a wrong-country or duplicate candidate",
                    pool.outbound_id
                ));
            }
            validate_profile(candidate)?;
        }
    }
    if desired.probe_assignments.len() > MAX_PROBE_ASSIGNMENTS {
        return Err(
            "desired VPN Gate catalogue probe assignment count exceeds the limit".to_owned(),
        );
    }
    let mut countries = BTreeSet::new();
    let mut total_candidates = 0_usize;
    for assignment in &desired.probe_assignments {
        if !valid_country(&assignment.country_code)
            || !countries.insert(assignment.country_code.as_str())
            || assignment.candidates.is_empty()
            || assignment.candidates.len() > MAX_PROBE_CANDIDATES
        {
            return Err("desired VPN Gate catalogue probe assignment is invalid".to_owned());
        }
        total_candidates = total_candidates
            .checked_add(assignment.candidates.len())
            .ok_or_else(|| "desired VPN Gate catalogue probe batch is too large".to_owned())?;
        let mut candidates = BTreeSet::new();
        for candidate in &assignment.candidates {
            if candidate.country_code != assignment.country_code
                || !candidates.insert((
                    candidate.server_id.as_str(),
                    candidate.profile_sha256.as_str(),
                ))
            {
                return Err(
                    "desired VPN Gate catalogue probe contains a wrong-country or duplicate candidate"
                        .to_owned(),
                );
            }
            validate_profile(candidate)?;
        }
    }
    if total_candidates > MAX_TOTAL_PROBE_CANDIDATES {
        return Err("desired VPN Gate catalogue probe batch exceeds the limit".to_owned());
    }
    Ok(())
}

fn validate_profile(candidate: &VpngateCandidate) -> Result<(), String> {
    let has_intelligence = valid_intelligence(
        &candidate.verified_ip_scores,
        &candidate.verified_ip_networks,
    );
    if !safe_id(&candidate.server_id)
        || !valid_country(&candidate.country_code)
        || candidate.openvpn_config.is_empty()
        || candidate.openvpn_config.len() > MAX_PROFILE_BYTES
        || sha256_hex(candidate.openvpn_config.as_bytes()) != candidate.profile_sha256
        || candidate.remote_port == 0
        || candidate.remote_address.parse::<IpAddr>().is_err()
        || candidate.verified_exit_ip.is_some() != has_intelligence
        || (!has_intelligence
            && (!candidate.verified_ip_scores.is_empty()
                || !candidate.verified_ip_networks.is_empty()))
        || candidate
            .verified_exit_ip
            .as_deref()
            .is_some_and(|value| value.parse::<IpAddr>().is_err())
        || candidate
            .verified_exit_country_code
            .as_deref()
            .is_some_and(|value| !valid_country(value))
    {
        return Err(format!(
            "VPN Gate candidate {} has an invalid profile identity",
            candidate.server_id
        ));
    }
    let mut in_block = None::<String>;
    let mut blocks = BTreeSet::new();
    let mut remote_seen = false;
    let mut proto_seen = false;
    let mut script_security_seen = false;
    for raw in candidate.openvpn_config.lines() {
        let line = raw.trim();
        if let Some(block) = &in_block {
            if line.eq_ignore_ascii_case(&format!("</{block}>")) {
                in_block = None;
            } else if line.starts_with('<') || raw.as_bytes().contains(&0) {
                return Err(format!(
                    "VPN Gate candidate {} has a malformed block",
                    candidate.server_id
                ));
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(block) = line
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
        {
            let block = block.to_ascii_lowercase();
            if !matches!(
                block.as_str(),
                "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "tls-crypt-v2"
            ) || !blocks.insert(block.clone())
            {
                return Err(format!(
                    "VPN Gate candidate {} has a forbidden block",
                    candidate.server_id
                ));
            }
            in_block = Some(block);
            continue;
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let name = fields[0].to_ascii_lowercase();
        if !agent_allowed_option(&name) {
            return Err(format!(
                "VPN Gate candidate {} contains forbidden option {name}",
                candidate.server_id
            ));
        }
        match name.as_str() {
            "remote" => {
                if remote_seen || fields.len() != 3 {
                    return Err(format!(
                        "VPN Gate candidate {} has invalid remote",
                        candidate.server_id
                    ));
                }
                remote_seen = true;
                if fields[1] != candidate.remote_address
                    || fields[2].parse::<u16>().ok() != Some(candidate.remote_port)
                {
                    return Err(format!(
                        "VPN Gate candidate {} remote changed",
                        candidate.server_id
                    ));
                }
            }
            "proto" => {
                if proto_seen || fields.len() != 2 {
                    return Err(format!(
                        "VPN Gate candidate {} has invalid proto",
                        candidate.server_id
                    ));
                }
                proto_seen = true;
                let expected = match candidate.transport {
                    VpngateTransport::Udp => "udp",
                    VpngateTransport::Tcp => "tcp-client",
                };
                if fields[1] != expected {
                    return Err(format!(
                        "VPN Gate candidate {} transport changed",
                        candidate.server_id
                    ));
                }
            }
            "script-security" => {
                script_security_seen = fields.as_slice() == ["script-security", "1"];
            }
            _ => {}
        }
    }
    if in_block.is_some()
        || !remote_seen
        || !proto_seen
        || !script_security_seen
        || !["ca", "cert", "key"]
            .iter()
            .all(|block| blocks.contains(*block))
    {
        return Err(format!(
            "VPN Gate candidate {} profile is incomplete",
            candidate.server_id
        ));
    }
    Ok(())
}

fn valid_intelligence(scores: &[VpngateIpScore], networks: &[VpngateIpNetwork]) -> bool {
    let score_providers = scores
        .iter()
        .map(|score| score.provider)
        .collect::<BTreeSet<_>>();
    let network_providers = networks
        .iter()
        .map(|network| network.provider)
        .collect::<BTreeSet<_>>();
    (1..=3).contains(&scores.len())
        && scores.len() == score_providers.len()
        && scores.len() == networks.len()
        && score_providers == network_providers
        && scores
            .iter()
            .all(|score| score.score <= 100 && valid_country(&score.country_code))
}

fn agent_allowed_option(name: &str) -> bool {
    matches!(
        name,
        "client"
            | "dev"
            | "proto"
            | "remote"
            | "nobind"
            | "persist-key"
            | "persist-tun"
            | "auth-nocache"
            | "resolv-retry"
            | "verb"
            | "script-security"
            | "allow-compression"
            | "auth"
            | "cipher"
            | "comp-lzo"
            | "compress"
            | "data-ciphers"
            | "data-ciphers-fallback"
            | "disable-dco"
            | "explicit-exit-notify"
            | "key-direction"
            | "peer-fingerprint"
            | "remote-cert-eku"
            | "remote-cert-ku"
            | "remote-cert-tls"
            | "reneg-sec"
            | "tls-cipher"
            | "tls-ciphersuites"
            | "tls-version-max"
            | "tls-version-min"
            | "verify-x509-name"
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeBackendMetadata {
    slot: u8,
    server_id: String,
    profile_sha256: String,
    peer_address: String,
    /// Time spent establishing this backend. Older metadata omitted it; zero then means unknown,
    /// never an instantaneous connection or a timeout.
    #[serde(default)]
    connect_ms: u32,
    last_verified_unix_secs: i64,
    last_success_unix_millis: i64,
    exit_ip: String,
    consecutive_failures: u8,
    #[serde(default)]
    failure_reason: Option<VpngateFailureReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeServerCooldown {
    server_id: String,
    until_unix_millis: i64,
}

/// One successful end-to-end measurement made by this Agent for one pool candidate.
///
/// Selection compares the arithmetic mean inside the configurable window. Sample count is not a
/// ranking key, so elapsed runtime cannot directly make a server rank better. Samples are bounded
/// per candidate; once the last one expires, that candidate returns to the randomized cold-start
/// group.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeCandidateSample {
    server_id: String,
    profile_sha256: String,
    connect_ms: u32,
    download_bps: u64,
    probed_at_unix_secs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeMetadata {
    outbound_id: String,
    runtime_slot: u16,
    topology_revision: u64,
    catalog_generation: u64,
    active_slot: u8,
    backends: Vec<RuntimeBackendMetadata>,
    #[serde(default)]
    refill_not_before_unix_millis: i64,
    #[serde(default)]
    refill_backoff_secs: u32,
    #[serde(default)]
    last_refill_failed: bool,
    #[serde(default)]
    refill_excluded_server_ids: Vec<String>,
    #[serde(default)]
    last_failover_reason: Option<VpngateFailureReason>,
    #[serde(default)]
    cooldowns: Vec<RuntimeServerCooldown>,
    #[serde(default)]
    candidate_samples: Vec<RuntimeCandidateSample>,
    #[serde(default)]
    last_manual_switch_result: Option<VpngateManualSwitchResult>,
}

#[derive(Debug, Clone, Default)]
struct PoolCounters {
    probes: u64,
    probe_failures: u64,
    failovers: u64,
    refill_attempts: u64,
    refill_failures: u64,
}

#[derive(Debug, Clone)]
struct PreviousPoolState {
    state: VpngateRuntimeState,
    active_slot: Option<u8>,
    active_server: Option<String>,
    backend_servers: BTreeSet<String>,
    failure_started_unix_millis: Option<i64>,
    last_refill_failed: bool,
    reason: Option<VpngateFailureReason>,
}

struct TelemetrySupervisor {
    boot_id: String,
    sequence: u64,
    event_sequence: u64,
    counters: BTreeMap<String, PoolCounters>,
    previous: BTreeMap<String, PreviousPoolState>,
    events: VecDeque<VpngateRuntimeEvent>,
}

impl TelemetrySupervisor {
    fn new() -> Self {
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{}-{}", std::process::id(), unix_millis()));
        Self {
            boot_id,
            sequence: 0,
            event_sequence: 0,
            counters: BTreeMap::new(),
            previous: BTreeMap::new(),
            events: VecDeque::new(),
        }
    }

    fn publish(
        &mut self,
        state_dir: &Path,
        desired: &VpngateDesiredState,
        reports: &[VpngatePoolReport],
        cache: &crate::realtime::VpngateCache,
    ) {
        let now = unix_millis();
        let pools_root = vpngate_root(state_dir).join("pools");
        let reports = reports
            .iter()
            .map(|report| (report.outbound_id.as_str(), report))
            .collect::<BTreeMap<_, _>>();
        let mut pools = Vec::with_capacity(desired.pools.len());
        let mut backends = Vec::with_capacity(desired.pools.len() * RUNTIME_BACKENDS_PER_POOL);

        for pool in &desired.pools {
            let metadata = read_metadata(&pools_root.join(&pool.outbound_id).join("runtime.json"));
            let report_status = reports
                .get(pool.outbound_id.as_str())
                .map(|report| report.runtime_status.as_str())
                .unwrap_or("failed");
            let state = match report_status {
                "running" => VpngateRuntimeState::Healthy,
                "degraded" => VpngateRuntimeState::Degraded,
                "pending" => VpngateRuntimeState::Pending,
                _ => VpngateRuntimeState::Failed,
            };
            let active_slot = metadata.as_ref().map(|metadata| metadata.active_slot);
            let active = metadata.as_ref().and_then(|metadata| {
                metadata
                    .backends
                    .iter()
                    .find(|backend| backend.slot == metadata.active_slot)
            });
            let active_server = active.map(|backend| backend.server_id.clone());
            let reason = active
                .and_then(|backend| backend.failure_reason)
                .or_else(|| {
                    (state == VpngateRuntimeState::Failed)
                        .then_some(VpngateFailureReason::NoCandidate)
                });
            let backend_servers = metadata
                .as_ref()
                .map(|metadata| {
                    metadata
                        .backends
                        .iter()
                        .map(|backend| backend.server_id.clone())
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let previous = self.previous.get(&pool.outbound_id).cloned();
            let counters = self.counters.entry(pool.outbound_id.clone()).or_default();
            let backend_count = metadata
                .as_ref()
                .map(|metadata| metadata.backends.len())
                .unwrap_or_default();
            counters.probes = counters
                .probes
                .saturating_add(u64::try_from(backend_count).unwrap_or(u64::MAX));
            counters.probe_failures = counters.probe_failures.saturating_add(
                metadata
                    .as_ref()
                    .map(|metadata| {
                        u64::try_from(
                            metadata
                                .backends
                                .iter()
                                .filter(|backend| backend.failure_reason.is_some())
                                .count(),
                        )
                        .unwrap_or(u64::MAX)
                    })
                    .unwrap_or_default(),
            );
            let mut pending_events = Vec::new();
            if let Some(previous) = previous.as_ref() {
                if previous.active_server.is_some() && active_server.is_none() {
                    pending_events.push((
                        VpngateRuntimeEventKind::ActiveFailed,
                        previous.active_slot,
                        None,
                        previous.reason,
                        None,
                    ));
                }
                if previous.active_server.is_some()
                    && previous.active_server != active_server
                    && active_server.is_some()
                {
                    counters.failovers = counters.failovers.saturating_add(1);
                    let transition_reason = metadata
                        .as_ref()
                        .and_then(|metadata| metadata.last_failover_reason)
                        .or(previous.reason);
                    let elapsed = previous
                        .failure_started_unix_millis
                        .and_then(|started| u64::try_from(now.saturating_sub(started)).ok());
                    pending_events.push((
                        VpngateRuntimeEventKind::ActiveFailed,
                        previous.active_slot,
                        None,
                        transition_reason,
                        None,
                    ));
                    pending_events.push((
                        VpngateRuntimeEventKind::FailoverStarted,
                        previous.active_slot,
                        active_slot,
                        transition_reason,
                        None,
                    ));
                    pending_events.push((
                        VpngateRuntimeEventKind::FailoverCompleted,
                        previous.active_slot,
                        active_slot,
                        transition_reason,
                        elapsed,
                    ));
                }
                if !previous.backend_servers.is_subset(&backend_servers) {
                    pending_events.push((
                        VpngateRuntimeEventKind::StandbyLost,
                        None,
                        None,
                        None,
                        None,
                    ));
                }
                if !backend_servers.is_subset(&previous.backend_servers) {
                    counters.refill_attempts = counters.refill_attempts.saturating_add(1);
                    pending_events.push((
                        VpngateRuntimeEventKind::RefillStarted,
                        None,
                        None,
                        None,
                        None,
                    ));
                    pending_events.push((
                        VpngateRuntimeEventKind::RefillCompleted,
                        None,
                        None,
                        None,
                        None,
                    ));
                }
                if previous.state != VpngateRuntimeState::Healthy
                    && state == VpngateRuntimeState::Healthy
                {
                    pending_events.push((
                        VpngateRuntimeEventKind::PoolRecovered,
                        None,
                        active_slot,
                        None,
                        None,
                    ));
                }
            }
            let last_refill_failed = metadata
                .as_ref()
                .is_some_and(|metadata| metadata.last_refill_failed);
            if last_refill_failed
                && previous
                    .as_ref()
                    .is_none_or(|previous| !previous.last_refill_failed)
            {
                counters.refill_failures = counters.refill_failures.saturating_add(1);
                pending_events.push((
                    VpngateRuntimeEventKind::RefillFailed,
                    None,
                    None,
                    Some(VpngateFailureReason::NoCandidate),
                    None,
                ));
            }
            let failure_started = if state == VpngateRuntimeState::Healthy {
                None
            } else {
                previous
                    .as_ref()
                    .and_then(|previous| previous.failure_started_unix_millis)
                    .or(Some(now))
            };
            let counters = counters.clone();
            for (kind, from_slot, to_slot, event_reason, recovery_elapsed_millis) in pending_events
            {
                self.push_event(VpngateRuntimeEvent {
                    sequence: 0,
                    at_unix_millis: now,
                    outbound_id: pool.outbound_id.clone(),
                    kind,
                    from_slot,
                    to_slot,
                    reason: event_reason,
                    recovery_elapsed_millis,
                });
            }

            if let Some(metadata) = metadata.as_ref() {
                for backend in &metadata.backends {
                    backends.push(VpngateRealtimeBackend {
                        outbound_id: pool.outbound_id.clone(),
                        slot: backend.slot,
                        role: if backend.slot == metadata.active_slot {
                            VpngateBackendRole::Active
                        } else {
                            VpngateBackendRole::Standby
                        },
                        state: if backend.consecutive_failures == 0 {
                            VpngateBackendState::Healthy
                        } else {
                            VpngateBackendState::Unhealthy
                        },
                        reason: backend.failure_reason,
                        server_id: backend.server_id.clone(),
                        last_success_age_millis: u64::try_from(
                            now.saturating_sub(backend.last_success_unix_millis),
                        )
                        .ok(),
                        consecutive_failures: backend.consecutive_failures,
                        backoff_remaining_millis: 0,
                    });
                }
            }
            let ready_standbys = metadata
                .as_ref()
                .map(|metadata| {
                    metadata
                        .backends
                        .iter()
                        .filter(|backend| {
                            backend.slot != metadata.active_slot
                                && backend.consecutive_failures == 0
                        })
                        .count()
                })
                .and_then(|count| u8::try_from(count).ok())
                .unwrap_or_default();
            pools.push(VpngateRealtimePool {
                outbound_id: pool.outbound_id.clone(),
                country_code: pool.country_code.clone(),
                state,
                reason,
                active_slot,
                ready_standbys,
                candidate_count: u8::try_from(pool.candidates.len()).unwrap_or(u8::MAX),
                consecutive_failures: active
                    .map(|backend| backend.consecutive_failures)
                    .unwrap_or_default(),
                last_success_age_millis: active.and_then(|backend| {
                    u64::try_from(now.saturating_sub(backend.last_success_unix_millis)).ok()
                }),
                probes: counters.probes,
                probe_failures: counters.probe_failures,
                failovers: counters.failovers,
                refill_attempts: counters.refill_attempts,
                refill_failures: counters.refill_failures,
                refill_backoff_remaining_millis: metadata
                    .as_ref()
                    .and_then(|metadata| {
                        u64::try_from(metadata.refill_not_before_unix_millis.saturating_sub(now))
                            .ok()
                    })
                    .unwrap_or_default(),
            });
            self.previous.insert(
                pool.outbound_id.clone(),
                PreviousPoolState {
                    state,
                    active_slot,
                    active_server,
                    backend_servers,
                    failure_started_unix_millis: failure_started,
                    last_refill_failed,
                    reason,
                },
            );
        }
        self.previous.retain(|outbound_id, _| {
            desired
                .pools
                .iter()
                .any(|pool| &pool.outbound_id == outbound_id)
        });
        self.counters.retain(|outbound_id, _| {
            desired
                .pools
                .iter()
                .any(|pool| &pool.outbound_id == outbound_id)
        });
        backends.sort_by(|left, right| {
            left.outbound_id
                .cmp(&right.outbound_id)
                .then_with(|| left.slot.cmp(&right.slot))
        });
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let report = VpngateRealtimeReport {
            boot_id: self.boot_id.clone(),
            sequence: self.sequence,
            sampled_at_unix_millis: now,
            pools,
            backends,
            events: self.events.iter().cloned().collect(),
        };
        if let Ok(mut current) = cache.lock() {
            *current = Some(report);
        }
    }

    fn push_event(&mut self, mut event: VpngateRuntimeEvent) {
        self.event_sequence = self.event_sequence.wrapping_add(1).max(1);
        event.sequence = self.event_sequence;
        self.events.push_back(event);
        while self.events.len() > 32 {
            self.events.pop_front();
        }
    }
}

fn reconcile(
    state_dir: &Path,
    desired: &VpngateDesiredState,
    stats_window: Duration,
) -> Result<Vec<VpngatePoolReport>, String> {
    let root = vpngate_root(state_dir);
    let pools_root = root.join("pools");
    create_private_dir(&pools_root)?;
    cleanup_omitted(&pools_root, desired)?;
    Ok(bounded_parallel_map(
        &desired.pools,
        VPNGATE_RUNTIME_MAX_POOLS_PER_NODE,
        |_worker_slot, pool| match reconcile_pool(&pools_root, desired, pool, stats_window) {
            Ok(report) => report,
            Err(error) => {
                eprintln!("vpngate {}: {error}", pool.outbound_id);
                VpngatePoolReport {
                    topology_revision: desired.topology_revision,
                    catalog_generation: desired.catalog_generation,
                    outbound_id: pool.outbound_id.clone(),
                    runtime_status: "failed".to_owned(),
                    selected_server_id: None,
                    applied_profile_sha256: None,
                    manual_switch_result: None,
                    samples: Vec::new(),
                }
            }
        },
    ))
}

fn reconcile_probe_assignments(
    state_dir: &Path,
    desired: &VpngateDesiredState,
) -> Result<Vec<VpngateProbeReport>, String> {
    if desired.probe_assignments.is_empty() {
        return Ok(Vec::new());
    }
    let probe_dir = vpngate_root(state_dir).join("catalog-probe");
    create_private_dir(&probe_dir)?;
    let host_ip = host_public_ip();
    let jobs = desired
        .probe_assignments
        .iter()
        .enumerate()
        .flat_map(|(assignment_index, assignment)| {
            assignment
                .candidates
                .iter()
                .map(move |candidate| CatalogueProbeJob {
                    assignment_index,
                    candidate,
                })
        })
        .collect::<Vec<_>>();
    let speed_gate = ConcurrencyGate::new(CATALOG_SPEED_WORKERS);
    let completed = bounded_parallel_map(
        &jobs,
        usize::from(CATALOG_PROBE_WORKERS),
        |worker_slot, job| {
            let worker_dir = probe_dir.join(format!("worker-{worker_slot:02}"));
            let sample = match create_private_dir(&worker_dir) {
                Ok(()) => probe_candidate_in_namespace(
                    &worker_dir,
                    &catalog_probe_namespace(worker_slot),
                    job.candidate,
                    host_ip,
                    "catalogue-probe-failed",
                    Some(&speed_gate),
                ),
                Err(error) => failed_sample(job.candidate, "catalogue-probe-failed", &error),
            };
            (job.assignment_index, sample)
        },
    );
    let mut samples_by_assignment = vec![Vec::new(); desired.probe_assignments.len()];
    for (assignment_index, sample) in completed {
        samples_by_assignment[assignment_index].push(sample);
    }
    Ok(desired
        .probe_assignments
        .iter()
        .zip(samples_by_assignment)
        .map(|(assignment, samples)| VpngateProbeReport {
            catalog_generation: desired.catalog_generation,
            country_code: assignment.country_code.clone(),
            samples,
        })
        .collect())
}

#[derive(Clone, Copy)]
struct CatalogueProbeJob<'a> {
    assignment_index: usize,
    candidate: &'a VpngateCandidate,
}

fn bounded_parallel_map<T, R, F>(items: &[T], workers: usize, operation: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let worker_count = workers.max(1).min(items.len());
    let next = AtomicUsize::new(0);
    let completed = Mutex::new(Vec::<(usize, R)>::with_capacity(items.len()));
    thread::scope(|scope| {
        for worker_slot in 0..worker_count {
            let operation = &operation;
            let next = &next;
            let completed = &completed;
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else {
                    break;
                };
                let result = operation(worker_slot, item);
                completed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((index, result));
            });
        }
    });
    let mut completed = completed
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    completed.sort_by_key(|(index, _)| *index);
    completed.into_iter().map(|(_, result)| result).collect()
}

struct ConcurrencyGate {
    available: Mutex<usize>,
    wake: Condvar,
}

impl ConcurrencyGate {
    fn new(limit: usize) -> Self {
        assert!(limit > 0, "concurrency gate requires a positive limit");
        Self {
            available: Mutex::new(limit),
            wake: Condvar::new(),
        }
    }

    fn enter(&self) -> ConcurrencyPermit<'_> {
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *available == 0 {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *available -= 1;
        ConcurrencyPermit { gate: self }
    }
}

struct ConcurrencyPermit<'a> {
    gate: &'a ConcurrencyGate,
}

impl Drop for ConcurrencyPermit<'_> {
    fn drop(&mut self) {
        let mut available = self
            .gate
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *available += 1;
        self.gate.wake.notify_one();
    }
}

fn reconcile_pool(
    pools_root: &Path,
    desired: &VpngateDesiredState,
    pool: &VpngateDesiredPool,
    stats_window: Duration,
) -> Result<VpngatePoolReport, String> {
    let pool_dir = pools_root.join(&pool.outbound_id);
    create_private_dir(&pool_dir)?;
    let metadata_path = pool_dir.join("runtime.json");
    let now = unix_secs();
    let now_millis = unix_millis();
    let mut samples = Vec::new();
    let stored_metadata = read_metadata(&metadata_path);
    let previous_active = stored_metadata
        .as_ref()
        .and_then(|metadata| {
            metadata
                .backends
                .iter()
                .find(|backend| backend.slot == metadata.active_slot)
        })
        .map(|backend| (backend.server_id.clone(), backend.profile_sha256.clone()));
    let had_metadata = stored_metadata.is_some();
    let mut metadata = stored_metadata.unwrap_or_else(|| RuntimeMetadata {
        outbound_id: pool.outbound_id.clone(),
        runtime_slot: pool.runtime_slot,
        topology_revision: desired.topology_revision,
        catalog_generation: desired.catalog_generation,
        active_slot: 0,
        backends: Vec::new(),
        refill_not_before_unix_millis: 0,
        refill_backoff_secs: 0,
        last_refill_failed: false,
        refill_excluded_server_ids: Vec::new(),
        last_failover_reason: None,
        cooldowns: Vec::new(),
        candidate_samples: Vec::new(),
        last_manual_switch_result: None,
    });
    if !had_metadata
        || metadata.outbound_id != pool.outbound_id
        || metadata.runtime_slot != pool.runtime_slot
    {
        cleanup_pool_runtime(&pool_dir, pool)?;
        metadata = RuntimeMetadata {
            outbound_id: pool.outbound_id.clone(),
            runtime_slot: pool.runtime_slot,
            topology_revision: desired.topology_revision,
            catalog_generation: desired.catalog_generation,
            active_slot: 0,
            backends: Vec::new(),
            refill_not_before_unix_millis: 0,
            refill_backoff_secs: 0,
            last_refill_failed: false,
            refill_excluded_server_ids: Vec::new(),
            last_failover_reason: None,
            cooldowns: Vec::new(),
            candidate_samples: Vec::new(),
            last_manual_switch_result: None,
        };
    } else {
        // Upgrade from the former single namespace layout. It cannot coexist with the stable
        // DNAT target used by the two-backend supervisor.
        let _ = cleanup_namespace(&persistent_namespace(pool));
    }

    let stats_window_secs = i64::try_from(stats_window.as_secs())
        .map_err(|_| "VPN Gate local statistics window is too large".to_owned())?;
    retain_fresh_candidate_samples(
        &mut metadata.candidate_samples,
        pool,
        now,
        stats_window_secs,
    );

    metadata
        .cooldowns
        .retain(|cooldown| cooldown.until_unix_millis > now_millis);
    let cooled_server_ids = metadata
        .cooldowns
        .iter()
        .map(|cooldown| cooldown.server_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut retained = Vec::with_capacity(metadata.backends.len());
    let mut backend_slots = BTreeSet::new();
    for backend in metadata.backends.drain(..) {
        let valid_slot = backend.slot < u8::try_from(RUNTIME_BACKENDS_PER_POOL).unwrap_or(u8::MAX);
        let unique_slot = valid_slot && backend_slots.insert(backend.slot);
        let keep = unique_slot
            && !cooled_server_ids.contains(backend.server_id.as_str())
            && backend.peer_address
                == backend_namespace(pool, backend.slot)
                    .peer_address
                    .to_string()
            && pool.candidates.iter().any(|candidate| {
                candidate.server_id == backend.server_id
                    && candidate.profile_sha256 == backend.profile_sha256
                    && candidate_trust_matches(
                        pool,
                        &desired.admission_policy,
                        candidate,
                        &backend.exit_ip,
                    )
            });
        if keep {
            retained.push(backend);
        } else if unique_slot {
            if backend.slot == metadata.active_slot {
                metadata.last_failover_reason = Some(VpngateFailureReason::CandidateRemoved);
            }
            let _ = cleanup_backend_runtime(&pool_dir, pool, backend.slot);
        }
    }
    metadata.backends = retained;

    let backend_process_counts = backend_process_counts(&pool_dir, &metadata.backends);
    let health = bounded_parallel_map(
        &metadata.backends,
        RUNTIME_BACKENDS_PER_POOL,
        |_, backend| {
            if backend.failure_reason == Some(VpngateFailureReason::AdmissionRejected) {
                return (backend.slot, backend.failure_reason);
            }
            let spec = backend_namespace(pool, backend.slot);
            if let Some(reason) = backend_runtime_failure(
                backend_process_counts
                    .get(&backend.slot)
                    .copied()
                    .unwrap_or_default(),
                &spec,
                pool.socks_port,
            ) {
                return (backend.slot, Some(reason));
            }
            let reason = (!egress_reachable(&backend.peer_address, pool.socks_port))
                .then_some(brocade_deployment::protocol::VpngateFailureReason::EgressUnreachable);
            (backend.slot, reason)
        },
    );
    for (slot, reason) in health {
        let Some(backend) = metadata
            .backends
            .iter_mut()
            .find(|backend| backend.slot == slot)
        else {
            continue;
        };
        if reason.is_some() {
            backend.consecutive_failures = if matches!(
                reason,
                Some(VpngateFailureReason::ProcessExited | VpngateFailureReason::SocksUnavailable)
            ) {
                HEALTH_FAILURE_THRESHOLD
            } else {
                backend.consecutive_failures.saturating_add(1)
            };
            backend.failure_reason = reason;
        } else {
            backend.consecutive_failures = 0;
            backend.failure_reason = None;
            backend.last_success_unix_millis = now_millis;
        }
    }

    let active_failed = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
        .is_none_or(|backend| backend.consecutive_failures >= HEALTH_FAILURE_THRESHOLD);
    let active_failure_reason = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
        .and_then(|backend| backend.failure_reason)
        .or(metadata.last_failover_reason);
    let mut did_failover = false;
    if active_failed {
        if let Some(standby) = metadata.backends.iter().find(|backend| {
            backend.slot != metadata.active_slot && backend.consecutive_failures == 0
        }) {
            set_active_backend(pool, standby)?;
            metadata.active_slot = standby.slot;
            metadata.last_failover_reason = active_failure_reason;
            did_failover = true;
            metadata.topology_revision = desired.topology_revision;
            metadata.catalog_generation = desired.catalog_generation;
            write_metadata(&metadata_path, &metadata)?;
        }
    }

    if active_failed && !did_failover {
        let failed_active = metadata.active_slot;
        metadata.refill_excluded_server_ids.extend(
            metadata
                .backends
                .iter()
                .filter(|backend| backend.slot == failed_active)
                .map(|backend| backend.server_id.clone()),
        );
        metadata
            .backends
            .retain(|backend| backend.slot != failed_active);
        let _ = cleanup_backend_runtime(&pool_dir, pool, failed_active);
    }

    let failed_slots = metadata
        .backends
        .iter()
        .filter(|backend| {
            backend.consecutive_failures >= HEALTH_FAILURE_THRESHOLD
                && backend.slot != metadata.active_slot
        })
        .map(|backend| backend.slot)
        .collect::<Vec<_>>();
    metadata.refill_excluded_server_ids.extend(
        metadata
            .backends
            .iter()
            .filter(|backend| failed_slots.contains(&backend.slot))
            .map(|backend| backend.server_id.clone()),
    );
    metadata
        .backends
        .retain(|backend| !failed_slots.contains(&backend.slot));
    for slot in failed_slots {
        let _ = cleanup_backend_runtime(&pool_dir, pool, slot);
    }

    if let Some(command) = pool.manual_switch.as_ref() {
        if metadata
            .last_manual_switch_result
            .as_ref()
            .is_none_or(|result| result.request_id != command.request_id)
        {
            did_failover = apply_manual_switch(
                &pool_dir,
                &metadata_path,
                desired,
                pool,
                command,
                &mut metadata,
                &mut samples,
                now_millis,
            )?;
        }
    }

    if metadata.last_refill_failed && now_millis >= metadata.refill_not_before_unix_millis {
        metadata.refill_excluded_server_ids.clear();
    }
    let before_refill = metadata.backends.len();
    while metadata.backends.len() < RUNTIME_BACKENDS_PER_POOL
        && now_millis >= metadata.refill_not_before_unix_millis
        && !did_failover
    {
        let slot = (0..u8::try_from(RUNTIME_BACKENDS_PER_POOL).unwrap_or_default())
            .find(|slot| {
                metadata
                    .backends
                    .iter()
                    .all(|backend| backend.slot != *slot)
            })
            .ok_or("VPN Gate backend slot accounting is inconsistent")?;
        let mut excluded = metadata
            .backends
            .iter()
            .map(|backend| backend.server_id.clone())
            .chain(metadata.refill_excluded_server_ids.iter().cloned())
            .chain(
                metadata
                    .cooldowns
                    .iter()
                    .map(|cooldown| cooldown.server_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        let Some(backend) = start_verified_backend(
            &pool_dir,
            pool,
            &desired.admission_policy,
            slot,
            &mut excluded,
            &mut metadata.candidate_samples,
            &mut samples,
        )?
        else {
            metadata.refill_excluded_server_ids = excluded.into_iter().collect();
            break;
        };
        if metadata.backends.is_empty() {
            set_active_backend(pool, &backend)?;
            metadata.active_slot = backend.slot;
        }
        metadata.backends.push(backend);
        metadata.refill_excluded_server_ids = excluded.into_iter().collect();
    }
    if metadata.backends.len() == RUNTIME_BACKENDS_PER_POOL {
        metadata.refill_backoff_secs = 0;
        metadata.refill_not_before_unix_millis = 0;
        metadata.last_refill_failed = false;
        metadata.refill_excluded_server_ids.clear();
    } else if now_millis >= metadata.refill_not_before_unix_millis && !did_failover {
        metadata.last_refill_failed = metadata.backends.len() == before_refill;
        metadata.refill_backoff_secs = if metadata.refill_backoff_secs == 0 {
            10
        } else {
            metadata.refill_backoff_secs.saturating_mul(2).min(300)
        };
        metadata.refill_not_before_unix_millis = now_millis
            .saturating_add(i64::from(metadata.refill_backoff_secs).saturating_mul(1_000));
    }

    if metadata
        .backends
        .iter()
        .all(|backend| backend.slot != metadata.active_slot)
    {
        if let Some(backend) = metadata.backends.first() {
            set_active_backend(pool, backend)?;
            metadata.active_slot = backend.slot;
        }
    }

    // Reassert the stable endpoint on every convergence pass. Besides selecting a backend during
    // startup and failover, this repairs an externally removed route or NAT rule within the next
    // five-second health interval without waiting for another backend switch.
    if let Some(active) = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
    {
        set_active_backend(pool, active)?;
    }

    let current_active = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
        .map(|backend| (backend.server_id.clone(), backend.profile_sha256.clone()));
    let active_changed_this_pass = current_active != previous_active;
    // Always carry the active backend's freshest local sample in the converged snapshot. Reports
    // are content-deduplicated by the runtime loop and samples are idempotent in the Console, so
    // this heals a previously missed upload without creating periodic writes.
    if let Some(sample) = cached_active_sample(pool, &metadata, &samples) {
        samples.push(sample);
    } else if active_changed_this_pass && current_active.is_some() {
        // Switching the stable route must stay fast. If there is no cached sample, make the new
        // active backend immediately due for a full probe on the next five-second pass instead of
        // blocking this switch for the probe timeout.
        if !samples.iter().any(|sample| {
            current_active
                .as_ref()
                .is_some_and(|(server_id, profile_sha256)| {
                    sample.status == VpngateProbeStatus::Succeeded
                        && sample.server_id == *server_id
                        && sample.profile_sha256 == *profile_sha256
                })
        }) {
            if let Some(active) = metadata
                .backends
                .iter_mut()
                .find(|backend| backend.slot == metadata.active_slot)
            {
                active.last_verified_unix_secs = 0;
            }
        }
    }

    // A full exit/risk/speed check remains periodic, while the five-second health loop uses only
    // tiny requests. Refresh at most one backend per pass so maintenance cannot double its cost.
    // A just-completed switch deliberately skips this block; a missing cached sample was marked
    // due above and is probed on the next pass without extending the switching critical path.
    if !active_changed_this_pass {
        if let Some(backend_index) = next_full_probe_index(&metadata, now) {
            let backend = &mut metadata.backends[backend_index];
            if let Some(candidate) = pool.candidates.iter().find(|candidate| {
                candidate.server_id == backend.server_id
                    && candidate.profile_sha256 == backend.profile_sha256
            }) {
                match probe_route(
                    CurlRoute::Socks(&backend.peer_address, pool.socks_port),
                    host_public_ip(),
                    backend.connect_ms,
                    candidate,
                    None,
                ) {
                    Ok(sample) if sample_meets_policy(pool, &desired.admission_policy, &sample) => {
                        backend.last_verified_unix_secs = sample.probed_at_unix_secs;
                        backend.last_success_unix_millis = now_millis;
                        backend.exit_ip = sample.exit_ip.clone().unwrap_or_default();
                        record_candidate_sample(
                            &mut metadata.candidate_samples,
                            candidate,
                            &sample,
                        );
                        samples.push(sample);
                    }
                    Ok(sample) => {
                        backend.consecutive_failures = HEALTH_FAILURE_THRESHOLD;
                        backend.failure_reason = Some(VpngateFailureReason::AdmissionRejected);
                        samples.push(sample);
                    }
                    Err(_) => {
                        backend.consecutive_failures =
                            backend.consecutive_failures.saturating_add(1);
                        backend.failure_reason = Some(VpngateFailureReason::EgressUnreachable);
                    }
                }
            }
        }
    }

    metadata.topology_revision = desired.topology_revision;
    metadata.catalog_generation = desired.catalog_generation;
    if metadata.backends.is_empty() {
        cleanup_pool_routing(pool);
        let _ = fs::remove_file(&metadata_path);
        return Ok(VpngatePoolReport {
            topology_revision: desired.topology_revision,
            catalog_generation: desired.catalog_generation,
            outbound_id: pool.outbound_id.clone(),
            runtime_status: if pool.candidates.is_empty() {
                "pending".to_owned()
            } else {
                "failed".to_owned()
            },
            selected_server_id: None,
            applied_profile_sha256: None,
            manual_switch_result: metadata.last_manual_switch_result,
            samples,
        });
    }

    write_metadata(&metadata_path, &metadata)?;
    let active_degraded = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
        .is_some_and(|backend| backend.consecutive_failures > 0);
    let status = if metadata.backends.len() == RUNTIME_BACKENDS_PER_POOL && !active_degraded {
        "running"
    } else {
        "degraded"
    };
    Ok(report_for_metadata(
        desired, pool, metadata, samples, status,
    ))
}

fn start_verified_backend(
    pool_dir: &Path,
    pool: &VpngateDesiredPool,
    admission_policy: &brocade_deployment::protocol::VpngateAdmissionPolicy,
    slot: u8,
    excluded: &mut BTreeSet<String>,
    candidate_samples: &mut Vec<RuntimeCandidateSample>,
    samples: &mut Vec<VpngateProbeSample>,
) -> Result<Option<RuntimeBackendMetadata>, String> {
    let host_ip = host_public_ip();
    let candidates = refill_candidates(pool, excluded, candidate_samples)?;
    for candidate in candidates {
        let connect_ms = match start_backend(pool_dir, pool, slot, candidate) {
            Ok(connect_ms) => connect_ms,
            Err(error) => {
                samples.push(failed_sample(candidate, "runtime-start-failed", &error));
                excluded.insert(candidate.server_id.clone());
                continue;
            }
        };
        let spec = backend_namespace(pool, slot);
        let peer_address = spec.peer_address.to_string();
        match probe_route(
            CurlRoute::Socks(&peer_address, pool.socks_port),
            host_ip,
            connect_ms,
            candidate,
            None,
        ) {
            Ok(sample) if sample_meets_policy(pool, admission_policy, &sample) => {
                let backend = RuntimeBackendMetadata {
                    slot,
                    server_id: candidate.server_id.clone(),
                    profile_sha256: candidate.profile_sha256.clone(),
                    peer_address: spec.peer_address.to_string(),
                    connect_ms,
                    last_verified_unix_secs: sample.probed_at_unix_secs,
                    last_success_unix_millis: unix_millis(),
                    exit_ip: sample.exit_ip.clone().unwrap_or_default(),
                    consecutive_failures: 0,
                    failure_reason: None,
                };
                record_candidate_sample(candidate_samples, candidate, &sample);
                samples.push(sample);
                return Ok(Some(backend));
            }
            Ok(sample) => samples.push(sample),
            Err(error) => samples.push(failed_sample(candidate, "runtime-failed", &error)),
        }
        excluded.insert(candidate.server_id.clone());
        let _ = cleanup_backend_runtime(pool_dir, pool, slot);
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn apply_manual_switch(
    pool_dir: &Path,
    metadata_path: &Path,
    desired: &VpngateDesiredState,
    pool: &VpngateDesiredPool,
    command: &VpngateManualSwitchCommand,
    metadata: &mut RuntimeMetadata,
    samples: &mut Vec<VpngateProbeSample>,
    now_millis: i64,
) -> Result<bool, String> {
    let Some(active) = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)
        .cloned()
    else {
        metadata.last_manual_switch_result = Some(VpngateManualSwitchResult {
            request_id: command.request_id,
            status: VpngateManualSwitchStatus::Failed,
            previous_server_id: command.previous_server_id.clone(),
            selected_server_id: None,
            cooldown_until_unix_secs: None,
            error_detail: Some("当前没有可切换的主节点".to_owned()),
        });
        write_metadata(metadata_path, metadata)?;
        return Ok(false);
    };

    if active.server_id != command.previous_server_id {
        let cooldown_until = manual_switch_cooldown_until(now_millis, command.cooldown_secs);
        add_runtime_cooldown(metadata, &command.previous_server_id, cooldown_until);
        let removed_slots = metadata
            .backends
            .iter()
            .filter(|backend| backend.server_id == command.previous_server_id)
            .map(|backend| backend.slot)
            .collect::<Vec<_>>();
        metadata
            .backends
            .retain(|backend| backend.server_id != command.previous_server_id);
        metadata.last_manual_switch_result = Some(VpngateManualSwitchResult {
            request_id: command.request_id,
            status: VpngateManualSwitchStatus::Applied,
            previous_server_id: command.previous_server_id.clone(),
            selected_server_id: Some(active.server_id),
            cooldown_until_unix_secs: Some(cooldown_until / 1_000),
            error_detail: None,
        });
        metadata.topology_revision = desired.topology_revision;
        metadata.catalog_generation = desired.catalog_generation;
        write_metadata(metadata_path, metadata)?;
        for slot in removed_slots {
            let _ = cleanup_backend_runtime(pool_dir, pool, slot);
        }
        return Ok(true);
    }

    let mut replacement = metadata
        .backends
        .iter()
        .find(|backend| backend.slot != active.slot && backend.consecutive_failures == 0)
        .cloned();
    if replacement.is_none() {
        let slot = (0..u8::try_from(RUNTIME_BACKENDS_PER_POOL).unwrap_or_default())
            .find(|slot| {
                metadata
                    .backends
                    .iter()
                    .all(|backend| backend.slot != *slot)
            })
            .ok_or("VPN Gate backend slot accounting is inconsistent")?;
        let mut excluded = metadata
            .backends
            .iter()
            .map(|backend| backend.server_id.clone())
            .chain(metadata.refill_excluded_server_ids.iter().cloned())
            .chain(
                metadata
                    .cooldowns
                    .iter()
                    .map(|cooldown| cooldown.server_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        replacement = start_verified_backend(
            pool_dir,
            pool,
            &desired.admission_policy,
            slot,
            &mut excluded,
            &mut metadata.candidate_samples,
            samples,
        )?;
        metadata.refill_excluded_server_ids = excluded.into_iter().collect();
        if let Some(backend) = replacement.clone() {
            metadata.backends.push(backend);
        }
    }
    let Some(replacement) = replacement else {
        metadata.last_manual_switch_result = Some(VpngateManualSwitchResult {
            request_id: command.request_id,
            status: VpngateManualSwitchStatus::Failed,
            previous_server_id: command.previous_server_id.clone(),
            selected_server_id: None,
            cooldown_until_unix_secs: None,
            error_detail: Some("没有通过验证的可用替代节点，已保持当前节点".to_owned()),
        });
        write_metadata(metadata_path, metadata)?;
        return Ok(false);
    };

    set_active_backend(pool, &replacement)?;
    metadata.active_slot = replacement.slot;
    metadata.last_failover_reason = None;
    let cooldown_until = manual_switch_cooldown_until(now_millis, command.cooldown_secs);
    add_runtime_cooldown(metadata, &active.server_id, cooldown_until);
    metadata
        .backends
        .retain(|backend| backend.server_id != active.server_id);
    metadata.last_manual_switch_result = Some(VpngateManualSwitchResult {
        request_id: command.request_id,
        status: VpngateManualSwitchStatus::Applied,
        previous_server_id: active.server_id,
        selected_server_id: Some(replacement.server_id),
        cooldown_until_unix_secs: Some(cooldown_until / 1_000),
        error_detail: None,
    });
    metadata.topology_revision = desired.topology_revision;
    metadata.catalog_generation = desired.catalog_generation;
    write_metadata(metadata_path, metadata)?;
    let _ = cleanup_backend_runtime(pool_dir, pool, active.slot);
    Ok(true)
}

fn manual_switch_cooldown_until(now_millis: i64, cooldown_secs: u32) -> i64 {
    now_millis.saturating_add(i64::from(cooldown_secs).saturating_mul(1_000))
}

fn add_runtime_cooldown(metadata: &mut RuntimeMetadata, server_id: &str, until_unix_millis: i64) {
    metadata
        .cooldowns
        .retain(|cooldown| cooldown.server_id != server_id);
    metadata.cooldowns.push(RuntimeServerCooldown {
        server_id: server_id.to_owned(),
        until_unix_millis,
    });
}

fn refill_candidates<'a>(
    pool: &'a VpngateDesiredPool,
    excluded: &BTreeSet<String>,
    candidate_samples: &[RuntimeCandidateSample],
) -> Result<Vec<&'a VpngateCandidate>, String> {
    refill_candidates_with_random(pool, excluded, candidate_samples, random_u64)
}

fn refill_candidates_with_random<'a>(
    pool: &'a VpngateDesiredPool,
    excluded: &BTreeSet<String>,
    candidate_samples: &[RuntimeCandidateSample],
    mut random_key: impl FnMut() -> Result<u64, String>,
) -> Result<Vec<&'a VpngateCandidate>, String> {
    let mut candidates = Vec::with_capacity(pool.candidates.len());
    for candidate in pool
        .candidates
        .iter()
        .filter(|candidate| !excluded.contains(&candidate.server_id))
    {
        let quality = local_candidate_quality(candidate, candidate_samples);
        let cold_start_key = if quality.is_none() { random_key()? } else { 0 };
        candidates.push((candidate, quality, cold_start_key));
    }
    candidates.sort_by(|left, right| match (left.1, right.1) {
        (Some(left_quality), Some(right_quality)) => right_quality
            .0
            .cmp(&left_quality.0)
            .then_with(|| left_quality.1.cmp(&right_quality.1))
            .then_with(|| left.0.server_id.cmp(&right.0.server_id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left
            .2
            .cmp(&right.2)
            .then_with(|| left.0.server_id.cmp(&right.0.server_id)),
    });
    Ok(candidates
        .into_iter()
        .map(|(candidate, _, _)| candidate)
        .collect())
}

fn local_candidate_quality(
    candidate: &VpngateCandidate,
    candidate_samples: &[RuntimeCandidateSample],
) -> Option<(u64, u32)> {
    let matching = candidate_samples.iter().filter(|sample| {
        sample.server_id == candidate.server_id && sample.profile_sha256 == candidate.profile_sha256
    });
    let mut count = 0_u64;
    let mut download_sum = 0_u128;
    let mut connect_sum = 0_u64;
    for sample in matching {
        count = count.saturating_add(1);
        download_sum = download_sum.saturating_add(u128::from(sample.download_bps));
        connect_sum = connect_sum.saturating_add(u64::from(sample.connect_ms));
    }
    if count == 0 {
        return None;
    }
    let average_download = u64::try_from(download_sum / u128::from(count)).unwrap_or(u64::MAX);
    let average_connect = u32::try_from(connect_sum / count).unwrap_or(u32::MAX);
    Some((average_download, average_connect))
}

fn random_u64() -> Result<u64, String> {
    let mut bytes = [0_u8; std::mem::size_of::<u64>()];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("cannot randomize VPN Gate cold-start candidates: {error}"))?;
    Ok(u64::from_ne_bytes(bytes))
}

fn retain_fresh_candidate_samples(
    samples: &mut Vec<RuntimeCandidateSample>,
    pool: &VpngateDesiredPool,
    now_unix_secs: i64,
    window_secs: i64,
) {
    let mut by_candidate = BTreeMap::<(String, String), Vec<RuntimeCandidateSample>>::new();
    for sample in samples.drain(..) {
        let still_a_candidate = pool.candidates.iter().any(|candidate| {
            candidate.server_id == sample.server_id
                && candidate.profile_sha256 == sample.profile_sha256
        });
        let fresh = sample.probed_at_unix_secs <= now_unix_secs
            && now_unix_secs.saturating_sub(sample.probed_at_unix_secs) <= window_secs;
        if !still_a_candidate || !fresh || sample.connect_ms == 0 || sample.download_bps == 0 {
            continue;
        }
        let key = (sample.server_id.clone(), sample.profile_sha256.clone());
        by_candidate.entry(key).or_default().push(sample);
    }
    for candidate_samples in by_candidate.values_mut() {
        candidate_samples.sort_by(|left, right| {
            left.probed_at_unix_secs
                .cmp(&right.probed_at_unix_secs)
                .then_with(|| left.download_bps.cmp(&right.download_bps))
                .then_with(|| left.connect_ms.cmp(&right.connect_ms))
        });
        if candidate_samples.len() > MAX_LOCAL_SAMPLES_PER_CANDIDATE {
            candidate_samples.drain(
                ..candidate_samples
                    .len()
                    .saturating_sub(MAX_LOCAL_SAMPLES_PER_CANDIDATE),
            );
        }
    }
    *samples = by_candidate.into_values().flatten().collect();
}

fn record_candidate_sample(
    samples: &mut Vec<RuntimeCandidateSample>,
    candidate: &VpngateCandidate,
    sample: &VpngateProbeSample,
) {
    let (Some(connect_ms), Some(download_bps)) = (sample.connect_ms, sample.download_bps) else {
        return;
    };
    if sample.status != VpngateProbeStatus::Succeeded || connect_ms == 0 || download_bps == 0 {
        return;
    }
    samples.push(RuntimeCandidateSample {
        server_id: candidate.server_id.clone(),
        profile_sha256: candidate.profile_sha256.clone(),
        connect_ms,
        download_bps,
        probed_at_unix_secs: sample.probed_at_unix_secs,
    });
    while samples
        .iter()
        .filter(|sample| {
            sample.server_id == candidate.server_id
                && sample.profile_sha256 == candidate.profile_sha256
        })
        .count()
        > MAX_LOCAL_SAMPLES_PER_CANDIDATE
    {
        let Some((oldest_index, _)) = samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| {
                sample.server_id == candidate.server_id
                    && sample.profile_sha256 == candidate.profile_sha256
            })
            .min_by_key(|(_, sample)| sample.probed_at_unix_secs)
        else {
            break;
        };
        samples.remove(oldest_index);
    }
}

fn cached_active_sample(
    pool: &VpngateDesiredPool,
    metadata: &RuntimeMetadata,
    pending_samples: &[VpngateProbeSample],
) -> Option<VpngateProbeSample> {
    let active = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot)?;
    if pending_samples.iter().any(|sample| {
        sample.status == VpngateProbeStatus::Succeeded
            && sample.server_id == active.server_id
            && sample.profile_sha256 == active.profile_sha256
    }) {
        return None;
    }
    let sample = metadata
        .candidate_samples
        .iter()
        .filter(|sample| {
            sample.server_id == active.server_id && sample.profile_sha256 == active.profile_sha256
        })
        .max_by_key(|sample| sample.probed_at_unix_secs)?;
    let candidate = pool.candidates.iter().find(|candidate| {
        candidate.server_id == active.server_id && candidate.profile_sha256 == active.profile_sha256
    })?;
    let exit_ip = active.exit_ip.parse::<IpAddr>().ok()?.to_string();
    let trusted = candidate.verified_exit_ip.as_deref() == Some(exit_ip.as_str());
    Some(VpngateProbeSample {
        server_id: active.server_id.clone(),
        profile_sha256: active.profile_sha256.clone(),
        status: VpngateProbeStatus::Succeeded,
        exit_ip: Some(exit_ip),
        exit_country_code: trusted
            .then(|| candidate.verified_exit_country_code.clone())
            .flatten(),
        connect_ms: Some(sample.connect_ms),
        download_bps: Some(sample.download_bps),
        ip_scores: if trusted {
            candidate.verified_ip_scores.clone()
        } else {
            Vec::new()
        },
        ip_networks: if trusted {
            candidate.verified_ip_networks.clone()
        } else {
            Vec::new()
        },
        error_code: None,
        error_detail: None,
        probed_at_unix_secs: sample.probed_at_unix_secs,
    })
}

fn next_full_probe_index(metadata: &RuntimeMetadata, now_unix_secs: i64) -> Option<usize> {
    metadata
        .backends
        .iter()
        .position(|backend| {
            backend.slot == metadata.active_slot
                && backend.consecutive_failures == 0
                && backend.last_verified_unix_secs == 0
        })
        .or_else(|| {
            metadata.backends.iter().position(|backend| {
                backend.consecutive_failures == 0
                    && now_unix_secs.saturating_sub(backend.last_verified_unix_secs)
                        >= FULL_PROBE_INTERVAL_SECS
            })
        })
}

fn sample_meets_policy(
    pool: &VpngateDesiredPool,
    admission_policy: &brocade_deployment::protocol::VpngateAdmissionPolicy,
    sample: &VpngateProbeSample,
) -> bool {
    sample.status == VpngateProbeStatus::Succeeded
        && sample
            .connect_ms
            .is_some_and(|value| value > 0 && value <= pool.max_connect_ms)
        && sample
            .download_bps
            .is_some_and(|value| value >= pool.min_download_bps)
        && evaluate_vpngate_admission(admission_policy, &pool.country_code, &sample.ip_scores)
            == VpngateAdmissionDecision::Admitted
}

fn candidate_trust_matches(
    pool: &VpngateDesiredPool,
    admission_policy: &brocade_deployment::protocol::VpngateAdmissionPolicy,
    candidate: &VpngateCandidate,
    exit_ip: &str,
) -> bool {
    candidate.verified_exit_ip.as_deref() == Some(exit_ip)
        && evaluate_vpngate_admission(
            admission_policy,
            &pool.country_code,
            &candidate.verified_ip_scores,
        ) == VpngateAdmissionDecision::Admitted
}

fn report_for_metadata(
    desired: &VpngateDesiredState,
    pool: &VpngateDesiredPool,
    metadata: RuntimeMetadata,
    samples: Vec<VpngateProbeSample>,
    status: &str,
) -> VpngatePoolReport {
    let active = metadata
        .backends
        .iter()
        .find(|backend| backend.slot == metadata.active_slot);
    VpngatePoolReport {
        topology_revision: desired.topology_revision,
        catalog_generation: desired.catalog_generation,
        outbound_id: pool.outbound_id.clone(),
        runtime_status: status.to_owned(),
        selected_server_id: active.map(|backend| backend.server_id.clone()),
        applied_profile_sha256: active.map(|backend| backend.profile_sha256.clone()),
        manual_switch_result: metadata.last_manual_switch_result,
        samples,
    }
}

fn probe_candidate_in_namespace(
    directory: &Path,
    namespace: &NamespaceSpec,
    candidate: &VpngateCandidate,
    host_ip: Option<IpAddr>,
    failure_code: &str,
    speed_gate: Option<&ConcurrencyGate>,
) -> VpngateProbeSample {
    let profile = directory.join("probe.ovpn");
    let result = (|| {
        cleanup_namespace(namespace)?;
        setup_namespace(namespace)?;
        write_private_atomic(&profile, candidate.openvpn_config.as_bytes())?;
        let started = Instant::now();
        let (mut child, ready) = spawn_openvpn(&namespace.name, &profile, true)?;
        if let Err(error) = wait_openvpn(&mut child, &ready) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let connect_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let sample = probe_route(
            CurlRoute::Namespace(&namespace.name),
            host_ip,
            connect_ms,
            candidate,
            speed_gate,
        );
        let _ = child.kill();
        let _ = child.wait();
        sample
    })();
    let _ = cleanup_namespace(namespace);
    let _ = fs::remove_file(profile);
    match result {
        Ok(sample) => sample,
        Err(error) => failed_sample(candidate, failure_code, &error),
    }
}

fn start_backend(
    pool_dir: &Path,
    pool: &VpngateDesiredPool,
    slot: u8,
    candidate: &VpngateCandidate,
) -> Result<u32, String> {
    let namespace = backend_namespace(pool, slot);
    let result = (|| {
        cleanup_backend_runtime(pool_dir, pool, slot)?;
        setup_namespace(&namespace)?;
        let profile = pool_dir.join(format!("profile-{slot}.ovpn"));
        write_private_atomic(&profile, candidate.openvpn_config.as_bytes())?;
        let started = Instant::now();
        let (mut openvpn, ready) = spawn_openvpn(&namespace.name, &profile, false)?;
        if let Err(error) = wait_openvpn(&mut openvpn, &ready) {
            let _ = openvpn.kill();
            let _ = openvpn.wait();
            return Err(error);
        }
        reap_in_background(openvpn);
        let connect_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        let helper_config = pool_dir.join(format!("socks-xray-{slot}.json"));
        let config = serde_json::json!({
            "log": { "loglevel": "warning" },
            "inbounds": [{
                "tag": "vpngate-socks",
                "listen": namespace.peer_address,
                "port": pool.socks_port,
                "protocol": "socks",
                "settings": { "udp": true }
            }],
            "outbounds": [{ "tag": "direct", "protocol": "freedom" }]
        });
        write_private_atomic(
            &helper_config,
            &serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?,
        )?;
        let xray = crate::options::xray_binary_path();
        let test = command_output(
            Command::new("ip")
                .args(["netns", "exec", &namespace.name])
                .arg(&xray)
                .args(["-test", "-config"])
                .arg(&helper_config),
        )?;
        if !test.status.success() {
            return Err("VPN Gate helper Xray rejected its generated config".to_owned());
        }
        let child = Command::new("ip")
            .args(["netns", "exec", &namespace.name])
            .arg(&xray)
            .args(["run", "-config"])
            .arg(&helper_config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start VPN Gate helper Xray: {error}"))?;
        reap_in_background(child);
        let deadline = Instant::now() + HELPER_START_TIMEOUT;
        while Instant::now() < deadline {
            if tcp_reachable(&namespace.peer_address.to_string(), pool.socks_port) {
                return Ok(connect_ms);
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err("VPN Gate helper SOCKS listener did not become ready".to_owned())
    })();
    if result.is_err() {
        let _ = cleanup_backend_runtime(pool_dir, pool, slot);
    }
    result
}

fn spawn_openvpn(
    namespace: &str,
    profile: &Path,
    one_attempt: bool,
) -> Result<(Child, Arc<AtomicBool>), String> {
    let mut command = Command::new("ip");
    command
        .args(["netns", "exec", namespace, "openvpn", "--config"])
        .arg(profile);
    if openvpn_has_dns_updown()? {
        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot locate the Brocade Agent executable: {error}"))?;
        let resolver = namespace_resolver_file(namespace)?;
        let hook = openvpn_dns_hook_command(&executable, &resolver)?;
        // The catalogue sanitizer removes every provider-controlled executable option. Raising
        // this to level 2 therefore authorizes only the command supplied here, after the profile,
        // while retaining OpenVPN's DNS up/down lifecycle.
        command
            .args(["--script-security", "2", "--dns-updown"])
            .arg(hook);
    }
    if one_attempt {
        command.args(["--connect-retry-max", "1"]);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start OpenVPN: {error}"))?;
    let ready = Arc::new(AtomicBool::new(false));
    if let Some(stdout) = child.stdout.take() {
        drain_openvpn(stdout, Arc::clone(&ready));
    }
    if let Some(stderr) = child.stderr.take() {
        drain_openvpn(stderr, Arc::clone(&ready));
    }
    Ok((child, ready))
}

fn openvpn_has_dns_updown() -> Result<bool, String> {
    OPENVPN_HAS_DNS_UPDOWN
        .get_or_init(|| {
            let output = Command::new("openvpn")
                .arg("--help")
                .stdin(Stdio::null())
                .output()
                .map_err(|error| format!("cannot inspect OpenVPN DNS hook support: {error}"))?;
            // OpenVPN 2.7 prints valid help text but exits with status 1. The advertised option,
            // rather than the process status, is the capability boundary; older 2.6 builds simply
            // omit it and continue down the compatibility path without the custom hook.
            Ok(openvpn_help_has_dns_updown(&output.stdout, &output.stderr))
        })
        .clone()
}

fn openvpn_help_has_dns_updown(stdout: &[u8], stderr: &[u8]) -> bool {
    stdout
        .windows(b"--dns-updown".len())
        .any(|window| window == b"--dns-updown")
        || stderr
            .windows(b"--dns-updown".len())
            .any(|window| window == b"--dns-updown")
}

fn openvpn_dns_hook_command(executable: &Path, resolver: &Path) -> Result<String, String> {
    Ok(format!(
        "{} vpngate-dns-updown {}",
        quote_openvpn_command_argument(executable.as_os_str())?,
        quote_openvpn_command_argument(resolver.as_os_str())?
    ))
}

fn quote_openvpn_command_argument(value: &OsStr) -> Result<String, String> {
    let value = value.to_str().ok_or("OpenVPN DNS hook path is not UTF-8")?;
    if value.as_bytes().contains(&0) {
        return Err("OpenVPN DNS hook path contains NUL".to_owned());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Apply OpenVPN's DNS lifecycle to the resolver file belonging to its network namespace.
///
/// A network-interface index is meaningful only inside one network namespace. Calling
/// `resolvectl` here would send that local integer to the host's systemd-resolved service over the
/// shared system bus; a coincidentally equal host link index would then be modified. The file path
/// is supplied by the privileged parent and constrained to `/etc/netns/<safe-id>/resolv.conf`.
pub(crate) fn run_dns_updown_hook(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("vpngate-dns-updown requires one namespace resolver path".to_owned());
    }
    let resolver = validate_namespace_resolver_file(Path::new(&args[0]))?;
    let script_type = std::env::var("script_type")
        .map_err(|_| "OpenVPN DNS hook is missing script_type".to_owned())?;
    let content = match script_type.as_str() {
        "dns-up" => resolv_conf_from_openvpn_dns(&read_openvpn_dns_values()?)?,
        "dns-down" => NAMESPACE_RESOLV_CONF.to_vec(),
        value => return Err(format!("unexpected OpenVPN DNS hook type {value}")),
    };
    write_private_atomic_in_existing_dir(&resolver, &content)
        .map_err(|error| format!("cannot update {}: {error}", resolver.display()))
}

fn read_openvpn_dns_values() -> Result<BTreeMap<String, String>, String> {
    let mut values = std::env::vars()
        .filter(|(name, _)| name.starts_with("dns_"))
        .collect::<BTreeMap<_, _>>();
    let Some(path) = std::env::var_os("dns_vars_file").filter(|path| !path.is_empty()) else {
        return Ok(values);
    };
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(PathBuf::from(path))
        .map_err(|error| format!("cannot open OpenVPN DNS variables: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect OpenVPN DNS variables: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_OPENVPN_DNS_VARS_BYTES {
        return Err("OpenVPN DNS variables file is not a bounded regular file".to_owned());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_OPENVPN_DNS_VARS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read OpenVPN DNS variables: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_OPENVPN_DNS_VARS_BYTES {
        return Err("OpenVPN DNS variables file exceeds 64 KiB".to_owned());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "OpenVPN DNS variables are not UTF-8".to_owned())?;
    for line in text.lines() {
        let Some((name, encoded)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with("dns_")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            continue;
        }
        values.insert(name.to_owned(), decode_openvpn_dns_value(encoded.trim())?);
    }
    Ok(values)
}

fn decode_openvpn_dns_value(value: &str) -> Result<String, String> {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        let value = &value[1..value.len() - 1];
        if value.contains('\'') {
            return Err("OpenVPN DNS variable contains unsupported shell quoting".to_owned());
        }
        return Ok(value.to_owned());
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let mut decoded = String::new();
        let mut escaped = false;
        for character in value[1..value.len() - 1].chars() {
            if escaped {
                decoded.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else {
                decoded.push(character);
            }
        }
        if escaped {
            return Err("OpenVPN DNS variable ends with an escape".to_owned());
        }
        return Ok(decoded);
    }
    if value.bytes().any(|byte| {
        byte.is_ascii_whitespace() || matches!(byte, b';' | b'`' | b'$' | b'\\' | b'\'' | b'"')
    }) {
        return Err("OpenVPN DNS variable contains unsupported shell syntax".to_owned());
    }
    Ok(value.to_owned())
}

fn resolv_conf_from_openvpn_dns(values: &BTreeMap<String, String>) -> Result<Vec<u8>, String> {
    let mut selected = None::<Vec<IpAddr>>;
    for server in 1..=16 {
        let first = format!("dns_server_{server}_address_1");
        if !values.contains_key(&first) {
            break;
        }
        let transport = values
            .get(&format!("dns_server_{server}_transport"))
            .map(|value| value.to_ascii_lowercase());
        let dnssec = values
            .get(&format!("dns_server_{server}_dnssec"))
            .map(|value| value.to_ascii_lowercase());
        if transport.as_deref().is_some_and(|value| value != "plain")
            || dnssec.as_deref().is_some_and(|value| value != "no")
        {
            continue;
        }
        let mut addresses = Vec::new();
        let mut compatible = true;
        for address_index in 1..=16 {
            let Some(address) = values.get(&format!("dns_server_{server}_address_{address_index}"))
            else {
                break;
            };
            let port = values.get(&format!("dns_server_{server}_port_{address_index}"));
            if port.is_some_and(|port| port != "53") {
                compatible = false;
                break;
            }
            addresses.push(
                address.parse::<IpAddr>().map_err(|_| {
                    format!("OpenVPN DNS server {server} contains an invalid address")
                })?,
            );
        }
        if compatible && !addresses.is_empty() {
            selected = Some(addresses);
            break;
        }
    }
    let addresses = selected.ok_or("OpenVPN supplied no resolv.conf-compatible DNS server")?;
    let mut content = String::new();
    for address in addresses.iter().take(MAX_RESOLV_CONF_NAMESERVERS) {
        content.push_str(&format!("nameserver {address}\n"));
    }
    let search = (1..=MAX_RESOLV_CONF_SEARCH_DOMAINS)
        .filter_map(|index| values.get(&format!("dns_search_domain_{index}")))
        .map(|domain| validate_dns_search_domain(domain).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if !search.is_empty() {
        content.push_str("search ");
        content.push_str(&search.join(" "));
        content.push('\n');
    }
    content.push_str("options timeout:2 attempts:2\n");
    Ok(content.into_bytes())
}

fn validate_dns_search_domain(domain: &str) -> Result<&str, String> {
    if domain.is_empty()
        || domain.len() > 253
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain
            .split('.')
            .any(|label| label.is_empty() || label.len() > 63)
        || !domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("OpenVPN supplied an invalid DNS search domain".to_owned());
    }
    Ok(domain)
}

fn drain_openvpn(input: impl std::io::Read + Send + 'static, ready: Arc<AtomicBool>) {
    thread::spawn(move || {
        for line in BufReader::new(input).lines().map_while(Result::ok) {
            if line.contains("Initialization Sequence Completed") {
                ready.store(true, Ordering::Release);
            }
        }
    });
}

fn wait_openvpn(child: &mut Child, ready: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + OPENVPN_START_TIMEOUT;
    loop {
        if ready.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("OpenVPN exited before initialization ({status})"));
        }
        if Instant::now() >= deadline {
            return Err("OpenVPN initialization timed out".to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

enum CurlRoute<'a> {
    Namespace(&'a str),
    Socks(&'a str, u16),
}

fn probe_route(
    route: CurlRoute<'_>,
    host_ip: Option<IpAddr>,
    connect_ms: u32,
    candidate: &VpngateCandidate,
    speed_gate: Option<&ConcurrencyGate>,
) -> Result<VpngateProbeSample, String> {
    let first = curl_text(&route, "https://api.ipify.org", 12)?;
    let second = curl_text(&route, "https://icanhazip.com", 12)?;
    let first_ip = first
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| "first exit service returned no IP".to_owned())?;
    let second_ip = second
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| "second exit service returned no IP".to_owned())?;
    if first_ip != second_ip {
        return Err("independent exit services disagree".to_owned());
    }
    if host_ip == Some(first_ip) {
        return Err("VPN Gate exit is identical to the host exit".to_owned());
    }
    let _speed_permit = speed_gate.map(ConcurrencyGate::enter);
    let speed_text = curl_speed(&route)?;
    let bytes_per_second = speed_text
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| "speed endpoint returned an invalid rate".to_owned())?;
    let download_bps = (bytes_per_second * 8.0).round().clamp(0.0, u64::MAX as f64) as u64;
    let exit_ip = first_ip.to_string();
    let trusted = candidate.verified_exit_ip.as_deref() == Some(exit_ip.as_str());
    let (exit_country_code, ip_scores, ip_networks) = if trusted {
        (
            candidate.verified_exit_country_code.clone(),
            candidate.verified_ip_scores.clone(),
            candidate.verified_ip_networks.clone(),
        )
    } else {
        (None, Vec::new(), Vec::new())
    };
    Ok(VpngateProbeSample {
        server_id: candidate.server_id.clone(),
        profile_sha256: candidate.profile_sha256.clone(),
        status: VpngateProbeStatus::Succeeded,
        exit_ip: Some(exit_ip),
        exit_country_code,
        connect_ms: Some(connect_ms),
        download_bps: Some(download_bps),
        ip_scores,
        ip_networks,
        error_code: None,
        error_detail: None,
        probed_at_unix_secs: unix_secs(),
    })
}

fn failed_sample(candidate: &VpngateCandidate, code: &str, detail: &str) -> VpngateProbeSample {
    VpngateProbeSample {
        server_id: candidate.server_id.clone(),
        profile_sha256: candidate.profile_sha256.clone(),
        status: VpngateProbeStatus::Failed,
        exit_ip: None,
        exit_country_code: None,
        connect_ms: None,
        download_bps: None,
        ip_scores: Vec::new(),
        ip_networks: Vec::new(),
        error_code: Some(code.to_owned()),
        error_detail: Some(detail.chars().take(500).collect()),
        probed_at_unix_secs: unix_secs(),
    }
}

fn curl_text(route: &CurlRoute<'_>, url: &str, timeout_secs: u16) -> Result<String, String> {
    let mut command = curl_command(route);
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--ipv4",
        "--max-time",
        &timeout_secs.to_string(),
        url,
    ]);
    successful_stdout(command, "VPN Gate verification request")
}

fn curl_speed(route: &CurlRoute<'_>) -> Result<String, String> {
    let mut command = curl_command(route);
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--ipv4",
        "--max-time",
        "20",
        "--output",
        "/dev/null",
        "--write-out",
        "%{speed_download}",
        "https://speed.cloudflare.com/__down?bytes=3000000",
    ]);
    successful_stdout(command, "VPN Gate speed request")
}

fn curl_command(route: &CurlRoute<'_>) -> Command {
    match route {
        CurlRoute::Namespace(namespace) => {
            let mut command = Command::new("ip");
            command.args(["netns", "exec", namespace, "curl"]);
            command
        }
        CurlRoute::Socks(address, port) => {
            let mut command = Command::new("curl");
            command.args(["--socks5-hostname", &format!("{address}:{port}")]);
            command
        }
    }
}

fn host_public_ip() -> Option<IpAddr> {
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--ipv4",
        "--max-time",
        "8",
        "https://api.ipify.org",
    ]);
    successful_stdout(command, "host exit lookup")
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[derive(Debug, Clone)]
struct NamespaceSpec {
    name: String,
    host_interface: String,
    peer_interface: String,
    host_address: Ipv4Addr,
    peer_address: Ipv4Addr,
}

impl NamespaceSpec {
    fn cidr(&self) -> String {
        let mut octets = self.host_address.octets();
        octets[3] &= !3;
        format!("{}/30", Ipv4Addr::from(octets))
    }
}

fn persistent_namespace(pool: &VpngateDesiredPool) -> NamespaceSpec {
    NamespaceSpec {
        name: format!("brocade-vg-{:04}", pool.runtime_slot),
        host_interface: format!("bvg{:04}h", pool.runtime_slot),
        peer_interface: format!("bvg{:04}n", pool.runtime_slot),
        host_address: pool
            .host_address
            .parse()
            .expect("desired addresses were validated"),
        peer_address: pool
            .peer_address
            .parse()
            .expect("desired addresses were validated"),
    }
}

fn backend_namespace(pool: &VpngateDesiredPool, slot: u8) -> NamespaceSpec {
    assert!(usize::from(slot) < RUNTIME_BACKENDS_PER_POOL);
    let ordinal = pool
        .runtime_slot
        .checked_mul(u16::try_from(RUNTIME_BACKENDS_PER_POOL).expect("backend count fits u16"))
        .and_then(|value| value.checked_add(u16::from(slot)))
        .expect("validated runtime slots fit the backend address block");
    let offset = ordinal * 4;
    let third = 132 + offset / 256;
    let fourth = offset % 256;
    NamespaceSpec {
        name: format!("brocade-vgb-{:04}-{slot}", pool.runtime_slot),
        host_interface: format!("bvb{:04}{slot}h", pool.runtime_slot),
        peer_interface: format!("bvb{:04}{slot}n", pool.runtime_slot),
        host_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(third).expect("runtime backend slots fit the reserved block"),
            u8::try_from(fourth + 1).expect("/30 host fits an octet"),
        ),
        peer_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(third).expect("runtime backend slots fit the reserved block"),
            u8::try_from(fourth + 2).expect("/30 peer fits an octet"),
        ),
    }
}

fn routing_chain(pool: &VpngateDesiredPool) -> String {
    format!("BRVG{:04}", pool.runtime_slot)
}

fn stable_route(pool: &VpngateDesiredPool, backend_slot: u8) -> (String, String) {
    (
        format!("{}/32", pool.peer_address),
        backend_namespace(pool, backend_slot).host_interface,
    )
}

fn set_active_backend(
    pool: &VpngateDesiredPool,
    backend: &RuntimeBackendMetadata,
) -> Result<(), String> {
    let chain = routing_chain(pool);
    let (stable, active_interface) = stable_route(pool, backend.slot);
    // The stable address is deliberately not assigned to an interface: the OUTPUT DNAT below
    // rewrites it to the selected backend. Linux still performs an initial route lookup before
    // that hook, though, and without this host route it sends the SYN towards the machine's
    // default gateway. Pointing the route at the active backend veth also keeps it alive for the
    // lifetime of that backend and lets failover replace it atomically with the DNAT target.
    run(
        "ip",
        &[
            "route",
            "replace",
            &stable,
            "dev",
            &active_interface,
            "scope",
            "link",
        ],
    )?;
    let _ = Command::new("iptables")
        .args(["-w", "5", "-t", "nat", "-N", &chain])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let port = pool.socks_port.to_string();
    ensure_iptables(
        Some("nat"),
        &[
            "OUTPUT", "-d", &stable, "-p", "tcp", "--dport", &port, "-j", &chain,
        ],
    )?;
    let destination = format!("{}:{}", backend.peer_address, pool.socks_port);
    let replacement = [
        "-w",
        "5",
        "-t",
        "nat",
        "-R",
        &chain,
        "1",
        "-p",
        "tcp",
        "-j",
        "DNAT",
        "--to-destination",
        &destination,
    ];
    let replaced = Command::new("iptables")
        .args(replacement)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !replaced {
        run(
            "iptables",
            &[
                "-w",
                "5",
                "-t",
                "nat",
                "-A",
                &chain,
                "-p",
                "tcp",
                "-j",
                "DNAT",
                "--to-destination",
                &destination,
            ],
        )?;
    }
    if tcp_reachable(&pool.peer_address, pool.socks_port) {
        Ok(())
    } else {
        Err("VPN Gate stable SOCKS route did not reach the selected backend".to_owned())
    }
}

fn cleanup_pool_routing(pool: &VpngateDesiredPool) {
    let chain = routing_chain(pool);
    let stable = format!("{}/32", pool.peer_address);
    let port = pool.socks_port.to_string();
    delete_iptables(
        Some("nat"),
        &[
            "OUTPUT", "-d", &stable, "-p", "tcp", "--dport", &port, "-j", &chain,
        ],
    );
    for action in ["-F", "-X"] {
        let _ = Command::new("iptables")
            .args(["-w", "5", "-t", "nat", action, &chain])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = Command::new("ip")
        .args(["route", "delete", &stable])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn cleanup_pool_runtime(pool_dir: &Path, pool: &VpngateDesiredPool) -> Result<(), String> {
    cleanup_pool_routing(pool);
    cleanup_namespace(&persistent_namespace(pool))?;
    for slot in 0..u8::try_from(RUNTIME_BACKENDS_PER_POOL).unwrap_or_default() {
        cleanup_backend_runtime(pool_dir, pool, slot)?;
    }
    Ok(())
}

fn cleanup_backend_runtime(
    pool_dir: &Path,
    pool: &VpngateDesiredPool,
    slot: u8,
) -> Result<(), String> {
    terminate_backend_processes(pool_dir, slot);
    cleanup_namespace(&backend_namespace(pool, slot))
}

#[cfg(test)]
fn probe_namespace(pool: &VpngateDesiredPool) -> NamespaceSpec {
    let offset = pool.runtime_slot * 4;
    let third = 128 + offset / 256;
    let fourth = offset % 256;
    NamespaceSpec {
        name: format!("brocade-vgt-{:04}", pool.runtime_slot),
        host_interface: format!("bvt{:04}h", pool.runtime_slot),
        peer_interface: format!("bvt{:04}n", pool.runtime_slot),
        host_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(third).expect("sixteen slots fit the staging block"),
            u8::try_from(fourth + 1).expect("/30 host fits an octet"),
        ),
        peer_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(third).expect("sixteen slots fit the staging block"),
            u8::try_from(fourth + 2).expect("/30 peer fits an octet"),
        ),
    }
}

fn catalog_probe_namespace(slot: usize) -> NamespaceSpec {
    assert!(slot < usize::from(CATALOG_PROBE_WORKERS));
    let offset = u16::try_from(slot * 4).expect("catalogue probe slots fit two /24s");
    NamespaceSpec {
        name: format!("brocade-vgc-{slot:02}"),
        host_interface: format!("bvgc{slot:02}h"),
        peer_interface: format!("bvgc{slot:02}n"),
        host_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(129 + offset / 256).expect("catalogue probe third octet fits"),
            u8::try_from(offset % 256 + 1).expect("catalogue probe host octet fits"),
        ),
        peer_address: Ipv4Addr::new(
            169,
            254,
            u8::try_from(129 + offset / 256).expect("catalogue probe third octet fits"),
            u8::try_from(offset % 256 + 2).expect("catalogue probe peer octet fits"),
        ),
    }
}

fn legacy_catalog_probe_namespace() -> NamespaceSpec {
    NamespaceSpec {
        name: "brocade-vg-cat".to_owned(),
        host_interface: "bvgcath".to_owned(),
        peer_interface: "bvgcatn".to_owned(),
        host_address: Ipv4Addr::new(169, 254, 128, 65),
        peer_address: Ipv4Addr::new(169, 254, 128, 66),
    }
}

fn cleanup_catalog_probe_namespaces() {
    let _ = cleanup_namespace(&legacy_catalog_probe_namespace());
    for slot in 0..usize::from(CATALOG_PROBE_WORKERS) {
        let _ = cleanup_namespace(&catalog_probe_namespace(slot));
    }
}

fn setup_namespace(spec: &NamespaceSpec) -> Result<(), String> {
    let result = (|| {
        run("ip", &["netns", "add", &spec.name])?;
        // Docker's embedded resolver (normally 127.0.0.11) and systemd-resolved's loopback
        // stub are scoped to the host namespace. They are unreachable after `ip netns exec`,
        // including from the helper Xray that resolves end-user destinations. iproute2
        // deliberately bind-mounts this per-netns file over /etc/resolv.conf for us.
        let resolver_dir = namespace_resolver_dir(spec)?;
        create_private_dir(&resolver_dir)?;
        write_private_atomic(&resolver_dir.join("resolv.conf"), NAMESPACE_RESOLV_CONF)?;
        run(
            "ip",
            &[
                "link",
                "add",
                &spec.host_interface,
                "type",
                "veth",
                "peer",
                "name",
                &spec.peer_interface,
            ],
        )?;
        run(
            "ip",
            &["link", "set", &spec.peer_interface, "netns", &spec.name],
        )?;
        run(
            "ip",
            &[
                "address",
                "add",
                &format!("{}/30", spec.host_address),
                "dev",
                &spec.host_interface,
            ],
        )?;
        run("ip", &["link", "set", &spec.host_interface, "up"])?;
        run("ip", &["-n", &spec.name, "link", "set", "lo", "up"])?;
        run(
            "ip",
            &[
                "-n",
                &spec.name,
                "address",
                "add",
                &format!("{}/30", spec.peer_address),
                "dev",
                &spec.peer_interface,
            ],
        )?;
        run(
            "ip",
            &["-n", &spec.name, "link", "set", &spec.peer_interface, "up"],
        )?;
        run(
            "ip",
            &[
                "-n",
                &spec.name,
                "route",
                "add",
                "default",
                "via",
                &spec.host_address.to_string(),
            ],
        )?;
        run("sysctl", &["-w", "net.ipv4.ip_forward=1"])?;
        ensure_iptables(
            Some("nat"),
            &["POSTROUTING", "-s", &spec.cidr(), "-j", "MASQUERADE"],
        )?;
        ensure_iptables(
            None,
            &["FORWARD", "-i", &spec.host_interface, "-j", "ACCEPT"],
        )?;
        ensure_iptables(
            None,
            &[
                "FORWARD",
                "-o",
                &spec.host_interface,
                "-m",
                "conntrack",
                "--ctstate",
                "RELATED,ESTABLISHED",
                "-j",
                "ACCEPT",
            ],
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = cleanup_namespace(spec);
    }
    result
}

fn cleanup_namespace(spec: &NamespaceSpec) -> Result<(), String> {
    delete_iptables(
        Some("nat"),
        &["POSTROUTING", "-s", &spec.cidr(), "-j", "MASQUERADE"],
    );
    delete_iptables(
        None,
        &["FORWARD", "-i", &spec.host_interface, "-j", "ACCEPT"],
    );
    delete_iptables(
        None,
        &[
            "FORWARD",
            "-o",
            &spec.host_interface,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    );
    let mut namespace_pids = Vec::new();
    if let Ok(output) = command_output(Command::new("ip").args(["netns", "pids", &spec.name])) {
        if output.status.success() {
            for pid in String::from_utf8_lossy(&output.stdout).split_ascii_whitespace() {
                if !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()) {
                    namespace_pids.push(pid.to_owned());
                }
            }
            for pid in &namespace_pids {
                let _ = Command::new("kill")
                    .args(["-TERM", pid.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    if !namespace_pids.is_empty() {
        thread::sleep(Duration::from_millis(250));
        for pid in &namespace_pids {
            let alive = Command::new("kill")
                .args(["-0", pid.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if alive {
                let _ = Command::new("kill")
                    .args(["-KILL", pid.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    let _ = Command::new("ip")
        .args(["netns", "delete", &spec.name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("ip")
        .args(["link", "delete", &spec.host_interface])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Ok(resolver_dir) = namespace_resolver_dir(spec) {
        let _ = fs::remove_dir_all(resolver_dir);
    }
    Ok(())
}

fn namespace_resolver_dir(spec: &NamespaceSpec) -> Result<PathBuf, String> {
    namespace_resolver_file(&spec.name).map(|path| {
        path.parent()
            .expect("validated namespace resolver has a parent")
            .to_path_buf()
    })
}

fn namespace_resolver_file(namespace: &str) -> Result<PathBuf, String> {
    if !safe_id(namespace) {
        return Err("VPN Gate namespace name is unsafe".to_owned());
    }
    Ok(Path::new("/etc/netns").join(namespace).join("resolv.conf"))
}

fn validate_namespace_resolver_file(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or("VPN Gate namespace resolver has no parent")?;
    let namespace = parent
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or("VPN Gate namespace resolver has an invalid namespace")?;
    let expected = namespace_resolver_file(namespace)?;
    if path != expected {
        return Err("VPN Gate DNS hook may only update a namespace resolver".to_owned());
    }
    Ok(expected)
}

fn ensure_iptables(table: Option<&str>, rule: &[&str]) -> Result<(), String> {
    let mut check = vec!["-w", "5"];
    if let Some(table) = table {
        check.extend(["-t", table]);
    }
    check.push("-C");
    check.extend_from_slice(rule);
    if Command::new("iptables")
        .args(&check)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    let mut add = vec!["-w", "5"];
    if let Some(table) = table {
        add.extend(["-t", table]);
    }
    add.push("-A");
    add.extend_from_slice(rule);
    run("iptables", &add)
}

fn delete_iptables(table: Option<&str>, rule: &[&str]) {
    let mut args = vec!["-w", "5"];
    if let Some(table) = table {
        args.extend(["-t", table]);
    }
    args.push("-D");
    args.extend_from_slice(rule);
    let _ = Command::new("iptables")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BackendProcessCounts {
    openvpn: usize,
    xray: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendProcessKind {
    Openvpn,
    Xray,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagedBackendProcess {
    pid: u32,
    slot: u8,
    kind: BackendProcessKind,
}

fn command_uses_config(cmdline: &[u8], flag: &str, config: &Path) -> bool {
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    args.windows(2)
        .any(|pair| pair[0] == flag.as_bytes() && pair[1] == config.as_os_str().as_bytes())
}

fn managed_backend_processes_in(
    proc_root: &Path,
    pool_dir: &Path,
    slots: &BTreeSet<u8>,
) -> Vec<ManagedBackendProcess> {
    let Ok(entries) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut processes = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let process_dir = entry.path();
        let Ok(comm) = fs::read_to_string(process_dir.join("comm")) else {
            continue;
        };
        let comm = comm.trim();
        if comm != "openvpn" && comm != "xray" {
            continue;
        }
        let Ok(cmdline) = fs::read(process_dir.join("cmdline")) else {
            continue;
        };
        for slot in slots {
            let (kind, flag, config) = if comm == "openvpn" {
                (
                    BackendProcessKind::Openvpn,
                    "--config",
                    pool_dir.join(format!("profile-{slot}.ovpn")),
                )
            } else {
                (
                    BackendProcessKind::Xray,
                    "-config",
                    pool_dir.join(format!("socks-xray-{slot}.json")),
                )
            };
            if command_uses_config(&cmdline, flag, &config) {
                processes.push(ManagedBackendProcess {
                    pid,
                    slot: *slot,
                    kind,
                });
                break;
            }
        }
    }
    processes.sort_by_key(|process| process.pid);
    processes
}

fn managed_backend_processes(pool_dir: &Path, slots: &BTreeSet<u8>) -> Vec<ManagedBackendProcess> {
    managed_backend_processes_in(Path::new("/proc"), pool_dir, slots)
}

fn managed_catalog_probe_processes_in(proc_root: &Path, state_dir: &Path) -> Vec<u32> {
    let probe_root = vpngate_root(state_dir).join("catalog-probe");
    let Ok(entries) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let process_dir = entry.path();
        let Ok(comm) = fs::read_to_string(process_dir.join("comm")) else {
            continue;
        };
        if comm.trim() != "openvpn" {
            continue;
        }
        let Ok(cmdline) = fs::read(process_dir.join("cmdline")) else {
            continue;
        };
        let managed = (0..usize::from(CATALOG_PROBE_WORKERS)).any(|slot| {
            let config = probe_root
                .join(format!("worker-{slot:02}"))
                .join("probe.ovpn");
            command_uses_config(&cmdline, "--config", &config)
        });
        if managed {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

fn managed_catalog_probe_processes(state_dir: &Path) -> Vec<u32> {
    managed_catalog_probe_processes_in(Path::new("/proc"), state_dir)
}

fn backend_process_counts(
    pool_dir: &Path,
    backends: &[RuntimeBackendMetadata],
) -> BTreeMap<u8, BackendProcessCounts> {
    let slots = backends
        .iter()
        .map(|backend| backend.slot)
        .collect::<BTreeSet<_>>();
    let mut counts = slots
        .iter()
        .map(|slot| (*slot, BackendProcessCounts::default()))
        .collect::<BTreeMap<_, _>>();
    for process in managed_backend_processes(pool_dir, &slots) {
        let count = counts.entry(process.slot).or_default();
        match process.kind {
            BackendProcessKind::Openvpn => count.openvpn += 1,
            BackendProcessKind::Xray => count.xray += 1,
        }
    }
    counts
}

fn signal_processes(processes: &[ManagedBackendProcess], signal: &str) {
    for process in processes {
        signal_pid(process.pid, signal);
    }
}

fn signal_pids(pids: &[u32], signal: &str) {
    for pid in pids {
        signal_pid(*pid, signal);
    }
}

fn signal_pid(pid: u32, signal: &str) {
    let pid = pid.to_string();
    let _ = Command::new("kill")
        .args([signal, pid.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn terminate_backend_processes(pool_dir: &Path, slot: u8) {
    let slots = BTreeSet::from([slot]);
    let processes = managed_backend_processes(pool_dir, &slots);
    if processes.is_empty() {
        return;
    }
    signal_processes(&processes, "-TERM");
    thread::sleep(Duration::from_millis(250));
    // Scan again before SIGKILL so a rapidly reused PID can never target an unrelated process.
    let remaining = managed_backend_processes(pool_dir, &slots);
    signal_processes(&remaining, "-KILL");
}

fn terminate_catalog_probe_processes(state_dir: &Path) {
    let processes = managed_catalog_probe_processes(state_dir);
    if processes.is_empty() {
        return;
    }
    signal_pids(&processes, "-TERM");
    thread::sleep(Duration::from_millis(250));
    // As for runtime backends, re-scan before SIGKILL to make PID reuse harmless.
    let remaining = managed_catalog_probe_processes(state_dir);
    signal_pids(&remaining, "-KILL");
}

fn backend_runtime_failure(
    process_counts: BackendProcessCounts,
    namespace: &NamespaceSpec,
    socks_port: u16,
) -> Option<brocade_deployment::protocol::VpngateFailureReason> {
    // A systemd LogNamespace gives every Agent invocation a private mount namespace. The named
    // netns handles created by the previous process are therefore not usable after self-update,
    // even though the OpenVPN and helper Xray children (and their veths) deliberately stay alive.
    // Exact managed config paths let the new Agent adopt those children and also detect duplicates.
    if process_counts.openvpn != 1 || process_counts.xray != 1 {
        return Some(brocade_deployment::protocol::VpngateFailureReason::ProcessExited);
    }
    if !tcp_reachable(&namespace.peer_address.to_string(), socks_port) {
        return Some(brocade_deployment::protocol::VpngateFailureReason::SocksUnavailable);
    }
    None
}

fn egress_reachable(address: &str, port: u16) -> bool {
    thread::scope(|scope| {
        let first =
            scope.spawn(|| curl_health(address, port, "https://cp.cloudflare.com/generate_204"));
        let second =
            scope.spawn(|| curl_health(address, port, "https://www.gstatic.com/generate_204"));
        first.join().unwrap_or(false) || second.join().unwrap_or(false)
    })
}

fn curl_health(address: &str, port: u16, url: &str) -> bool {
    let proxy = format!("{address}:{port}");
    let timeout = HEALTH_REQUEST_TIMEOUT_SECS.to_string();
    Command::new("curl")
        .args([
            "--socks5-hostname",
            &proxy,
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--ipv4",
            "--connect-timeout",
            "3",
            "--max-time",
            &timeout,
            "--output",
            "/dev/null",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn tcp_reachable(address: &str, port: u16) -> bool {
    let Ok(ip) = address.parse::<IpAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), TCP_REACH_TIMEOUT).is_ok()
}

fn cleanup_omitted(pools_root: &Path, desired: &VpngateDesiredState) -> Result<(), String> {
    let desired_slots = desired
        .pools
        .iter()
        .map(|pool| (pool.outbound_id.as_str(), pool.runtime_slot))
        .collect::<BTreeMap<_, _>>();
    for entry in fs::read_dir(pools_root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !safe_id(&id) {
            continue;
        }
        let runtime_slot = read_runtime_slot(&entry.path().join("runtime.json"));
        let keep = runtime_slot.is_some_and(|runtime_slot| {
            desired_slots.get(id.as_str()).copied() == Some(runtime_slot)
        });
        if keep {
            continue;
        }
        if let Some(runtime_slot) = runtime_slot {
            if usize::from(runtime_slot) < VPNGATE_RUNTIME_MAX_POOLS_PER_NODE {
                let peer = vpngate_runtime_peer(usize::from(runtime_slot))
                    .ok_or("stored VPN Gate slot is invalid")?;
                let mut host = peer.octets();
                host[3] -= 1;
                let pool = VpngateDesiredPool {
                    outbound_id: id.clone(),
                    country_code: "ZZ".to_owned(),
                    max_connect_ms: 1,
                    min_download_bps: 0,
                    max_candidates: 1,
                    runtime_slot,
                    host_address: Ipv4Addr::from(host).to_string(),
                    peer_address: peer.to_string(),
                    prefix_len: 30,
                    socks_port: VPNGATE_RUNTIME_SOCKS_PORT,
                    candidates: Vec::new(),
                    manual_switch: None,
                };
                cleanup_pool_runtime(&entry.path(), &pool)?;
            }
        }
        fs::remove_dir_all(entry.path())
            .map_err(|error| format!("cannot remove obsolete VPN Gate pool {id}: {error}"))?;
    }
    Ok(())
}

fn read_metadata(path: &Path) -> Option<RuntimeMetadata> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_runtime_slot(path: &Path) -> Option<u16> {
    #[derive(Deserialize)]
    struct StoredRuntimeSlot {
        runtime_slot: u16,
    }
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<StoredRuntimeSlot>(&bytes)
        .ok()
        .map(|metadata| metadata.runtime_slot)
}

fn write_metadata(path: &Path, metadata: &RuntimeMetadata) -> Result<(), String> {
    write_private_atomic(
        path,
        &serde_json::to_vec_pretty(metadata).map_err(|error| error.to_string())?,
    )
}

fn vpngate_root(state_dir: &Path) -> PathBuf {
    state_dir.join("vpngate")
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())
}

fn write_private_atomic(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("private file has no parent")?;
    create_private_dir(parent)?;
    write_private_atomic_in_existing_dir(path, content)
}

fn write_private_atomic_in_existing_dir(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("private file has no parent")?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() {
        return Err("private file parent is not a directory".to_owned());
    }
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or("private file name is not UTF-8")?,
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(content).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn require_linux_runtime() -> Result<(), String> {
    // SAFETY: `geteuid` reads process credentials, takes no pointer, and has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("VPN Gate Linux apply mode requires root".to_owned());
    }
    for command in ["ip", "iptables", "openvpn", "curl", "sysctl"] {
        if !command_exists(command) {
            return Err(format!("VPN Gate runtime dependency {command} is missing"));
        }
    }
    let xray = crate::options::xray_binary_path();
    if !xray.is_file() && !command_exists(xray.to_string_lossy().as_ref()) {
        return Err("VPN Gate helper requires the Brocade Xray binary".to_owned());
    }
    Ok(())
}

fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(512)
            .collect::<String>();
        let command = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        if detail.is_empty() {
            Err(format!("{command} exited with {}", output.status))
        } else {
            Err(format!("{command} exited with {}: {detail}", output.status))
        }
    }
}

fn command_output(command: &mut Command) -> Result<std::process::Output, String> {
    command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())
}

fn successful_stdout(mut command: Command, what: &str) -> Result<String, String> {
    let output = command_output(&mut command)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(512)
            .collect::<String>();
        return if detail.is_empty() {
            Err(format!("{what} failed with {}", output.status))
        } else {
            Err(format!("{what} failed with {}: {detail}", output.status))
        };
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{what} returned non-UTF-8"))
}

fn reap_in_background(mut child: Child) {
    thread::spawn(move || {
        let _ = child.wait();
    });
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_proxycheck_api_key(value: &str) -> bool {
    let mut groups = value.split('-');
    (0..4).all(|_| {
        groups.next().is_some_and(|group| {
            group.len() == 6 && group.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    }) && groups.next().is_none()
}

fn valid_country(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(1)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn openvpn_dns_hook_writes_only_compatible_servers_to_the_namespace() {
        let values = BTreeMap::from([
            ("dns_server_1_address_1".to_owned(), "192.0.2.53".to_owned()),
            ("dns_server_1_transport".to_owned(), "DoH".to_owned()),
            (
                "dns_server_2_address_1".to_owned(),
                "10.211.254.254".to_owned(),
            ),
            ("dns_server_2_address_2".to_owned(), "8.8.8.8".to_owned()),
            ("dns_server_2_port_1".to_owned(), "53".to_owned()),
            ("dns_search_domain_1".to_owned(), "vpn.example".to_owned()),
        ]);

        assert_eq!(
            String::from_utf8(resolv_conf_from_openvpn_dns(&values).unwrap()).unwrap(),
            "nameserver 10.211.254.254\nnameserver 8.8.8.8\nsearch vpn.example\noptions timeout:2 attempts:2\n"
        );
    }

    #[test]
    fn openvpn_dns_hook_capability_comes_from_help_text_not_exit_status() {
        assert!(openvpn_help_has_dns_updown(
            b"options:\n--dns-updown cmd|force|disable\n",
            b""
        ));
        assert!(openvpn_help_has_dns_updown(
            b"",
            b"--dns-updown cmd|force|disable\n"
        ));
        assert!(!openvpn_help_has_dns_updown(b"OpenVPN 2.6 options\n", b""));
    }

    #[test]
    fn openvpn_dns_hook_cannot_target_the_host_resolver() {
        assert_eq!(
            validate_namespace_resolver_file(Path::new(
                "/etc/netns/brocade-vgb-0000-0/resolv.conf"
            ))
            .unwrap(),
            PathBuf::from("/etc/netns/brocade-vgb-0000-0/resolv.conf")
        );
        assert!(validate_namespace_resolver_file(Path::new("/etc/resolv.conf")).is_err());
        assert!(validate_namespace_resolver_file(Path::new("/etc/netns/../resolv.conf")).is_err());
    }

    #[test]
    fn openvpn_dns_hook_command_preserves_paths_as_single_arguments() {
        assert_eq!(
            openvpn_dns_hook_command(
                Path::new("/opt/Brocade Agent/brocade-agent"),
                Path::new("/etc/netns/brocade-vgb-0000-0/resolv.conf")
            )
            .unwrap(),
            "\"/opt/Brocade Agent/brocade-agent\" vpngate-dns-updown \"/etc/netns/brocade-vgb-0000-0/resolv.conf\""
        );
    }

    #[test]
    fn openvpn_dns_variable_parser_never_evaluates_shell_syntax() {
        assert_eq!(
            decode_openvpn_dns_value("'10.211.254.254'").unwrap(),
            "10.211.254.254"
        );
        assert_eq!(
            decode_openvpn_dns_value("\"vpn.example\"").unwrap(),
            "vpn.example"
        );
        assert!(decode_openvpn_dns_value("$(touch /tmp/should-not-exist)").is_err());
    }

    #[test]
    fn intelligence_work_is_claimed_from_its_dedicated_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.starts_with("GET /agent/v1/vpngate/intelligence-assignment "),
                "{request}"
            );
            assert!(request.contains(&format!(
                "X-Brocade-Protocol-Version: {}\r\n",
                brocade_deployment::protocol::AGENT_PROTOCOL_VERSION
            )));
            let body = r#"{"exit_ip":"198.51.100.7","lease_generation":9}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir: PathBuf::from("unused-in-assignment-test"),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();

        let assignment = fetch_intelligence_assignment(&client, &options)
            .unwrap()
            .expect("the dedicated lane returns one leased exit IP");

        server.join().unwrap();
        assert_eq!(assignment.exit_ip, "198.51.100.7");
        assert_eq!(assignment.lease_generation, 9);
        assert!(assignment.proxycheck_api_key.is_none());
    }

    #[test]
    fn proxycheck_quota_exhaustion_retries_once_without_the_api_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for (index, body) in [
                r#"{"status":"denied","message":"1,000 Free queries exhausted. Please try the API again tomorrow."}"#,
                r#"{"status":"ok","203.0.113.8":{"network":{"provider":"Example ISP","type":"Business"},"location":{"country_code":"US"},"detections":{"risk":12,"hosting":false}}}"#,
            ]
            .into_iter()
            .enumerate()
            {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                if index == 0 {
                    assert!(request.starts_with(
                        "GET /v3/203.0.113.8?key=111111-222222-333333-444444&p=0 "
                    ));
                } else {
                    assert!(request.starts_with("GET /v3/203.0.113.8?p=0 "));
                    assert!(!request.contains("key="));
                }
                let status = if index == 0 {
                    "429 Too Many Requests"
                } else {
                    "200 OK"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let client = HttpClient::new(&format!("http://{address}")).unwrap();

        let observation = query_proxycheck_with_client(
            &client,
            "203.0.113.8",
            Some("111111-222222-333333-444444"),
        )
        .unwrap();

        server.join().unwrap();
        assert_eq!(observation.score, 12);
        assert_eq!(observation.isp.as_deref(), Some("Example ISP"));
    }

    #[test]
    fn proxycheck_fallback_does_not_treat_rate_limits_as_key_quota_exhaustion() {
        assert!(proxycheck_key_quota_exhausted(
            429,
            r#"{"status":"denied","message":"1,000 Free queries exhausted and a burst token has already been consumed."}"#
        ));
        assert!(!proxycheck_key_quota_exhausted(
            429,
            r#"{"status":"denied","message":"You're sending more than 200 requests per second."}"#
        ));
        assert!(!proxycheck_key_quota_exhausted(
            403,
            r#"{"status":"warning","message":"1,000 Free queries exhausted."}"#
        ));
    }

    #[test]
    fn catalogue_collection_has_a_separate_leased_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.starts_with("GET /agent/v1/vpngate/catalogue-assignment "),
                "{request}"
            );
            let body = r#"{"run_id":7,"lease_generation":11,"source_url":"https://www.vpngate.net/api/iphone/"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir: PathBuf::from("unused-in-catalogue-test"),
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();

        let assignment = fetch_catalogue_assignment(&client, &options)
            .unwrap()
            .expect("the catalogue lane returns this selected Agent's leased collection");

        server.join().unwrap();
        assert_eq!(assignment.run_id, 7);
        assert_eq!(assignment.lease_generation, 11);
    }

    #[test]
    fn empty_reconcile_report_is_sent_and_cleared_after_acknowledgement() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let (header_end, content_length) = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0, "request ended before its body arrived");
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .expect("request includes Content-Length")
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break (header_end, content_length);
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(
                headers.starts_with("POST /agent/v1/vpngate/reconcile-report "),
                "{headers}"
            );
            let body = &request[header_end + 4..header_end + 4 + content_length];
            let body: serde_json::Value = serde_json::from_slice(body).unwrap();
            assert_eq!(body["topology_revision"], 7);
            assert_eq!(body["catalog_generation"], 9);
            assert_eq!(body["pools"], serde_json::json!([]));
            let response = r#"{"accepted_samples":0,"current_state_updated":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let state_dir = std::env::temp_dir().join(format!(
            "brocade-vpngate-empty-report-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&state_dir);
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir,
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();
        let report = VpngateReconcileReport {
            topology_revision: 7,
            catalog_generation: 9,
            pools: Vec::new(),
        };

        persist_and_send_reconcile_report(&client, &options, &report).unwrap();

        server.join().unwrap();
        assert!(!pending_reconcile_report_path(&options.state_dir).exists());
        let _ = fs::remove_dir_all(&options.state_dir);
    }

    #[test]
    fn reconcile_report_is_retained_when_console_does_not_update_current_state() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let (header_end, content_length) = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0, "request ended before its body arrived");
                request.extend_from_slice(&chunk[..read]);
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .expect("request includes Content-Length")
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break (header_end, content_length);
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(
                headers.starts_with("POST /agent/v1/vpngate/reconcile-report "),
                "{headers}"
            );
            assert_eq!(
                &request[header_end + 4..header_end + 4 + content_length],
                br#"{"topology_revision":7,"catalog_generation":9,"pools":[]}"#
            );
            let response = r#"{"accepted_samples":0,"current_state_updated":false}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let state_dir = std::env::temp_dir().join(format!(
            "brocade-vpngate-rejected-report-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&state_dir);
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir,
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();
        let report = VpngateReconcileReport {
            topology_revision: 7,
            catalog_generation: 9,
            pools: Vec::new(),
        };

        let error = persist_and_send_reconcile_report(&client, &options, &report).unwrap_err();

        server.join().unwrap();
        assert!(error.contains("did not update current state"), "{error}");
        assert!(pending_reconcile_report_path(&options.state_dir).exists());
        let _ = fs::remove_dir_all(&options.state_dir);
    }

    #[test]
    fn permanently_rejected_probe_reports_are_quarantined_without_blocking_the_queue() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the complete body before closing the socket. Closing with unread client bytes
            // sends a TCP reset on Linux, so the client can lose the valid 400 response and turn
            // this deterministic rejection test into a transport-error race.
            let mut request = Vec::new();
            let headers_end = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0, "request ended before its body arrived");
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .expect("request includes Content-Length")
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break headers_end;
                }
            };
            let request = String::from_utf8_lossy(&request[..headers_end]);
            assert!(
                request.starts_with("POST /agent/v1/vpngate/probe-report "),
                "{request}"
            );
            let body = "stale profile\nno longer belongs to this generation";
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let state_dir = std::env::temp_dir().join(format!(
            "brocade-vpngate-permanently-rejected-probe-report-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&state_dir);
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir,
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();
        let reports = vec![VpngateProbeReport {
            catalog_generation: 9,
            country_code: "US".to_owned(),
            samples: Vec::new(),
        }];

        persist_and_send_probe_reports(&client, &options, reports.clone()).unwrap();

        server.join().unwrap();
        assert!(!pending_probe_reports_path(&options.state_dir).exists());
        assert_eq!(
            serde_json::from_slice::<Vec<VpngateProbeReport>>(
                &fs::read(rejected_probe_reports_path(&options.state_dir)).unwrap()
            )
            .unwrap(),
            reports
        );
        let _ = fs::remove_dir_all(&options.state_dir);
    }

    #[test]
    fn retryable_probe_report_failure_remains_pending() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let headers_end = loop {
                let mut chunk = [0_u8; 4096];
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0, "request ended before its body arrived");
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .expect("request includes Content-Length")
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break headers_end;
                }
            };
            let request = String::from_utf8_lossy(&request[..headers_end]);
            assert!(
                request.starts_with("POST /agent/v1/vpngate/probe-report "),
                "{request}"
            );
            write!(
                stream,
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let state_dir = std::env::temp_dir().join(format!(
            "brocade-vpngate-retryable-probe-report-{}-{}",
            std::process::id(),
            address.port()
        ));
        let _ = fs::remove_dir_all(&state_dir);
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir,
            apply_mode: ApplyMode::Linux,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let client = HttpClient::new(&options.server).unwrap();
        let reports = vec![VpngateProbeReport {
            catalog_generation: 9,
            country_code: "US".to_owned(),
            samples: Vec::new(),
        }];

        let error = persist_and_send_probe_reports(&client, &options, reports).unwrap_err();

        server.join().unwrap();
        assert!(error.contains("HTTP 503"), "{error}");
        assert!(pending_probe_reports_path(&options.state_dir).exists());
        assert!(!rejected_probe_reports_path(&options.state_dir).exists());
        let _ = fs::remove_dir_all(&options.state_dir);
    }

    fn scores(proxycheck: u8, ffraud: u8, iplogs: u8) -> Vec<VpngateIpScore> {
        vec![
            VpngateIpScore {
                provider: VpngateIpProvider::Proxycheck,
                score: proxycheck,
                country_code: "JP".to_owned(),
            },
            VpngateIpScore {
                provider: VpngateIpProvider::Ffraud,
                score: ffraud,
                country_code: "JP".to_owned(),
            },
            VpngateIpScore {
                provider: VpngateIpProvider::Iplogs,
                score: iplogs,
                country_code: "JP".to_owned(),
            },
        ]
    }

    fn networks() -> Vec<VpngateIpNetwork> {
        [
            VpngateIpProvider::Proxycheck,
            VpngateIpProvider::Ffraud,
            VpngateIpProvider::Iplogs,
        ]
        .into_iter()
        .map(|provider| VpngateIpNetwork {
            provider,
            isp: Some("Example ISP".to_owned()),
            network_type: VpngateNetworkType::Business,
        })
        .collect()
    }

    #[test]
    fn three_provider_responses_preserve_score_isp_and_network_type() {
        let proxycheck = parse_proxycheck_response(
            "8.8.8.8",
            &serde_json::json!({
                "status": "ok",
                "8.8.8.8": {
                    "network": { "provider": "Google LLC", "type": "Business" },
                    "location": { "country_code": "US" },
                    "detections": { "risk": "11", "hosting": false }
                }
            }),
        )
        .unwrap();
        assert_eq!(proxycheck.provider, VpngateIpProvider::Proxycheck);
        assert_eq!(proxycheck.score, 11);
        assert_eq!(proxycheck.isp.as_deref(), Some("Google LLC"));
        assert_eq!(proxycheck.network_type, VpngateNetworkType::Business);

        let ffraud = parse_ffraud_response(&serde_json::json!({
            "success": true,
            "fraud_score": 22,
            "hosting": false,
            "mobile": false,
            "connection_type": "Residential",
            "ISP": "Example Fiber",
            "geo": { "country": "US" }
        }))
        .unwrap();
        assert_eq!(ffraud.provider, VpngateIpProvider::Ffraud);
        assert_eq!(ffraud.score, 22);
        assert_eq!(ffraud.network_type, VpngateNetworkType::Residential);

        let iplogs = parse_iplogs_response(&serde_json::json!({
            "score": 0.333,
            "ip_info": {
                "country_code": "US",
                "isp": "Example Mobile",
                "type": "cellular"
            }
        }))
        .unwrap();
        assert_eq!(iplogs.provider, VpngateIpProvider::Iplogs);
        assert_eq!(iplogs.score, 33);
        assert_eq!(iplogs.network_type, VpngateNetworkType::Mobile);
    }

    fn candidate(profile: String) -> VpngateCandidate {
        VpngateCandidate {
            server_id: "vpn1".to_owned(),
            hostname: "vpn1".to_owned(),
            country_code: "JP".to_owned(),
            remote_address: "192.0.2.10".to_owned(),
            remote_port: 1194,
            transport: VpngateTransport::Udp,
            profile_sha256: sha256_hex(profile.as_bytes()),
            openvpn_config: profile,
            verified_exit_ip: None,
            verified_exit_country_code: None,
            verified_ip_scores: Vec::new(),
            verified_ip_networks: Vec::new(),
        }
    }

    fn desired_pool() -> VpngateDesiredPool {
        VpngateDesiredPool {
            outbound_id: "vpngate-1111-1111".to_owned(),
            country_code: "JP".to_owned(),
            max_connect_ms: 15_000,
            min_download_bps: 1_000_000,
            max_candidates: 16,
            runtime_slot: 0,
            host_address: "169.254.240.1".to_owned(),
            peer_address: "169.254.240.2".to_owned(),
            prefix_len: 30,
            socks_port: 1080,
            candidates: Vec::new(),
            manual_switch: None,
        }
    }

    fn desired_pool_with_candidates(count: usize) -> VpngateDesiredPool {
        let mut pool = desired_pool();
        pool.candidates = (0..count)
            .map(|index| {
                let mut candidate = candidate(safe_profile());
                candidate.server_id = format!("vpn-{index}");
                candidate.hostname = format!("vpn-{index}");
                candidate
            })
            .collect();
        pool
    }

    fn local_sample(
        candidate: &VpngateCandidate,
        download_bps: u64,
        connect_ms: u32,
        probed_at_unix_secs: i64,
    ) -> RuntimeCandidateSample {
        RuntimeCandidateSample {
            server_id: candidate.server_id.clone(),
            profile_sha256: candidate.profile_sha256.clone(),
            connect_ms,
            download_bps,
            probed_at_unix_secs,
        }
    }

    fn runtime_metadata(
        pool: &VpngateDesiredPool,
        active_slot: u8,
        candidate_samples: Vec<RuntimeCandidateSample>,
    ) -> RuntimeMetadata {
        RuntimeMetadata {
            outbound_id: pool.outbound_id.clone(),
            runtime_slot: pool.runtime_slot,
            topology_revision: 1,
            catalog_generation: 1,
            active_slot,
            backends: pool
                .candidates
                .iter()
                .take(2)
                .enumerate()
                .map(|(slot, candidate)| RuntimeBackendMetadata {
                    slot: u8::try_from(slot).unwrap(),
                    server_id: candidate.server_id.clone(),
                    profile_sha256: candidate.profile_sha256.clone(),
                    peer_address: format!("169.254.132.{}", slot * 4 + 2),
                    connect_ms: 500,
                    last_verified_unix_secs: 1_000,
                    last_success_unix_millis: 1_000_000,
                    exit_ip: candidate
                        .verified_exit_ip
                        .clone()
                        .unwrap_or_else(|| format!("198.51.100.{}", slot + 10)),
                    consecutive_failures: 0,
                    failure_reason: None,
                })
                .collect(),
            refill_not_before_unix_millis: 0,
            refill_backoff_secs: 0,
            last_refill_failed: false,
            refill_excluded_server_ids: Vec::new(),
            last_failover_reason: None,
            cooldowns: Vec::new(),
            candidate_samples,
            last_manual_switch_result: None,
        }
    }

    fn safe_profile() -> String {
        "client\ndev tun\nproto udp\nremote 192.0.2.10 1194\nnobind\npersist-key\n\
         persist-tun\nauth-nocache\nresolv-retry 0\nverb 3\nscript-security 1\n\
         <ca>\nCA\n</ca>\n<cert>\nCERT\n</cert>\n<key>\nKEY\n</key>\n"
            .to_owned()
    }

    #[test]
    fn privileged_boundary_rejects_a_reintroduced_hook_even_with_a_matching_digest() {
        let mut profile = safe_profile();
        profile.push_str("up /tmp/provider-hook\n");
        assert!(validate_profile(&candidate(profile)).is_err());
        assert!(validate_profile(&candidate(safe_profile())).is_ok());
    }

    #[test]
    fn measurement_gates_use_provider_local_policy_and_missing_risk_fails_closed() {
        let pool = desired_pool();
        let policy = brocade_deployment::protocol::VpngateAdmissionPolicy::default();
        let sample = VpngateProbeSample {
            server_id: "vpn1".to_owned(),
            profile_sha256: "a".repeat(64),
            status: VpngateProbeStatus::Succeeded,
            exit_ip: Some("192.0.2.1".to_owned()),
            exit_country_code: Some("JP".to_owned()),
            connect_ms: Some(500),
            download_bps: Some(20_000_000),
            ip_scores: scores(10, 8, 6),
            ip_networks: networks(),
            error_code: None,
            error_detail: None,
            probed_at_unix_secs: 1,
        };
        assert!(sample_meets_policy(&pool, &policy, &sample));

        let mut slow_connect = sample.clone();
        slow_connect.connect_ms = Some(pool.max_connect_ms + 1);
        assert!(!sample_meets_policy(&pool, &policy, &slow_connect));

        let mut missing_connect_measurement = sample.clone();
        missing_connect_measurement.connect_ms = Some(0);
        assert!(!sample_meets_policy(
            &pool,
            &policy,
            &missing_connect_measurement
        ));

        let mut slow_download = sample.clone();
        slow_download.download_bps = Some(pool.min_download_bps - 1);
        assert!(!sample_meets_policy(&pool, &policy, &slow_download));

        let mut risky = sample.clone();
        risky.ip_scores = scores(81, 8, 6);
        assert!(!sample_meets_policy(&pool, &policy, &risky));

        let mut any_pass = policy.clone();
        any_pass.risk_decision_policy =
            brocade_deployment::protocol::VpngateRiskDecisionPolicy::AnyAvailablePass;
        assert!(sample_meets_policy(&pool, &any_pass, &risky));

        let mut unknown_risk = sample;
        unknown_risk.ip_scores.clear();
        assert!(!sample_meets_policy(&pool, &policy, &unknown_risk));

        let candidate = VpngateCandidate {
            verified_exit_ip: Some("198.51.100.20".to_owned()),
            verified_exit_country_code: Some("JP".to_owned()),
            verified_ip_scores: scores(8, 7, 6),
            verified_ip_networks: networks(),
            ..candidate(safe_profile())
        };
        assert!(candidate_trust_matches(
            &pool,
            &policy,
            &candidate,
            "198.51.100.20"
        ));
        assert!(!candidate_trust_matches(
            &pool,
            &policy,
            &candidate,
            "198.51.100.21"
        ));
    }

    #[test]
    fn local_candidate_rank_uses_window_averages_before_unmeasured_candidates() {
        let pool = desired_pool_with_candidates(4);
        let samples = vec![
            local_sample(&pool.candidates[0], 100_000_000, 100, 100),
            local_sample(&pool.candidates[0], 20_000_000, 300, 200),
            local_sample(&pool.candidates[1], 80_000_000, 900, 200),
            local_sample(&pool.candidates[1], 80_000_000, 700, 300),
            local_sample(&pool.candidates[2], 60_000_000, 100, 300),
        ];
        let mut random_keys = [0_u64].into_iter();
        let ranked = refill_candidates_with_random(&pool, &BTreeSet::new(), &samples, || {
            random_keys.next().ok_or("missing random key".to_owned())
        })
        .unwrap();
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.server_id.as_str())
                .collect::<Vec<_>>(),
            ["vpn-1", "vpn-2", "vpn-0", "vpn-3"]
        );
    }

    #[test]
    fn cold_start_candidates_use_random_keys_instead_of_server_order() {
        let pool = desired_pool_with_candidates(4);
        let excluded = ["vpn-2".to_owned()].into_iter().collect();
        let mut random_keys = [30_u64, 10, 0].into_iter();
        let ranked = refill_candidates_with_random(&pool, &excluded, &[], || {
            random_keys.next().ok_or("missing random key".to_owned())
        })
        .unwrap();

        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.server_id.as_str())
                .collect::<Vec<_>>(),
            ["vpn-3", "vpn-1", "vpn-0"]
        );
    }

    #[test]
    fn local_candidate_samples_expire_at_the_configured_window() {
        let mut pool = desired_pool_with_candidates(2);
        let now = 10_000;
        let window = 900;
        let mut samples = vec![
            local_sample(&pool.candidates[0], 10_000_000, 500, now - window),
            local_sample(&pool.candidates[1], 20_000_000, 400, now - window - 1),
        ];
        let mut replaced_profile = local_sample(&pool.candidates[0], 99_000_000, 100, now);
        replaced_profile.profile_sha256 = "f".repeat(64);
        samples.push(replaced_profile);

        retain_fresh_candidate_samples(&mut samples, &pool, now, window);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].server_id, "vpn-0");

        pool.candidates[0].profile_sha256 = "e".repeat(64);
        retain_fresh_candidate_samples(&mut samples, &pool, now, window);
        assert!(samples.is_empty());
    }

    #[test]
    fn active_switch_reuses_the_freshest_local_sample_without_reprobing() {
        let mut pool = desired_pool_with_candidates(2);
        pool.candidates[1].verified_exit_ip = Some("198.51.100.11".to_owned());
        pool.candidates[1].verified_exit_country_code = Some("JP".to_owned());
        pool.candidates[1].verified_ip_scores = scores(8, 7, 6);
        pool.candidates[1].verified_ip_networks = networks();
        let candidate_samples = vec![
            local_sample(&pool.candidates[1], 20_000_000, 800, 1_100),
            local_sample(&pool.candidates[1], 40_000_000, 600, 1_200),
        ];
        let metadata = runtime_metadata(&pool, 1, candidate_samples);

        let sample = cached_active_sample(&pool, &metadata, &[]).unwrap();

        assert_eq!(sample.server_id, "vpn-1");
        assert_eq!(sample.exit_ip.as_deref(), Some("198.51.100.11"));
        assert_eq!(sample.exit_country_code.as_deref(), Some("JP"));
        assert_eq!(sample.connect_ms, Some(600));
        assert_eq!(sample.download_bps, Some(40_000_000));
        assert_eq!(sample.probed_at_unix_secs, 1_200);
        assert_eq!(sample.ip_scores, scores(8, 7, 6));
        assert!(cached_active_sample(&pool, &metadata, &[sample]).is_none());
    }

    #[test]
    fn missing_switch_sample_prioritizes_the_active_backend_on_the_next_pass() {
        let pool = desired_pool_with_candidates(2);
        let mut metadata = runtime_metadata(&pool, 1, Vec::new());
        metadata.backends[0].last_verified_unix_secs = 100;
        metadata.backends[1].last_verified_unix_secs = 0;

        assert!(cached_active_sample(&pool, &metadata, &[]).is_none());
        assert_eq!(next_full_probe_index(&metadata, 2_000), Some(1));
    }

    #[test]
    fn runtime_health_budget_leaves_room_for_switching_within_thirty_seconds() {
        let worst_detection = RUNTIME_INTERVAL
            + Duration::from_secs(
                u64::from(HEALTH_FAILURE_THRESHOLD) * u64::from(HEALTH_REQUEST_TIMEOUT_SECS),
            );
        let iptables_lock_budget = Duration::from_secs(5);
        assert!(
            worst_detection + iptables_lock_budget + TCP_REACH_TIMEOUT < Duration::from_secs(30)
        );
    }

    #[test]
    fn desired_pool_accepts_sixteen_candidates_and_rejects_seventeen() {
        let make_desired = |count: usize| {
            let mut pool = desired_pool();
            pool.max_candidates = u8::try_from(count).unwrap();
            pool.candidates = (0..count)
                .map(|index| {
                    let mut candidate = candidate(safe_profile());
                    candidate.server_id = format!("vpn-{index}");
                    candidate.hostname = format!("vpn-{index}");
                    candidate
                })
                .collect();
            VpngateDesiredState {
                topology_revision: 1,
                catalog_generation: 1,
                admission_policy: Default::default(),
                pools: vec![pool],
                probe_assignments: Vec::new(),
            }
        };
        assert!(validate_desired(&make_desired(16)).is_ok());
        assert!(validate_desired(&make_desired(17)).is_err());
    }

    #[test]
    fn manual_switch_requires_the_fixed_ten_minute_cooldown() {
        let make_desired = |cooldown_secs| {
            let mut pool = desired_pool();
            pool.manual_switch = Some(VpngateManualSwitchCommand {
                request_id: 7,
                previous_server_id: "vpn-previous".to_owned(),
                cooldown_secs,
            });
            VpngateDesiredState {
                topology_revision: 1,
                catalog_generation: 1,
                admission_policy: Default::default(),
                pools: vec![pool],
                probe_assignments: Vec::new(),
            }
        };
        assert!(validate_desired(&make_desired(600)).is_ok());
        assert!(validate_desired(&make_desired(599)).is_err());
        assert_eq!(manual_switch_cooldown_until(12_345, 600), 612_345);
    }

    #[test]
    fn desired_catalogue_batch_accepts_128_workers_independent_from_pool_size() {
        let make_desired = |count: usize| VpngateDesiredState {
            topology_revision: 1,
            catalog_generation: 1,
            admission_policy: Default::default(),
            pools: Vec::new(),
            probe_assignments: vec![brocade_deployment::protocol::VpngateProbeAssignment {
                country_code: "JP".to_owned(),
                candidates: (0..count)
                    .map(|index| {
                        let mut candidate = candidate(safe_profile());
                        candidate.server_id = format!("vpn-{index}");
                        candidate.hostname = format!("vpn-{index}");
                        candidate
                    })
                    .collect(),
            }],
        };

        assert!(validate_desired(&make_desired(128)).is_ok());
        assert!(validate_desired(&make_desired(129)).is_err());
    }

    #[test]
    fn failed_active_and_standby_are_replaced_from_the_remaining_reservoir() {
        let mut pool = desired_pool();
        pool.candidates = (0..4)
            .map(|index| {
                let mut candidate = candidate(safe_profile());
                candidate.server_id = format!("vpn-{index}");
                candidate
            })
            .collect();
        let excluded = ["vpn-0".to_owned(), "vpn-1".to_owned()]
            .into_iter()
            .collect();
        let mut random_keys = [0_u64, 1].into_iter();
        assert_eq!(
            refill_candidates_with_random(&pool, &excluded, &[], || {
                random_keys.next().ok_or("missing random key".to_owned())
            })
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.server_id.as_str())
            .collect::<Vec<_>>(),
            ["vpn-2", "vpn-3"]
        );
    }

    #[test]
    fn staging_and_persistent_networks_do_not_overlap() {
        let pool = desired_pool();
        assert_eq!(persistent_namespace(&pool).cidr(), "169.254.240.0/30");
        assert_eq!(probe_namespace(&pool).cidr(), "169.254.128.0/30");
        assert_eq!(backend_namespace(&pool, 0).cidr(), "169.254.132.0/30");
        assert_eq!(backend_namespace(&pool, 1).cidr(), "169.254.132.4/30");
        assert_ne!(
            backend_namespace(&pool, 0).cidr(),
            backend_namespace(&pool, 1).cidr()
        );
        let catalogue_networks = (0..usize::from(CATALOG_PROBE_WORKERS))
            .map(|slot| catalog_probe_namespace(slot).cidr())
            .collect::<BTreeSet<_>>();
        assert_eq!(catalogue_networks.len(), usize::from(CATALOG_PROBE_WORKERS));
        assert!(catalogue_networks.contains("169.254.129.0/30"));
        assert!(catalogue_networks.contains("169.254.130.252/30"));
        assert!(!catalogue_networks.contains(&persistent_namespace(&pool).cidr()));
        assert!(!catalogue_networks.contains(&probe_namespace(&pool).cidr()));
        assert!(!catalogue_networks.contains(&legacy_catalog_probe_namespace().cidr()));
    }

    #[test]
    fn stable_route_keeps_one_xray_endpoint_while_following_the_active_backend() {
        let pool = desired_pool();

        assert_eq!(
            stable_route(&pool, 0),
            ("169.254.240.2/32".to_owned(), "bvb00000h".to_owned())
        );
        assert_eq!(
            stable_route(&pool, 1),
            ("169.254.240.2/32".to_owned(), "bvb00001h".to_owned())
        );
    }

    #[test]
    fn managed_backend_processes_survive_agent_mount_namespace_changes_without_duplicates() {
        let root = std::env::temp_dir().join(format!(
            "brocade-vpngate-proc-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let proc_root = root.join("proc");
        let state_dir = root.join("state");
        let pool_dir = state_dir.join("vpngate/pools/vpngate-1111-1111");
        let profile = pool_dir.join("profile-0.ovpn");
        let helper = pool_dir.join("socks-xray-0.json");
        let write_process = |pid: u32, comm: &str, cmdline: Vec<u8>| {
            let process_dir = proc_root.join(pid.to_string());
            fs::create_dir_all(&process_dir).unwrap();
            fs::write(process_dir.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(process_dir.join("cmdline"), cmdline).unwrap();
        };
        write_process(
            101,
            "openvpn",
            format!("openvpn\0--config\0{}\0", profile.display()).into_bytes(),
        );
        write_process(
            102,
            "xray",
            format!("/usr/local/bin/xray\0run\0-config\0{}\0", helper.display()).into_bytes(),
        );
        // Merely mentioning a managed path is not enough: the exact config flag/path pair and
        // executable name are required before cleanup is allowed to signal a process.
        write_process(
            103,
            "sh",
            format!("sh\0-c\0inspect {}\0", profile.display()).into_bytes(),
        );
        write_process(
            104,
            "openvpn",
            format!(
                "openvpn\0--config\0{}\0",
                pool_dir.join("other.ovpn").display()
            )
            .into_bytes(),
        );
        write_process(
            105,
            "openvpn",
            format!(
                "openvpn\0--config\0{}\0",
                state_dir
                    .join("vpngate/catalog-probe/worker-07/probe.ovpn")
                    .display()
            )
            .into_bytes(),
        );
        write_process(
            106,
            "openvpn",
            format!(
                "openvpn\0--config\0{}\0",
                state_dir
                    .join("vpngate/catalog-probe/worker-128/probe.ovpn")
                    .display()
            )
            .into_bytes(),
        );

        let processes = managed_backend_processes_in(&proc_root, &pool_dir, &BTreeSet::from([0]));

        assert_eq!(
            processes,
            vec![
                ManagedBackendProcess {
                    pid: 101,
                    slot: 0,
                    kind: BackendProcessKind::Openvpn,
                },
                ManagedBackendProcess {
                    pid: 102,
                    slot: 0,
                    kind: BackendProcessKind::Xray,
                },
            ]
        );
        assert_eq!(
            managed_catalog_probe_processes_in(&proc_root, &state_dir),
            vec![105]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn catalogue_executor_is_bounded_and_preserves_input_order() {
        let tasks = (0..256_u32).collect::<Vec<_>>();
        let active_workers = AtomicUsize::new(0);
        let peak_workers = AtomicUsize::new(0);
        let active_speed_tests = AtomicUsize::new(0);
        let peak_speed_tests = AtomicUsize::new(0);
        let speed_gate = ConcurrencyGate::new(CATALOG_SPEED_WORKERS);
        let completed =
            bounded_parallel_map(&tasks, usize::from(CATALOG_PROBE_WORKERS), |_slot, task| {
                let workers = active_workers.fetch_add(1, Ordering::SeqCst) + 1;
                peak_workers.fetch_max(workers, Ordering::SeqCst);
                let _speed_permit = speed_gate.enter();
                let speed_tests = active_speed_tests.fetch_add(1, Ordering::SeqCst) + 1;
                peak_speed_tests.fetch_max(speed_tests, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active_speed_tests.fetch_sub(1, Ordering::SeqCst);
                active_workers.fetch_sub(1, Ordering::SeqCst);
                *task
            });

        assert_eq!(completed, tasks);
        assert_eq!(peak_workers.load(Ordering::SeqCst), 128);
        assert_eq!(
            peak_speed_tests.load(Ordering::SeqCst),
            CATALOG_SPEED_WORKERS
        );
    }

    #[test]
    fn catalogue_worker_budget_covers_twelve_hundred_observed_duration_tasks_per_hour() {
        const TARGET_CANDIDATES: usize = 1_200;
        const OBSERVED_DURATION_BUDGET_SECS: usize = 45;
        let waves = TARGET_CANDIDATES.div_ceil(usize::from(CATALOG_PROBE_WORKERS));
        assert!(waves * OBSERVED_DURATION_BUDGET_SECS <= 60 * 60);
    }

    #[test]
    fn completed_catalogue_batch_immediately_requests_more_work() {
        assert_eq!(next_cycle_delay(true), CONTINUOUS_PROBE_INTERVAL);
        assert_eq!(next_cycle_delay(false), DESIRED_INTERVAL);
    }
}
