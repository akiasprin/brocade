//! Persistent, optional machine maintenance. Policy refresh and receipts never wait for a round.
mod behavior;
mod session;
#[cfg(test)]
mod tests;
mod worker;

use crate::{
    fsutil::{atomic_write_private, read_bounded},
    http::HttpClient,
    options::Options,
};
use brocade_deployment::ip_maintenance::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

fn now() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock_before_epoch")?
            .as_secs(),
    )
    .map_err(|_| "clock_range".into())
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RoundState {
    manual_sequence: u64,
    last_started_at: i64,
    cooldown_until: i64,
    round_bytes: u64,
    current: Option<IpMaintenanceReport>,
    #[serde(default)]
    last_evidence: Option<IpMaintenanceReport>,
    #[serde(default)]
    scheduled: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Checkpoint {
    sequence: u64,
    acknowledged_sequence: u64,
    desired: Option<IpMaintenanceDesired>,
    day: i64,
    daily_requests: u32,
    daily_bytes: u64,
    next_scheduled_at: i64,
    terminal: VecDeque<IpMaintenanceReport>,
    // The flattened round preserves the existing checkpoint format on upgrade.
    #[serde(flatten)]
    round: RoundState,
    #[serde(default)]
    families: std::collections::BTreeMap<MaintenanceFamily, RoundState>,
}
impl std::ops::Deref for Checkpoint {
    type Target = RoundState;
    fn deref(&self) -> &RoundState {
        &self.round
    }
}
impl std::ops::DerefMut for Checkpoint {
    fn deref_mut(&mut self) -> &mut RoundState {
        &mut self.round
    }
}
struct Context {
    directory: PathBuf,
    checkpoint: Arc<Mutex<Checkpoint>>,
    last_sync: Arc<Mutex<Option<tokio::time::Instant>>>,
    assets: Arc<Mutex<Option<Arc<data::MaintenanceData>>>>,
    stop: Arc<AtomicBool>,
    instance: String,
    family: Option<MaintenanceFamily>,
}
impl Context {
    fn synced(&self) -> Result<(), String> {
        *self.last_sync.lock().map_err(|_| "maintenance_sync_lock")? =
            Some(tokio::time::Instant::now());
        Ok(())
    }
    fn lease_expired(&self) -> bool {
        self.last_sync.lock().map_or(true, |sync| {
            sync.is_none_or(|stamp| stamp.elapsed() >= Duration::from_secs(120))
        })
    }
    fn locked(&self) -> Result<Checkpoint, String> {
        let mut state = self
            .checkpoint
            .lock()
            .map_err(|_| "maintenance_state_lock")?
            .clone();
        if let Some(family) = self.family {
            state.round = state.families.get(&family).cloned().unwrap_or_default();
        }
        Ok(state)
    }
    fn persist(&self, state: &Checkpoint) -> Result<(), String> {
        let bytes = serde_json::to_vec(state).map_err(|_| "maintenance_checkpoint_encode")?;
        if bytes.len() > 1024 * 1024 {
            return Err("maintenance_checkpoint_limit".into());
        }
        atomic_write_private(&self.directory.join("checkpoint.json"), &bytes)
    }
    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Checkpoint) -> Result<T, String>,
    ) -> Result<T, String> {
        self.change(true, change)
    }
    fn change<T>(
        &self,
        project: bool,
        change: impl FnOnce(&mut Checkpoint) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self
            .checkpoint
            .lock()
            .map_err(|_| "maintenance_state_lock")?;
        let mut next = state.clone();
        if project {
            if let Some(family) = self.family {
                next.round = next.families.get(&family).cloned().unwrap_or_default();
            }
        }
        let value = change(&mut next)?;
        if project {
            if let Some(family) = self.family {
                next.families.insert(family, next.round.clone());
            }
        }
        // Round state is isolated, while sequences and budgets commit under the same durable lock.
        self.persist(&next)?;
        *state = next;
        Ok(value)
    }
    fn for_family(self: &Arc<Self>, family: MaintenanceFamily) -> Arc<Self> {
        Arc::new(Self {
            directory: self.directory.clone(),
            checkpoint: Arc::clone(&self.checkpoint),
            last_sync: Arc::clone(&self.last_sync),
            assets: Arc::clone(&self.assets),
            stop: Arc::clone(&self.stop),
            instance: self.instance.clone(),
            family: Some(family),
        })
    }
    fn apply(&self, desired: &IpMaintenanceDesired) -> Result<(), String> {
        if self
            .checkpoint
            .lock()
            .map_err(|_| "maintenance_state_lock")?
            .desired
            .as_ref()
            == Some(desired)
        {
            return Ok(());
        }
        self.change(false, |state| {
            if state.desired.as_ref() == Some(desired) {
                return Ok(());
            }
            if state.desired.as_ref().is_some_and(|old| {
                old.generation > desired.generation
                    || old.session_generation > desired.session_generation
                    || old.control_sequence > desired.control_sequence
            }) {
                return Ok(());
            }
            let reset = state
                .desired
                .as_ref()
                .is_some_and(|old| old.session_generation < desired.session_generation);
            if reset
                && state
                    .families
                    .values()
                    .any(|round| round.current.as_ref().is_some_and(|r| r.phase.active()))
            {
                return Ok(());
            }
            if reset {
                session::clear(&self.directory)?;
                state.round = RoundState {
                    manual_sequence: desired.manual_sequence,
                    ..Default::default()
                };
                for round in state.families.values_mut() {
                    *round = RoundState {
                        manual_sequence: desired.manual_sequence,
                        ..Default::default()
                    };
                }
                state.terminal.clear();
                state.day = now()?.div_euclid(86400);
                state.daily_requests = 0;
                state.daily_bytes = 0;
                state.next_scheduled_at = 0;
            }
            if !reset
                && state.desired.as_ref().is_none_or(|old| {
                    old.generation != desired.generation || old.policy != desired.policy
                })
            {
                let last = state
                    .families
                    .values()
                    .map(|r| r.last_started_at)
                    .chain([state.last_started_at])
                    .max()
                    .unwrap_or(0);
                state.next_scheduled_at =
                    worker::schedule(now()?, &desired.policy, last, &mut behavior::Random)?;
                state.scheduled = false;
                for round in state.families.values_mut() {
                    round.scheduled = false;
                }
            }
            state.sequence = state.sequence.max(desired.report_sequence_floor);
            state.desired = Some(desired.clone());
            Ok(())
        })
    }
    fn schedule_families(&self, desired: &IpMaintenanceDesired, stamp: i64) -> Result<(), String> {
        if self.family.is_none()
            || !desired.policy.enabled
            || self.locked()?.next_scheduled_at > stamp
        {
            return Ok(());
        }
        self.change(false, |state| {
            if state.next_scheduled_at > stamp || state.desired.as_ref() != Some(desired) {
                return Ok(());
            }
            for family in [MaintenanceFamily::V4, MaintenanceFamily::V6] {
                if desired.policy.family.includes(family) {
                    state.families.entry(family).or_default().scheduled = true;
                }
            }
            let last = state
                .families
                .values()
                .map(|r| r.last_started_at)
                .max()
                .unwrap_or(0);
            state.next_scheduled_at =
                worker::schedule(stamp, &desired.policy, last, &mut behavior::Random)?;
            Ok(())
        })
    }
    fn emit(
        &self,
        phase: MaintenancePhase,
        reason: Option<&str>,
        deadline: Option<i64>,
    ) -> Result<(), String> {
        self.mutate(|state| {
            let generation = state.desired.as_ref().map_or(0, |d| d.generation);
            let day = now()?.div_euclid(86400);
            if day > state.day {
                state.day = day;
                state.daily_requests = 0;
                state.daily_bytes = 0;
            }
            let prior = state
                .last_evidence
                .as_ref()
                .filter(|r| r.generation == generation);
            let sequence = state
                .sequence
                .checked_add(1)
                .ok_or("maintenance_sequence_limit")?;
            let mut report = state.current.clone().unwrap_or(IpMaintenanceReport {
                family: self.family,
                generation,
                run_generation: generation,
                agent_instance_id: self.instance.clone(),
                applied_control_sequence: 0,
                applied_stop_sequence: 0,
                applied_session_generation: 0,
                worker_observed_at_unix_secs: now()?,
                last_started_at_unix_secs: None,
                cooldown_until_unix_secs: None,
                sequence,
                phase,
                round_id: None,
                module: None,
                actual_exit_ip: prior.and_then(|r| r.actual_exit_ip),
                data_digest: None,
                actions_total: 0,
                action_index: 0,
                phase_deadline_unix_secs: None,
                next_run_unix_secs: None,
                daily_requests: 0,
                daily_response_bytes: 0,
                actions: Vec::new(),
                evidence: prior.map(|r| r.evidence.clone()).unwrap_or_default(),
                evidence_observed_at_unix_secs: prior
                    .and_then(|r| r.evidence_observed_at_unix_secs),
                reason: None,
                observed_at_unix_secs: now()?,
            });
            let terminal_before = state.current.as_ref().is_some_and(|r| r.phase.terminal());
            report.family = self.family.or(report.family);
            report.generation = generation;
            report.agent_instance_id = self.instance.clone();
            let manual_pending = state.desired.as_ref().is_some_and(|d| {
                d.manual_sequence > state.manual_sequence
                    && d.manual_expires_at
                        .is_some_and(|until| until >= report.worker_observed_at_unix_secs)
            });
            if !manual_pending {
                report.applied_control_sequence =
                    state.desired.as_ref().map_or(0, |d| d.control_sequence);
            }
            report.applied_stop_sequence = state.desired.as_ref().map_or(0, |d| d.stop_sequence);
            report.applied_session_generation =
                state.desired.as_ref().map_or(0, |d| d.session_generation);
            report.worker_observed_at_unix_secs = now()?;
            report.last_started_at_unix_secs =
                (state.last_started_at > 0).then_some(state.last_started_at);
            report.cooldown_until_unix_secs =
                (state.cooldown_until > 0).then_some(state.cooldown_until);
            report.sequence = sequence;
            report.phase = phase;
            report.reason = reason.map(str::to_owned);
            report.phase_deadline_unix_secs = deadline;
            report.next_run_unix_secs =
                (state.next_scheduled_at > 0).then_some(state.next_scheduled_at);
            report.observed_at_unix_secs = now()?;
            report.daily_requests = state.daily_requests;
            report.daily_response_bytes = state.daily_bytes;
            if matches!(
                phase,
                MaintenancePhase::Complete
                    | MaintenancePhase::Interrupted
                    | MaintenancePhase::Failed
                    | MaintenancePhase::Cooldown
                    | MaintenancePhase::BlockedEgress
                    | MaintenancePhase::BudgetWait
            ) && report.round_id.is_some()
                && !terminal_before
            {
                if state.terminal.len() >= 64 {
                    return Err("maintenance_receipt_buffer_full".into());
                }
                state.terminal.push_back(report.clone());
            }
            state.sequence = sequence;
            state.current = Some(report);
            Ok(())
        })
    }
    fn reserve_request(&self, policy: &IpMaintenancePolicy) -> Result<(), String> {
        self.mutate(|state| {
            let day = now()?.div_euclid(86400);
            // A backward clock adjustment must not replenish an already-used budget.
            if day > state.day {
                state.day = day;
                state.daily_requests = 0;
                state.daily_bytes = 0;
            }
            if state.daily_requests >= policy.daily_requests
                || state.daily_bytes >= u64::from(policy.daily_download_mib) * 1024 * 1024
            {
                return Err("daily_budget".into());
            }
            state.daily_requests += 1;
            Ok(())
        })
    }
    fn received(&self, bytes: usize, policy: &IpMaintenancePolicy) -> Result<(), String> {
        let reason = self.mutate(|state| {
            let day = now()?.div_euclid(86400);
            if day > state.day {
                state.day = day;
                state.daily_requests = 0;
                state.daily_bytes = 0;
            }
            state.round_bytes = state.round_bytes.saturating_add(bytes as u64);
            state.daily_bytes = state
                .daily_bytes
                .checked_add(bytes as u64)
                .ok_or("daily_byte_overflow")?;
            Ok(
                if state.daily_bytes > u64::from(policy.daily_download_mib) * 1024 * 1024 {
                    Some("daily_budget")
                } else if state.round_bytes > 32 * 1024 * 1024 {
                    Some("round_download_limit")
                } else {
                    None
                },
            )
        })?;
        reason.map_or(Ok(()), |reason| Err(reason.into()))
    }
    fn begin_round(
        &self,
        desired: &IpMaintenanceDesired,
        module: MaintenanceModule,
        total: u32,
    ) -> Result<(), String> {
        self.mutate(|state| {
            if state.desired.as_ref() != Some(desired) {
                return Err("policy_changed".into());
            }
            if state.terminal.len() >= 64 {
                return Err("maintenance_receipt_buffer_full".into());
            }
            let stamp = now()?;
            state.manual_sequence = desired.manual_sequence;
            state.scheduled = false;
            state.last_started_at = stamp;
            state.round_bytes = 0;
            let id = format!("{stamp}-{:?}-{}", self.family, state.sequence + 1);
            state.current = Some(IpMaintenanceReport {
                family: self.family,
                generation: desired.generation,
                run_generation: desired.generation,
                agent_instance_id: self.instance.clone(),
                applied_control_sequence: desired.control_sequence,
                applied_stop_sequence: desired.stop_sequence,
                applied_session_generation: desired.session_generation,
                worker_observed_at_unix_secs: stamp,
                last_started_at_unix_secs: Some(stamp),
                cooldown_until_unix_secs: None,
                sequence: state.sequence + 1,
                phase: MaintenancePhase::Preflight,
                round_id: Some(id),
                module: Some(module),
                actual_exit_ip: None,
                data_digest: None,
                actions_total: total,
                action_index: 0,
                phase_deadline_unix_secs: None,
                next_run_unix_secs: None,
                daily_requests: state.daily_requests,
                daily_response_bytes: state.daily_bytes,
                actions: Vec::new(),
                evidence: Vec::new(),
                evidence_observed_at_unix_secs: None,
                reason: None,
                observed_at_unix_secs: stamp,
            });
            Ok(())
        })?;
        self.emit(MaintenancePhase::Preflight, None, None)
    }
    fn clear_round(&self) -> Result<(), String> {
        self.mutate(|state| {
            state.current = None;
            Ok(())
        })
    }
}

