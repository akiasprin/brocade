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
    AgentRealtimeCommand, AgentRealtimeSample, RealtimeNodeSnapshot, RealtimeSampleEvent,
    RealtimeTelemetryPolicy, VpngateBackendRole,
};
use serde::Serialize;
use tokio::sync::{broadcast, watch, Mutex};

const RING_RETENTION: Duration = Duration::from_secs(120);
const RING_MAX_SAMPLES: usize = 600;
const EVENT_CHANNEL_CAPACITY: usize = 2048;
const DEFAULT_STOP_GRACE: Duration = Duration::from_secs(15);
const MAX_INTERFACE_CHARS: usize = 32;
const MIN_ELAPSED_MILLIS: u32 = 100;
const MAX_ELAPSED_MILLIS: u32 = 60_000;
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
}

#[derive(Debug)]
struct Inner {
    policy: RealtimeTelemetryPolicy,
    next_session: u64,
    agents: HashMap<String, AgentConnection>,
    watchers: HashMap<String, usize>,
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
}

pub struct AgentRealtimeSession {
    pub session: u64,
    pub commands: watch::Receiver<AgentRealtimeCommand>,
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
}

impl Drop for WatchLease {
    fn drop(&mut self) {
        let service = self.service.clone();
        let nodes = std::mem::take(&mut self.nodes);
        // HTTP streams always live on a Tokio runtime. If shutdown has already torn it down there
        // is no Agent connection left to stop, so failing to obtain a handle is harmless.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { service.release(nodes).await });
        }
    }
}

