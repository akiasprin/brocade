//! Process-local live NIC telemetry.
//!
//! Agents keep one outbound WebSocket open, but produce samples only while at least one browser
//! is watching the node. The console owns the demand lease, fans samples out, and retains a short
//! in-memory ring. Nothing in this module writes a sample to PostgreSQL or changes the existing
//! diagnostic and accounting paths.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use brocade_core::model::VPNGATE_MAX_CANDIDATES;
use brocade_deployment::protocol::{
    AgentRealtimeCommand, AgentRealtimeSample, RealtimeForwardingTarget, RealtimeNodeSnapshot,
    RealtimeSampleEvent, RealtimeTelemetryPolicy, RealtimeUserTrafficSelector,
    RealtimeUserTrafficState, VpngateBackendRole, MAX_REALTIME_FORWARDING_TARGETS,
    MAX_REALTIME_USER_TRAFFIC_ROWS,
};
use serde::Serialize;
use tokio::sync::{broadcast, watch, Mutex};

const RING_RETENTION: Duration = Duration::from_secs(120);
const WAKE_DOMAIN_COUNT: usize = brocade_deployment::agent_channel::WakeDomain::ALL.len();
const RING_MAX_SAMPLES: usize = 600;
const EVENT_CHANNEL_CAPACITY: usize = 2048;
const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(15);
const MAX_INTERFACE_CHARS: usize = 32;
const LIVE_GAP_ELAPSED_MILLIS: u32 = 60_000;
const MAX_FUTURE_CLOCK_SKEW_MILLIS: i64 = 10 * 60 * 1000;
const MAX_VPNGATE_POOLS: usize = 16;
const MAX_VPNGATE_BACKENDS: usize = MAX_VPNGATE_POOLS * 2;
const MAX_VPNGATE_EVENTS: usize = 32;
const MAX_VPNGATE_REPORT_BYTES: usize = 128 * 1024;
const VPNGATE_LIVE_MAX_AGE_MILLIS: i64 = 15_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeBroadcast {
    Sample(Box<RealtimeSampleEvent>),
    Status(RealtimeNodeStatus),
}