pub(crate) struct WorkerHandle {
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<()>,
}
impl WorkerHandle {
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn finish(self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .join()
            .map_err(|_| "maintenance_shutdown_panic".into())
    }
}
fn terminating(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Acquire) || crate::TERMINATION_REQUESTED.load(Ordering::Acquire)
}
fn wait(stop: &AtomicBool, duration: Duration) {
    let until = std::time::Instant::now() + duration;
    while !terminating(stop) && std::time::Instant::now() < until {
        thread::sleep(Duration::from_millis(100));
    }
}
pub(crate) fn spawn(options: Options) -> Result<WorkerHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("ip-maintenance".into())
        .spawn(move || {
            let mut backoff = 1;
            while !terminating(&worker_stop) {
                match start(options.clone(), Arc::clone(&worker_stop)) {
                    Ok(()) => break,
                    Err(error) => eprintln!("ip-maintenance: {error}; retry in {backoff}s"),
                }
                wait(&worker_stop, Duration::from_secs(backoff));
                backoff = (backoff * 2).min(30);
            }
        })
        .map_err(|_| "maintenance_thread")?;
    Ok(WorkerHandle { stop, thread })
}
fn start(options: Options, stop: Arc<AtomicBool>) -> Result<(), String> {
    let directory = options.state_dir.join("ip-maintenance-v2");
    if fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("maintenance_directory_symlink".into());
    }
    fs::create_dir_all(&directory).map_err(|_| "maintenance_directory")?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| "maintenance_directory_permissions")?;
    let _ownership = crate::lock_convergence(&directory)?;
    // A malformed budget checkpoint is a blocking fault, never a fresh budget.
    let mut state = read_bounded(&directory.join("checkpoint.json"), 1024 * 1024)?
        .map(|bytes| {
            serde_json::from_slice::<Checkpoint>(&bytes)
                .map_err(|_| "maintenance_checkpoint_invalid".to_owned())
        })
        .transpose()?
        .unwrap_or_default();
    if state.terminal.len() > 64
        || state.families.len() > 2
        || state.families.contains_key(&MaintenanceFamily::Dual)
    {
        return Err("maintenance_checkpoint_limit".into());
    }
    if let Some(desired) = &state.desired {
        desired.policy.validate()?;
    }
    for report in state
        .current
        .iter()
        .chain(state.terminal.iter())
        .chain(state.last_evidence.iter())
    {
        report.validate()?;
    }
    for (family, round) in &state.families {
        for report in round.current.iter().chain(round.last_evidence.iter()) {
            report.validate()?;
            if report.family.is_some_and(|reported| reported != *family) {
                return Err("maintenance_checkpoint_family".into());
            }
        }
    }
    let assets = read_bounded(&directory.join("data.json"), 4 * 1024 * 1024)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<data::MaintenanceData>(&bytes).ok())
        .filter(|data| data.validate().is_ok());
    if state.families.is_empty() {
        let family = state
            .current
            .as_ref()
            .and_then(|r| {
                r.family.or_else(|| {
                    r.actual_exit_ip.map(|ip| {
                        if ip.is_ipv4() {
                            MaintenanceFamily::V4
                        } else {
                            MaintenanceFamily::V6
                        }
                    })
                })
            })
            .unwrap_or_else(|| {
                state
                    .desired
                    .as_ref()
                    .map_or(MaintenanceFamily::V4, |d| d.policy.family)
            });
        if family != MaintenanceFamily::Dual {
            state.families.insert(family, state.round.clone());
        }
    }
    for family in [MaintenanceFamily::V4, MaintenanceFamily::V6] {
        state.families.entry(family).or_default().scheduled = false;
    }
    state.next_scheduled_at = state.next_scheduled_at.max(now()? + 30);
    let initial = state.desired.clone();
    let instance = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock")?
            .as_nanos()
    );
    let context = Arc::new(Context {
        directory,
        checkpoint: Arc::new(Mutex::new(state)),
        last_sync: Arc::new(Mutex::new(None)),
        assets: Arc::new(Mutex::new(assets.map(Arc::new))),
        stop: Arc::clone(&stop),
        instance,
        family: None,
    });
    let workers = [
        context.for_family(MaintenanceFamily::V4),
        context.for_family(MaintenanceFamily::V6),
    ];
    for worker in &workers {
        if worker
            .locked()?
            .current
            .as_ref()
            .is_some_and(|r| r.phase.active())
        {
            worker.emit(MaintenancePhase::Interrupted, Some("agent_restarted"), None)?;
        }
    }
    let (send, receive) = watch::channel(initial);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "maintenance_runtime")?;
    thread::scope(|scope| {
        let data_context = Arc::clone(&context);
        let data_options = options.clone();
        let data_receive = receive.clone();
        scope.spawn(move || data_loop(data_options, data_context, data_receive));
        let policy_context = Arc::clone(&context);
        let policy_options = options.clone();
        scope.spawn(move || policy_loop(policy_options, policy_context, send));
        let report_context = Arc::clone(&context);
        scope.spawn(move || report_loop(options, report_context));
        runtime.block_on(async {
            tokio::join!(
                supervise(Arc::clone(&workers[0]), receive.clone()),
                supervise(Arc::clone(&workers[1]), receive.clone()),
            );
        });
        stop.store(true, Ordering::Release);
    });
    Ok(())
}
async fn supervise(context: Arc<Context>, receiver: watch::Receiver<Option<IpMaintenanceDesired>>) {
    let mut backoff = 1;
    while !terminating(&context.stop) {
        let task = tokio::spawn(worker::run(Arc::clone(&context), receiver.clone()));
        let outcome = task.await;
        // The policy sender also exits on shutdown. Its closure is not a worker fault.
        if terminating(&context.stop) {
            break;
        }
        match outcome {
            Ok(Ok(())) => break,
            Ok(Err(error)) => eprintln!("maintenance-worker: {error}"),
            Err(_) => eprintln!("maintenance-worker: panic"),
        }
        let _ = context.emit(MaintenancePhase::Failed, Some("worker_recovering"), None);
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}
fn data_loop(
    options: Options,
    context: Arc<Context>,
    receive: watch::Receiver<Option<IpMaintenanceDesired>>,
) {
    while !terminating(&context.stop) {
        let desired = receive.borrow().clone();
        let update = || -> Result<(), String> {
            let Some(desired) = desired else {
                return Ok(());
            };
            if desired.generation == 0 {
                return Ok(());
            }
            let current = context
                .assets
                .lock()
                .map_err(|_| "maintenance_data_lock")?
                .clone();
            if current.as_ref().is_some_and(|data| {
                data.region.id == desired.policy.region
                    && Some(&data.digest) == desired.data_digest.as_ref()
            }) {
                return Ok(());
            }
            let client = HttpClient::new(&options.server)?;
            // Data packages exceed the channel's 128 KiB RPC limit; use authenticated HTTP.
            let response =
                client.request("GET", "/agent/v1/ip-maintenance/data", &options.token, None)?;
            if response.status != 200 {
                return Err(format!("maintenance_data_http_{}", response.status));
            }
            if response.body.len() > 4 * 1024 * 1024 {
                return Err("maintenance_data_size".into());
            }
            let data: data::MaintenanceData =
                serde_json::from_str(&response.body).map_err(|_| "maintenance_data_json")?;
            data.validate()?;
            if data.region.id != desired.policy.region
                || Some(&data.digest) != desired.data_digest.as_ref()
            {
                return Err("maintenance_data_changed_during_fetch".into());
            }
            let bytes = serde_json::to_vec(&data).map_err(|_| "maintenance_data_encoding")?;
            atomic_write_private(&context.directory.join("data.json"), &bytes)?;
            *context.assets.lock().map_err(|_| "maintenance_data_lock")? = Some(Arc::new(data));
            Ok(())
        }();
        if let Err(error) = update {
            eprintln!("ip-maintenance-data: {error}");
        }
        wait(&context.stop, Duration::from_secs(15));
    }
}
fn policy_loop(
    options: Options,
    context: Arc<Context>,
    send: watch::Sender<Option<IpMaintenanceDesired>>,
) {
    let mut wake = crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::Desired);
    while !terminating(&context.stop) {
        let fetch = || -> Result<IpMaintenanceDesired, String> {
            let client = HttpClient::new(&options.server)?;
            let response = client.request(
                "GET",
                "/agent/v1/ip-maintenance/desired",
                &options.token,
                None,
            )?;
            if response.status != 200 {
                return Err(format!("policy_http_{}", response.status));
            }
            let desired: IpMaintenanceDesired =
                serde_json::from_str(&response.body).map_err(|_| "policy_response")?;
            desired.policy.validate()?;
            if data::region(&desired.policy.region).is_none() {
                return Err("unknown_maintenance_region".into());
            }
            Ok(desired)
        };
        match fetch() {
            Ok(desired) => {
                if let Err(error) = context.synced() {
                    eprintln!("ip-maintenance-policy: {error}");
                    continue;
                }
                if send.borrow().as_ref() != Some(&desired) {
                    send.send_replace(Some(desired));
                }
            }
            Err(error) => {
                eprintln!("ip-maintenance-policy: {error}");
                // Revocation, retirement or an older Console must stop cached activity too.
                if matches!(
                    error.as_str(),
                    "policy_http_400"
                        | "policy_http_401"
                        | "policy_http_403"
                        | "policy_http_404"
                        | "policy_response"
                ) {
                    let previous = send.borrow().clone();
                    if let Some(mut disabled) = previous {
                        disabled.policy.enabled = false;
                        disabled.manual_expires_at = None;
                        if let Err(error) = context.apply(&disabled) {
                            eprintln!("ip-maintenance-stop: {error}");
                        }
                        send.send_replace(Some(disabled));
                    }
                }
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !terminating(&context.stop) && std::time::Instant::now() < deadline {
            if wake.wait_connected(Duration::from_secs(1), Duration::from_secs(1)) {
                break;
            }
        }
    }
}
fn report_loop(options: Options, context: Arc<Context>) {
    while !terminating(&context.stop) {
        let next = context.locked().map(|state| {
            state
                .terminal
                .iter()
                .chain(
                    state
                        .families
                        .values()
                        .filter_map(|round| round.current.as_ref()),
                )
                .chain(state.current.iter().filter(|_| state.families.is_empty()))
                .filter(|r| r.sequence > state.acknowledged_sequence)
                .min_by_key(|r| r.sequence)
                .cloned()
        });
        let result = || -> Result<(), String> {
            let Some(report) = next.map_err(|_| "maintenance_report_lock")? else {
                return Ok(());
            };
            let client = HttpClient::new(&options.server)?;
            let encoded =
                serde_json::to_string(&report).map_err(|_| "maintenance_report_encode")?;
            let response = client.request(
                "POST",
                "/agent/v1/ip-maintenance/report",
                &options.token,
                Some(&encoded),
            )?;
            if !(200..300).contains(&response.status) {
                return Err(format!("report_http_{}", response.status));
            }
            context.mutate(|state| {
                if state
                    .terminal
                    .front()
                    .is_some_and(|r| r.sequence == report.sequence)
                {
                    state.terminal.pop_front();
                }
                state.acknowledged_sequence = state.acknowledged_sequence.max(report.sequence);
                Ok(())
            })
        }();
        if let Err(error) = result {
            eprintln!("ip-maintenance-report: {error}");
        }
        wait(&context.stop, Duration::from_secs(1));
    }
}
