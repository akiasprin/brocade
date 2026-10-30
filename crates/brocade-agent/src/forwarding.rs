use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use brocade_core::artifacts::xray::FORWARD_OUTBOUND_TAG;
use brocade_core::forwarding::{target_host_is_public_ip, target_ip_is_blocked_by_policy};
use brocade_deployment::protocol::{
    ForwardingDesiredState, ForwardingReconcileReport, ForwardingRuleDesired,
    ForwardingRuleReconcileResult, ForwardingUsageCounter, ForwardingUsageReportRequest,
};

use crate::{
    current_unix_secs,
    fsutil::atomic_write_private,
    http::HttpClient,
    next_periodic_tick,
    options::Options,
    reserve_usage_sequence,
    spool::{spool_push, FORWARDING_USAGE_SPOOL},
    xray_api_address, xray_grpc, xray_process_identity,
};

const FORWARDING_INTERVAL: Duration = Duration::from_secs(15);
const FORWARDING_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const FORWARDING_STATE_FILE: &str = "forwarding-applied.json";
const FORWARDING_DESIRED_FILE: &str = "forwarding-desired.json";
const FORWARDING_TAG_PREFIX: &str = "fwd:";

pub(crate) fn run_forever(options: Options, meter: Arc<Mutex<()>>) {
    let mut wake = crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::Desired);
    let mut tick = Instant::now();
    loop {
        let interval = match run_round(&options, &meter) {
            Ok(true) => FORWARDING_INTERVAL,
            Ok(false) => FORWARDING_RETRY_INTERVAL,
            Err(error) => {
                eprintln!("forwarding: {error}");
                FORWARDING_RETRY_INTERVAL
            }
        };
        let now = Instant::now();
        tick = next_periodic_tick(tick, interval, now);
        if wake.wait(tick.saturating_duration_since(now)) {
            tick = Instant::now();
        }
    }
}

fn run_round(options: &Options, meter: &Mutex<()>) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    let desired = fetch_desired(&client, options)?;
    // Persist the complete control-plane answer before changing the runtime. The applied
    // checkpoint below is intentionally allowed to become partial when one listener fails; using
    // that partial set as the restart source would silently forget the missing rule forever.
    write_desired_state(&options.state_dir, &desired)?;
    let (report, usage) = {
        let _guard = meter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _deadline = crate::xray_grpc::deadline_after(
            options
                .state_dir
                .join(crate::xrayupdate::UPGRADE_RUNTIME_FILE)
                .exists()
                .then_some(Duration::from_secs(1)),
        );
        let report = reconcile(&options.state_dir, &desired);
        let usage = collect_usage(&options.state_dir);
        (report, usage)
    };
    if let Ok(usage_report) = usage {
        spool_push(FORWARDING_USAGE_SPOOL, &options.state_dir, &usage_report)?;
    } else if let Err(error) = usage {
        eprintln!("forwarding usage: {error}");
    }
    let complete = report.error.is_none();
    send_reconcile(&client, options, &report)?;
    Ok(complete)
}

/// Restore the last complete desired set without contacting the control plane.
///
/// Older Agents only wrote `forwarding-applied.json`. On the first restart after upgrading, that
/// proven-serving set is the safest recovery source available; persist the migration immediately
/// so later partial reconciles can no longer shrink it.
pub(crate) fn restore_after_xray_restart(
    state_dir: &Path,
    xray_content: &str,
    api_address: SocketAddr,
) -> Result<Option<ForwardingReconcileReport>, String> {
    let Some(desired) = desired_state_on_disk(state_dir)? else {
        return Ok(None);
    };
    let report = reconcile_listeners(
        state_dir,
        &desired,
        forwarding_outbound_is_ready(xray_content),
        &mut XrayForwardingApi(api_address, target_policy_supported(api_address)),
    )?;
    Ok(Some(report))
}

pub(crate) fn recovery_complete(state_dir: &Path, api_address: SocketAddr) -> Result<bool, String> {
    let Some(mut desired) = desired_state_on_disk(state_dir)? else {
        return Ok(true);
    };
    for rule in &mut desired.rules {
        rule.allow_private_targets = Some(rule.allow_private_targets.unwrap_or(false));
    }
    let wanted = desired
        .rules
        .iter()
        .map(|rule| rule.id)
        .collect::<BTreeSet<_>>();
    let actual = xray_grpc::inbound_tags(api_address)?
        .into_iter()
        .filter_map(|tag| forwarding_id(&tag))
        .collect::<BTreeSet<_>>();
    if actual != wanted {
        return Ok(false);
    }
    let applied = read_state(state_dir)?;
    let applied = applied
        .rules
        .into_iter()
        .map(|rule| (rule.id, rule))
        .collect::<BTreeMap<_, _>>();
    let desired = desired
        .rules
        .iter()
        .cloned()
        .map(|rule| (rule.id, rule))
        .collect::<BTreeMap<_, _>>();
    Ok(applied == desired)
}