impl RealtimeBroadcast {
    pub fn node_id(&self) -> &str {
        match self {
            Self::Sample(event) => &event.node_id,
            Self::Status(status) => &status.node_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RealtimeNodeStatus {
    pub node_id: String,
    pub connected: bool,
    pub active: bool,
    pub interval_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordSampleError {
    Invalid(&'static str),
    SessionGone,
    Superseded,
    Inactive,
    Sequence,
}

#[derive(Debug)]
struct AgentConnection {
    session: u64,
    last_sequence: Option<u64>,
    active: bool,
    // `watch` carries desired state rather than a queue of transitions. Rapid setting edits or
    // tab churn coalesce to the newest command instead of growing an unbounded per-node queue.
    commands: watch::Sender<AgentRealtimeCommand>,
    changes: watch::Sender<[u64; WAKE_DOMAIN_COUNT]>,
}

#[derive(Debug, Clone, Default)]
struct WatchDemand {
    total: usize,
    diagnostics: usize,
    user_all: usize,
    user_labels: HashMap<String, usize>,
    forwarding: HashMap<RealtimeForwardingTarget, usize>,
}

impl WatchDemand {
    fn is_watched(&self) -> bool {
        self.total > 0
    }

    fn wants_diagnostics(&self) -> bool {
        self.diagnostics > 0
    }

    fn user_traffic(&self) -> Option<RealtimeUserTrafficSelector> {
        let mut forwarding = self
            .forwarding
            .iter()
            .filter_map(|(target, count)| (*count > 0).then_some(target.clone()))
            .collect::<Vec<_>>();
        forwarding.sort();
        forwarding.truncate(MAX_REALTIME_FORWARDING_TARGETS);
        if self.user_all > 0 {
            return Some(RealtimeUserTrafficSelector::All { forwarding });
        }
        let mut labels = self
            .user_labels
            .iter()
            .filter_map(|(label, count)| (*count > 0).then_some(label.clone()))
            .collect::<Vec<_>>();
        if labels.is_empty() && forwarding.is_empty() {
            return None;
        }
        labels.sort();
        if labels.len() > MAX_REALTIME_USER_TRAFFIC_ROWS
            || labels.iter().map(|label| label.len() + 4).sum::<usize>()
                + forwarding
                    .iter()
                    .map(|target| target.tenant_id.len() + target.user_id.len() + 80)
                    .sum::<usize>()
                > 256 * 1024
        {
            // This can require many simultaneous user-detail tabs. Sampling all active users is
            // bounded on the Agent and preserves useful live data without growing a command past
            // the realtime frame limit.
            Some(RealtimeUserTrafficSelector::All { forwarding })
        } else {
            Some(RealtimeUserTrafficSelector::Labels { labels, forwarding })
        }
    }
}

fn start_command(policy: RealtimeTelemetryPolicy, demand: &WatchDemand) -> AgentRealtimeCommand {
    AgentRealtimeCommand::Start {
        interval_millis: policy.interval_secs * 1000,
        diagnostics: demand.wants_diagnostics(),
        user_traffic: demand.user_traffic(),
    }
}

#[derive(Debug)]
struct Inner {
    policy: RealtimeTelemetryPolicy,
    next_session: u64,
    agents: HashMap<String, AgentConnection>,
    watchers: HashMap<String, WatchDemand>,
    /// Incremented whenever the watch state changes. A grace-period task may stop a node only if
    /// its captured generation is still current.
    stop_generation: HashMap<String, u64>,
    rings: HashMap<String, VecDeque<RealtimeSampleEvent>>,
}

#[derive(Clone, Debug)]
pub struct RealtimeService {
    inner: Arc<Mutex<Inner>>,
    events: broadcast::Sender<RealtimeBroadcast>,
    stop_grace: Duration,
    listener_started: Arc<std::sync::atomic::AtomicBool>,
}

pub struct AgentRealtimeSession {
    pub session: u64,
    pub commands: watch::Receiver<AgentRealtimeCommand>,
    pub changes: watch::Receiver<[u64; WAKE_DOMAIN_COUNT]>,
}

pub struct RealtimeSubscription {
    pub policy: RealtimeTelemetryPolicy,
    pub snapshots: Vec<RealtimeNodeSnapshot>,
    pub events: broadcast::Receiver<RealtimeBroadcast>,
    pub visible_nodes: HashSet<String>,
    _lease: WatchLease,
}

struct WatchLease {
    service: RealtimeService,
    nodes: Vec<String>,
    diagnostics: bool,
    user_all: bool,
    user_labels: HashMap<String, Vec<String>>,
    forwarding: HashMap<String, Vec<RealtimeForwardingTarget>>,
}

impl Drop for WatchLease {
    fn drop(&mut self) {
        let service = self.service.clone();
        let nodes = std::mem::take(&mut self.nodes);
        // HTTP streams always live on a Tokio runtime. If shutdown has already torn it down there
        // is no Agent connection left to stop, so failing to obtain a handle is harmless.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let diagnostics = self.diagnostics;
            let user_all = self.user_all;
            let user_labels = std::mem::take(&mut self.user_labels);
            let forwarding = std::mem::take(&mut self.forwarding);
            runtime.spawn(async move {
                service
                    .release(nodes, diagnostics, user_all, user_labels, forwarding)
                    .await
            });
        }
    }
}

impl RealtimeService {
    pub(crate) async fn wake_agents(
        &self,
        domains: &[brocade_deployment::agent_channel::WakeDomain],
    ) {
        self.wake_matching(domains, None).await;
    }
    async fn wake_matching(
        &self,
        domains: &[brocade_deployment::agent_channel::WakeDomain],
        node: Option<&str>,
    ) {
        let inner = self.inner.lock().await;
        for (node_id, agent) in &inner.agents {
            if node.is_some_and(|node| node != node_id) {
                continue;
            }
            agent.changes.send_modify(|versions| {
                for domain in domains {
                    versions[*domain as usize] = versions[*domain as usize].wrapping_add(1);
                }
            });
        }
    }
    pub(crate) fn ensure_change_listener(
        &self,
        store: brocade_store::PgStore,
        shutdown: crate::lifecycle::ShutdownSignal,
    ) {
        use std::sync::atomic::Ordering;
        if self.listener_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let listening = async {
                    let mut listener =
                        sqlx::postgres::PgListener::connect_with(store.pool()).await?;
                    listener.listen("brocade_agent_change").await?;
                    service
                        .wake_agents(&brocade_deployment::agent_channel::WakeDomain::ALL)
                        .await;
                    backoff = Duration::from_secs(1);
                    loop {
                        let Some(notification) = listener.try_recv().await? else {
                            // Reconnect can miss notifications. The next connection re-LISTENs
                            // before asking all workers to re-read their durable desired state.
                            break;
                        };
                        #[derive(serde::Deserialize)]
                        struct Change {
                            domain: String,
                            node: Option<String>,
                        }
                        let Ok(change) = serde_json::from_str::<Change>(notification.payload())
                        else {
                            continue;
                        };
                        use brocade_deployment::agent_channel::WakeDomain;
                        let domains = match change.domain.as_str() {
                            "release" => vec![WakeDomain::AgentRelease, WakeDomain::XrayRelease],
                            "desired" => vec![
                                WakeDomain::Desired,
                                WakeDomain::UserTraffic,
                                WakeDomain::Probe,
                            ],
                            payload => match serde_json::from_value(serde_json::Value::String(
                                payload.into(),
                            )) {
                                Ok(domain) => vec![domain],
                                Err(_) => continue,
                            },
                        };
                        service
                            .wake_matching(&domains, change.node.as_deref())
                            .await;
                    }
                    Ok::<(), sqlx::Error>(())
                };
                tokio::select! {
                    _ = shutdown.requested() => return,
                    result = listening => if let Err(error) = result { eprintln!("Agent change listener: {error}"); },
                }
                tokio::select! { _ = shutdown.requested() => return, _ = tokio::time::sleep(backoff) => {} }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        });
    }
    pub fn new(policy: RealtimeTelemetryPolicy) -> Self {
        Self::with_stop_grace(policy, DEFAULT_STOP_GRACE)
    }

    fn with_stop_grace(policy: RealtimeTelemetryPolicy, stop_grace: Duration) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                policy,
                next_session: 0,
                agents: HashMap::new(),
                watchers: HashMap::new(),
                stop_generation: HashMap::new(),
                rings: HashMap::new(),
            })),
            events,
            stop_grace,
            listener_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub async fn set_policy(&self, policy: RealtimeTelemetryPolicy) {
        let mut inner = self.inner.lock().await;
        inner.policy = policy;
        let watched = inner.watchers.clone();
        for (node_id, agent) in &mut inner.agents {
            let demand = watched.get(node_id).cloned().unwrap_or_default();
            let wanted = policy.enabled && demand.is_watched();
            let command = if wanted {
                start_command(policy, &demand)
            } else {
                AgentRealtimeCommand::Stop
            };
            agent.commands.send_replace(command);
            agent.active = wanted;
            send_status(&self.events, node_id, agent, policy.interval_secs);
        }
    }

    /// Registering a second socket for one node replaces the first. The session id subsequently
    /// rejects late samples and a late disconnect from the superseded socket.
    pub async fn register_agent(&self, node_id: String) -> AgentRealtimeSession {
        let mut inner = self.inner.lock().await;
        inner.next_session = inner.next_session.wrapping_add(1).max(1);
        let session = inner.next_session;
        let demand = inner.watchers.get(&node_id).cloned().unwrap_or_default();
        let active = inner.policy.enabled && demand.is_watched();
        let initial = if active {
            start_command(inner.policy, &demand)
        } else {
            AgentRealtimeCommand::Stop
        };
        let (commands, receiver) = watch::channel(initial);
        let (changes, change_receiver) = watch::channel([0; WAKE_DOMAIN_COUNT]);
        inner.agents.insert(
            node_id.clone(),
            AgentConnection {
                session,
                last_sequence: None,
                active,
                commands,
                changes,
            },
        );
        let interval_secs = inner.policy.interval_secs;
        if let Some(agent) = inner.agents.get(&node_id) {
            send_status(&self.events, &node_id, agent, interval_secs);
        }
        AgentRealtimeSession {
            session,
            commands: receiver,
            changes: change_receiver,
        }
    }

    pub async fn unregister_agent(&self, node_id: &str, session: u64) {
        let mut inner = self.inner.lock().await;
        if inner
            .agents
            .get(node_id)
            .is_none_or(|agent| agent.session != session)
        {
            return;
        }
        inner.agents.remove(node_id);
        let _ = self
            .events
            .send(RealtimeBroadcast::Status(RealtimeNodeStatus {
                node_id: node_id.to_owned(),
                connected: false,
                active: false,
                interval_secs: inner.policy.interval_secs,
            }));
    }

    pub async fn record_sample(
        &self,
        node_id: &str,
        session: u64,
        mut sample: AgentRealtimeSample,
    ) -> Result<(), RecordSampleError> {
        normalize_optional_reports(&mut sample);
        validate_sample(&sample).map_err(RecordSampleError::Invalid)?;
        if sample.elapsed_millis > LIVE_GAP_ELAPSED_MILLIS {
            sample.has_gap = true;
        }
        let materialize_delta = sample.diagnostics_unchanged
            && (sample.reverse_health.is_some()
                || sample.mux.is_some()
                || sample.vpngate.is_some());
        let received_at_unix_millis = unix_millis();
        let mut inner = self.inner.lock().await;
        if inner
            .watchers
            .get(node_id)
            .is_some_and(|demand| demand.forwarding.len() > MAX_REALTIME_FORWARDING_TARGETS)
        {
            if let Some(report) = sample
                .user_traffic
                .as_mut()
                .filter(|report| report.state == RealtimeUserTrafficState::Ready)
            {
                report.truncated = true;
            }
        }
        let agent = inner
            .agents
            .get_mut(node_id)
            .ok_or(RecordSampleError::SessionGone)?;
        if agent.session != session {
            return Err(RecordSampleError::Superseded);
        }
        if !agent.active {
            return Err(RecordSampleError::Inactive);
        }
        match agent.last_sequence {
            Some(previous) if sample.sequence <= previous => {
                return Err(RecordSampleError::Sequence);
            }
            Some(previous) if previous.checked_add(1) != Some(sample.sequence) => {
                // WebSocket ordering is reliable. A jump therefore means the producer skipped a
                // reading, and drawing across it would invent continuity.
                sample.has_gap = true;
            }
            None => sample.has_gap = true,
            Some(_) => {}
        }
        agent.last_sequence = Some(sample.sequence);

        let mut stored_event = RealtimeSampleEvent {
            node_id: node_id.to_owned(),
            received_at_unix_millis,
            sample,
        };
        // The broadcast keeps the Agent's delta shape. The ring, however, is also the initial
        // state for a newly opened browser, so its newest entry must always be self-contained.
        // Clone before enriching it: diagnostics-unchanged samples are tiny, while a complete
        // diagnostic refresh already required one clone for ring + broadcast in the old path.
        let mut broadcast_event = stored_event.clone();
        let oldest = received_at_unix_millis
            - i64::try_from(RING_RETENTION.as_millis()).expect("retention fits i64");
        let ring = inner.rings.entry(node_id.to_owned()).or_default();
        // Keep bulky worker state only on the newest ring entry; NIC history stays small. Move
        // the previous snapshot forward for an Agent delta instead of cloning it every second.
        if let Some(previous) = ring.back_mut() {
            // Rates are complete per-frame and only the latest one is rendered. Keeping every
            // user's rows in the NIC history would multiply a large report by 120 seconds.
            previous.sample.user_traffic = None;
            if stored_event.sample.diagnostics_unchanged {
                stored_event.sample.reverse_health = stored_event
                    .sample
                    .reverse_health
                    .take()
                    .or_else(|| previous.sample.reverse_health.take());
                stored_event.sample.mux = stored_event
                    .sample
                    .mux
                    .take()
                    .or_else(|| previous.sample.mux.take());
                stored_event.sample.vpngate = stored_event
                    .sample
                    .vpngate
                    .take()
                    .or_else(|| previous.sample.vpngate.take());
                previous.sample.reverse_health = None;
                previous.sample.mux = None;
                previous.sample.vpngate = None;
            } else {
                previous.sample.reverse_health = None;
                previous.sample.vpngate = None;
                if stored_event.sample.mux_unchanged {
                    stored_event.sample.mux = previous.sample.mux.take().map(|mut report| {
                        // The Agent has freshly confirmed that the normalized Mux state is
                        // unchanged. Materialize that confirmation in the self-contained ring
                        // snapshot so a newly opened browser does not inherit the old baseline's
                        // age and immediately call a healthy idle pool stale.
                        report.sampled_at_unix_ms = stored_event.sample.sampled_at_unix_millis;
                        report
                    });
                } else {
                    previous.sample.mux = None;
                }
            }
        }
        // A snapshot is authoritative, never a delta. If the first event after reconnect was an
        // invalid delta there is no previous state to preserve, so it correctly becomes empty.
        stored_event.sample.diagnostics_unchanged = false;
        stored_event.sample.mux_unchanged = false;
        if materialize_delta {
            // Actual reports win over omission hints. Send one complete corrective snapshot
            // so existing browsers honor the payload and retain unrelated diagnostic baselines.
            broadcast_event = stored_event.clone();
        }
        ring.push_back(stored_event);
        trim_ring(ring, oldest);
        let _ = self
            .events
            .send(RealtimeBroadcast::Sample(Box::new(broadcast_event)));
        Ok(())
    }

    pub async fn subscribe(&self, nodes: impl IntoIterator<Item = String>) -> RealtimeSubscription {
        self.subscribe_with_demand(nodes, true, true, HashMap::new(), HashMap::new())
            .await
    }

    pub async fn subscribe_with_forwarding(
        &self,
        node_id: String,
        forwarding: Vec<RealtimeForwardingTarget>,
    ) -> RealtimeSubscription {
        self.subscribe_with_demand(
            [node_id.clone()],
            true,
            true,
            HashMap::new(),
            HashMap::from([(node_id, forwarding)]),
        )
        .await
    }

    pub async fn subscribe_diagnostics(
        &self,
        nodes: impl IntoIterator<Item = String>,
    ) -> RealtimeSubscription {
        self.subscribe_with_demand(nodes, true, false, HashMap::new(), HashMap::new())
            .await
    }

    /// Fleet totals consume only NIC rates. Keeping this a distinct lease prevents a global
    /// header from turning machine-only worker snapshots into permanent fleet-wide traffic.
    pub async fn subscribe_nic(
        &self,
        nodes: impl IntoIterator<Item = String>,
    ) -> RealtimeSubscription {
        self.subscribe_with_demand(nodes, false, false, HashMap::new(), HashMap::new())
            .await
    }

    /// The fleet traffic page needs every active user rate, but none of the comparatively heavy
    /// worker diagnostics. Forwarding ownership remains machine-local so an Agent can attribute
    /// those counters without receiving another machine's rule ids.
    pub async fn subscribe_fleet_traffic(
        &self,
        nodes: impl IntoIterator<Item = String>,
        forwarding: HashMap<String, Vec<RealtimeForwardingTarget>>,
    ) -> RealtimeSubscription {
        self.subscribe_with_demand(nodes, false, true, HashMap::new(), forwarding)
            .await
    }

    /// Subscribe to exact compiled grant labels across several machines. Diagnostics stay off and
    /// the browser response is filtered again by the HTTP layer, so concurrent user pages share
    /// one Agent sample without learning one another's identities.
    pub async fn subscribe_user_traffic(
        &self,
        labels_by_node: HashMap<String, Vec<String>>,
    ) -> RealtimeSubscription {
        let nodes = labels_by_node.keys().cloned().collect::<Vec<_>>();
        self.subscribe_with_demand(nodes, false, false, labels_by_node, HashMap::new())
            .await
    }

    pub async fn subscribe_user_targets(
        &self,
        targets: Vec<brocade_store::UserRealtimeTarget>,
    ) -> RealtimeSubscription {
        let nodes = targets
            .iter()
            .map(|t| t.node_id.clone())
            .collect::<Vec<_>>();
        let labels = targets
            .iter()
            .map(|t| (t.node_id.clone(), t.labels.clone()))
            .collect();
        let forwarding = targets
            .into_iter()
            .map(|t| (t.node_id, t.forwarding))
            .collect();
        self.subscribe_with_demand(nodes, false, false, labels, forwarding)
            .await
    }

    async fn subscribe_with_demand(
        &self,
        nodes: impl IntoIterator<Item = String>,
        diagnostics: bool,
        user_all: bool,
        user_labels: HashMap<String, Vec<String>>,
        forwarding: HashMap<String, Vec<RealtimeForwardingTarget>>,
    ) -> RealtimeSubscription {
        let visible_nodes = nodes
            .into_iter()
            .filter(|node| !node.is_empty())
            .collect::<HashSet<_>>();
        let mut ordered = visible_nodes.iter().cloned().collect::<Vec<_>>();
        ordered.sort();

        // Subscribe before taking the snapshot. `record_sample` holds the same mutex, so after
        // this lock is acquired each sample is either in the snapshot or waiting in the receiver.
        let events = self.events.subscribe();
        let mut inner = self.inner.lock().await;
        let policy = inner.policy;
        for node_id in &ordered {
            let demand = inner.watchers.entry(node_id.clone()).or_default();
            let was_watched = demand.is_watched();
            let previous_command = start_command(policy, demand);
            demand.total = demand.total.saturating_add(1);
            if diagnostics {
                demand.diagnostics = demand.diagnostics.saturating_add(1);
            }
            if user_all {
                demand.user_all = demand.user_all.saturating_add(1);
            }
            if let Some(labels) = user_labels.get(node_id) {
                for label in labels {
                    let count = demand.user_labels.entry(label.clone()).or_default();
                    *count = count.saturating_add(1);
                }
            }
            if let Some(targets) = forwarding.get(node_id) {
                for target in targets {
                    *demand.forwarding.entry(target.clone()).or_default() += 1;
                }
            }
            let next_command = start_command(policy, demand);
            *inner.stop_generation.entry(node_id.clone()).or_default() += 1;
            if policy.enabled && (!was_watched || previous_command != next_command) {
                if let Some(agent) = inner.agents.get_mut(node_id) {
                    agent.commands.send_replace(next_command);
                    agent.active = true;
                    if !was_watched {
                        send_status(&self.events, node_id, agent, policy.interval_secs);
                    }
                }
            }
        }
        let oldest =
            unix_millis() - i64::try_from(RING_RETENTION.as_millis()).expect("retention fits i64");
        for node_id in &ordered {
            if let Some(ring) = inner.rings.get_mut(node_id) {
                trim_ring(ring, oldest);
            }
        }
        let snapshots = ordered
            .iter()
            .map(|node_id| snapshot(&inner, node_id))
            .collect();
        drop(inner);

        RealtimeSubscription {
            policy,
            snapshots,
            events,
            visible_nodes,
            _lease: WatchLease {
                service: self.clone(),
                nodes: ordered,
                diagnostics,
                user_all,
                user_labels,
                forwarding,
            },
        }
    }

    /// Live roles take precedence over a reconcile summary that may still be in transit. Use
    /// only connected, actively sampled nodes and a fresh supervisor report; never persist this
    /// diagnostic snapshot as a substitute for the reliable reconciliation path.
    pub async fn vpngate_selections(&self) -> Vec<brocade_store::VpngateRuntimeSelection> {
        let inner = self.inner.lock().await;
        let now = unix_millis();
        let mut selections = Vec::new();
        for (node_id, agent) in &inner.agents {
            if !agent.active || agent.last_sequence.is_none() {
                continue;
            }
            let Some(event) = inner.rings.get(node_id).and_then(|ring| ring.back()) else {
                continue;
            };
            let Some(report) = event.sample.vpngate.as_ref() else {
                continue;
            };
            if now.saturating_sub(event.received_at_unix_millis) > VPNGATE_LIVE_MAX_AGE_MILLIS
                || event
                    .sample
                    .sampled_at_unix_millis
                    .saturating_sub(report.sampled_at_unix_millis)
                    > VPNGATE_LIVE_MAX_AGE_MILLIS
            {
                continue;
            }
            for pool in &report.pools {
                let selected = report.backends.iter().find(|backend| {
                    backend.outbound_id == pool.outbound_id
                        && backend.role == VpngateBackendRole::Active
                        && Some(backend.slot) == pool.active_slot
                });
                selections.push(brocade_store::VpngateRuntimeSelection {
                    node_id: node_id.clone(),
                    outbound_id: pool.outbound_id.clone(),
                    selected_server_id: selected.map(|backend| backend.server_id.clone()),
                });
            }
        }
        selections.sort_by(|left, right| {
            (&left.node_id, &left.outbound_id).cmp(&(&right.node_id, &right.outbound_id))
        });
        selections
    }

    async fn release(
        &self,
        nodes: Vec<String>,
        diagnostics: bool,
        user_all: bool,
        user_labels: HashMap<String, Vec<String>>,
        forwarding: HashMap<String, Vec<RealtimeForwardingTarget>>,
    ) {
        let mut scheduled = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            let policy = inner.policy;
            for node_id in nodes {
                let demand = inner.watchers.entry(node_id.clone()).or_default();
                let previous_command = start_command(policy, demand);
                demand.total = demand.total.saturating_sub(1);
                if diagnostics {
                    demand.diagnostics = demand.diagnostics.saturating_sub(1);
                }
                if user_all {
                    demand.user_all = demand.user_all.saturating_sub(1);
                }
                if let Some(labels) = user_labels.get(&node_id) {
                    for label in labels {
                        let remove = demand.user_labels.get_mut(label).is_some_and(|count| {
                            *count = count.saturating_sub(1);
                            *count == 0
                        });
                        if remove {
                            demand.user_labels.remove(label);
                        }
                    }
                }
                if let Some(targets) = forwarding.get(&node_id) {
                    for target in targets {
                        let remove = demand.forwarding.get_mut(target).is_some_and(|count| {
                            *count = count.saturating_sub(1);
                            *count == 0
                        });
                        if remove {
                            demand.forwarding.remove(target);
                        }
                    }
                }
                let next_command = start_command(policy, demand);
                let is_watched = demand.is_watched();
                if policy.enabled
                    && previous_command != next_command
                    && is_watched
                    && inner.agents.get(&node_id).is_some_and(|agent| agent.active)
                {
                    if let Some(agent) = inner.agents.get_mut(&node_id) {
                        agent.commands.send_replace(next_command);
                    }
                }
                if !is_watched {
                    let generation = inner.stop_generation.entry(node_id.clone()).or_default();
                    *generation += 1;
                    scheduled.push((node_id, *generation));
                }
            }
        }
        for (node_id, generation) in scheduled {
            let service = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(service.stop_grace).await;
                service.stop_if_still_idle(node_id, generation).await;
            });
        }
    }

    /// How many browsers the control plane believes are watching this node. The Agent samples
    /// only while this is above zero, so a lease that is released early silences a live view.
    #[cfg(test)]
    pub(crate) async fn watcher_count(&self, node_id: &str) -> usize {
        self.inner
            .lock()
            .await
            .watchers
            .get(node_id)
            .cloned()
            .unwrap_or_default()
            .total
    }

    async fn stop_if_still_idle(&self, node_id: String, generation: u64) {
        let mut inner = self.inner.lock().await;
        if inner
            .watchers
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
            .is_watched()
            || inner.stop_generation.get(&node_id).copied() != Some(generation)
        {
            return;
        }
        let interval_secs = inner.policy.interval_secs;
        if let Some(agent) = inner.agents.get_mut(&node_id) {
            agent.commands.send_replace(AgentRealtimeCommand::Stop);
            agent.active = false;
            send_status(&self.events, &node_id, agent, interval_secs);
        }
    }
}