impl RealtimeService {
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
        }
    }

    pub async fn set_policy(&self, policy: RealtimeTelemetryPolicy) {
        let mut inner = self.inner.lock().await;
        inner.policy = policy;
        let watched = inner.watchers.clone();
        for (node_id, agent) in &mut inner.agents {
            let wanted = policy.enabled && watched.get(node_id).copied().unwrap_or(0) > 0;
            let command = if wanted {
                AgentRealtimeCommand::Start {
                    interval_millis: policy.interval_secs * 1000,
                }
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
        let watched = inner.watchers.get(&node_id).copied().unwrap_or(0) > 0;
        let active = inner.policy.enabled && watched;
        let initial = if active {
            AgentRealtimeCommand::Start {
                interval_millis: inner.policy.interval_secs * 1000,
            }
        } else {
            AgentRealtimeCommand::Stop
        };
        let (commands, receiver) = watch::channel(initial);
        inner.agents.insert(
            node_id.clone(),
            AgentConnection {
                session,
                last_sequence: None,
                active,
                commands,
            },
        );
        let interval_secs = inner.policy.interval_secs;
        if let Some(agent) = inner.agents.get(&node_id) {
            send_status(&self.events, &node_id, agent, interval_secs);
        }
        AgentRealtimeSession {
            session,
            commands: receiver,
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
        validate_sample(&sample).map_err(RecordSampleError::Invalid)?;
        let received_at_unix_millis = unix_millis();
        let mut inner = self.inner.lock().await;
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
        let broadcast_event = stored_event.clone();
        let oldest = received_at_unix_millis
            - i64::try_from(RING_RETENTION.as_millis()).expect("retention fits i64");
        let ring = inner.rings.entry(node_id.to_owned()).or_default();
        // Keep bulky worker state only on the newest ring entry; NIC history stays small. Move
        // the previous snapshot forward for an Agent delta instead of cloning it every second.
        if let Some(previous) = ring.back_mut() {
            if stored_event.sample.diagnostics_unchanged {
                stored_event.sample.reverse_health = previous.sample.reverse_health.take();
                stored_event.sample.mux = previous.sample.mux.take();
                stored_event.sample.vpngate = previous.sample.vpngate.take();
            } else {
                previous.sample.reverse_health = None;
                previous.sample.mux = None;
                previous.sample.vpngate = None;
            }
        }
        // A snapshot is authoritative, never a delta. If the first event after reconnect was an
        // invalid delta there is no previous state to preserve, so it correctly becomes empty.
        stored_event.sample.diagnostics_unchanged = false;
        ring.push_back(stored_event);
        trim_ring(ring, oldest);
        let _ = self
            .events
            .send(RealtimeBroadcast::Sample(Box::new(broadcast_event)));
        Ok(())
    }

    pub async fn subscribe(&self, nodes: impl IntoIterator<Item = String>) -> RealtimeSubscription {
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
            let count = inner.watchers.entry(node_id.clone()).or_default();
            let was_zero = *count == 0;
            *count = count.saturating_add(1);
            *inner.stop_generation.entry(node_id.clone()).or_default() += 1;
            if was_zero && policy.enabled {
                if let Some(agent) = inner.agents.get_mut(node_id) {
                    agent.commands.send_replace(AgentRealtimeCommand::Start {
                        interval_millis: policy.interval_secs * 1000,
                    });
                    agent.active = true;
                    send_status(&self.events, node_id, agent, policy.interval_secs);
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

    async fn release(&self, nodes: Vec<String>) {
        let mut scheduled = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            for node_id in nodes {
                let count = inner.watchers.entry(node_id.clone()).or_default();
                *count = count.saturating_sub(1);
                if *count == 0 {
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
            .copied()
            .unwrap_or(0)
    }

    async fn stop_if_still_idle(&self, node_id: String, generation: u64) {
        let mut inner = self.inner.lock().await;
        if inner.watchers.get(&node_id).copied().unwrap_or(0) != 0
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
    if sample.diagnostics_unchanged
        && (sample.reverse_health.is_some() || sample.mux.is_some() || sample.vpngate.is_some())
    {
        return Err("unchanged diagnostics sample carries a report");
    }
    let interface_chars = sample.interface.chars().count();
    if interface_chars == 0 || interface_chars > MAX_INTERFACE_CHARS {
        return Err("interface name has an invalid length");
    }
    if !sample
        .interface
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '@'))
    {
        return Err("interface name contains invalid characters");
    }
    if !(MIN_ELAPSED_MILLIS..=MAX_ELAPSED_MILLIS).contains(&sample.elapsed_millis) {
        return Err("sample elapsed time is outside the live range");
    }
    let now = unix_millis();
    if sample.sampled_at_unix_millis <= 0
        || sample.sampled_at_unix_millis > now.saturating_add(MAX_FUTURE_CLOCK_SKEW_MILLIS)
    {
        return Err("sample timestamp is invalid");
    }
    if let Some(vpngate) = sample.vpngate.as_ref() {
        if vpngate.boot_id.is_empty()
            || vpngate.boot_id.chars().count() > 128
            || vpngate.pools.len() > MAX_VPNGATE_POOLS
            || vpngate.backends.len() > MAX_VPNGATE_BACKENDS
            || vpngate.events.len() > MAX_VPNGATE_EVENTS
            || vpngate
                .pools
                .iter()
                .any(|pool| pool.candidate_count > VPNGATE_MAX_CANDIDATES)
        {
            return Err("VPN Gate realtime report exceeds its bounds");
        }
        if serde_json::to_vec(vpngate)
            .map(|body| body.len() > MAX_VPNGATE_REPORT_BYTES)
            .unwrap_or(true)
        {
            return Err("VPN Gate realtime report is too large");
        }
    }
    Ok(())
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
        *commands.borrow_and_update()
    }

    async fn next(commands: &mut watch::Receiver<AgentRealtimeCommand>) -> AgentRealtimeCommand {
        commands.changed().await.unwrap();
        current(commands)
    }

    fn sample(sequence: u64) -> AgentRealtimeSample {
        AgentRealtimeSample {
            diagnostics_unchanged: false,
            sequence,
            sampled_at_unix_millis: unix_millis(),
            elapsed_millis: 1000,
            interface: "eth0".to_owned(),
            rx_bytes_per_sec: 123,
            tx_bytes_per_sec: 45,
            reverse_health: None,
            mux: None,
            vpngate: None,
            has_gap: false,
        }
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
                interval_millis: 1000
            }
        );
        drop(subscription);
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
                interval_millis: 5000
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