fn fetch_desired(client: &HttpClient, options: &Options) -> Result<ForwardingDesiredState, String> {
    let response = client.request("GET", "/agent/v1/forwarding/desired", &options.token, None)?;
    if response.status != 200 {
        return Err(format!(
            "desired request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body)
        .map_err(|error| format!("decode forwarding desired state: {error}"))
}

fn send_reconcile(
    client: &HttpClient,
    options: &Options,
    report: &ForwardingReconcileReport,
) -> Result<(), String> {
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/forwarding/reconcile",
        &options.token,
        Some(&body),
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "reconcile report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

fn reconcile(state_dir: &Path, desired: &ForwardingDesiredState) -> ForwardingReconcileReport {
    match reconcile_inner(state_dir, desired) {
        Ok(report) => report,
        Err(error) => {
            let applied_rule_ids = current_forwarding_rule_ids(state_dir).unwrap_or_default();
            ForwardingReconcileReport {
                rule_results: None,
                generation: desired.generation,
                applied_rule_ids,
                error: Some(error.chars().take(900).collect()),
            }
        }
    }
}

fn reconcile_inner(
    state_dir: &Path,
    desired: &ForwardingDesiredState,
) -> Result<ForwardingReconcileReport, String> {
    let xray_path = state_dir.join("xray.json");
    let content = match fs::read_to_string(&xray_path) {
        Ok(content) if !state_dir.join("xray.disabled").exists() => content,
        Ok(_) | Err(_) if desired.rules.is_empty() => {
            write_state(state_dir, desired)?;
            return Ok(ForwardingReconcileReport {
                rule_results: None,
                generation: desired.generation,
                applied_rule_ids: Vec::new(),
                error: None,
            });
        }
        Ok(_) => return Err("Xray is disabled while forwarding rules are enabled".to_owned()),
        Err(error) => return Err(format!("read {}: {error}", xray_path.display())),
    };
    let api_address = xray_api_address(&content).ok_or("xray config has no API address")?;
    reconcile_listeners(
        state_dir,
        desired,
        forwarding_outbound_is_ready(&content),
        &mut XrayForwardingApi(api_address, target_policy_supported(api_address)),
    )
}

trait ForwardingRuntime {
    fn target_policy_supported(&self) -> bool {
        true
    }
    fn public_target(&self, _tag: &str) -> bool {
        false
    }
    fn inbound_tags(&mut self) -> Result<Vec<String>, String>;
    fn remove(&mut self, tag: &str) -> Result<(), String>;
    fn add(&mut self, rule: &ForwardingRuleDesired) -> Result<(), String>;
    fn preflight_replacement(
        &mut self,
        _previous: &ForwardingRuleDesired,
        _replacement: &ForwardingRuleDesired,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct XrayForwardingApi(SocketAddr, bool);

fn target_policy_supported(api_address: SocketAddr) -> bool {
    let _deadline = xray_grpc::deadline_after(Some(Duration::from_secs(2)));
    xray_grpc::forwarding_target_policy_supported(api_address)
}

impl ForwardingRuntime for XrayForwardingApi {
    fn target_policy_supported(&self) -> bool {
        self.1
    }
    fn public_target(&self, tag: &str) -> bool {
        xray_grpc::forwarding_target_is_public(self.0, tag)
    }
    fn inbound_tags(&mut self) -> Result<Vec<String>, String> {
        xray_grpc::inbound_tags(self.0)
    }

    fn remove(&mut self, tag: &str) -> Result<(), String> {
        xray_grpc::remove_inbound(self.0, tag)
    }

    fn add(&mut self, rule: &ForwardingRuleDesired) -> Result<(), String> {
        xray_grpc::add_forwarding_inbound(
            self.0,
            &forwarding_tag(rule.id),
            rule.protocol,
            rule.listen_port,
            &rule.target_host,
            rule.target_port,
            rule.allow_private_targets.unwrap_or(false),
        )
        .map_err(|error| {
            format!(
                "listen port {} could not be installed: {error}",
                rule.listen_port
            )
        })
    }

    fn preflight_replacement(
        &mut self,
        previous: &ForwardingRuleDesired,
        replacement: &ForwardingRuleDesired,
    ) -> Result<(), String> {
        preflight_forwarding_listener(previous, replacement)
    }
}

fn preflight_forwarding_listener(
    previous: &ForwardingRuleDesired,
    replacement: &ForwardingRuleDesired,
) -> Result<(), String> {
    let port_changed = previous.listen_port != replacement.listen_port;
    let adds_tcp = protocol_has_tcp(replacement.protocol)
        && (port_changed || !protocol_has_tcp(previous.protocol));
    let adds_udp = protocol_has_udp(replacement.protocol)
        && (port_changed || !protocol_has_udp(previous.protocol));
    let _tcp = adds_tcp
        .then(|| TcpListener::bind((Ipv4Addr::UNSPECIFIED, replacement.listen_port)))
        .transpose()
        .map_err(|error| {
            format!(
                "TCP port {} is unavailable: {error}",
                replacement.listen_port
            )
        })?;
    let _udp = adds_udp
        .then(|| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, replacement.listen_port)))
        .transpose()
        .map_err(|error| {
            format!(
                "UDP port {} is unavailable: {error}",
                replacement.listen_port
            )
        })?;
    Ok(())
}

fn protocol_has_tcp(protocol: brocade_deployment::protocol::ForwardingProtocol) -> bool {
    matches!(
        protocol,
        brocade_deployment::protocol::ForwardingProtocol::Tcp
            | brocade_deployment::protocol::ForwardingProtocol::TcpUdp
    )
}

fn protocol_has_udp(protocol: brocade_deployment::protocol::ForwardingProtocol) -> bool {
    matches!(
        protocol,
        brocade_deployment::protocol::ForwardingProtocol::Udp
            | brocade_deployment::protocol::ForwardingProtocol::TcpUdp
    )
}

// DNS is deliberately absent: only Xray knows the resolver and final address used by a flow.
// The checkpoint contains only definitions we can prove are installed, including partial rounds.
fn reconcile_listeners(
    state_dir: &Path,
    desired: &ForwardingDesiredState,
    outbound_ready: bool,
    runtime: &mut impl ForwardingRuntime,
) -> Result<ForwardingReconcileReport, String> {
    let mut desired = desired.clone();
    for rule in &mut desired.rules {
        rule.allow_private_targets = Some(rule.allow_private_targets.unwrap_or(false));
    }
    let checkpoint = read_checkpoint(state_dir);
    let checkpoint_error = checkpoint.as_ref().err().cloned();
    // Missing evidence cannot authorize replacing wanted listeners, but must not prevent
    // revocations. Preserve those listeners and explicitly report a damaged checkpoint.
    let checkpoint = checkpoint.unwrap_or_else(|_| ForwardingCheckpoint {
        applied: ForwardingDesiredState {
            generation: 0,
            rules: Vec::new(),
        },
        installing_rule_ids: BTreeSet::new(),
    });
    let mut installing = checkpoint.installing_rule_ids;
    let mut known = checkpoint
        .applied
        .rules
        .into_iter()
        .map(|rule| (rule.id, rule))
        .collect::<BTreeMap<_, _>>();
    let policy_supported = runtime.target_policy_supported();
    let wanted = desired
        .rules
        .iter()
        .filter(|rule| {
            let literal_blocked =
                rule.target_host
                    .trim_matches(['[', ']'])
                    .parse()
                    .is_ok_and(|address| {
                        target_ip_is_blocked_by_policy(
                            address,
                            rule.allow_private_targets.unwrap_or(false),
                        )
                    });
            !literal_blocked && (policy_supported || target_host_is_public_ip(&rule.target_host))
        })
        .map(|rule| (rule.id, rule))
        .collect::<BTreeMap<_, _>>();
    let actual = runtime
        .inbound_tags()?
        .into_iter()
        .filter_map(|tag| forwarding_id(&tag).map(|id| (id, tag)))
        .collect::<BTreeMap<_, _>>();
    let mut remaining = actual.keys().copied().collect::<BTreeSet<_>>();
    // The checkpoint describes definitions that are still proven to exist. Keep the previous
    // definition until its listener has actually been removed: it is also the rollback payload
    // if the replacement cannot be installed.
    known.retain(|id, _| actual.contains_key(id));
    installing.retain(|id| actual.contains_key(id) && !known.contains_key(id));
    write_checkpoint(state_dir, desired.generation, &known, &installing)?;
    let mut errors = ReconcileErrors::default();
    for rule in &desired.rules {
        if !wanted.contains_key(&rule.id) {
            errors.push_rule(
                rule.id,
                format!(
                    "rule {} target is blocked or running Xray needs forwarding-target-policy-v1",
                    rule.id
                ),
            );
        }
    }
    if let Some(error) = checkpoint_error {
        errors.push(error);
    }
    if !outbound_ready && !desired.rules.is_empty() {
        errors.push(format!(
            "Xray forwarding requires {FORWARD_OUTBOUND_TAG} as the first freedom outbound; publish a compatible Xray configuration first"
        ));
    }

    // Revoke before adding anything. If the shared outbound is temporarily unsuitable, security
    // revocations still proceed, but authorized listeners are frozen instead of being torn down.
    // A failed operation must not block unrelated rules.
    for (id, tag) in actual {
        let unknown = !known.contains_key(&id) && !installing.contains(&id);
        let proven_public = unknown
            && wanted
                .get(&id)
                .is_some_and(|rule| target_host_is_public_ip(&rule.target_host))
            && runtime.public_target(&tag);
        // A fixed public IP has the same allowed destination under either private-network
        // policy. Preserve its socket, including legacy checkpoints, when only that flag changes.
        if let (Some(previous), Some(replacement)) = (known.get_mut(&id), wanted.get(&id)) {
            if target_host_is_public_ip(&replacement.target_host) {
                previous.allow_private_targets = replacement.allow_private_targets;
            }
        }
        let unchanged = known
            .get(&id)
            .zip(wanted.get(&id))
            .is_some_and(|(previous, desired)| previous == *desired);
        let revoked = !wanted.contains_key(&id);
        let security_revocation = (unknown && !proven_public)
            || wanted.get(&id).is_some_and(|replacement| {
                !target_host_is_public_ip(&replacement.target_host)
                    && previous_policy_requires_replacement(known.get(&id), replacement)
            });
        if revoked || (outbound_ready && !unchanged) || security_revocation {
            let previous = known.get(&id).cloned();
            if !revoked && previous.is_none() && !installing.contains(&id) && proven_public {
                errors.push_rule(
                    id,
                    format!(
                        "rule {id} has no applied checkpoint; preserving its existing listener"
                    ),
                );
                continue;
            }
            if let (Some(previous), Some(replacement)) = (previous.as_ref(), wanted.get(&id)) {
                if let Err(error) = runtime.preflight_replacement(previous, replacement) {
                    errors.push_rule(id, format!("rule {id} preflight failed: {error}"));
                    continue;
                }
            }
            match runtime.remove(&tag) {
                Ok(()) => {
                    remaining.remove(&id);
                    known.remove(&id);
                    installing.remove(&id);
                    write_checkpoint(state_dir, desired.generation, &known, &installing)?;

                    if let Some(replacement) = wanted.get(&id).filter(|_| outbound_ready) {
                        match install_listener(
                            state_dir,
                            desired.generation,
                            runtime,
                            replacement,
                            &mut known,
                            &mut installing,
                        ) {
                            Ok(()) => {
                                remaining.insert(id);
                            }
                            Err(error) => {
                                errors.push_rule(id, format!("rule {id} add failed: {error}"));
                                if let Some(previous) = previous.filter(|previous| {
                                    replacement.allow_private_targets.unwrap_or(false)
                                        || !previous.allow_private_targets.unwrap_or(false)
                                }) {
                                    match restore_listener(
                                        state_dir,
                                        desired.generation,
                                        runtime,
                                        &previous,
                                        &mut known,
                                        &mut installing,
                                    ) {
                                        Ok(()) => {
                                            remaining.insert(id);
                                        }
                                        Err(rollback_error) => errors.push_rule(
                                            id,
                                            format!("rule {id} restore failed: {rollback_error}"),
                                        ),
                                    }
                                }
                            }
                        }
                    }
                }
                Err(error) => errors.push_rule(id, format!("rule {id} remove failed: {error}")),
            }
        }
    }
    if outbound_ready {
        for rule in wanted.values() {
            if remaining.contains(&rule.id) {
                continue;
            }
            known.remove(&rule.id);
            if let Err(error) = install_listener(
                state_dir,
                desired.generation,
                runtime,
                rule,
                &mut known,
                &mut installing,
            ) {
                errors.push_rule(rule.id, format!("rule {} add failed: {error}", rule.id));
            }
        }
    }
    let actual = runtime
        .inbound_tags()?
        .into_iter()
        .filter_map(|tag| forwarding_id(&tag))
        .collect::<BTreeSet<_>>();
    known.retain(|id, _| actual.contains(id));
    if actual != wanted.keys().copied().collect()
        || wanted.iter().any(|(id, rule)| known.get(id) != Some(*rule))
    {
        errors.push(
            "Xray forwarding listeners have not fully converged; retrying next round".to_owned(),
        );
    }
    write_checkpoint(state_dir, desired.generation, &known, &installing)?;
    Ok(ForwardingReconcileReport {
        rule_results: Some(
            desired.rules
                .iter()
                .map(|rule| ForwardingRuleReconcileResult {
                    rule: rule.clone(),
                    applied: wanted.contains_key(&rule.id) && known.get(&rule.id) == Some(rule),
                    error: errors
                        .rules
                        .get(&rule.id)
                        .filter(|_| !wanted.contains_key(&rule.id) || known.get(&rule.id) != Some(rule))
                        .cloned()
                        .or_else(|| (!outbound_ready && known.get(&rule.id) != Some(rule)).then(||
                            "Forwarding outbound is unavailable; publish a compatible Xray configuration first".to_owned())),
                })
                .collect(),
        ),
        generation: desired.generation,
        applied_rule_ids: actual.into_iter().collect(),
        error: errors.finish(),
    })
}

fn previous_policy_requires_replacement(
    previous: Option<&ForwardingRuleDesired>,
    replacement: &ForwardingRuleDesired,
) -> bool {
    previous.is_none_or(|rule| {
        rule.allow_private_targets.is_none()
            || (!replacement.allow_private_targets.unwrap_or(false)
                && rule.allow_private_targets.unwrap_or(false))
    })
}

fn restore_listener(
    state_dir: &Path,
    generation: i64,
    runtime: &mut impl ForwardingRuntime,
    previous: &ForwardingRuleDesired,
    known: &mut BTreeMap<i64, ForwardingRuleDesired>,
    installing: &mut BTreeSet<i64>,
) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 0..20 {
        match install_listener(state_dir, generation, runtime, previous, known, installing) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt < 19 {
            thread::sleep(Duration::from_millis(50));
        }
    }
    Err(last_error.unwrap_or_else(|| "listener restore failed".to_owned()))
}

fn write_checkpoint(
    state_dir: &Path,
    generation: i64,
    known: &BTreeMap<i64, ForwardingRuleDesired>,
    installing: &BTreeSet<i64>,
) -> Result<(), String> {
    let checkpoint = ForwardingCheckpoint {
        applied: ForwardingDesiredState {
            generation,
            rules: known.values().cloned().collect(),
        },
        installing_rule_ids: installing.clone(),
    };
    let content = serde_json::to_vec_pretty(&checkpoint).map_err(|error| error.to_string())?;
    atomic_write_private(&state_dir.join(FORWARDING_STATE_FILE), &content)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ForwardingCheckpoint {
    #[serde(flatten)]
    applied: ForwardingDesiredState,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    installing_rule_ids: BTreeSet<i64>,
}

fn read_checkpoint(state_dir: &Path) -> Result<ForwardingCheckpoint, String> {
    match fs::read(state_dir.join(FORWARDING_STATE_FILE)) {
        Ok(content) => serde_json::from_slice(&content)
            .map_err(|error| format!("decode forwarding checkpoint: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ForwardingCheckpoint {
            applied: ForwardingDesiredState {
                generation: 0,
                rules: Vec::new(),
            },
            installing_rule_ids: BTreeSet::new(),
        }),
        Err(error) => Err(format!("read forwarding checkpoint: {error}")),
    }
}

fn install_listener(
    state_dir: &Path,
    generation: i64,
    runtime: &mut impl ForwardingRuntime,
    rule: &ForwardingRuleDesired,
    known: &mut BTreeMap<i64, ForwardingRuleDesired>,
    installing: &mut BTreeSet<i64>,
) -> Result<(), String> {
    // Ownership must survive a lost AddInbound response or process exit. Never tear down an
    // uncheckpointed listener unless this Agent durably recorded its installation attempt.
    installing.insert(rule.id);
    write_checkpoint(state_dir, generation, known, installing)?;
    if let Err(error) = runtime.add(rule) {
        let cleanup = (|| {
            if runtime.inbound_tags()?.contains(&forwarding_tag(rule.id)) {
                runtime.remove(&forwarding_tag(rule.id))?;
            }
            installing.remove(&rule.id);
            write_checkpoint(state_dir, generation, known, installing)
        })();
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; cleanup pending: {cleanup}"),
        });
    }
    known.insert(rule.id, rule.clone());
    installing.remove(&rule.id);
    write_checkpoint(state_dir, generation, known, installing)
}

#[derive(Default)]
struct ReconcileErrors {
    messages: Vec<String>,
    omitted: bool,
    rules: BTreeMap<i64, String>,
}

impl ReconcileErrors {
    fn push_rule(&mut self, id: i64, error: String) {
        if self.rules.len() < 1_024 {
            self.rules
                .entry(id)
                .or_insert_with(|| error.chars().take(96).collect());
        }
        self.push(error);
    }
    fn push(&mut self, error: String) {
        // Eight messages of at most 384 UTF-8 bytes fit the protocol's 4096-byte bound.
        if self.messages.len() < 8 {
            self.messages.push(error.chars().take(96).collect());
        } else {
            self.omitted = true;
        }
    }

    fn finish(mut self) -> Option<String> {
        if self.omitted {
            self.messages.push("additional errors omitted".to_owned());
        }
        (!self.messages.is_empty()).then(|| self.messages.join("; "))
    }
}

fn forwarding_outbound_is_ready(content: &str) -> bool {
    let Ok(config) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    let Some(outbound) = config
        .get("outbounds")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
    else {
        return false;
    };
    outbound.get("tag").and_then(|v| v.as_str()) == Some(FORWARD_OUTBOUND_TAG)
        && outbound.get("protocol").and_then(|v| v.as_str()) == Some("freedom")
}

fn current_forwarding_rule_ids(state_dir: &Path) -> Result<Vec<i64>, String> {
    let content = fs::read_to_string(state_dir.join("xray.json"))
        .map_err(|error| format!("read xray config: {error}"))?;
    let api_address = xray_api_address(&content).ok_or("xray config has no API address")?;
    Ok(xray_grpc::inbound_tags(api_address)?
        .into_iter()
        .filter_map(|tag| forwarding_id(&tag))
        .collect())
}

fn collect_usage(state_dir: &Path) -> Result<ForwardingUsageReportRequest, String> {
    let content = fs::read_to_string(state_dir.join("xray.json"))
        .map_err(|error| format!("read xray config: {error}"))?;
    let api_address = xray_api_address(&content).ok_or("xray config has no API address")?;
    let process = xray_process_identity(state_dir)?;
    let counter_epoch = crate::accounting::epoch(state_dir, &process.epoch, process.pid)?;
    let (agent_instance_id, sequence) = reserve_usage_sequence(state_dir)?;
    let counters = read_counters(api_address)?;
    crate::accounting::sync_live_journal(state_dir, &process.epoch, process.pid)?;
    crate::validate_xray_sample_process(state_dir, &process)?;
    Ok(ForwardingUsageReportRequest {
        agent_instance_id,
        sequence,
        read_at_unix_secs: current_unix_secs()?,
        xray_epoch: counter_epoch,
        counters: counters
            .into_iter()
            .map(
                |(rule_id, (uplink_bytes, downlink_bytes))| ForwardingUsageCounter {
                    rule_id,
                    uplink_bytes,
                    downlink_bytes,
                },
            )
            .collect(),
    })
}

pub(crate) fn read_counters(api_address: SocketAddr) -> Result<BTreeMap<i64, (u64, u64)>, String> {
    let mut counters = BTreeMap::<i64, (u64, u64)>::new();
    for stat in xray_grpc::query_stats(api_address, "inbound>>>fwd:")? {
        let Ok(value) = u64::try_from(stat.value) else {
            continue;
        };
        let Some((rule_id, direction)) = parse_stat_name(&stat.name) else {
            continue;
        };
        let entry = counters.entry(rule_id).or_default();
        match direction {
            "uplink" => entry.0 = value,
            "downlink" => entry.1 = value,
            _ => {}
        }
    }
    Ok(counters)
}

fn parse_stat_name(name: &str) -> Option<(i64, &str)> {
    let parts = name.split(">>>").collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "inbound" || parts[2] != "traffic" {
        return None;
    }
    let id = forwarding_id(parts[1])?;
    matches!(parts[3], "uplink" | "downlink").then_some((id, parts[3]))
}

fn forwarding_tag(id: i64) -> String {
    format!("{FORWARDING_TAG_PREFIX}{id}")
}

fn forwarding_id(tag: &str) -> Option<i64> {
    tag.strip_prefix(FORWARDING_TAG_PREFIX)?.parse().ok()
}

fn read_state(state_dir: &Path) -> Result<ForwardingDesiredState, String> {
    let content = fs::read_to_string(state_dir.join(FORWARDING_STATE_FILE))
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&content).map_err(|error| error.to_string())
}

pub(crate) fn desired_state_on_disk(
    state_dir: &Path,
) -> Result<Option<ForwardingDesiredState>, String> {
    let desired_path = state_dir.join(FORWARDING_DESIRED_FILE);
    match fs::read_to_string(&desired_path) {
        Ok(content) => serde_json::from_str(&content)
            .map(Some)
            .map_err(|error| format!("decode {}: {error}", desired_path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let applied_path = state_dir.join(FORWARDING_STATE_FILE);
            match fs::read_to_string(&applied_path) {
                Ok(content) => {
                    let state = serde_json::from_str::<ForwardingDesiredState>(&content)
                        .map_err(|error| format!("decode {}: {error}", applied_path.display()))?;
                    write_desired_state(state_dir, &state)?;
                    Ok(Some(state))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("read {}: {error}", applied_path.display())),
            }
        }
        Err(error) => Err(format!("read {}: {error}", desired_path.display())),
    }
}

fn write_state(state_dir: &Path, desired: &ForwardingDesiredState) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(desired).map_err(|error| error.to_string())?;
    atomic_write_private(&state_dir.join(FORWARDING_STATE_FILE), &content)
}

fn write_desired_state(state_dir: &Path, desired: &ForwardingDesiredState) -> Result<(), String> {
    let content = serde_json::to_vec_pretty(desired).map_err(|error| error.to_string())?;
    atomic_write_private(&state_dir.join(FORWARDING_DESIRED_FILE), &content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_deployment::protocol::ForwardingProtocol;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "brocade-forwarding-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct FakeRuntime {
        unsupported_target_policy: bool,
        rules: BTreeMap<i64, ForwardingRuleDesired>,
        fail_add: BTreeSet<i64>,
        partial_add: BTreeSet<i64>,
        fail_target_ports: BTreeSet<u16>,
        fail_target_attempts: BTreeMap<u16, usize>,
        fail_remove: BTreeSet<i64>,
        snapshot_calls: usize,
        fail_snapshot: Option<usize>,
        operations: Vec<String>,
    }
    impl ForwardingRuntime for FakeRuntime {
        fn target_policy_supported(&self) -> bool {
            !self.unsupported_target_policy
        }
        fn public_target(&self, tag: &str) -> bool {
            forwarding_id(tag)
                .and_then(|id| self.rules.get(&id))
                .is_some_and(|rule| target_host_is_public_ip(&rule.target_host))
        }
        fn inbound_tags(&mut self) -> Result<Vec<String>, String> {
            self.snapshot_calls += 1;
            if self.fail_snapshot == Some(self.snapshot_calls) {
                return Err("injected snapshot failure".to_owned());
            }
            Ok(std::iter::once("api".to_owned())
                .chain(self.rules.keys().map(|id| forwarding_tag(*id)))
                .collect())
        }
        fn remove(&mut self, tag: &str) -> Result<(), String> {
            let id = forwarding_id(tag).unwrap();
            self.operations.push(format!("remove:{id}"));
            if self.fail_remove.contains(&id) {
                return Err("injected removal failure".to_owned());
            }
            self.rules.remove(&id);
            Ok(())
        }
        fn add(&mut self, rule: &ForwardingRuleDesired) -> Result<(), String> {
            self.operations.push(format!("add:{}", rule.id));
            if let Some(remaining) = self.fail_target_attempts.get_mut(&rule.target_port) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err("injected transient add failure".to_owned());
                }
            }
            if self.fail_add.contains(&rule.id)
                || self.fail_target_ports.contains(&rule.target_port)
            {
                if self.partial_add.contains(&rule.id) {
                    self.rules.insert(rule.id, rule.clone());
                }
                return Err("injected add failure".to_owned());
            }
            self.rules.insert(rule.id, rule.clone());
            Ok(())
        }
    }
    fn rule(id: i64) -> ForwardingRuleDesired {
        ForwardingRuleDesired {
            id,
            protocol: ForwardingProtocol::TcpUdp,
            listen_port: 40_000 + id as u16,
            target_host: "unresolvable.invalid".to_owned(),
            target_port: 443,
            allow_private_targets: Some(false),
        }
    }

    #[test]
    fn forwarding_policy_changes_preserve_public_ip_connections() {
        let dir = TestDir::new();
        let mut previous = rule(1);
        previous.target_host = "1.1.1.1".to_owned();
        previous.allow_private_targets = None;
        write_state(&dir.0, &desired(vec![previous.clone()])).unwrap();
        let mut runtime = FakeRuntime {
            unsupported_target_policy: true,
            ..Default::default()
        };
        runtime.rules.insert(1, previous.clone());
        let mut wanted = previous;
        wanted.allow_private_targets = Some(false);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![wanted]), true, &mut runtime).unwrap();
        assert!(report.error.is_none());
        assert!(
            runtime.operations.is_empty(),
            "public listener must remain open"
        );
    }

    #[test]
    fn forwarding_replaces_legacy_domain_listeners_with_a_policy() {
        let dir = TestDir::new();
        let mut previous = rule(1);
        previous.allow_private_targets = None;
        write_state(&dir.0, &desired(vec![previous.clone()])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, previous);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![rule(1)]), true, &mut runtime).unwrap();
        assert!(report.error.is_none());
        assert_eq!(runtime.operations, ["remove:1", "add:1"]);
    }

    #[test]
    fn forwarding_missing_checkpoint_never_trusts_a_public_target_from_desired_state() {
        let dir = TestDir::new();
        let mut actual = rule(1);
        actual.target_host = "127.0.0.1".to_owned();
        let mut wanted = rule(1);
        wanted.target_host = "1.1.1.1".to_owned();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, actual);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![wanted]), false, &mut runtime).unwrap();
        assert!(report.error.is_some());
        assert_eq!(runtime.operations, ["remove:1"]);
        assert!(!runtime.rules.contains_key(&1));
    }

    #[test]
    fn forwarding_policy_tightening_never_restores_the_permissive_listener() {
        let dir = TestDir::new();
        let mut previous = rule(1);
        previous.allow_private_targets = Some(true);
        write_state(&dir.0, &desired(vec![previous.clone()])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, previous);
        runtime.fail_add.insert(1);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![rule(1)]), true, &mut runtime).unwrap();
        assert!(report.error.is_some());
        assert_eq!(runtime.operations, ["remove:1", "add:1", "add:1"]);
        assert!(!runtime.rules.contains_key(&1));
    }

    #[test]
    fn forwarding_tightening_revokes_even_when_the_outbound_is_unavailable() {
        let dir = TestDir::new();
        let mut previous = rule(1);
        previous.allow_private_targets = Some(true);
        write_state(&dir.0, &desired(vec![previous.clone()])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, previous);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![rule(1)]), false, &mut runtime).unwrap();
        assert!(report.error.is_some());
        assert_eq!(runtime.operations, ["remove:1"]);
        assert!(!runtime.rules.contains_key(&1));
    }

    #[test]
    fn forwarding_old_xray_revokes_domains_but_preserves_existing_public_ips() {
        let dir = TestDir::new();
        let mut public = rule(2);
        public.target_host = "1.1.1.1".to_owned();
        write_state(&dir.0, &desired(vec![rule(1), public.clone()])).unwrap();
        let mut runtime = FakeRuntime {
            unsupported_target_policy: true,
            ..Default::default()
        };
        runtime.rules.extend([(1, rule(1)), (2, public.clone())]);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![rule(1), public]), true, &mut runtime)
                .unwrap();
        assert!(report.error.is_some());
        assert_eq!(runtime.operations, ["remove:1"]);
        assert_eq!(report.applied_rule_ids, [2]);
        assert!(!report.rule_results.unwrap()[0].applied);
    }
    fn desired(rules: Vec<ForwardingRuleDesired>) -> ForwardingDesiredState {
        ForwardingDesiredState {
            generation: 7,
            rules,
        }
    }

    #[test]
    fn forwarding_stats_keep_rule_identity() {
        assert_eq!(
            parse_stat_name("inbound>>>fwd:42>>>traffic>>>downlink"),
            Some((42, "downlink"))
        );
        assert_eq!(forwarding_id("fwd:7"), Some(7));
        assert_eq!(forwarding_id("in:app/ingress"), None);
        assert_eq!(parse_stat_name("user>>>fwd:42>>>traffic>>>uplink"), None);
    }

    #[test]
    fn forwarding_failed_add_cleans_partial_listener_and_retries_independently() {
        let dir = TestDir::new();
        write_state(&dir.0, &desired(vec![rule(1)])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, rule(1));
        runtime.fail_add.insert(2);
        runtime.partial_add.insert(2);
        let wanted = desired(vec![rule(1), rule(2), rule(3)]);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert_eq!(runtime.operations, ["add:2", "remove:2", "add:3"]);
        assert_eq!(report.applied_rule_ids, [1, 3]);
        let results = report.rule_results.unwrap();
        assert!(results[0].applied && results[0].error.is_none());
        assert!(!results[1].applied && results[1].error.is_some());
        assert!(results[2].applied && results[2].error.is_none());
        runtime.fail_add.clear();
        runtime.operations.clear();
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert!(report.error.is_none());
        assert_eq!(runtime.operations, ["add:2"]);
    }

    #[test]
    fn forwarding_failed_cleanup_survives_restart_without_touching_other_rules() {
        let dir = TestDir::new();
        write_state(&dir.0, &desired(vec![rule(1)])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, rule(1));
        runtime.fail_add.insert(2);
        runtime.partial_add.insert(2);
        runtime.fail_remove.insert(2);
        let wanted = desired(vec![rule(1), rule(2), rule(3)]);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert!(report.error.is_some());
        assert_eq!(
            read_checkpoint(&dir.0).unwrap().installing_rule_ids,
            BTreeSet::from([2])
        );
        runtime.fail_add.clear();
        runtime.fail_remove.clear();
        runtime.operations.clear();
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert!(report.error.is_none());
        assert_eq!(runtime.operations, ["remove:2", "add:2"]);
        assert!(read_checkpoint(&dir.0)
            .unwrap()
            .installing_rule_ids
            .is_empty());
    }

    #[test]
    fn forwarding_interrupted_installation_retries_only_the_owned_listener() {
        let dir = TestDir::new();
        write_checkpoint(
            &dir.0,
            7,
            &BTreeMap::from([(1, rule(1))]),
            &BTreeSet::from([2]),
        )
        .unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.extend([(1, rule(1)), (2, rule(2))]);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![rule(1), rule(2)]), true, &mut runtime)
                .unwrap();
        assert!(report.error.is_none());
        assert_eq!(runtime.operations, ["remove:2", "add:2"]);
    }

    #[test]
    fn forwarding_damaged_checkpoint_still_revokes_without_replacing_unknown_listeners() {
        let dir = TestDir::new();
        fs::write(dir.0.join(FORWARDING_STATE_FILE), "{damaged").unwrap();
        let mut runtime = FakeRuntime::default();
        let mut public = rule(1);
        public.target_host = "1.1.1.1".to_owned();
        runtime.rules.extend([(1, public.clone()), (2, rule(2))]);
        let report =
            reconcile_listeners(&dir.0, &desired(vec![public]), true, &mut runtime).unwrap();
        assert!(report
            .error
            .unwrap()
            .contains("decode forwarding checkpoint"));
        assert_eq!(report.applied_rule_ids, [1]);
        assert_eq!(runtime.operations, ["remove:2"]);
        assert!(!report.rule_results.unwrap()[0].applied);
    }

    #[test]
    fn forwarding_requires_a_dedicated_default_outbound_but_not_the_address_guard() {
        let mut config = serde_json::json!({"outbounds":[{
            "tag": FORWARD_OUTBOUND_TAG, "protocol": "freedom"
        }]});
        assert!(forwarding_outbound_is_ready(&config.to_string()));
        config["outbounds"][0]["settings"]["ipsBlocked"] = serde_json::json!([]);
        assert!(forwarding_outbound_is_ready(&config.to_string()));
        assert!(!forwarding_outbound_is_ready(
            r#"{"outbounds":[{"tag":"out:forward"}]}"#
        ));
        assert!(!forwarding_outbound_is_ready("{}"));
    }

    #[test]
    fn forwarding_dns_failure_cannot_block_removal_or_other_additions() {
        let dir = TestDir::new();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, rule(1));
        let wanted = desired(vec![rule(2), rule(3)]);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert_eq!(report.error, None);
        assert_eq!(report.applied_rule_ids, [2, 3]);
        assert_eq!(runtime.operations, ["remove:1", "add:2", "add:3"]);
        assert_eq!(read_state(&dir.0).unwrap(), wanted);
        runtime.operations.clear();
        reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert!(
            runtime.operations.is_empty(),
            "unchanged listeners must not be restarted"
        );
    }

    #[test]
    fn forwarding_partial_failures_checkpoint_success_and_retry_unknown_targets() {
        let dir = TestDir::new();
        let old = rule(1);
        let mut changed = old.clone();
        changed.target_port = 8443;
        write_state(&dir.0, &desired(vec![old.clone()])).unwrap();
        let wanted = desired(vec![changed.clone(), rule(2), rule(3)]);
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, old.clone());
        runtime.rules.insert(4, rule(4));
        runtime.rules.insert(5, rule(5));
        runtime.fail_remove.extend([1, 4]);
        runtime.fail_add.insert(2);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert!(report.error.unwrap().contains("rule 1 remove failed"));
        assert_eq!(report.applied_rule_ids, [1, 3, 4]);
        assert_eq!(
            runtime.operations,
            ["remove:1", "remove:4", "remove:5", "add:2", "add:3"]
        );
        assert_eq!(read_state(&dir.0).unwrap().rules, [old, rule(3)]);
        runtime.operations.clear();
        runtime.fail_remove.clear();
        runtime.fail_add.clear();
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert_eq!(report.error, None);
        assert_eq!(
            runtime.operations,
            ["remove:1", "add:1", "remove:4", "add:2"]
        );
        assert_eq!(runtime.rules.get(&1), Some(&changed));
        assert_eq!(read_state(&dir.0).unwrap(), wanted);
    }

    #[test]
    fn forwarding_interrupted_change_cannot_hide_a_later_rollback() {
        let dir = TestDir::new();
        let old = rule(1);
        let mut changed = old.clone();
        changed.target_port = 8443;
        write_state(&dir.0, &desired(vec![old.clone()])).unwrap();
        let mut runtime = FakeRuntime {
            fail_snapshot: Some(2),
            ..Default::default()
        };
        runtime.rules.insert(1, old.clone());
        assert!(
            reconcile_listeners(&dir.0, &desired(vec![changed.clone()]), true, &mut runtime)
                .is_err()
        );
        assert_eq!(runtime.rules.get(&1), Some(&changed));
        assert_eq!(read_state(&dir.0).unwrap().rules, [changed]);
        runtime.operations.clear();
        let report =
            reconcile_listeners(&dir.0, &desired(vec![old.clone()]), true, &mut runtime).unwrap();
        assert_eq!(report.error, None);
        assert_eq!(runtime.operations, ["remove:1", "add:1"]);
        assert_eq!(runtime.rules.get(&1), Some(&old));
    }

    #[test]
    fn forwarding_without_the_dedicated_outbound_freezes_existing_but_still_revokes() {
        let dir = TestDir::new();
        let mut runtime = FakeRuntime::default();
        let mut first = rule(1);
        first.target_host = "1.1.1.1".to_owned();
        let mut second = rule(2);
        second.target_host = "8.8.8.8".to_owned();
        runtime.rules.insert(1, first.clone());
        runtime.rules.insert(2, second.clone());
        let mut changed = second;
        changed.target_port = 8443;
        let report = reconcile_listeners(
            &dir.0,
            &desired(vec![first.clone(), changed, rule(3)]),
            false,
            &mut runtime,
        )
        .unwrap();
        assert!(report.error.is_some());
        assert_eq!(report.applied_rule_ids, [1, 2]);
        assert!(report
            .rule_results
            .as_ref()
            .unwrap()
            .iter()
            .all(|result| !result.applied && result.error.is_some()));
        assert!(runtime.operations.is_empty());

        runtime.rules.insert(4, rule(4));
        runtime.operations.clear();
        let report =
            reconcile_listeners(&dir.0, &desired(vec![first]), false, &mut runtime).unwrap();
        assert!(report.error.is_some());
        assert_eq!(report.applied_rule_ids, [1]);
        assert_eq!(runtime.operations, ["remove:2", "remove:4"]);

        let report = reconcile_listeners(&dir.0, &desired(vec![]), false, &mut runtime).unwrap();
        assert!(report.error.is_none());
        assert_eq!(runtime.operations, ["remove:2", "remove:4", "remove:1"]);
    }

    #[test]
    fn forwarding_failed_replacement_restores_the_previous_listener_immediately() {
        let dir = TestDir::new();
        let old = rule(1);
        let mut replacement = old.clone();
        replacement.target_port = 8443;
        write_state(&dir.0, &desired(vec![old.clone()])).unwrap();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, old.clone());
        runtime.fail_target_ports.insert(8443);
        runtime.fail_target_attempts.insert(443, 2);

        let report = reconcile_listeners(
            &dir.0,
            &desired(vec![replacement.clone()]),
            true,
            &mut runtime,
        )
        .unwrap();
        assert!(report.error.unwrap().contains("rule 1 add failed"));
        assert_eq!(report.applied_rule_ids, [1]);
        assert_eq!(runtime.rules.get(&1), Some(&old));
        assert_eq!(read_state(&dir.0).unwrap().rules, [old]);
        assert_eq!(
            runtime.operations,
            ["remove:1", "add:1", "add:1", "add:1", "add:1"]
        );

        runtime.fail_target_ports.clear();
        runtime.operations.clear();
        let report = reconcile_listeners(
            &dir.0,
            &desired(vec![replacement.clone()]),
            true,
            &mut runtime,
        )
        .unwrap();
        assert_eq!(report.error, None);
        assert_eq!(runtime.rules.get(&1), Some(&replacement));
        assert_eq!(read_state(&dir.0).unwrap().rules, [replacement]);
        assert_eq!(runtime.operations, ["remove:1", "add:1"]);
    }

    #[test]
    fn forwarding_preserves_a_wanted_listener_when_its_checkpoint_is_missing() {
        let dir = TestDir::new();
        let mut existing = rule(1);
        existing.target_host = "1.1.1.1".to_owned();
        let mut runtime = FakeRuntime::default();
        runtime.rules.insert(1, existing.clone());

        let report =
            reconcile_listeners(&dir.0, &desired(vec![existing]), true, &mut runtime).unwrap();
        assert!(report
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no applied checkpoint")));
        assert_eq!(report.applied_rule_ids, [1]);
        assert!(runtime.operations.is_empty());
        assert!(read_state(&dir.0).unwrap().rules.is_empty());
    }

    /// Exercises the failure path against the pinned Xray instead of the in-memory runtime.
    #[test]
    #[ignore = "requires the repository's pinned Xray binary"]
    fn native_failed_convergence_preserves_the_previous_xray_listener() {
        native_failed_convergence_on(false);
    }

    #[test]
    #[ignore = "requires the repository's pinned Xray binary and IPv6 loopback"]
    fn native_ipv6_api_restores_forwarding_after_restart() {
        native_failed_convergence_on(true);
    }

    fn native_failed_convergence_on(ipv6_api: bool) {
        use std::{
            net::{Ipv4Addr, TcpListener, TcpStream},
            path::PathBuf,
            process::{Command, Stdio},
        };

        let binary = std::env::var_os("BROCADE_XRAY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tools/xray"));
        assert!(binary.exists(), "{} is missing", binary.display());

        // Keep this socket open so the replacement AddInbound fails at the real bind boundary.
        let collision_guard = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .expect("reserve the conflicting listener");
        let collision_port = collision_guard
            .local_addr()
            .expect("collision address")
            .port();
        let api_address = if ipv6_api {
            TcpListener::bind("[::1]:0").unwrap().local_addr().unwrap()
        } else {
            SocketAddr::from(([127, 0, 0, 1], distinct_free_port(&[collision_port])))
        };
        let original_port = distinct_free_port(&[collision_port, api_address.port()]);
        let dir = TestDir::new();
        let config = dir.0.join("xray.json");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "log": { "loglevel": "warning" },
                "api": { "tag": "api", "services": ["HandlerService"] },
                "inbounds": [{
                    "tag": "api",
                    "listen": api_address.ip().to_string(),
                    "port": api_address.port(),
                    "protocol": "dokodemo-door",
                    "settings": { "address": "127.0.0.1" }
                }],
                "outbounds": [{
                    "tag": FORWARD_OUTBOUND_TAG,
                    "protocol": "freedom",
                    "settings": { "ipsBlocked": [] }
                }],
                "routing": { "rules": [{
                    "type": "field",
                    "inboundTag": ["api"],
                    "outboundTag": "api"
                }] }
            }))
            .expect("serialize Xray fixture"),
        )
        .expect("write Xray fixture");

        struct TestXray(std::process::Child);
        impl Drop for TestXray {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = TestXray(
            Command::new(&binary)
                .args([
                    "run",
                    "-config",
                    config.to_str().expect("utf-8 config path"),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start isolated Xray"),
        );
        let mut ready = false;
        for _ in 0..50 {
            if xray_grpc::inbound_tags(api_address).is_ok() {
                ready = true;
                break;
            }
            if let Some(status) = child.0.try_wait().expect("poll isolated Xray") {
                panic!("isolated Xray exited before its API was ready: {status}");
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(ready, "isolated Xray API did not become ready");

        // A host-wide proxy may select a reserved benchmarking address for default routes.
        // Pick a permitted address actually assigned to this host so the fixture stays local.
        let local_addresses = crate::run_command("ip", &["-o", "-4", "addr", "show"])
            .expect("native forwarding fixture needs iproute2");
        let target_host = local_addresses
            .split_whitespace()
            .filter_map(|field| {
                field
                    .split_once('/')
                    .and_then(|(address, _)| address.parse::<std::net::IpAddr>().ok())
            })
            .find(|address| !target_ip_is_blocked_by_policy(*address, true))
            .expect("native forwarding fixture needs a permitted local IPv4 address")
            .to_string();
        let original = ForwardingRuleDesired {
            id: 42,
            protocol: ForwardingProtocol::Tcp,
            listen_port: original_port,
            target_host,
            target_port: 9,
            allow_private_targets: Some(true),
        };
        let wanted = desired(vec![original.clone()]);
        write_desired_state(&dir.0, &wanted).unwrap();
        let mut runtime = XrayForwardingApi(api_address, true);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert_eq!(report.error, None);
        TcpStream::connect((Ipv4Addr::LOCALHOST, original_port))
            .expect("original forwarding listener accepts connections");

        let report = reconcile_listeners(&dir.0, &wanted, false, &mut runtime).unwrap();
        assert!(
            report.error.is_some(),
            "the outbound preflight must still report failure"
        );
        assert!(xray_grpc::inbound_tags(api_address)
            .unwrap()
            .contains(&forwarding_tag(42)));
        TcpStream::connect((Ipv4Addr::LOCALHOST, original_port))
            .expect("preflight failure preserves the original listener");

        let mut replacement = original.clone();
        replacement.listen_port = collision_port;
        let report =
            reconcile_listeners(&dir.0, &desired(vec![replacement]), true, &mut runtime).unwrap();
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|error| error.contains("rule 42 preflight failed")),
            "the real bind conflict must be rejected before replacement: {:?}",
            report.error
        );
        assert!(xray_grpc::inbound_tags(api_address)
            .unwrap()
            .contains(&forwarding_tag(42)));
        assert_eq!(read_state(&dir.0).unwrap().rules, [original]);
        TcpStream::connect((Ipv4Addr::LOCALHOST, original_port))
            .expect("failed replacement restores the original listener immediately");

        child.0.kill().expect("stop the first isolated Xray");
        child.0.wait().expect("reap the first isolated Xray");
        child.0 = Command::new(&binary)
            .args([
                "run",
                "-config",
                config.to_str().expect("utf-8 config path"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("restart isolated Xray");
        let mut ready = false;
        for _ in 0..50 {
            if xray_grpc::inbound_tags(api_address).is_ok() {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(ready, "restarted isolated Xray API did not become ready");
        assert!(!xray_grpc::inbound_tags(api_address)
            .unwrap()
            .contains(&forwarding_tag(42)));
        let content = fs::read_to_string(&config).unwrap();
        let report = restore_after_xray_restart(&dir.0, &content, api_address)
            .unwrap()
            .expect("persisted forwarding desired state");
        assert_eq!(report.error, None);
        assert!(recovery_complete(&dir.0, api_address).unwrap());
        TcpStream::connect((Ipv4Addr::LOCALHOST, original_port))
            .expect("restart recovery restores the forwarding listener immediately");

        let report = reconcile_listeners(&dir.0, &desired(vec![]), true, &mut runtime).unwrap();
        assert_eq!(report.error, None);
        assert!(!xray_grpc::inbound_tags(api_address)
            .unwrap()
            .contains(&forwarding_tag(42)));
        drop(collision_guard);

        fn distinct_free_port(excluded: &[u16]) -> u16 {
            loop {
                let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .expect("reserve local port")
                    .local_addr()
                    .expect("local address")
                    .port();
                if !excluded.contains(&port) {
                    return port;
                }
            }
        }
    }

    #[test]
    fn complete_desired_snapshot_survives_a_partial_runtime_checkpoint() {
        let dir = TestDir::new();
        let wanted = desired(vec![rule(1), rule(2)]);
        write_desired_state(&dir.0, &wanted).unwrap();

        let mut runtime = FakeRuntime::default();
        runtime.fail_add.insert(2);
        let report = reconcile_listeners(&dir.0, &wanted, true, &mut runtime).unwrap();
        assert_eq!(report.applied_rule_ids, [1]);
        assert_eq!(read_state(&dir.0).unwrap().rules, [rule(1)]);
        assert_eq!(desired_state_on_disk(&dir.0).unwrap(), Some(wanted));
    }

    #[test]
    fn legacy_applied_checkpoint_is_migrated_once_as_restart_desired_state() {
        let dir = TestDir::new();
        let legacy = desired(vec![rule(3)]);
        write_state(&dir.0, &legacy).unwrap();

        assert_eq!(desired_state_on_disk(&dir.0).unwrap(), Some(legacy.clone()));
        assert!(dir.0.join(FORWARDING_DESIRED_FILE).exists());

        write_state(&dir.0, &desired(vec![])).unwrap();
        assert_eq!(desired_state_on_disk(&dir.0).unwrap(), Some(legacy));
    }

    #[test]
    fn forwarding_errors_fit_the_report_limit() {
        let mut errors = ReconcileErrors::default();
        for _ in 0..100 {
            errors.push("错误".repeat(500));
        }
        let message = errors.finish().unwrap();
        assert!(message.len() <= 4096);
        assert!(message.contains("additional errors omitted"));
    }
}