fn snapshot(inner: &Inner, node_id: &str) -> RealtimeNodeSnapshot {
    let agent = inner.agents.get(node_id);
    RealtimeNodeSnapshot {
        node_id: node_id.to_owned(),
        connected: agent.is_some(),
        active: agent.is_some_and(|agent| agent.active),
        interval_secs: inner.policy.interval_secs,
        samples: inner
            .rings
            .get(node_id)
            .map(|ring| ring.iter().cloned().collect())
            .unwrap_or_default(),
    }
}

fn trim_ring(ring: &mut VecDeque<RealtimeSampleEvent>, oldest: i64) {
    while ring.len() > RING_MAX_SAMPLES
        || ring
            .front()
            .is_some_and(|sample| sample.received_at_unix_millis < oldest)
    {
        ring.pop_front();
    }
}

fn send_status(
    events: &broadcast::Sender<RealtimeBroadcast>,
    node_id: &str,
    agent: &AgentConnection,
    interval_secs: u32,
) {
    let _ = events.send(RealtimeBroadcast::Status(RealtimeNodeStatus {
        node_id: node_id.to_owned(),
        connected: true,
        active: agent.active,
        interval_secs,
    }));
}

fn validate_sample(sample: &AgentRealtimeSample) -> Result<(), &'static str> {
    let interface_chars = sample.interface.chars().count();
    if interface_chars == 0 || interface_chars > MAX_INTERFACE_CHARS {
        return Err("interface name has an invalid length");
    }
    if sample.elapsed_millis == 0 {
        return Err("sample elapsed time must be positive");
    }
    let now = unix_millis();
    if sample.sampled_at_unix_millis <= 0
        || sample.sampled_at_unix_millis > now.saturating_add(MAX_FUTURE_CLOCK_SKEW_MILLIS)
    {
        return Err("sample timestamp is invalid");
    }
    Ok(())
}

/// Optional diagnostics cannot invalidate a NIC reading. Keep useful user rows, mark their
/// coverage incomplete when filtering, and drop only an unusable optional report.
fn normalize_optional_reports(sample: &mut AgentRealtimeSample) {
    if sample.diagnostics_unchanged || sample.mux.is_some() {
        sample.mux_unchanged = false;
    }
    if let Some(report) = sample.user_traffic.as_mut() {
        match report.state {
            RealtimeUserTrafficState::Ready => {
                if report.elapsed_millis.is_none_or(|elapsed| elapsed == 0) {
                    sample.user_traffic = None;
                } else {
                    let original = report.users.len();
                    report.users.truncate(MAX_REALTIME_USER_TRAFFIC_ROWS);
                    let mut identities = HashSet::new();
                    report.users.retain(|user| {
                        let valid_source = matches!((&user.ingress_id, user.forwarding_rule_id),
                            (Some(id), None) if !id.is_empty() && id.len() <= 64)
                            || matches!((&user.ingress_id, user.forwarding_rule_id),
                                (None, Some(id)) if id > 0 && report.forwarding_supported);
                        !user.tenant_id.is_empty()
                            && user.tenant_id.len() <= 128
                            && !user.user_id.is_empty()
                            && user.user_id.len() <= 64
                            && valid_source
                            && identities.insert((
                                user.tenant_id.clone(),
                                user.user_id.clone(),
                                user.ingress_id.clone(),
                                user.forwarding_rule_id,
                            ))
                    });
                    if report.users.len() != original {
                        report.truncated = true;
                        report.has_gap = true;
                    }
                    if report
                        .elapsed_millis
                        .is_some_and(|elapsed| elapsed > LIVE_GAP_ELAPSED_MILLIS)
                    {
                        report.has_gap = true;
                    }
                }
            }
            RealtimeUserTrafficState::Warming | RealtimeUserTrafficState::Unavailable => {
                report.elapsed_millis = None;
                report.users.clear();
                report.truncated = false;
                report.has_gap = false;
            }
        }
    }
    if sample
        .vpngate
        .as_ref()
        .is_some_and(|report| !valid_vpngate_report(report))
    {
        sample.vpngate = None;
    }
}

fn valid_vpngate_report(report: &brocade_deployment::protocol::VpngateRealtimeReport) -> bool {
    !report.boot_id.is_empty()
        && report.boot_id.chars().count() <= 128
        && report.pools.len() <= MAX_VPNGATE_POOLS
        && report.backends.len() <= MAX_VPNGATE_BACKENDS
        && report.events.len() <= MAX_VPNGATE_EVENTS
        && report
            .pools
            .iter()
            .all(|pool| pool.candidate_count <= VPNGATE_MAX_CANDIDATES)
        && serde_json::to_vec(report).is_ok_and(|body| body.len() <= MAX_VPNGATE_REPORT_BYTES)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current(commands: &mut watch::Receiver<AgentRealtimeCommand>) -> AgentRealtimeCommand {
        commands.borrow_and_update().clone()
    }

    async fn next(commands: &mut watch::Receiver<AgentRealtimeCommand>) -> AgentRealtimeCommand {
        commands.changed().await.unwrap();
        current(commands)
    }

    fn sample(sequence: u64) -> AgentRealtimeSample {
        AgentRealtimeSample {
            diagnostics_unchanged: false,
            mux_unchanged: false,
            sequence,
            sampled_at_unix_millis: unix_millis(),
            elapsed_millis: 1000,
            interface: "eth0".to_owned(),
            rx_bytes_per_sec: 123,
            tx_bytes_per_sec: 45,
            reverse_health: None,
            mux: None,
            vpngate: None,
            user_traffic: None,
            has_gap: false,
        }
    }

    #[test]
    fn interface_display_names_do_not_need_a_shell_character_whitelist() {
        let mut live = sample(1);
        for name in ["uplink+0", "出口0", "eth0"] {
            live.interface = name.into();
            validate_sample(&live).unwrap();
        }
        live.interface = String::new();
        assert!(validate_sample(&live).is_err());
        live.interface = "x".repeat(MAX_INTERFACE_CHARS + 1);
        assert!(validate_sample(&live).is_err());
    }

    #[tokio::test]
    async fn invalid_optional_rows_and_reports_do_not_reject_nic_samples() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        let mut live = sample(1);
        live.user_traffic = Some(serde_json::from_value(serde_json::json!({
            "state":"ready", "all_users":true, "elapsed_millis":50,
            "users":[
                {"tenant_id":"platform","user_id":"a","ingress_id":"entry","uplink_bytes_per_sec":11,"downlink_bytes_per_sec":22},
                {"tenant_id":"","user_id":"b","ingress_id":"entry","uplink_bytes_per_sec":99,"downlink_bytes_per_sec":99},
                {"tenant_id":"platform","user_id":"a","ingress_id":"entry","uplink_bytes_per_sec":11,"downlink_bytes_per_sec":22}
            ]
        })).unwrap());
        live.vpngate = Some(brocade_deployment::protocol::VpngateRealtimeReport {
            boot_id: String::new(),
            sequence: 1,
            sampled_at_unix_millis: unix_millis(),
            pools: Vec::new(),
            backends: Vec::new(),
            events: Vec::new(),
        });
        service
            .record_sample("n1", agent.session, live)
            .await
            .unwrap();
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        let actual = &snapshot.snapshots[0].samples[0].sample;
        assert_eq!(actual.rx_bytes_per_sec, 123);
        assert!(actual.vpngate.is_none());
        let users = actual.user_traffic.as_ref().unwrap();
        assert_eq!(users.users.len(), 1);
        assert_eq!(users.users[0].uplink_bytes_per_sec, 11);
        assert!(users.truncated && users.has_gap);
    }

    #[test]
    fn malformed_optional_rate_intervals_do_not_discard_other_dimensions() {
        for state in ["ready", "warming", "unavailable"] {
            let mut live = sample(1);
            live.user_traffic = Some(
                serde_json::from_value(serde_json::json!({
                    "state":state,"all_users":false,"elapsed_millis":0,"users":[],"has_gap":true
                }))
                .unwrap(),
            );
            normalize_optional_reports(&mut live);
            validate_sample(&live).unwrap();
            assert_eq!(live.rx_bytes_per_sec, 123);
            assert!(
                !live.has_gap,
                "optional observations must not invent a NIC gap"
            );
            if state == "ready" {
                assert!(live.user_traffic.is_none());
            } else {
                assert_eq!(live.user_traffic.unwrap().elapsed_millis, None);
            }
        }
    }

    #[tokio::test]
    async fn positive_sampling_intervals_are_accepted_and_long_windows_show_a_gap() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        for (sequence, elapsed) in [(1, 50), (2, 90_000)] {
            let mut live = sample(sequence);
            live.elapsed_millis = elapsed;
            service
                .record_sample("n1", agent.session, live)
                .await
                .unwrap();
        }
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(
            snapshot.snapshots[0].samples[1].sample.rx_bytes_per_sec,
            123
        );
        assert!(snapshot.snapshots[0].samples[1].sample.has_gap);
        let mut zero = sample(3);
        zero.elapsed_millis = 0;
        assert!(service
            .record_sample("n1", agent.session, zero)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn actual_diagnostics_override_omission_hints_and_keep_other_baselines() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let mut subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        let mut first = sample(1);
        first.mux = Some(brocade_deployment::protocol::MuxReport {
            boot_id: "boot".into(),
            sequence: 1,
            sampled_at_unix_ms: unix_millis(),
            pools: Vec::new(),
            workers: Vec::new(),
            events: Vec::new(),
        });
        first.vpngate = Some(brocade_deployment::protocol::VpngateRealtimeReport {
            boot_id: "vpn-boot".into(),
            sequence: 1,
            sampled_at_unix_millis: unix_millis(),
            pools: Vec::new(),
            backends: Vec::new(),
            events: Vec::new(),
        });
        service
            .record_sample("n1", agent.session, first.clone())
            .await
            .unwrap();
        let mut second = sample(2);
        second.diagnostics_unchanged = true;
        second.mux_unchanged = true;
        second.mux = first.mux.take();
        second.mux.as_mut().unwrap().sequence = 2;
        service
            .record_sample("n1", agent.session, second)
            .await
            .unwrap();
        let broadcast = loop {
            if let RealtimeBroadcast::Sample(event) = subscription.events.recv().await.unwrap() {
                if event.sample.sequence == 2 {
                    break event;
                }
            }
        };
        assert!(!broadcast.sample.diagnostics_unchanged && !broadcast.sample.mux_unchanged);
        assert_eq!(broadcast.sample.mux.as_ref().unwrap().sequence, 2);
        assert_eq!(
            broadcast.sample.vpngate.as_ref().unwrap().boot_id,
            "vpn-boot"
        );
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(
            snapshot.snapshots[0].samples[1]
                .sample
                .mux
                .as_ref()
                .unwrap()
                .sequence,
            2
        );
        assert!(snapshot.snapshots[0].samples[0].sample.vpngate.is_none());
    }

    #[tokio::test]
    async fn vpngate_selection_uses_only_a_fresh_active_backend_from_the_current_session() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        let mut live = sample(1);
        live.vpngate = Some(serde_json::from_value(serde_json::json!({
            "boot_id": "boot", "sequence": 1, "sampled_at_unix_millis": live.sampled_at_unix_millis,
            "pools": [{
                "outbound_id": "pool", "country_code": "US", "state": "healthy", "active_slot": 1,
                "ready_standbys": 1, "candidate_count": 16, "consecutive_failures": 0,
                "probes": 20, "probe_failures": 2, "failovers": 8, "refill_attempts": 3,
                "refill_failures": 1
            }],
            "backends": [
                {"outbound_id": "pool", "slot": 0, "role": "standby", "state": "healthy", "server_id": "old",
                 "consecutive_failures": 0, "backoff_remaining_millis": 0},
                {"outbound_id": "pool", "slot": 1, "role": "active", "state": "healthy", "server_id": "new",
                 "consecutive_failures": 0, "backoff_remaining_millis": 0}
            ],
            "events": []
        })).unwrap());
        service
            .record_sample("n1", agent.session, live.clone())
            .await
            .unwrap();
        let selections = service.vpngate_selections().await;
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].selected_server_id.as_deref(), Some("new"));

        service
            .inner
            .lock()
            .await
            .rings
            .get_mut("n1")
            .unwrap()
            .back_mut()
            .unwrap()
            .received_at_unix_millis -= VPNGATE_LIVE_MAX_AGE_MILLIS + 1;
        assert!(service.vpngate_selections().await.is_empty());

        live.sequence = 2;
        live.vpngate.as_mut().unwrap().sampled_at_unix_millis -= VPNGATE_LIVE_MAX_AGE_MILLIS + 1;
        service
            .record_sample("n1", agent.session, live.clone())
            .await
            .unwrap();
        assert!(service.vpngate_selections().await.is_empty());

        live.sequence = 3;
        live.vpngate.as_mut().unwrap().sampled_at_unix_millis = live.sampled_at_unix_millis;
        live.vpngate.as_mut().unwrap().pools[0].active_slot = None;
        service
            .record_sample("n1", agent.session, live)
            .await
            .unwrap();
        assert!(service.vpngate_selections().await[0]
            .selected_server_id
            .is_none());

        service.unregister_agent("n1", agent.session).await;
        assert!(service.vpngate_selections().await.is_empty());
        let _new_agent = service.register_agent("n1".to_owned()).await;
        assert!(service.vpngate_selections().await.is_empty());
    }

    #[tokio::test]
    async fn agent_starts_only_while_a_node_is_watched() {
        let service = RealtimeService::with_stop_grace(
            RealtimeTelemetryPolicy::default(),
            Duration::from_millis(1),
        );
        let mut agent = service.register_agent("n1".to_owned()).await;
        assert_eq!(current(&mut agent.commands), AgentRealtimeCommand::Stop);

        let subscription = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: true,
                user_traffic: Some(RealtimeUserTrafficSelector::All {
                    forwarding: Vec::new()
                }),
            }
        );
        drop(subscription);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(next(&mut agent.commands).await, AgentRealtimeCommand::Stop);
    }

    #[tokio::test]
    async fn fleet_header_detail_and_node_leases_request_only_the_data_they_consume() {
        let service = RealtimeService::with_stop_grace(
            RealtimeTelemetryPolicy::default(),
            Duration::from_millis(1),
        );
        let mut agent = service.register_agent("n1".to_owned()).await;
        assert_eq!(current(&mut agent.commands), AgentRealtimeCommand::Stop);

        let fleet = service.subscribe_nic(["n1".to_owned()]).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: None,
            }
        );

        let fleet_traffic = service
            .subscribe_fleet_traffic(["n1".to_owned()], HashMap::new())
            .await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: Some(RealtimeUserTrafficSelector::All {
                    forwarding: Vec::new()
                }),
            }
        );
        drop(fleet_traffic);
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: None,
            }
        );

        let node = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: true,
                user_traffic: Some(RealtimeUserTrafficSelector::All {
                    forwarding: Vec::new()
                }),
            }
        );

        drop(node);
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: None,
            }
        );
        drop(fleet);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(next(&mut agent.commands).await, AgentRealtimeCommand::Stop);
    }

    #[tokio::test]
    async fn a_replacement_connection_rejects_the_old_session() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let old = service.register_agent("n1".to_owned()).await;
        let new = service.register_agent("n1".to_owned()).await;
        assert!(service
            .record_sample("n1", old.session, sample(1))
            .await
            .is_err());
        assert!(service
            .record_sample("n1", new.session, sample(1))
            .await
            .is_ok());
        service.unregister_agent("n1", old.session).await;
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        assert!(snapshot.snapshots[0].connected);
    }

    #[tokio::test]
    async fn user_label_demand_unions_and_returns_to_exact_scope_after_machine_view_closes() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let mut agent = service.register_agent("n1".to_owned()).await;
        let labels = |label: &str| HashMap::from([("n1".to_owned(), vec![label.to_owned()])]);
        let first = service.subscribe_user_traffic(labels("a")).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: Some(RealtimeUserTrafficSelector::Labels {
                    forwarding: Vec::new(),
                    labels: vec!["a".to_owned()]
                }),
            }
        );
        let second = service.subscribe_user_traffic(labels("b")).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: Some(RealtimeUserTrafficSelector::Labels {
                    forwarding: Vec::new(),
                    labels: vec!["a".to_owned(), "b".to_owned()]
                }),
            }
        );
        let machine = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: true,
                user_traffic: Some(RealtimeUserTrafficSelector::All {
                    forwarding: Vec::new()
                }),
            }
        );
        drop(machine);
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: Some(RealtimeUserTrafficSelector::Labels {
                    forwarding: Vec::new(),
                    labels: vec!["a".to_owned(), "b".to_owned()]
                }),
            }
        );
        drop(second);
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 1000,
                diagnostics: false,
                user_traffic: Some(RealtimeUserTrafficSelector::Labels {
                    forwarding: Vec::new(),
                    labels: vec!["a".to_owned()]
                }),
            }
        );
        drop(first);
    }

    #[tokio::test]
    async fn forwarding_demand_is_refcounted_and_bounds_are_explicit() {
        let service = RealtimeService::new(Default::default());
        let mut agent = service.register_agent("n1".to_owned()).await;
        let target = RealtimeForwardingTarget {
            rule_id: 7,
            tenant_id: "platform".to_owned(),
            user_id: "alice".to_owned(),
        };
        let targets = vec![brocade_store::UserRealtimeTarget {
            node_id: "n1".to_owned(),
            labels: vec![],
            forwarding: vec![target.clone()],
        }];
        let first = service.subscribe_user_targets(targets.clone()).await;
        let command = next(&mut agent.commands).await;
        assert!(
            matches!(command,AgentRealtimeCommand::Start { user_traffic:Some(RealtimeUserTrafficSelector::Labels { labels,forwarding }),.. } if labels.is_empty() && forwarding==vec![target.clone()])
        );
        let second = service.subscribe_user_targets(targets).await;
        drop(first);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(service.watcher_count("n1").await, 1);
        assert_eq!(
            service.inner.lock().await.watchers["n1"].forwarding[&target],
            1
        );
        drop(second);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(service.inner.lock().await.watchers["n1"]
            .forwarding
            .is_empty());
        let many = (1..=MAX_REALTIME_FORWARDING_TARGETS + 1)
            .map(|rule_id| RealtimeForwardingTarget {
                rule_id: rule_id as i64,
                tenant_id: "t".repeat(128),
                user_id: "u".repeat(64),
            })
            .collect::<Vec<_>>();
        let _lease = service
            .subscribe_with_forwarding("n1".to_owned(), many)
            .await;
        let command = next(&mut agent.commands).await;
        assert!(serde_json::to_vec(&command).unwrap().len() < 512 * 1024);
        assert!(
            matches!(command,AgentRealtimeCommand::Start { user_traffic:Some(RealtimeUserTrafficSelector::All { forwarding }),.. } if forwarding.len()==MAX_REALTIME_FORWARDING_TARGETS)
        );
        let mut sample = sample(1);
        sample.user_traffic=Some(serde_json::from_value(serde_json::json!({"state":"ready","all_users":true,"elapsed_millis":1000,"users":[],"forwarding_supported":true})).unwrap());
        service
            .record_sample("n1", agent.session, sample)
            .await
            .unwrap();
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        assert!(
            snapshot.snapshots[0].samples[0]
                .sample
                .user_traffic
                .as_ref()
                .unwrap()
                .truncated
        );
    }

    #[tokio::test]
    async fn sequence_is_monotonic_and_the_first_sample_is_a_gap() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        service
            .record_sample("n1", agent.session, sample(7))
            .await
            .unwrap();
        assert!(service
            .record_sample("n1", agent.session, sample(7))
            .await
            .is_err());
        service
            .record_sample("n1", agent.session, sample(9))
            .await
            .unwrap();
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        assert_eq!(snapshot.snapshots[0].samples.len(), 2);
        assert!(snapshot.snapshots[0].samples[0].sample.has_gap);
        assert!(snapshot.snapshots[0].samples[1].sample.has_gap);
    }

    #[tokio::test]
    async fn only_the_newest_ring_entry_keeps_bulky_runtime_state() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        let report = || brocade_deployment::protocol::MuxReport {
            boot_id: "boot".to_owned(),
            sequence: 1,
            sampled_at_unix_ms: unix_millis(),
            pools: Vec::new(),
            workers: Vec::new(),
            events: Vec::new(),
        };
        let mut first = sample(1);
        first.mux = Some(report());
        first.vpngate = Some(brocade_deployment::protocol::VpngateRealtimeReport {
            boot_id: "boot".to_owned(),
            sequence: 1,
            sampled_at_unix_millis: unix_millis(),
            pools: Vec::new(),
            backends: Vec::new(),
            events: Vec::new(),
        });
        service
            .record_sample("n1", agent.session, first)
            .await
            .unwrap();
        let mut second = sample(2);
        second.diagnostics_unchanged = true;
        service
            .record_sample("n1", agent.session, second)
            .await
            .unwrap();

        let snapshot = service.subscribe(["n1".to_owned()]).await;
        let samples = &snapshot.snapshots[0].samples;
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].sample.mux, None);
        assert_eq!(samples[0].sample.vpngate, None);
        assert!(samples[1].sample.mux.is_some());
        assert!(samples[1].sample.vpngate.is_some());
        assert!(!samples[1].sample.diagnostics_unchanged);
    }

    #[tokio::test]
    async fn a_mux_only_delta_keeps_the_latest_complete_ring_baseline() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let mut subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        let mut first = sample(1);
        first.mux = Some(brocade_deployment::protocol::MuxReport {
            boot_id: "boot".to_owned(),
            sequence: 1,
            sampled_at_unix_ms: unix_millis(),
            pools: Vec::new(),
            workers: Vec::new(),
            events: Vec::new(),
        });
        service
            .record_sample("n1", agent.session, first)
            .await
            .unwrap();

        let mut second = sample(2);
        second.mux_unchanged = true;
        let confirmed_at = second.sampled_at_unix_millis;
        service
            .record_sample("n1", agent.session, second)
            .await
            .unwrap();
        let delta = loop {
            if let RealtimeBroadcast::Sample(event) = subscription.events.recv().await.unwrap() {
                if event.sample.sequence == 2 {
                    break event;
                }
            }
        };
        assert!(delta.sample.mux_unchanged);
        assert!(delta.sample.mux.is_none());

        let snapshot = service.subscribe(["n1".to_owned()]).await;
        let latest = &snapshot.snapshots[0].samples[1].sample;
        assert!(!latest.mux_unchanged);
        assert_eq!(
            latest.mux.as_ref().map(|report| report.sampled_at_unix_ms),
            Some(confirmed_at)
        );
    }

    #[tokio::test]
    async fn watchers_share_one_lease_and_only_the_last_drop_stops_sampling() {
        let service = RealtimeService::with_stop_grace(
            RealtimeTelemetryPolicy::default(),
            Duration::from_millis(1),
        );
        let mut agent = service.register_agent("n1".to_owned()).await;
        assert_eq!(current(&mut agent.commands), AgentRealtimeCommand::Stop);
        let first = service.subscribe(["n1".to_owned()]).await;
        assert!(matches!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start { .. }
        ));
        let second = service.subscribe(["n1".to_owned()]).await;
        drop(first);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!agent.commands.has_changed().unwrap());
        drop(second);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(next(&mut agent.commands).await, AgentRealtimeCommand::Stop);
    }

    #[tokio::test]
    async fn a_policy_change_reconfigures_a_connected_watched_agent() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let mut agent = service.register_agent("n1".to_owned()).await;
        assert!(matches!(
            current(&mut agent.commands),
            AgentRealtimeCommand::Start { .. }
        ));
        service
            .set_policy(RealtimeTelemetryPolicy {
                enabled: true,
                interval_secs: 5,
            })
            .await;
        assert_eq!(
            next(&mut agent.commands).await,
            AgentRealtimeCommand::Start {
                interval_millis: 5000,
                diagnostics: true,
                user_traffic: Some(RealtimeUserTrafficSelector::All {
                    forwarding: Vec::new()
                }),
            }
        );
        service
            .set_policy(RealtimeTelemetryPolicy {
                enabled: false,
                interval_secs: 5,
            })
            .await;
        assert_eq!(next(&mut agent.commands).await, AgentRealtimeCommand::Stop);
    }

    #[tokio::test]
    async fn the_process_local_ring_has_a_hard_per_node_bound() {
        let service = RealtimeService::new(RealtimeTelemetryPolicy::default());
        let _subscription = service.subscribe(["n1".to_owned()]).await;
        let agent = service.register_agent("n1".to_owned()).await;
        for sequence in 1..=u64::try_from(RING_MAX_SAMPLES + 5).unwrap() {
            service
                .record_sample("n1", agent.session, sample(sequence))
                .await
                .unwrap();
        }
        let snapshot = service.subscribe(["n1".to_owned()]).await;
        let samples = &snapshot.snapshots[0].samples;
        assert_eq!(samples.len(), RING_MAX_SAMPLES);
        assert_eq!(samples[0].sample.sequence, 6);
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use brocade_deployment::agent_channel::WakeDomain;
    #[tokio::test]
    async fn wake_domains_are_retained_independently_of_sampling_and_other_nodes() {
        let service = RealtimeService::new(Default::default());
        let mut first = service.register_agent("n1".into()).await;
        let second = service.register_agent("n2".into()).await;
        service
            .wake_matching(&[WakeDomain::AgentRelease], Some("n1"))
            .await;
        service
            .wake_matching(&[WakeDomain::XrayRelease], Some("n1"))
            .await;
        let _subscription = service.subscribe(["n1".into()]).await;
        assert!(matches!(
            *first.commands.borrow_and_update(),
            AgentRealtimeCommand::Start { .. }
        ));
        let versions = *first.changes.borrow_and_update();
        assert_eq!(versions[WakeDomain::AgentRelease as usize], 1);
        assert_eq!(versions[WakeDomain::XrayRelease as usize], 1);
        assert_eq!(*second.changes.borrow(), [0; WAKE_DOMAIN_COUNT]);
    }
}
