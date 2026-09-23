use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::{
    AppliedArtifactState, AppliedGrantsState, ConfigArtifact, DeploymentKind, DeploymentPlan,
    NodeDesiredState, PlannedAction,
};

/// Oldest Agent wire contract accepted by this control plane. Protocols are forward-compatible
/// from v20 onward, so a newer Agent remains serviceable during a staggered Console rollout.
pub const MIN_AGENT_PROTOCOL_VERSION: u32 = 20;

/// Wire contract spoken by this Agent build.
pub const AGENT_PROTOCOL_VERSION: u32 = 21;

/// Runtime log-retention bounds shared by the control-plane validator and the agent. MiB is
/// intentional: the values shown to operators map exactly to disk allocation in binary units.
pub const DEFAULT_AGENT_LOG_MAX_MIB: u32 = 100;
pub const DEFAULT_PHANTUN_LOG_MAX_MIB: u32 = 16;
pub const MIN_AGENT_LOG_MAX_MIB: u32 = 16;
pub const MAX_AGENT_LOG_MAX_MIB: u32 = 4096;

/// Live telemetry is an operational stream, not another diagnostic or accounting cadence.
/// These are deliberately the only accepted values so the control plane can bound fan-out and
/// memory use while still giving the operator a genuinely live view.
pub const DEFAULT_REALTIME_INTERVAL_SECS: u32 = 1;
pub const REALTIME_INTERVAL_OPTIONS: &[u32] = &[1, 2, 5];

/// An OpenVPN transport accepted from VPN Gate after the control plane has sanitized the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateTransport {
    Udp,
    Tcp,
}

/// Stable source names for exit-IP intelligence. These spellings are persisted in JSON and shown
/// by the Console, so adding a source is a protocol and storage change rather than a display-only
/// label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateIpProvider {
    Proxycheck,
    Ffraud,
    Iplogs,
}

/// One provider's risk score and country claim for one exact exit IP.
///
/// `score` deliberately remains provider-attributed. Although the wire representation uses a
/// common 0–100 integer for convenient validation and display, the number is meaningful only to
/// the matching provider rule and must never be compared with or aggregated into another source's
/// score. `country_code` is carried on the same source fact so disagreement stays observable
/// instead of being collapsed into a synthetic country.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpScore {
    pub provider: VpngateIpProvider,
    pub score: u8,
    #[serde(default)]
    pub country_code: String,
}

/// How provider country claims are combined for a country-scoped VPN Gate pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateCountryPolicy {
    Ignore,
    AnyMatch,
    AllMatch,
}

/// How already provider-local risk decisions are combined.
///
/// These variants combine booleans, never raw scores. That distinction is essential because the
/// three providers do not calculate risk on the same scale even when all return a value rendered
/// as 0–100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateRiskDecisionPolicy {
    AnyAvailablePass,
    AllAvailablePass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateProviderRiskRule {
    pub provider: VpngateIpProvider,
    pub maximum_score: u8,
}

/// Operational VPN Gate admission policy. It is stored independently from immutable model
/// revisions so changing intelligence thresholds immediately re-evaluates retained raw evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateAdmissionPolicy {
    pub minimum_successful_sources: u8,
    pub country_policy: VpngateCountryPolicy,
    pub risk_decision_policy: VpngateRiskDecisionPolicy,
    pub provider_rules: Vec<VpngateProviderRiskRule>,
}

impl Default for VpngateAdmissionPolicy {
    fn default() -> Self {
        Self {
            minimum_successful_sources: 1,
            country_policy: VpngateCountryPolicy::AnyMatch,
            risk_decision_policy: VpngateRiskDecisionPolicy::AllAvailablePass,
            provider_rules: vec![
                VpngateProviderRiskRule {
                    provider: VpngateIpProvider::Proxycheck,
                    maximum_score: 80,
                },
                VpngateProviderRiskRule {
                    provider: VpngateIpProvider::Ffraud,
                    maximum_score: 80,
                },
                VpngateProviderRiskRule {
                    provider: VpngateIpProvider::Iplogs,
                    maximum_score: 80,
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpngateAdmissionDecision {
    Admitted,
    Rejected,
    InsufficientEvidence,
}

pub fn validate_vpngate_admission_policy(policy: &VpngateAdmissionPolicy) -> bool {
    let providers = policy
        .provider_rules
        .iter()
        .map(|rule| rule.provider)
        .collect::<std::collections::BTreeSet<_>>();
    (1..=3).contains(&policy.minimum_successful_sources)
        && policy.provider_rules.len() == 3
        && providers
            == std::collections::BTreeSet::from([
                VpngateIpProvider::Proxycheck,
                VpngateIpProvider::Ffraud,
                VpngateIpProvider::Iplogs,
            ])
        && policy
            .provider_rules
            .iter()
            .all(|rule| rule.maximum_score <= 100)
}

pub fn evaluate_vpngate_admission(
    policy: &VpngateAdmissionPolicy,
    expected_country_code: &str,
    scores: &[VpngateIpScore],
) -> VpngateAdmissionDecision {
    if !validate_vpngate_admission_policy(policy) {
        return VpngateAdmissionDecision::InsufficientEvidence;
    }
    let providers = scores
        .iter()
        .map(|score| score.provider)
        .collect::<std::collections::BTreeSet<_>>();
    if scores.len() != providers.len()
        || scores.len() < usize::from(policy.minimum_successful_sources)
        || scores.iter().any(|score| {
            score.score > 100
                || score.country_code.len() != 2
                || !score
                    .country_code
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase())
        })
    {
        return VpngateAdmissionDecision::InsufficientEvidence;
    }

    let country_matches = |score: &VpngateIpScore| score.country_code == expected_country_code;
    let country_accepted = match policy.country_policy {
        VpngateCountryPolicy::Ignore => true,
        VpngateCountryPolicy::AnyMatch => scores.iter().any(country_matches),
        VpngateCountryPolicy::AllMatch => scores.iter().all(country_matches),
    };
    if !country_accepted {
        return VpngateAdmissionDecision::Rejected;
    }

    let provider_passes = scores
        .iter()
        .map(|score| {
            policy
                .provider_rules
                .iter()
                .find(|rule| rule.provider == score.provider)
                .is_some_and(|rule| score.score <= rule.maximum_score)
        })
        .collect::<Vec<_>>();
    let risk_accepted = match policy.risk_decision_policy {
        VpngateRiskDecisionPolicy::AnyAvailablePass => provider_passes.iter().any(|pass| *pass),
        VpngateRiskDecisionPolicy::AllAvailablePass => provider_passes.iter().all(|pass| *pass),
    };
    if risk_accepted {
        VpngateAdmissionDecision::Admitted
    } else {
        VpngateAdmissionDecision::Rejected
    }
}

/// Broad access-network class. Provider-specific labels are normalized by the Agent while the
/// accompanying ISP name and provider attribution preserve where the claim came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateNetworkType {
    Datacenter,
    Residential,
    Business,
    Mobile,
    Relay,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpNetwork {
    pub provider: VpngateIpProvider,
    pub isp: Option<String>,
    pub network_type: VpngateNetworkType,
}

/// The answer to `/agent/v1/vpngate/intelligence-assignment`.
///
/// Exit-IP intelligence is a separate work lane from VPN Gate desired state. Keeping this lease
/// out of the catalogue response prevents a slow OpenVPN batch from delaying unrelated provider
/// lookups.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpIntelligenceAssignment {
    pub exit_ip: String,
    pub lease_generation: u64,
    /// Present only for the lifetime of this authenticated lease. This value must never be logged
    /// or persisted by the Agent; the report contains provider results, not credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxycheck_api_key: Option<String>,
}

/// One VPN Gate upstream snapshot fetch leased to an operator-selected Agent.
///
/// The Agent only transports a bounded gzip snapshot. Parsing, profile sanitization,
/// deduplication and catalogue publication remain Console responsibilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateCatalogSyncAssignment {
    pub run_id: u64,
    pub lease_generation: u64,
    pub source_url: String,
}

/// A terminal fetch failure for a leased VPN Gate catalogue collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateCatalogSyncFailure {
    pub run_id: u64,
    pub lease_generation: u64,
    pub code: String,
    pub detail: String,
}

/// Successful response from one provider. Country stays per-provider on the wire because
/// admission evaluates each source independently instead of trusting a merged worker value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpIntelligenceObservation {
    pub provider: VpngateIpProvider,
    pub score: u8,
    pub country_code: String,
    pub isp: Option<String>,
    pub network_type: VpngateNetworkType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpIntelligenceFailure {
    pub provider: VpngateIpProvider,
    pub code: String,
}

/// Completion of one lease. Every source must occur exactly once across observations and
/// failures. Any successful observation is immediately usable; failed sources remain retryable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateIpIntelligenceReport {
    pub exit_ip: String,
    pub lease_generation: u64,
    pub observations: Vec<VpngateIpIntelligenceObservation>,
    pub failures: Vec<VpngateIpIntelligenceFailure>,
}

/// One public profile the Agent may measure or select for a managed country pool.
///
/// `openvpn_config` is public VPN Gate material, but it is still treated as opaque configuration:
/// neither endpoint logs nor reports echo it. The digest is the durable identity used for
/// idempotent replacement.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateCandidate {
    pub server_id: String,
    pub hostname: String,
    pub country_code: String,
    pub remote_address: String,
    pub remote_port: u16,
    pub transport: VpngateTransport,
    pub profile_sha256: String,
    pub openvpn_config: String,
    /// Last exit IP whose country and intelligence were verified by a selected Agent. The Agent only
    /// reuses the accompanying facts when a fresh tunnel exposes this exact address; an exit change
    /// returns the candidate to the pending-intelligence state instead of inheriting stale trust.
    #[serde(default)]
    pub verified_exit_ip: Option<String>,
    #[serde(default)]
    pub verified_exit_country_code: Option<String>,
    #[serde(default)]
    pub verified_ip_scores: Vec<VpngateIpScore>,
    #[serde(default)]
    pub verified_ip_networks: Vec<VpngateIpNetwork>,
}

/// One bounded catalogue-measurement batch assigned to a selected VPN Gate probe node.
///
/// Catalogue probing is deliberately separate from [`VpngateDesiredPool`]: a machine can help
/// qualify a country candidate without running a revisioned outbound, and two outbounds for the
/// same country must not make that machine download the same speed-test object twice.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateProbeAssignment {
    pub country_code: String,
    pub candidates: Vec<VpngateCandidate>,
}

/// One audited request to move an automatic pool away from its current primary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateManualSwitchCommand {
    pub request_id: u64,
    pub previous_server_id: String,
    pub cooldown_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateManualSwitchStatus {
    Applied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateManualSwitchResult {
    pub request_id: u64,
    pub status: VpngateManualSwitchStatus,
    pub previous_server_id: String,
    pub selected_server_id: Option<String>,
    pub cooldown_until_unix_secs: Option<i64>,
    pub error_detail: Option<String>,
}

/// Runtime intent for one revisioned VPN Gate country pool on one node.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateDesiredPool {
    pub outbound_id: String,
    pub country_code: String,
    pub max_connect_ms: u32,
    pub min_download_bps: u64,
    pub max_candidates: u8,
    pub runtime_slot: u16,
    pub host_address: String,
    pub peer_address: String,
    pub prefix_len: u8,
    pub socks_port: u16,
    pub candidates: Vec<VpngateCandidate>,
    #[serde(default)]
    pub manual_switch: Option<VpngateManualSwitchCommand>,
}

/// The answer to `/agent/v1/vpngate/desired`.
///
/// It is operational desired state rather than a deployment artifact: catalogue refreshes and
/// failover choices must not create model revisions or restart the node's main Xray process.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateDesiredState {
    pub topology_revision: u64,
    pub catalog_generation: u64,
    #[serde(default)]
    pub admission_policy: VpngateAdmissionPolicy,
    pub pools: Vec<VpngateDesiredPool>,
    #[serde(default)]
    pub probe_assignments: Vec<VpngateProbeAssignment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateProbeStatus {
    Succeeded,
    Failed,
}

/// A single real connection attempt made inside the Agent's isolated network namespace.
///
/// A successful transport measurement always has an exit IP, setup time and download rate.
/// Country, risk and access-network facts are optional because a newly discovered exit is queried
/// asynchronously by a selected Agent. Runtime admission still fails closed until a later desired
/// state carries enough provider-local evidence for that exact IP under the configured policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateProbeSample {
    pub server_id: String,
    pub profile_sha256: String,
    pub status: VpngateProbeStatus,
    pub exit_ip: Option<String>,
    pub exit_country_code: Option<String>,
    pub connect_ms: Option<u32>,
    pub download_bps: Option<u64>,
    #[serde(default)]
    pub ip_scores: Vec<VpngateIpScore>,
    #[serde(default)]
    pub ip_networks: Vec<VpngateIpNetwork>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub probed_at_unix_secs: i64,
}

/// Current runtime state plus newly completed samples for one pool. Reports echo both generations
/// so a delayed attempt can be retained as history without replacing a newer selected profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngatePoolReport {
    pub topology_revision: u64,
    pub catalog_generation: u64,
    pub outbound_id: String,
    pub runtime_status: String,
    pub selected_server_id: Option<String>,
    pub applied_profile_sha256: Option<String>,
    #[serde(default)]
    pub manual_switch_result: Option<VpngateManualSwitchResult>,
    pub samples: Vec<VpngateProbeSample>,
}

/// One complete post-reconcile view of every VPN Gate pool still present on a node.
///
/// The empty list is meaningful: it confirms that the Agent removed every previously managed
/// pool. Sending the complete bounded set lets the Console delete omitted replaceable state
/// without treating the absence of an individual per-pool report as proof of convergence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateReconcileReport {
    pub topology_revision: u64,
    pub catalog_generation: u64,
    pub pools: Vec<VpngatePoolReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateRuntimeState {
    Pending,
    Healthy,
    Degraded,
    FailingOver,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateBackendRole {
    Active,
    Standby,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateBackendState {
    Starting,
    Healthy,
    Unhealthy,
    Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateFailureReason {
    ProcessExited,
    SocksUnavailable,
    EgressUnreachable,
    CandidateRemoved,
    AdmissionRejected,
    StartFailed,
    NoCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpngateRuntimeEventKind {
    ActiveFailed,
    FailoverStarted,
    FailoverCompleted,
    StandbyLost,
    RefillStarted,
    RefillCompleted,
    RefillFailed,
    PoolRecovered,
}

/// Bounded, in-memory VPN Gate supervisor state carried on the existing on-demand realtime path.
/// It deliberately excludes provider profiles, host addresses and free-form process errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateRealtimePool {
    pub outbound_id: String,
    pub country_code: String,
    pub state: VpngateRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<VpngateFailureReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_slot: Option<u8>,
    pub ready_standbys: u8,
    pub candidate_count: u8,
    pub consecutive_failures: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_age_millis: Option<u64>,
    pub probes: u64,
    pub probe_failures: u64,
    pub failovers: u64,
    pub refill_attempts: u64,
    pub refill_failures: u64,
    #[serde(default)]
    pub refill_backoff_remaining_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateRealtimeBackend {
    pub outbound_id: String,
    pub slot: u8,
    pub role: VpngateBackendRole,
    pub state: VpngateBackendState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<VpngateFailureReason>,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_age_millis: Option<u64>,
    pub consecutive_failures: u8,
    pub backoff_remaining_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateRuntimeEvent {
    pub sequence: u64,
    pub at_unix_millis: i64,
    pub outbound_id: String,
    pub kind: VpngateRuntimeEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_slot: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_slot: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<VpngateFailureReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_elapsed_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateRealtimeReport {
    pub boot_id: String,
    pub sequence: u64,
    pub sampled_at_unix_millis: i64,
    pub pools: Vec<VpngateRealtimePool>,
    pub backends: Vec<VpngateRealtimeBackend>,
    pub events: Vec<VpngateRuntimeEvent>,
}

/// Raw catalogue measurements for one country, independent from any model outbound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpngateProbeReport {
    pub catalog_generation: u64,
    pub country_code: String,
    pub samples: Vec<VpngateProbeSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeTelemetryPolicy {
    pub enabled: bool,
    pub interval_secs: u32,
}

impl Default for RealtimeTelemetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: DEFAULT_REALTIME_INTERVAL_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRealtimeTelemetryPolicyRequest {
    pub enabled: bool,
    pub interval_secs: u32,
}

/// Commands travel down the long-lived Agent WebSocket. An idle connection receives `Stop` and
/// sends no samples; opening a console stream leases the node and changes that to `Start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRealtimeCommand {
    Start { interval_millis: u32 },
    Stop,
}

/// One rate computed by the Agent from two monotonically increasing NIC counters.
///
/// It is intentionally not a cumulative accounting record. The control plane holds it only in
/// memory, and reconnects, interface changes and counter regressions are represented by
/// `has_gap` rather than guessed across.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRealtimeSample {
    /// `true` means the three diagnostic reports below were deliberately omitted because they
    /// have not reached their lower-frequency refresh deadline. It is distinct from all three
    /// fields being absent in a complete sample, which authoritatively clears the previous
    /// diagnostic state.
    ///
    /// The default keeps protocol v20/v21 Agents compatible: their samples are complete, just as
    /// they were before this bandwidth hint existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub diagnostics_unchanged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse_health: Option<ReverseHealthReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<MuxReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vpngate: Option<VpngateRealtimeReport>,
    pub sequence: u64,
    pub sampled_at_unix_millis: i64,
    pub elapsed_millis: u32,
    pub interface: String,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub has_gap: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeSampleEvent {
    pub node_id: String,
    /// Server receipt time is the display timeline shared by the fleet. The Agent timestamp above
    /// remains available for diagnosing a bad node clock, but it never orders different nodes.
    pub received_at_unix_millis: i64,
    pub sample: AgentRealtimeSample,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeNodeSnapshot {
    pub node_id: String,
    pub connected: bool,
    pub active: bool,
    pub interval_secs: u32,
    pub samples: Vec<RealtimeSampleEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDeploymentRequest {
    pub revision_id: u64,
    pub idempotency_key: String,
    pub actor: Option<String>,
    pub note: Option<String>,
    // The default is a configuration deployment, which is what the release page's button
    // creates: it covers every machine and every artifact. Grants deployments are created only
    // by list-only paths such as quota enforcement.
    #[serde(default)]
    pub kind: DeploymentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDeploymentResult {
    pub deployment_id: i64,
    pub status: String,
    pub reused: bool,
    pub plan: DeploymentPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRollbackRequest {
    pub target_deployment_id: i64,
    pub idempotency_key: String,
    pub actor: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDesiredDeployment {
    pub deployment_id: i64,
    pub node_id: String,
    /// Positive for work claimed from an isolation obligation. The report must echo it so an
    /// older in-flight result cannot settle a newer desired generation for the same node.
    pub claim_generation: u64,
    pub wave: u32,
    pub actions: Vec<PlannedAction>,
    /// Immutable ownership map for the Xray counters created by this work order.  It advances
    /// only after the agent has converged the target, so a reading queued before a permission or
    /// topology change is never interpreted through the model that happened to be current when
    /// it was replayed.
    pub usage_generation_id: Option<i64>,
    pub desired: NodeDesiredState,
    /// Where to fetch the phantun binaries.
    ///
    /// In the desired state rather than a node's environment, because this path has to repair
    /// machines that SSH cannot reach. The pull model exists so the control plane can drive a
    /// machine with no inbound access, and requiring SSH to install a mandatory binary removes
    /// that property. Placed in the desired state, a machine instructed to run phantun receives
    /// the download location at the same time.
    ///
    /// Empty means the control plane has no distribution source configured and the agent can use
    /// only what the machine already holds. Absent, the agent reports an explicit error.
    pub phantun_binary: Option<PhantunBinaries>,
}

/// The answer to `/agent/v1/desired`.
///
/// Replaces the bare `NodeDesiredDeployment` / 204 pair. The certificate check joins the
/// convergence decision as its own dimension, so three states exist where there were two:
/// a node with nothing to converge and a current certificate, a node owed a deployment (whose
/// response carries the certificate check's result), and a node owed only a certificate.
/// The last one is the converged machine whose cert went stale or missing — it never gets a
/// deployment-shaped answer because inventing a `deployment_id` for it would write a fake row
/// into the deployment ledger the agent reports against.
///
/// `NodeDesiredDeployment` itself stays certificate-free: the certificate is attached when the
/// response is built, and never stored in the snapshot the deployment reverts to. A rollback
/// therefore cannot resurrect an old certificate — the field derives from the currently
/// serving row, and the rollback machinery never sees it.
// This enum lives on the handler stack for the span of one response; it is never stored in bulk.
// `Converged` is never serialized at all (it is the 204), so its size disparity with the
// `Deployment` variant costs nothing and boxing the payload would only add an indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum DesiredStateResponse {
    /// Nothing to converge, certificate included. Sent as HTTP 204 rather than serialized.
    Converged,
    /// A deployment is owed. `certificates` contains every drifted trust track, so the
    /// deploy path always carries the dependency confirmation with it.
    Deployment {
        deployment: NodeDesiredDeployment,
        certificates: Vec<NodeCertificateMaterial>,
    },
    /// No deployment is owed but one or both independent certificate tracks are missing or stale.
    Certificates(Vec<NodeCertificateMaterial>),
}

/// What a node reports about the certificate it actually holds.
///
/// # Why three states rather than an `Option`
///
/// `None` would conflate an unmanaged certificate track with a managed track whose file is
/// missing. The two require opposite responses, so the state is explicit.
///
/// # Why only a digest
///
/// The control plane holds the certificate it issued, so it can compute the same digest and
/// compare. Reporting more, such as expiry, names or issuer, would require an X.509 parser on the
/// agent for values the control plane already has, and the agent is a static binary installed on
/// machines it does not own. A digest of the bytes it holds is both cheap and sufficient.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum CertificateObservation {
    /// This node does not manage certificate material.
    #[default]
    Unmanaged,
    /// Both independent certificate tracks were inspected. Public CA uses one atomically replaced
    /// file; self-signed keeps two physical slots so old and new pinned identities can overlap.
    /// A missing digest means that file is absent or unreadable.
    Managed {
        public_ca_sha256: Option<String>,
        self_signed: CertificatePairObservation,
    },
}

/// What is present in the self-signed track's two fixed runtime slots.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CertificatePairObservation {
    pub slot_a_sha256: Option<String>,
    pub slot_b_sha256: Option<String>,
}

/// The trust track a certificate belongs to. It is frozen on the issued certificate and is never
/// inferred from the control plane's current global signing setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CertificateTrack {
    PublicCa,
    SelfSigned,
}

/// A certificate and its key, on their way to one node.
///
/// # Attached to the desired response, never stored in the snapshot
///
/// The certificate travels in `DesiredStateResponse`, attached when the response is built, and
/// never in `NodeDesiredState` itself. That placement is what makes it immune to the two
/// operations a deployment expects: a rollback cannot resurrect an old certificate, because the
/// field derives from the currently serving row and not from the snapshot; and the certificate
/// is not a deployment of its own, so it can neither conflict with nor be gated, halted or
/// cancelled by unrelated deployments. The consistency check — the node's reported sha against
/// the serving certificate — runs on every desired poll and hands the certificate out the
/// moment it fails, whatever the deployment machinery is doing.
///
/// # Why not the separate endpoint it used to have
///
/// It had one (`/agent/v1/certificate`, polled by the agent on its own ten-minute cycle) after
/// an earlier attempt to put it inside `NodeDesiredDeployment` failed: that version only
/// reached a node together with a deployment, so a converged machine — answered 204 — kept
/// whatever it held, and a renewal waited for the next release. Two releases can be months
/// apart; a certificate is valid for 90 days. The separate poll fixed the deadline but left
/// two channels able to disagree: a machine could hold a deployment whose config references
/// the certificate while its certificate channel never ran. One channel, with the certificate
/// checked on every desired poll, removes both failure modes.
///
/// The key is transmitted because the control plane issues it. The reasoning for that trade-off
/// and the obligations it carries are at the top of `brocade-store/src/cert.rs`. The requirement
/// here is that this struct is never logged, included in an error, or returned by a console
/// route.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "track", rename_all = "kebab-case")]
pub enum NodeCertificateMaterial {
    /// A publicly trusted renewal keeps the same SNI and needs no leaf pin overlap. One combined
    /// PEM is atomically replaced and hot-reloaded by Xray.
    PublicCa {
        certificate: NodeCertificateSlotMaterial,
    },
    /// Self-signed generations use different SNI and leaf pins. Both identities must remain
    /// available while saved clients move from one generation to the next.
    SelfSigned {
        slots: [NodeCertificateSlotMaterial; 2],
    },
}

/// One complete certificate identity assigned to a fixed physical slot.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCertificateSlotMaterial {
    pub certificate_id: String,
    pub names: Vec<String>,
    /// The full chain, leaf first, which is what a TLS server presents. Not the leaf alone: a
    /// missing intermediate works in a browser with a cached intermediate and fails everywhere
    /// else.
    pub cert_pem: String,
    pub key_pem: String,
}

/// Hand-written so a `{:?}` in a log or an error cannot print a private key. Deriving `Debug`
/// would make printing the key the default behaviour, and this type exists so its contents never
/// reach a readable output.
impl std::fmt::Debug for NodeCertificateMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PublicCa { certificate } => f
                .debug_struct("NodeCertificateMaterial::PublicCa")
                .field("certificate", certificate)
                .finish(),
            Self::SelfSigned { slots } => f
                .debug_struct("NodeCertificateMaterial::SelfSigned")
                .field("slots", slots)
                .finish(),
        }
    }
}

impl std::fmt::Debug for NodeCertificateSlotMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCertificateSlotMaterial")
            .field("certificate_id", &self.certificate_id)
            .field("names", &self.names)
            .field("cert_pem", &"<redacted>")
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

/// The two executables are supplied separately: a machine that is only a client needs no server
/// binary, and the converse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhantunBinaries {
    pub server: BinarySource,
    pub client: BinarySource,
}

/// A verifiable binary download source. The sha256 is mandatory: without it this is a path that
/// downloads a file from the internet and runs it as root.
///
/// There are two callers, and the sha256 provides different guarantees in each. The field looks
/// equally strong in both places, so the difference is stated here:
///
/// - **phantun** (in the desired state). The bytes come from the location the operator
///   configured, commonly a GitHub release, while the sha comes from the control plane. The two
///   origins differ, so the sha is a real check on the download.
/// - **the agent itself** (`/agent/v1/agent-release`). Bytes and sha both come from this control
///   plane over the same connection, so anyone able to change one can change the other. Here the
///   sha detects a truncated download and a misconfigured URL; it is not a defence against
///   control of the control plane or the connection. That is covered by https to a
///   publicly-signed certificate plus staged rollout, where an operator releases to one node,
///   verifies it, and only then releases to the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinarySource {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetConvergenceReport {
    pub deployment_id: i64,
    pub node_id: String,
    pub claim_generation: u64,
    pub result: TargetApplyResult,
    pub observed_before: ReportedNodeState,
    pub observed_after: ReportedNodeState,
    pub error: Option<String>,
    /// Agent-clock instant immediately before applying the counter namespace change. It is the
    /// lower boundary for a newly authorized label whose first Xray counter starts at zero.
    pub usage_activated_at_unix_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteIpReport {
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

/// One direct request from an Agent to the operator-configured CGI trace endpoint. IPv4 and IPv6
/// are separate observations: lack of one family must never erase or delay a successful sample
/// from the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePublicIpObservation {
    pub observed_at_unix_secs: i64,
    pub family: PublicIpFamily,
    pub ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicIpFamily {
    V4,
    V6,
}

impl PublicIpFamily {
    pub fn number(self) -> i16 {
        match self {
            Self::V4 => 4,
            Self::V6 => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetApplyResult {
    Applied,
    FailedRecovered,
    FailedDirty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedNodeState {
    pub phantun: AppliedArtifactState,
    pub wireguard: AppliedArtifactState,
    pub xray: AppliedArtifactState,
    pub hy2_port_hop: AppliedArtifactState,
    pub grants: AppliedGrantsState,
}

impl ReportedNodeState {
    /// The machine's reported state for one artifact. Paired with `NodeDesiredState::artifacts`,
    /// it lets the judging stage iterate over all four rather than listing them individually.
    pub fn artifact(&self, which: ConfigArtifact) -> &AppliedArtifactState {
        match which {
            ConfigArtifact::Phantun => &self.phantun,
            ConfigArtifact::Hy2PortHop => &self.hy2_port_hop,
            ConfigArtifact::WireGuard => &self.wireguard,
            ConfigArtifact::Xray => &self.xray,
        }
    }
}

/// Each .dat file's actual state. A missing file is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeodataObservation {
    pub geoip: Option<GeodataFileState>,
    pub geosite: Option<GeodataFileState>,
    /// Which directory it was read from. xray searches for assets in the order
    /// `XRAY_LOCATION_ASSET` → the executable's directory → `/usr/local/share/xray/` →
    /// `/usr/share/xray/` → `/opt/share/xray/`, taking the first that exists, and the agent
    /// reproduces the same order. It is reported because reading the wrong directory and the file
    /// genuinely not updating are indistinguishable in every other field.
    pub asset_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeodataFileState {
    pub sha256: String,
    pub bytes: u64,
    /// The mtime in unix seconds. Whether an update landed rests on it: a sha can say whether the
    /// nodes share one copy and cannot say when that copy is from. xray replaces files with
    /// `os.Rename`, so the mtime follows the new file.
    pub modified_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTargetResult {
    pub deployment_id: i64,
    pub node_id: String,
    pub target_status: String,
    pub deployment_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentCommandResult {
    pub deployment_id: i64,
    pub status: String,
    pub active: Option<bool>,
    pub sync_deployment_id: Option<i64>,
    pub rollback_deployment_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentList {
    pub deployments: Vec<DeploymentListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentListItem {
    pub id: i64,
    pub revision_id: u64,
    pub status: String,
    pub activation_status: String,
    pub settlement_status: String,
    pub activated_at: Option<String>,
    pub active: Option<bool>,
    pub actor: Option<String>,
    // Configuration or grants. The two have to be distinguishable in the list, because they
    // differ in cost by an order of magnitude: restarting xray on three machines and adding one
    // account to a list must not appear the same.
    pub kind: DeploymentKind,
    pub note: Option<String>,
    // Which version it changed from. Fixed at creation and never recomputed, because rollbacks
    // make a retrospective calculation incorrect. None means this kind has never been deployed
    // successfully.
    pub base_revision_id: Option<u64>,
    pub rollback_of_deployment_id: Option<i64>,
    pub sync_of_deployment_id: Option<i64>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub total_targets: u64,
    pub changed_targets: u64,
    pub skipped_targets: u64,
    pub failed_targets: u64,
    pub debt_targets: u64,
    pub disruptive_targets: u64,
    pub max_wave: u32,
    // Waiting on an operator rather than on machines. A destructive wave requires a
    // confirmation before it continues, and the two kinds of pause are otherwise identical in
    // the list: the deployment shows as pushing and the header reports machines pending, with
    // nothing indicating that the operator is the blocking party.
    pub awaiting_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentWaveConfirmationRequest {
    pub actor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentWaveConfirmationResult {
    pub deployment_id: i64,
    pub wave: u32,
    pub confirmed: bool,
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentDetail {
    pub id: i64,
    pub revision_id: u64,
    pub status: String,
    pub activation_status: String,
    pub settlement_status: String,
    pub activated_at: Option<String>,
    pub debt_targets: u64,
    pub active: Option<bool>,
    pub actor: Option<String>,
    pub note: Option<String>,
    // The detail page uses it as the baseline for artifact diffs. See the field of the same name
    // on DeploymentListItem.
    pub base_revision_id: Option<u64>,
    pub warnings: Value,
    pub created_at: String,
    pub started_at: Option<String>,
    pub halted_at: Option<String>,
    pub finished_at: Option<String>,
    pub rollback_of_deployment_id: Option<i64>,
    pub sync_of_deployment_id: Option<i64>,
    pub divergence_cleared_at: Option<String>,
    pub targets: Vec<DeploymentTargetDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentTargetDetail {
    pub node_id: String,
    pub status: String,
    pub error: Option<String>,
    pub wave: u32,
    pub disruptive: bool,
    pub desired_structure: Value,
    pub observed_before: Option<Value>,
    pub observed_after: Option<Value>,
    pub verdict: Option<Value>,
    pub dispatched_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolateDeploymentTargetRequest {
    pub expected_target_status: String,
    #[serde(default)]
    pub acknowledge_uncertain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolateNodeRequest {
    #[serde(default)]
    pub acknowledge_uncertain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIsolationCommandResult {
    pub node_id: String,
    pub isolated: bool,
    pub affected_deployments: Vec<i64>,
    pub debt_count: u64,
    pub serving_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentObservationRequest {
    pub deployment_id: i64,
    pub claim_generation: u64,
    pub result: TargetApplyResult,
    pub observed_before: ReportedNodeState,
    pub observed_after: ReportedNodeState,
    pub error: Option<String>,
    pub route: RouteIpReport,
    /// See [`TargetConvergenceReport::usage_activated_at_unix_secs`]. `None` means this work did
    /// not change the authorized Xray counter namespace.
    pub usage_activated_at_unix_secs: Option<i64>,
}

/// The control plane telling the agent which endpoints to probe.
///
/// It cannot be derived from the local `wireguard.conf`. The `Endpoint` there is the address wg
/// dials, and for a peer behind phantun's fake TCP it is `127.0.0.1:<local port>` by design.
/// Probing that address measures local loopback, reads 65536, and the bisection reaches the
/// ceiling and reports a plausible 1500.
///
/// The real endpoint and encapsulation are known only to the control plane (`Node.public_ipv4`,
/// `Node.public_ipv4_nat`, and `Node.wireguard.transport`), so the control plane sends them. It
/// uses its own endpoint rather than being folded into desired state, because desired returns 204
/// when there is no work while probing has to keep running when nothing is being released.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTargetList {
    pub targets: Vec<ProbeTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTarget {
    pub peer_node_id: String,
    /// The peer's real underlay endpoint (`Node.public_ipv4`).
    ///
    /// A peer with `public_ipv4_nat=true` is absent from this list, because it cannot be probed.
    /// That link is probed from the other end instead, and the measurement holds for both
    /// directions of one path. The compiler guarantees every link has at least one reachable
    /// end, reporting `link.no-endpoint` otherwise, so no link is unprobeable from both sides.
    pub host: String,
    /// How the peer's wg entrance is encapsulated, which decides how much to subtract (see
    /// `LinkProbe::suggested_wg_mtu`).
    pub transport: ProbeTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeTransport {
    /// wg's direct UDP: an 8-byte UDP header.
    Udp,
    /// phantun's fake TCP: the UDP header becomes a 20-byte TCP header, 12 more than direct.
    FakeTcp,
}

/// One machine's path-MTU probe results toward each of its wg peers.
///
/// The agent reports rather than the control plane measuring, because the control plane has no
/// path to the underlay and the link between two machines is observable only from its two ends.
/// This has the same structure as usage reporting: machines report measurements and the control
/// plane records them.
///
/// It uses its own endpoint rather than the observation endpoint because an observation has to be
/// attached to a deployment (`AgentObservationRequest.deployment_id`), whereas probing has to run
/// when nothing is being released: an MTU usually changes because the upstream link changed
/// rather than because a config was edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProbeRequest {
    pub probed_at_unix_secs: i64,
    pub links: Vec<LinkProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProbe {
    /// The peer's node id, from wireguard.conf's `# <node name>` comment.
    pub peer_node_id: String,
    /// The probe targets the underlay endpoint, which is the host part of the peer's `Endpoint`,
    /// rather than the overlay address. The MTU inside the tunnel is determined by the tunnel's
    /// parameters rather than by the path, so probing the overlay measures the local
    /// configuration.
    pub endpoint_host: String,
    pub status: LinkProbeStatus,
    /// The largest IP packet that gets through unfragmented (IP header included).
    pub path_mtu: Option<u16>,
    /// The suggested wg MTU: `path_mtu` minus wg's encapsulation overhead.
    ///
    /// The overhead is UDP 8 plus WireGuard 32 plus the outer IP header: 20 for an IPv4-only
    /// endpoint, giving 60 in total, or IPv6's 40 where the endpoint also has an AAAA record,
    /// giving 80. The probe travels over IPv4, but wg resolving the name itself may select v6,
    /// and counting 60 in that case suggests a value 20 bytes too large. A value that is too
    /// small only reduces throughput; a value that is too large causes large packets to be
    /// dropped with no report.
    pub suggested_wg_mtu: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkProbeStatus {
    Ok,
    /// An ordinary ping is not answered. No MTU can be measured, and this result must not lower
    /// the global suggestion.
    Unreachable,
    /// An ordinary ping is answered while the smallest packet with DF set does not get through,
    /// which usually means ICMP is filtered.
    ///
    /// This has to stay distinct from `Ok`: recording an undeterminable result as a very small
    /// number would lead an operator to apply it and lower the network-wide MTU to 1000 on
    /// working links.
    Blocked,
    /// No ICMP socket can be opened on this machine, so nothing can be measured.
    ///
    /// It needs either `net.ipv4.ping_group_range` to allow it (`SOCK_DGRAM`) or CAP_NET_RAW
    /// (`SOCK_RAW`), and without either there is no measuring.
    ///
    /// This is a fact about ourselves rather than about the link and must stay apart from
    /// `Blocked`: conflated, the whole fleet reports as "ICMP filtered" and the operator goes
    /// hunting a filter that does not exist.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkProbeResult {
    pub node_id: String,
    pub accepted_links: u64,
    /// Reported peers absent from the current model (freshly decommissioned, say); these rows are
    /// dropped.
    pub unknown_peers: u64,
}

/// A relay hop's liveness.
///
/// The test is the delta of the outbound's counters rather than the observatory's own verdict.
/// The probe traffic travels that outbound, since the artifacts' `burstObservatory` sends every
/// 10 seconds, so the hop's state appears directly in
/// `outbound>>>out:{app}/{chain}>{to}>>>traffic>>>downlink`. This requires no new read channel:
/// `xray api` has no observatory verb, `GetOutboundStatus` is gRPC only, and the agent writes its
/// own HTTP.
///
/// It also removes the ambiguity between idle and dead: the observatory produces traffic for as
/// long as the hop is up, so a zero delta means the hop is down and no separate active probe is
/// needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHealthRequest {
    pub checked_at_unix_secs: i64,
    /// The elapsed time between the two readings. The control plane uses it to decide how far a
    /// zero delta can be relied on: too short an interval may not yet include a probe.
    pub window_secs: u64,
    pub hops: Vec<LinkHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHealth {
    pub chain_id: String,
    pub peer_node_id: String,
    pub alive: bool,
    /// How many bytes arrived over this hop in this window. `alive` is this value being above
    /// zero; the raw number is included so a reading taken at a probe-interval boundary can be
    /// distinguished from a hop carrying no traffic at all.
    pub downlink_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHealthResult {
    pub node_id: String,
    pub accepted_hops: u64,
    /// Reported hops absent from the current model (a freshly deleted chain, say).
    pub unknown_hops: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReportRequest {
    /// Stable across process restarts and regenerated only when the agent state directory is
    /// replaced. Together with `sequence` this is the idempotency key of a report.
    pub agent_instance_id: String,
    /// Persisted before sampling. Gaps are allowed; reuse and reversal are not.
    pub sequence: u64,
    /// The frozen ownership map active when the counters were read.
    pub usage_generation_id: i64,
    pub read_at_unix_secs: i64,
    pub xray_started_at_unix_secs: i64,
    /// Boot time plus the serving process's exact start ticks. Unlike the rounded unix second,
    /// this changes for two Xray processes started within the same second.
    pub xray_epoch: String,
    pub route: Option<RouteIpReport>,
    pub counters: Vec<UsageCounter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCounter {
    pub label: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReportResult {
    pub node_id: String,
    pub agent_instance_id: String,
    pub sequence: u64,
    pub usage_generation_id: i64,
    /// True when the control plane returned the durable result of an already committed report.
    pub duplicate: bool,
    pub accepted_readings: u64,
    pub inserted_samples: u64,
    /// The label has no corresponding owner in the report's frozen usage generation.
    pub skipped_counters: u64,
    /// The label belongs to another reporter, is out of order, or regressed inside one exact Xray
    /// epoch. Persistently non-zero requires investigation and is never billed.
    pub rejected_counters: u64,
    pub gap_samples: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSampleList {
    pub samples: Vec<UsageSample>,
    /// Link-hop usage. Kept separate from `samples` rather than as two row kinds in one table:
    /// combined into one list, summing by tenant would add link overhead to user bills, and
    /// nothing would report the error, because the bytes come from the same batch while the
    /// resources differ.
    pub chain_samples: Vec<UsageChainSample>,
}

/// One hop's usage on a relay chain. It has no user, because that hop's credential is
/// `{chain}@{node}` and carries no per-user dimension. This is the operator's bandwidth cost
/// rather than a billable amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageChainSample {
    pub id: i64,
    pub sampled_at: String,
    pub window_start: String,
    pub window_end: String,
    pub node_id: String,
    pub tenant_id: String,
    pub app_id: String,
    pub chain_id: String,
    pub hop_label: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub has_gap: bool,
    pub revision_id: Option<u64>,
    pub deployment_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSample {
    pub id: i64,
    pub sampled_at: String,
    pub window_start: String,
    pub window_end: String,
    pub node_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub ingress_id: String,
    pub grant_label: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub has_gap: bool,
    pub revision_id: Option<u64>,
    pub deployment_id: Option<i64>,
}

/// A calendar-month rollup, one row per (user × view). A view is an app (shown in the console as
/// the app's label). Ingresses hang off apps and samples group by app_id through a JOIN on
/// ingresses, so a view's total is that user's traffic across all its access points for the month.
/// Months are the calendar months of the control plane's local zone (+08). The UI may select the
/// current or previous month by offset, but never supplies timestamp boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMonthlyViewRow {
    pub tenant_id: String,
    pub user_id: String,
    /// The view's (app's) id. The UI matches it against the snapshot's apps for a label.
    pub app_id: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    /// At least one sample this month has has_gap set: an interval went uncollected, so the
    /// total is an underestimate
    pub has_gap: bool,
}

/// One local-calendar day's user traffic inside a [`UsageMonthlySummary`]. The same sample set is
/// used for the daily bars and the per-view totals so the two presentations keep one accounting
/// boundary. Days without traffic are omitted; the UI can fill the short, known calendar range
/// without transferring placeholder rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDailyRow {
    /// A +08 local-calendar date in `YYYY-MM-DD` form.
    pub day: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    pub has_gap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMonthlySummary {
    /// A +08 wall-clock string of the form "2026-08-01 00:00:00" that does not vary with the
    /// database session's timezone. Do not populate it from a timestamptz's text form: under a
    /// session that is not +08 the month is rendered incorrectly.
    pub month_start: String,
    pub month_end: String,
    pub views: Vec<UsageMonthlyViewRow>,
    pub days: Vec<UsageDailyRow>,
}

/// One machine's total for one reporting window. The buckets are the agent's USAGE windows
/// themselves, 30 seconds each, with no further bucketing: the boundaries are the agent's, and
/// re-bucketing would split one window across two buckets.
///
/// User traffic and relay traffic are reported as two groups. An ingress machine's bytes belong
/// to a user (`usage_samples`), and a relay machine's belong to a link hop with no user dimension
/// (`usage_chain_samples`). Their sum is what the machine carried, and their separation is what
/// keeps billing correct: the same bytes are counted once at the ingress and once at the relay,
/// and only the ingress count may be billed to a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageNodeBucket {
    /// The window's right edge, as timestamptz text (carrying an offset)
    pub window_end: String,
    pub user_uplink_bytes: u64,
    pub user_downlink_bytes: u64,
    pub relay_uplink_bytes: u64,
    pub relay_downlink_bytes: u64,
}

/// One machine's usage series plus its month total. The list page needs recent throughput and the
/// month's volume, both aggregated per machine. Fetching grant-level detail into the browser and
/// summing there would produce thousands of rows for one machine with a few dozen users, and
/// `/usage/samples` caps its limit at 500.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageNodeSeries {
    pub node_id: String,
    /// In chronological order (oldest to newest), containing only windows with samples. A machine
    /// that never ran yields an empty array.
    pub buckets: Vec<UsageNodeBucket>,
    pub month_user_uplink_bytes: u64,
    pub month_user_downlink_bytes: u64,
    pub month_relay_uplink_bytes: u64,
    pub month_relay_downlink_bytes: u64,
    /// At least one sample this month has has_gap set: an interval went uncollected, so the
    /// total is an underestimate
    pub month_has_gap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageNodeSeriesList {
    /// The series window's left edge (timestamptz text), used by the UI to draw the axis
    pub since: String,
    /// The +08 wall-clock month boundaries, on the same convention as `UsageMonthlySummary`
    pub month_start: String,
    pub nodes: Vec<UsageNodeSeries>,
}

// ══════════════════════════════════════════════════════════════════════
// End-to-end probing
//
// Distinct from the link probing above; the two are not interchangeable:
// - `LinkProbe` measures one underlay segment, the MTU of the line between two machines
// - this measures a chain's entire data plane, from the port a user dials, through every relay,
//   to the exit
//
// Every hop being up does not imply the chain works. `link_health` tests whether a node's
// outbound toward its next hop is still accumulating bytes. A mistyped REALITY parameter, a
// missing routing rule, or a blocked exit stops none of the hop counters, while users can no
// longer connect.
// ══════════════════════════════════════════════════════════════════════

/// Which chains this machine probes as their head.
///
/// It takes its own endpoint for the same reason as `ProbeTargetList`: probing must keep running
/// when nothing is being released, while desired returns 204 when there is no work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeTargetList {
    pub targets: Vec<E2eProbeTarget>,
    /// Where to send the request once connected. Sent by the control plane rather than compiled
    /// into the agent: changing the endpoint is an operational decision (the old one got blocked,
    /// or a closer one is wanted) and should not wait for an agent upgrade.
    pub endpoint_url: String,
    /// No first byte within this time counts as down.
    pub timeout_secs: u64,
    /// How long until the next round. Sent by the control plane rather than compiled into the
    /// agent: a round's cost depends on the fleet, since on a machine with many chains once a
    /// minute and once every ten minutes differ substantially, and changing it should not require
    /// an agent upgrade.
    pub interval_secs: u64,
}

impl E2eProbeTargetList {
    /// The next round's interval, clamped between 15 seconds and one day. The control plane has
    /// the same CHECK; clamping again here keeps a malformed value from leaving the agent
    /// polling continuously or never polling.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs.clamp(15, 86_400))
    }
}

/// Everything needed to probe one chain. Its fields come from the same source as a user's
/// subscription (`brocade_core::physical::probe`); one differing parameter would measure a
/// different path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct E2eProbeTarget {
    pub app_id: Option<String>,
    pub chain_id: String,
    pub chain_name: String,
    pub ingress_id: String,
    /// Which local address to dial: loopback, or the address the ingress explicitly bound. Not a
    /// public IP, because dialing over the public internet would also measure this machine's
    /// inbound routing, which is not a property of the chain.
    pub dial_host: String,
    pub port: u16,
    /// The probe credential. Derived from the ingress's private key (`model::probe_uuid`), and not
    /// stored by the control plane.
    pub uuid: String,
    /// Exactly one security/protocol shape. A tagged union prevents contradictory combinations
    /// such as a TLS target carrying ignored REALITY credentials.
    pub security: E2eProbeSecurity,
    /// Present when the ingress is carried inside HTTP, in which case the probe's client has to
    /// be as well. Absent, the probe dials TCP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp: Option<E2eProbeXhttp>,
    /// Which machines this chain may exit from. The IP the endpoint saw must fall in this set to
    /// agree.
    ///
    /// Empty means the check cannot be made, because an exit is behind NAT, has no public address,
    /// or may traverse an external tunnel. That outcome is reported explicitly rather than
    /// counted as a verified address match.
    pub expected_exit_ips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum E2eProbeSecurity {
    VlessEncryption {
        encryption: String,
    },
    Reality(E2eProbeReality),
    Tls(E2eProbeTls),
    AnyTls {
        settings: E2eProbeAnyTls,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reality: Option<E2eProbeReality>,
    },
    Hysteria2(E2eProbeHysteria2),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeAnyTls {
    /// The name on the machine's certificate. The client verifies it as ordinary TLS.
    pub server_name: String,
    /// Comma-separated DER SHA-256 values for every retained leaf in the certificate group.
    /// Present while the group still trusts any self-signed leaf; Xray verifies one of these
    /// instead of disabling TLS verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_peer_cert_sha256: Option<String>,
    /// Optional client session-pool settings. Omitted values use the pinned Xray defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_session_check_interval_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_session_timeout_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_idle_session: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeHysteria2 {
    pub server_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_peer_cert_sha256: Option<String>,
    pub congestion: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<String>,
    /// Absent is Xray's standard profile, matching artifact omission semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbr_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_stream_receive_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_stream_receive_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_connection_receive_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connection_receive_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_idle_timeout_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive_period_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disable_path_mtu_discovery: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salamander_password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeXhttp {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xmux: Option<E2eProbeXhttpXmux>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_padding_bytes: Option<E2eProbeXhttpRange>,
    /// The literal value xray expects, or `None` to let both ends resolve it themselves.
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeXhttpXmux {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u16>,
    pub h_max_request_times: E2eProbeXhttpRange,
    pub h_max_reusable_secs: E2eProbeXhttpRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h_keep_alive_period_secs: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeXhttpRange {
    pub from: u32,
    pub to: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeTls {
    /// The name on the machine's certificate. The client verifies it, so unlike REALITY's
    /// impersonated name, an incorrect value fails the probe at the client end.
    pub server_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_peer_cert_sha256: Option<String>,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeReality {
    pub public_key: String,
    pub short_id: String,
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeRequest {
    pub probed_at_unix_secs: i64,
    pub chains: Vec<E2eProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbe {
    pub app_id: Option<String>,
    pub chain_id: String,
    pub status: E2eProbeStatus,
    /// Time to first byte. Present only where the probe connected: a failure's timing equals the
    /// timeout value and carries no information about the chain's speed, so reporting it would
    /// invite comparison against healthy figures.
    pub ttfb_ms: Option<u32>,
    /// The caller's IP as the endpoint saw it.
    pub exit_ip: Option<String>,
    /// The location the endpoint reported (cloudflare trace's `loc=`). Where the IP cannot be
    /// checked, it still identifies the country the traffic left from.
    pub exit_loc: Option<String>,
    /// The exit check's verdict.
    pub exit_verdict: E2eExitVerdict,
    /// On failure, a human-readable sentence identifying where it failed. Not intended to be
    /// parsed.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum E2eProbeStatus {
    /// A first byte arrived.
    Ok,
    /// The local ingress could not be reached. A failed REALITY handshake is included here,
    /// because the probe cannot distinguish the two, and the next step for both is to inspect
    /// the xray on the chain's head.
    HandshakeFailed,
    /// The handshake succeeded and the request did not complete. The failure is somewhere on the
    /// chain, or the exit cannot reach the internet.
    ChainBroken,
    /// Timed out. Kept distinct from `ChainBroken`, because a timeout may indicate latency rather
    /// than a failure, and the two require different responses.
    Timeout,
    /// This machine cannot probe: the probe process does not start, or there is no xray binary.
    ///
    /// A property of the machine rather than of the chain, on the same reasoning as
    /// `LinkProbeStatus::Unsupported`. Reported as `ChainBroken`, it would send the operator to
    /// investigate a working chain.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum E2eExitVerdict {
    /// The IP the endpoint saw is among the expected set.
    Match,
    /// The probe connected, and the exit IP is not one this chain should use.
    ///
    /// This is neither a success nor a failure. It means the traffic did not travel the whole
    /// chain, and it occurs with a valid rule table, every hop up, and no compilation warning, so
    /// static validation does not detect it. This is the primary reason end-to-end probing
    /// exists.
    Mismatch,
    /// The expected set is empty, so the check cannot run: an exit is behind NAT — on either
    /// family, which disqualifies the machine's other family too — has no public address, or may
    /// traverse an external tunnel. It is its own outcome so that an unrunnable check is not
    /// reported as a verified one.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeResult {
    pub node_id: String,
    pub accepted_chains: u64,
    /// Reported chains absent from the current model (freshly deleted or reassigned); these rows
    /// are dropped.
    pub unknown_chains: u64,
}

// ─────────────── Node runtime reconcile ───────────────
//
// This family covers state the control plane cannot observe through artifacts. None of these
// values appear in any artifact: artifact reconciliation compares what the control plane sent,
// and these runtime observations were never sent by it.
//
// They use their own low-frequency endpoint rather than being folded into observations. An
// observation carries a `deployment_id` and exists only during a release, whereas these values
// matter most when nothing is being released: a machine that has not deployed for a month is the
// one most likely to have drifted undetected.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRuntimeReport {
    /// When this snapshot finished being collected on the node. The source timestamp prevents a
    /// delayed snapshot from overwriting runtime state observed later.
    pub observed_at_unix_secs: i64,
    pub versions: NodeVersions,
    /// Which certificate this machine is actually holding.
    ///
    /// Here for the same reason as the geodata field below: it is an observation unrelated to a
    /// release. Reported through `ReportedNodeState` instead, a machine that does not deploy for
    /// a month would leave its certificate state unknown for a month, and the certificate is the
    /// only value here with an expiry.
    pub certificate: CertificateObservation,
    /// The on-disk state of the rule databases (`geoip.dat` / `geosite.dat`).
    ///
    /// Here rather than in `ReportedNodeState`. That family holds observations of a release,
    /// produced only when the agent reports a convergence result, whereas these two files are
    /// replaced daily by xray's own cron and are unrelated to releases. Attached to releases, a
    /// machine that does not deploy for a month would leave its rule-database state unknown for a
    /// month, which is the condition this feature exists to detect.
    ///
    /// It also cannot reuse `AppliedArtifactState`: that family compares whether what the control
    /// plane sent is unchanged, with an expected value on both sides, whereas these two files
    /// were never sent by it. Only observed values can be reported here, namely size, timestamp
    /// and digest, leaving the comparison to the control plane.
    ///
    /// `None` means no asset directory was found, so this machine holds no .dat file. That is
    /// distinct from never having reported, which is the whole `NodeRuntimeReport` not arriving.
    pub geodata: Option<GeodataObservation>,
    /// What the last local reconcile did. `None` where none ever ran.
    pub local_reconcile: Option<LocalReconcileReport>,
    /// Latest verdict from the same WireGuard peer check used by the node health command and
    /// watchdog. `None` covers the few seconds before the Agent's first watchdog round or a host
    /// where the check cannot run.
    pub wireguard_health: Option<WireGuardHealth>,
    /// Durable byte meter for the interface carrying the node's default route.
    ///
    /// These are Agent-lifetime logical counters rather than the kernel interface counters: the
    /// Agent persists them and carries them across process and machine restarts. `None` keeps the
    /// additive protocol change readable from older Agents; it means unsupported/not reported,
    /// never a measured zero.
    #[serde(default)]
    pub traffic: Option<NodeTrafficReading>,
    pub spool: SpoolBacklog,
}

/// One durable reading of the node's automatically selected default-route interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficReading {
    /// Stable random identity of the local meter state. A change tells the control plane that the
    /// Agent's durable state was replaced and that the two counter epochs cannot be joined.
    pub meter_id: String,
    /// Monotonic within `meter_id`; makes retries and delayed runtime reports idempotent.
    pub sequence: u64,
    pub interface: String,
    /// Linux boot id used to explain a kernel-counter reset without resetting the logical total.
    pub boot_id: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Monotonic count of boundaries at which an exact delta was unknowable (reboot, interface
    /// replacement or a counter regression). The logical counters never guess across one.
    pub discontinuities: u64,
}

/// The versions of the components running on this machine.
///
/// The two `wg` fields are the most significant, because the control plane has no expected value
/// for them. xray and phantun are shipped by the control plane against a sha256, so a version
/// mismatch has a baseline to compare against. wireguard-tools is installed by the install script
/// through the distribution's package manager (`install_pkg wireguard-tools` in `install.sh`),
/// the distribution determines which version arrives, and the control plane has no expected
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVersions {
    /// The sha256 of the running Agent binary, lowercase hex. `unknown` means the Agent could not
    /// read `/proc/self/exe`. The control plane computes its embedded build id at compile time
    /// (`brocade-console/build.rs`), so rollout state can be compared directly.
    pub agent: String,
    /// The first line of `xray version`. Unreadable does not mean absent: the binary may simply
    /// not be on PATH.
    ///
    /// This field has a specific use: `geodata` auto-update reached xray's main branch on
    /// 2026-04-25 (XTLS/Xray-core#5992), and earlier versions ignore that section without
    /// reporting anything. Without this field, a machine whose .dat never updates and a machine
    /// that cannot reach the download source are indistinguishable in every other field.
    pub xray: Option<String>,
    /// The digest of the binary at the managed Xray path. This and `xray_running_sha256` are
    /// deliberately separate: an atomic replacement changes the path while the old process keeps
    /// executing its original inode until it is restarted.
    #[serde(default)]
    pub xray_installed_sha256: Option<String>,
    /// The digest of `/proc/<serving-xray-pid>/exe`. `None` means no serving Xray could be
    /// identified, not that the managed binary is absent.
    #[serde(default)]
    pub xray_running_sha256: Option<String>,
    pub phantun: Option<String>,
    /// The first line of `openvpn --version`.
    ///
    /// VPN Gate is an optional node capability: `None` is a valid Agent installation, but that
    /// machine must not receive or be offered VPN Gate egress work. OpenVPN is started on demand
    /// inside a managed network namespace; this does not describe a system-wide daemon.
    #[serde(default)]
    pub openvpn: Option<String>,
    /// Maximum number of catalogue profiles this Agent can probe concurrently.
    ///
    /// Older Agents omit the field and therefore keep receiving the legacy single-country,
    /// two-candidate assignments. This capability lets Console and Agent roll independently: a
    /// newer Console must not send a parallel batch until the node reports support for it.
    #[serde(default)]
    pub vpngate_catalog_probe_workers: Option<u8>,
    /// `wg --version`. Below wireguard-tools 1.0.20200121 there is no `wg syncconf`, which is the
    /// only second-rung remedy that does not interrupt sessions. Without it, every drift
    /// escalates to restarting the interface, which drops every session on that machine.
    pub wg_tools: Option<String>,
    /// `kernel` or `userspace`.
    ///
    /// `None` means WireGuard is disabled locally or its backend could not be observed.
    ///
    /// Kernels 5.6 and above have WireGuard built in; without it `wg-quick` falls back to
    /// `wireguard-go` or `boringtun`. Their `wg show` output is identical while throughput
    /// differs by an order of magnitude, so the backend has to be queried explicitly.
    pub wg_backend: Option<String>,
}

/// One Xray binary rollout assignment offered to an authenticated node.
///
/// It lives outside desired state because a machine with no configuration work is exactly as
/// eligible for a runtime upgrade as a busy one. `attempt` fences a retry of the same immutable
/// release from a delayed report produced by its preceding attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XrayReleaseOffer {
    pub release_id: i64,
    pub attempt: u32,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub previous_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum XrayReleaseOutcome {
    Succeeded,
    FailedRecovered,
    FailedDirty,
    Unsupported,
}

/// Final, idempotent result of one node's Xray rollout attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XrayReleaseReport {
    pub release_id: i64,
    pub attempt: u32,
    pub outcome: XrayReleaseOutcome,
    /// True only when this attempt actually replaced the managed path. Defaulting to false keeps
    /// v8 reports from the first supporting Agent readable while preventing a no-op from proving a
    /// canary transition.
    #[serde(default)]
    pub performed_update: bool,
    pub xray_enabled: bool,
    #[serde(default)]
    pub installed_sha256: Option<String>,
    #[serde(default)]
    pub running_sha256: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// What the local reconcile (`reconcile_local`) did this round.
///
/// It runs when the control plane answers 204, which is when nothing is being released, so a
/// machine that drifted and repaired itself previously did so without the control plane
/// recording it. The number of occurrences, what was repaired, and what was not all stayed in
/// that machine's stderr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalReconcileReport {
    /// Unix seconds.
    pub at: i64,
    /// Which of `wireguard` / `xray` / `phantun` were replayed. Empty means no action was needed.
    pub actions: Vec<String>,
    /// Why the repair failed. A value here is more serious than a non-empty `actions`: that field
    /// means the state drifted and was repaired, this one means it drifted and was not.
    pub error: Option<String>,
}

/// Current WireGuard state on one node. It is a runtime observation rather than an artifact
/// verdict: a perfectly converged config can still have an unreachable peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardHealth {
    pub enabled: bool,
    pub error: Option<String>,
    pub peers: Vec<WireGuardPeerHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeerHealth {
    pub peer_node_id: String,
    pub overlay_ip: Option<String>,
    pub handshake_age_secs: Option<i64>,
    pub status: WireGuardPeerStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireGuardPeerStatus {
    Up,
    Down,
    Unknown,
}

/// How much undeliverable reporting has piled up locally.
///
/// While the agent cannot reach the control plane it accumulates reports in a spool file and
/// drops the oldest past the limit. What is dropped is accounting data, so the symptom is a
/// machine reporting no traffic for the month, which the UI cannot distinguish from a machine
/// that carried none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolBacklog {
    pub observation: u32,
    pub usage: u32,
    /// The cumulative count dropped for exceeding the limit. It only increases, and a non-zero
    /// value means accounting data was permanently lost.
    pub dropped: u64,
}

// ── Telemetry: host load and per-hop link estimates ────────────────────────────────────────
//
// Two families of measurements on one report, because they share a window and a cadence:
//
//   host load     read from /proc and statvfs, describing remaining machine capacity
//   link quality  read from netlink inet_diag (tcp_info + tcp_bbr_info), describing each hop's
//                 line
//
// The second costs nothing extra. BBR updates its bottleneck-bandwidth and min-RTT estimates on
// every forwarding connection, once per RTT, so the values are already in the kernel. The two
// existing probes are active and infrequent, and their traffic is not user traffic: link_probes
// measures path MTU over ICMP every 30 minutes, and e2e measures TTFB every 5.
//
// Unlike usage, the agent differences locally and reports **rates** rather than cumulative
// counters. Usage reports counters and lets the control plane difference them because billing
// data has to be auditable and replay-proof. A load reading has no value once it is stale, and
// keeping a previous reading per machine on the control plane would provide nothing. The cost is
// that the control plane can no longer detect a counter reset itself, which is why `btime` is
// included: it changes when the machine reboots and every cumulative counter returns to zero.
// Usage makes the same determination with `xray_started_at`, and this reuses that approach rather
// than introducing a second one.
//
// None of this enters the model and none of it creates a revision. It is runtime observation, in
// the same category as `user_app_quotas`.

/// One round from one machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadReportRequest {
    /// When the agent finished this round, in unix seconds. Checked against the control plane's
    /// own clock, as usage is, so a machine with an incorrect clock cannot record readings with a
    /// future timestamp.
    pub read_at_unix_secs: i64,
    /// `/proc/stat`'s btime. A change means the machine rebooted and every cumulative counter
    /// restarted, which invalidates the differences in this round's first window.
    pub btime_unix_secs: i64,
    pub host: HostFacts,
    /// Usually one entry. More only when a round could not be sent and the next one carries the
    /// backlog, which is best-effort: unlike usage there is no spool behind this. See
    /// `LoadReportResult`.
    pub samples: Vec<LoadSample>,
    pub processes: Vec<ProcessSample>,
    /// Empty on a machine with no xray, or one whose hops carry no connections right now.
    pub hops: Vec<HopLinkSample>,
}

/// What changes slowly enough not to belong in a 30-second series.
///
/// Split out rather than repeated per sample for two reasons. Storage: writing the kernel version
/// and disk capacity 2 880 times a day per machine adds nothing. Accuracy: a machine's disk
/// capacity is not a per-window value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    /// `/proc/sys/kernel/osrelease`. Kept as a measurement rather than a derived verdict because
    /// WireGuard backend and BBR capability decisions depend on the exact kernel.
    pub kernel: String,
    /// Human-readable processor model read from `/proc/cpuinfo`; an empty value is valid on
    /// architectures whose kernel exposes no model identity.
    pub cpu_model: String,
    pub cores: u32,
    /// Maximum frequency exposed by cpufreq, in MHz. Virtual machines commonly expose no cpufreq
    /// tree at all; `None` means unsupported, not a zero-frequency processor.
    pub cpu_freq_max_mhz: Option<u64>,
    /// The common scaling governor across online CPUs. Empty/mixed governors are represented as
    /// `None`; this is a capability detail rather than an alarm.
    pub cpu_governor: Option<String>,
    /// `/proc/sys/net/ipv4/tcp_congestion_control`. Do not assume this is cubic or bbr: low-cost
    /// VPS images often carry a patched kernel offering `bbrplus` or `bbr2`.
    pub cc_algo: String,
    /// `tcp_available_congestion_control`. Without it, an unloaded module and a kernel without
    /// the capability are indistinguishable, and only the second cannot be corrected.
    pub available_cc: Vec<String>,
    /// `net.core.default_qdisc`.
    pub default_qdisc: String,
    /// What is attached to the main interface (`tc qdisc show`). Sampled separately from
    /// `default_qdisc` by necessity: changing the default affects only queues created afterwards,
    /// an interface already up keeps its existing qdisc, and reading back the default alone
    /// reports success on a machine where nothing changed.
    pub nic_qdisc: String,
    pub nic: String,
    /// The live MTU of `nic`, read from sysfs. This is the local egress interface ceiling, not the
    /// end-to-end path MTU reported by `LinkProbe`; both are needed to distinguish a bad local
    /// interface configuration from a smaller hop farther along the path.
    ///
    /// `None` means sysfs did not expose a readable MTU.
    pub nic_mtu: Option<u32>,
    pub mem_total_bytes: u64,
    /// The filesystem holding the state directory, which is where the spool is written.
    pub disk_total_bytes: u64,
    /// Identity of the filesystem whose capacity is reported above. Overlay/container filesystems
    /// do not always have a block device, so every field remains optional independently.
    pub disk_mount: Option<String>,
    pub disk_filesystem: Option<String>,
    pub disk_device: Option<String>,
    pub disk_read_only: Option<bool>,
    /// `nf_conntrack_max`. `None` means the module is not loaded, which is not a fault: a machine
    /// doing no NAT simply has no such table.
    pub conntrack_max: Option<u64>,
    /// Kernel-selected anonymous local-port range after subtracting
    /// `ip_local_reserved_ports`. Stored with host facts for explanation; each network sample also
    /// carries the contemporaneous capacity so a later sysctl change cannot rewrite history.
    pub ephemeral_port_low: Option<u16>,
    pub ephemeral_port_high: Option<u16>,
    pub ephemeral_port_capacity: Option<u64>,
    /// Whether the installer set the congestion control algorithm on this machine.
    ///
    /// This distinguishes a machine where the installer set bbr and something later changed it
    /// from a machine that was never configured. Without the field, both report only that the
    /// current algorithm is not bbr, and the operator cannot tell them apart.
    ///
    /// Read as the presence of the bbr key inside `/etc/sysctl.d/99-brocade.conf` rather than of
    /// the file itself: the same file also carries the conntrack sizing, which the installer
    /// writes even on machines where bbr could not be set. See `load.rs`.
    pub sysctl_managed: bool,
    /// `std::env::consts::ARCH` — x86_64 / aarch64. The kernel string alone does not carry it,
    /// and the fleet runs both.
    pub arch: String,
    /// `PRETTY_NAME` from `/etc/os-release`. Empty when the file is missing (minimal images).
    pub os_pretty: String,
    /// The virtualization platform's short name (KVM / VMware / Hyper-V / …), mapped from DMI and
    /// friends. Empty means bare metal or unrecognized: the DMI string of a bare-metal machine is
    /// its mainboard model, which is not virtualization information, so it is not shown.
    pub virt: String,
    /// `net.core.rmem_max` / `wmem_max`: the ceiling every socket buffer is clamped by. The
    /// installer does not touch these, so the UI renders them a shade dimmer (inherited from the
    /// distribution) — the same convention as `sysctl_managed` establishes for congestion control.
    pub rmem_max: u64,
    pub wmem_max: u64,
    /// `net.core.somaxconn`: the accept-queue ceiling. Same inherited-value treatment as the
    /// buffer ceilings.
    pub somaxconn: u64,
}

/// One logical CPU's mutually-exclusive time shares inside a load window.
///
/// The aggregate CPU value cannot reveal a single saturated forwarding queue on a many-core
/// machine. Keeping the three kinds of work separate also preserves the existing distinction
/// between encryption/user work and packet-processing softirqs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuCoreSample {
    pub cpu: u32,
    pub user_pct: f32,
    pub system_pct: f32,
    pub softirq_pct: f32,
    pub iowait_pct: f32,
    pub steal_pct: f32,
}

/// Optional deep CPU diagnostics from agents that support them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuDetailSample {
    /// `/proc/stat`'s iowait. Kept outside CPU busy time: waiting for storage is not CPU work.
    pub iowait_pct: f32,
    pub load5: f32,
    pub load15: f32,
    /// CPU PSI `some`, averaged over this exact window from the cumulative microsecond counter.
    pub pressure_some_pct: Option<f32>,
    /// I/O PSI over this exact window. Unlike `/proc/stat`'s iowait, PSI measures task stall time
    /// directly and is therefore the signal used for the UI's I/O-pressure diagnosis.
    pub io_pressure_some_pct: Option<f32>,
    pub io_pressure_full_pct: Option<f32>,
    pub procs_running: Option<u64>,
    pub procs_total: Option<u64>,
    pub context_switches_per_sec: Option<u64>,
    pub net_rx_softirqs_per_sec: Option<u64>,
    pub net_tx_softirqs_per_sec: Option<u64>,
    /// Cgroup CPU time denied during this window. Root cgroups or kernels without the controller
    /// expose no usable counter and report `None` rather than pretending no throttling occurred.
    pub throttled_usec: Option<u64>,
    /// Mean current frequency across CPUs with a readable cpufreq entry.
    pub frequency_mhz: Option<u64>,
    pub cores: Vec<CpuCoreSample>,
}

/// Optional deep memory diagnostics from agents that support them.
///
/// Raw `/proc/meminfo` counters overlap. The first five fields are deliberately an exclusive
/// physical composition: file cache excludes shmem, and `kernel_other_bytes` is the residual that
/// makes the five add back to MemTotal. The remaining counters are diagnostic lenses and must not
/// be stacked on top of that composition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDetailSample {
    pub available_min_bytes: u64,
    pub free_bytes: u64,
    pub anon_bytes: u64,
    pub file_cache_bytes: u64,
    pub shmem_bytes: u64,
    pub kernel_other_bytes: u64,
    pub buffers_bytes: u64,
    pub kernel_reclaimable_bytes: u64,
    pub slab_unreclaimable_bytes: u64,
    pub unevictable_bytes: u64,
    pub mlocked_bytes: u64,
    pub dirty_bytes: u64,
    pub writeback_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_cached_bytes: u64,
    pub zswap_bytes: Option<u64>,
    pub zswapped_bytes: Option<u64>,
    /// Best-effort estimate from nr_foll_pin_acquired - nr_foll_pin_released.
    pub gup_pinned_bytes: Option<u64>,
    pub swap_in_bytes: u64,
    pub swap_out_bytes: u64,
    pub pressure_some_pct: Option<f32>,
    pub pressure_full_pct: Option<f32>,
    pub major_faults: u64,
    pub direct_reclaim_pages: u64,
}

/// Optional block-I/O diagnostics for the filesystem containing the agent state directory.
///
/// `statvfs` describes capacity but cannot say whether the disk is busy or merely full. These
/// values come from the exact backing device's `/proc/diskstats` row. A container overlay or
/// network filesystem may have no block-device row; that absence is represented by `None` rather
/// than a fabricated idle disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskDetailSample {
    /// Contemporaneous capacity. HostFacts keeps the latest copy for the summary, while this copy
    /// prevents a later volume resize from rewriting historical utilization.
    pub total_bytes: Option<u64>,
    pub inode_total: Option<u64>,
    pub inode_free: Option<u64>,
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
    pub read_iops: Option<f32>,
    pub write_iops: Option<f32>,
    pub read_await_ms: Option<f32>,
    pub write_await_ms: Option<f32>,
    /// Wall-clock share for which this device had at least one I/O in flight.
    pub busy_pct: Option<f32>,
    /// Time-weighted queue length (`weighted_io_ms / elapsed_ms`).
    pub queue_depth: Option<f32>,
    /// Requests in flight at the end of the window.
    pub in_flight: Option<u64>,
    /// Host-wide I/O PSI. It is kept beside the device counters because it answers the missing
    /// half: device utilization alone cannot tell whether applications were actually stalled.
    pub pressure_some_pct: Option<f32>,
    pub pressure_full_pct: Option<f32>,
}

/// Optional deep network diagnostics from agents that support them.
///
/// The socket inventory fields are end-of-window levels. The remaining fields are differences of
/// kernel counters over this exact 30-second window. Keeping those two kinds explicit prevents a
/// UI from accidentally presenting a lifetime `TcpRetransSegs` counter as a current rate, or from
/// averaging a current socket count that only has meaning at one instant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkDetailSample {
    /// TCP sockets currently in ESTABLISHED or CLOSE-WAIT (`Tcp.CurrEstab`).
    pub tcp_curr_estab: Option<u64>,
    /// IPv4 and IPv6 socket inventory. `alloc`, `orphan`, `tw` and memory are global kernel
    /// counters and are exposed only by `/proc/net/sockstat`; the in-use fields add sockstat6.
    pub tcp_inuse: Option<u64>,
    pub tcp_time_wait: Option<u64>,
    pub tcp_orphan: Option<u64>,
    pub tcp_alloc: Option<u64>,
    pub tcp_mem_bytes: Option<u64>,
    pub udp_inuse: Option<u64>,
    pub udp_mem_bytes: Option<u64>,

    /// Anonymous local-port pressure, estimated from an all-state inet_diag snapshot. A plain
    /// TIME_WAIT/range ratio is not a capacity measure because Linux may reuse the same local port
    /// for different destinations. `top_target` is therefore the largest number of distinct
    /// local ports occupied by one (source address, destination address, destination port) tuple
    /// space. No peer address leaves the machine.
    pub ephemeral_port_capacity: Option<u64>,
    pub tcp_ephemeral_inuse_v4: Option<u64>,
    pub tcp_ephemeral_inuse_v6: Option<u64>,
    pub tcp_ephemeral_time_wait_v4: Option<u64>,
    pub tcp_ephemeral_time_wait_v6: Option<u64>,
    pub tcp_ephemeral_top_target_v4: Option<u64>,
    pub tcp_ephemeral_top_target_v6: Option<u64>,

    /// TCP lifecycle and reliability events during the window (`/proc/net/snmp` and TcpExt).
    pub tcp_active_opens: Option<u64>,
    pub tcp_passive_opens: Option<u64>,
    pub tcp_attempt_fails: Option<u64>,
    pub tcp_estab_resets: Option<u64>,
    pub tcp_retrans_segs: Option<u64>,
    pub tcp_syn_retrans: Option<u64>,
    pub tcp_in_errors: Option<u64>,
    pub tcp_out_resets: Option<u64>,
    pub tcp_timeouts: Option<u64>,
    pub tcp_listen_overflows: Option<u64>,
    pub tcp_listen_drops: Option<u64>,

    /// UDP delivery failures during the window. IPv4 and IPv6 counters are added where both are
    /// available; absence remains `None`, never a fabricated zero.
    pub udp_in_errors: Option<u64>,
    pub udp_no_ports: Option<u64>,
    pub udp_rcvbuf_errors: Option<u64>,
    pub udp_sndbuf_errors: Option<u64>,
}

/// One window of one machine's resource use. Rates, already differenced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadSample {
    pub window_start_unix_secs: i64,
    pub window_end_unix_secs: i64,
    /// btime changed, or a window was missed. The rates in this entry are not comparable with the
    /// previous one, and the UI has to break the line rather than join it, because a joined line
    /// displays a value that was never measured.
    pub has_gap: bool,

    // CPU in three parts rather than one percentage. Forwarding load appears as softirq, and once
    // summed, a NIC saturating a core with interrupts and xray consuming CPU on encryption are
    // indistinguishable. The first calls for multi-queue or a larger machine, and the second for
    // moving traffic elsewhere.
    pub cpu_user_pct: f32,
    pub cpu_sys_pct: f32,
    pub cpu_softirq_pct: f32,
    /// The largest single sub-sample inside this window. An average removes the spikes, and
    /// forwarding load consists largely of spikes: a machine averaging 40% may saturate a core
    /// every minute.
    pub cpu_peak_pct: f32,
    /// `/proc/stat`'s `steal`: the share of CPU time this machine wanted but the hypervisor gave
    /// to another guest. Kept apart from the three shares above on purpose — steal is not work
    /// this machine does, and folding it in would dress an oversold host up as a busy one.
    pub cpu_steal_pct: f32,
    pub load1: f32,
    /// `None` means detailed CPU counters were unavailable; it is distinct from a zero reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_detail: Option<CpuDetailSample>,

    /// `MemAvailable` rather than `free`. On any machine with a page cache `free` is always
    /// small, so using it reports every healthy machine as low on memory.
    pub mem_available_bytes: u64,
    pub swap_used_bytes: u64,
    /// Optional composition, reclaimability and pressure diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_detail: Option<MemoryDetailSample>,
    /// `/proc/vmstat`'s `oom_kill`, differenced. A non-zero value means the kernel killed a
    /// process in this window. It is a direct measurement rather than an inference, and nothing
    /// else reports it; the only other symptom is a user reporting a brief outage.
    pub oom_kills: u64,

    /// Free space on the state directory's filesystem. This is a leading indicator of lost
    /// accounting data: a full disk fails the spool's fsync, and the spool holds billing data.
    /// The existing `dropped` counter only reports the loss after it happens.
    pub disk_free_bytes: u64,
    pub disk_inode_free_pct: f32,
    /// Filesystems without a local block-device view still carry an object whose device-specific
    /// fields are `None`, preserving capacity/inode drill-down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_detail: Option<DiskDetailSample>,

    pub nic_rx_bps: u64,
    pub nic_tx_bps: u64,
    /// Drops and errors, differenced. Byte counts are excluded, because usage measures those with
    /// per-label attribution. What usage cannot measure is what was dropped.
    pub nic_rx_drop: u64,
    pub nic_tx_drop: u64,
    pub nic_err: u64,

    pub conntrack_count: Option<u64>,
    /// Optional socket levels plus differenced TCP/UDP kernel counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_detail: Option<NetworkDetailSample>,
    pub uptime_secs: u64,
}

/// One of the processes this system installs on the machine.
///
/// This layer exists for one reason: it is what separates load caused by the machine's other work
/// from load caused by these processes. When a customer's machine misbehaves, the agent is the
/// first suspect, and without this there is no measurement to answer with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessSample {
    /// `xray` / `wg` / `phantun` / `agent`.
    pub proc: String,
    /// `None` where the process is absent. For xray that means the machine is a wg-only relay,
    /// which is a role rather than a fault. wg is always `None`, because it is a kernel module
    /// and has no RSS.
    pub rss_bytes: Option<u64>,
    pub cpu_pct: Option<f32>,
    /// Unix seconds. A change means the process restarted, which nothing else reports, even
    /// though usage already tracks xray's start time for its own counter-reset check.
    pub started_at_unix_secs: Option<i64>,
    pub fds: Option<u64>,
    /// `RLIMIT_NOFILE`. Reaching it does not terminate xray; it makes xray refuse new
    /// connections, so the process stays up, the logs stay quiet, and users cannot connect.
    pub fd_limit: Option<u64>,
}

/// One hop's line quality for one window, aggregated from every TCP connection carrying it.
///
/// Keyed `(chain_id, peer_node_id)`, the same key `link_health` uses and for the same reason:
/// the measurement covers this machine's **outbound** leg towards its next hop, and the agent
/// already derives that pair from xray's outbound tags (`out:{app}/{chain}>{to}`, see
/// `probe.rs::hop_of_tag`).
///
/// `hop_label` is deliberately not part of the key: it is `steps.accept_label`, an **inbound**
/// identity. On the leg hk-01 → sg-02 the label belongs to sg-02 while the measurement is taken on
/// hk-01, so the two never share a `node_id` and joining them would align different machines.
///
/// Byte counts are deliberately absent: `link_health` already carries this hop's downlink for the
/// same window, reported by the same agent in the same round. The console reads both and displays
/// them together, and one value arriving by two paths eventually disagrees with itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopLinkSample {
    pub chain_id: String,
    pub peer_node_id: String,
    pub window_start_unix_secs: i64,
    pub window_end_unix_secs: i64,

    pub conns: u32,
    /// Of those, the ones whose `delivery_rate` did **not** carry the `app_limited` flag.
    ///
    /// Only these count towards the bandwidth estimate. A connection with nothing to send reports
    /// a rate describing the application rather than the line, so without this filter a few idle
    /// connections lower the percentiles enough for the line to appear failed.
    pub conns_measured: u32,

    /// `tcp_bbr_info`'s bw, converted from the kernel's bytes/sec to bits/sec. `None` on a machine
    /// not running BBR, because cubic keeps no bottleneck-bandwidth estimate. That is the second
    /// reason to enable BBR: throughput, and the availability of this measurement.
    pub btlbw_p50_bps: Option<u64>,
    pub btlbw_p90_bps: Option<u64>,

    /// The minimum across all connections rather than the mean: propagation delay is the smallest
    /// measurement by definition, and averaging includes queueing delay in it.
    pub min_rtt_us: u32,
    pub rtt_p50_us: u32,
    pub rtt_p90_us: u32,
    /// Σ bytes_retrans / Σ bytes_sent.
    pub retrans_pct: f32,

    /// What limits this hop's connections. The three need not sum to 100, because a connection
    /// can be limited by none of them. `busy` is congestion, meaning the line is at capacity;
    /// `rwnd` is the far end not reading; and `sndbuf` is **this machine's own send buffer**. The
    /// last is the only one of the three that justifies changing a sysctl, and the reason every
    /// other kernel parameter is left alone until comparable evidence exists.
    pub busy_pct: f32,
    pub rwnd_limited_pct: f32,
    pub sndbuf_limited_pct: f32,
}

/// What the control plane made of one report.
///
/// Load reporting deliberately has no spool, and this type is where that shows: a rejected window
/// is discarded. The spool exists so a locally established fact survives an outage, because usage
/// drives billing and a convergence result is the only record that convergence happened. A CPU
/// reading from five minutes ago is neither, so adding usage's spool here would introduce a file
/// that can fill a disk in exchange for readings that are no longer used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadReportResult {
    pub node_id: String,
    pub accepted_samples: u64,
    pub accepted_hops: u64,
    /// Windows refused for overlapping one already stored, or for arriving out of order.
    pub skipped_samples: u64,
    /// Hops naming a chain this machine is not on. A persistently non-zero value means the agent
    /// is reporting hops it does not carry.
    pub rejected_hops: u64,
}

/// What the console reads back for one machine.
///
/// The sample types are reused unchanged from the reporting side rather than mirrored with
/// `timestamptz` strings as `UsageNodeBucket` does. Seventeen numeric fields defined twice would
/// diverge over time, and converting unix seconds to a Date in the browser costs one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeLoadView {
    pub node_id: String,
    /// For an absolute query, the exact half-open interval requested by the reader. For a latest
    /// window query, the extent of the returned samples (or zero when there are none). Charts use
    /// this to keep missing time at its real position instead of moving a stale row to the right
    /// edge and making stale telemetry look current.
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    /// `None` means this machine has never reported, which is distinct from reporting zeros. The
    /// UI has to state that explicitly: a newly enrolled machine with no sample is not a machine
    /// with a fault.
    pub reported_at_unix_secs: Option<i64>,
    /// The reporting agent's clock minus the control plane's, in seconds, measured when the
    /// latest report arrived. It can only be measured at receipt — `read_at` reads the agent's
    /// clock, which is gone afterwards. Rounds past ±600s are rejected outright, so a stored
    /// value is always inside that range.
    pub clock_skew_secs: Option<i64>,
    pub host: Option<HostFacts>,
    /// The newest stored sample, independent of the requested series. This lets the UI retain a
    /// machine's last known state when an absolute interval contains no samples; its timestamp is
    /// still authoritative and must not be presented as a reading from the requested interval.
    pub latest_sample: Option<LoadSample>,
    /// Oldest first, so the UI can draw it left to right without sorting.
    pub series: Vec<LoadSample>,
    pub processes: Vec<ProcessSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeLoadList {
    pub nodes: Vec<NodeLoadView>,
}

// ── Active PING probe (TCP connect + ICMP echo) ────────────────────────────────────────────
//
// A target's URI selects the operation: `tcp://host:port` measures a TCP handshake and
// `icmp://host` measures one echo round trip. Both may be present in the same round. Name
// resolution and local capability checks happen outside the timer. The durable contract keeps
// only what the graph uses: whether a wire attempt actually happened and its optional latency.
// Detailed DNS/socket/errno diagnostics remain in the node-local journal.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeTarget {
    pub name: String,
    /// Canonical `tcp://host:port` or `icmp://host` address. It is also the stable series id.
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeSettings {
    pub targets: Vec<PingProbeTarget>,
    pub interval_secs: u32,
    /// Maximum handshake/echo duration. A reply above the boundary is represented as no response.
    pub timeout_ms: u32,
}

impl Default for PingProbeSettings {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            interval_secs: 60,
            timeout_ms: 420,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeSample {
    /// Matches `PingProbeTarget.address` from the settings fetched for this round.
    pub target: String,
    /// False means no wire measurement was possible (for example no IPv6 route or no ICMP socket).
    /// It must stay distinct from an attempted probe that received no response.
    pub attempted: bool,
    /// Whole microseconds preserve sub-millisecond ICMP readings without storing a floating point
    /// value. `None` with `attempted = true` means no response within the configured timeout.
    pub latency_us: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeReportRequest {
    pub probed_at_unix_secs: i64,
    pub samples: Vec<PingProbeSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeReportResult {
    pub node_id: String,
    pub accepted_samples: u64,
    pub skipped_samples: u64,
    pub unknown_targets: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbePoint {
    pub probed_at_unix_secs: i64,
    pub attempted: bool,
    pub latency_us: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeTargetSeries {
    pub name: String,
    pub address: String,
    /// Oldest first. Attempted null points are no-response intervals; unattempted null points are
    /// capability/route gaps and must not be counted as packet loss by clients.
    pub samples: Vec<PingProbePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePingProbeView {
    pub node_id: String,
    pub targets: Vec<PingProbeTargetSeries>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePingProbeList {
    pub nodes: Vec<NodePingProbeView>,
}

/// Latest observation for one configured target on a machine-list card.
///
/// The list view answers a current-state question, so it must not carry a history window merely
/// to derive one number in the browser. `None` means this target has never produced a retained
/// sample for the machine. An attempted sample with no latency remains a timeout, while an
/// unattempted sample remains a capability or route gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingProbeTargetLatest {
    pub name: String,
    pub address: String,
    pub latest: Option<PingProbePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePingProbeLatestView {
    pub node_id: String,
    pub targets: Vec<PingProbeTargetLatest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePingProbeLatestList {
    /// The configured Agent sampling interval lets clients distinguish a current observation from
    /// an old last-known value without duplicating probe settings onto the public settings route.
    pub interval_secs: u32,
    pub nodes: Vec<NodePingProbeLatestView>,
}

/// One hop, as the console reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopLinkView {
    pub node_id: String,
    pub sample: HopLinkSample,
    /// This machine's congestion algorithm, carried down from `HostFacts` so the table can state
    /// that cubic provides no bandwidth measurement on the row where the bandwidth is missing,
    /// rather than showing a dash the reader has to explain from elsewhere.
    pub cc_algo: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HopLinkList {
    pub hops: Vec<HopLinkView>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_observation_has_stable_family_spelling() {
        let encoded = serde_json::to_value(NodePublicIpObservation {
            observed_at_unix_secs: 1,
            family: PublicIpFamily::V6,
            ip: "2606:4700:4700::1111".to_owned(),
            country_code: Some("US".to_owned()),
        })
        .unwrap();
        assert_eq!(encoded["family"], "v6");
        assert_eq!(encoded["country_code"], "US");
    }

    #[test]
    fn legacy_runtime_versions_default_new_xray_identities() {
        let versions: NodeVersions = serde_json::from_value(serde_json::json!({
            "agent": "brocade-agent/old",
            "xray": "Xray 26.4.25",
            "phantun": null,
            "wg_tools": null,
            "wg_backend": null
        }))
        .unwrap();
        assert_eq!(versions.xray_installed_sha256, None);
        assert_eq!(versions.xray_running_sha256, None);
        assert_eq!(versions.openvpn, None);
        assert_eq!(versions.vpngate_catalog_probe_workers, None);
    }

    #[test]
    fn legacy_runtime_report_defaults_missing_traffic_meter() {
        let report: NodeRuntimeReport = serde_json::from_value(serde_json::json!({
            "observed_at_unix_secs": 1,
            "versions": {
                "agent": "old-agent",
                "xray": null,
                "phantun": null,
                "wg_tools": null,
                "wg_backend": null
            },
            "certificate": { "t": "unmanaged" },
            "geodata": null,
            "local_reconcile": null,
            "wireguard_health": null,
            "spool": { "observation": 0, "usage": 0, "dropped": 0 }
        }))
        .unwrap();
        assert_eq!(report.traffic, None);
    }

    #[test]
    fn vpngate_v12_extensions_default_when_decoding_older_desired_state() {
        let desired: VpngateDesiredState = serde_json::from_value(serde_json::json!({
            "topology_revision": 7,
            "catalog_generation": 9,
            "pools": []
        }))
        .unwrap();
        assert!(desired.probe_assignments.is_empty());
        assert_eq!(desired.admission_policy, VpngateAdmissionPolicy::default());

        let candidate: VpngateCandidate = serde_json::from_value(serde_json::json!({
            "server_id": "vpn1",
            "hostname": "vpn1",
            "country_code": "JP",
            "remote_address": "192.0.2.10",
            "remote_port": 1194,
            "transport": "udp",
            "profile_sha256": "a".repeat(64),
            "openvpn_config": "client"
        }))
        .unwrap();
        assert_eq!(candidate.verified_exit_ip, None);
        assert_eq!(candidate.verified_exit_country_code, None);
        assert!(candidate.verified_ip_scores.is_empty());
        assert!(candidate.verified_ip_networks.is_empty());
    }

    #[test]
    fn xray_release_outcome_uses_a_stable_wire_spelling() {
        assert_eq!(
            serde_json::to_string(&XrayReleaseOutcome::FailedRecovered).unwrap(),
            r#""failed-recovered""#
        );
    }

    #[test]
    fn xray_rollout_extensions_remain_backward_compatible_with_protocol_v8() {
        #[derive(Deserialize)]
        struct EarlierV8Report {
            release_id: i64,
            attempt: u32,
            outcome: XrayReleaseOutcome,
            xray_enabled: bool,
            installed_sha256: Option<String>,
            running_sha256: Option<String>,
            error: Option<String>,
        }

        assert_eq!(AGENT_PROTOCOL_VERSION, 21);
        assert_eq!(MIN_AGENT_PROTOCOL_VERSION, 20);
        let report: XrayReleaseReport = serde_json::from_value(serde_json::json!({
            "release_id": 7,
            "attempt": 1,
            "outcome": "succeeded",
            "xray_enabled": true,
            "installed_sha256": "a".repeat(64),
            "running_sha256": "a".repeat(64)
        }))
        .unwrap();
        assert!(!report.performed_update);

        let encoded = serde_json::to_value(XrayReleaseReport {
            performed_update: true,
            error: None,
            ..report
        })
        .unwrap();
        let earlier: EarlierV8Report = serde_json::from_value(encoded).unwrap();
        assert_eq!(earlier.release_id, 7);
        assert_eq!(earlier.attempt, 1);
        assert_eq!(earlier.outcome, XrayReleaseOutcome::Succeeded);
        assert!(earlier.xray_enabled);
        assert_eq!(earlier.installed_sha256, Some("a".repeat(64)));
        assert_eq!(earlier.running_sha256, Some("a".repeat(64)));
        assert_eq!(earlier.error, None);
    }

    #[test]
    fn vpngate_admission_never_aggregates_provider_scores() {
        let policy = VpngateAdmissionPolicy::default();
        let scores = vec![
            VpngateIpScore {
                provider: VpngateIpProvider::Proxycheck,
                score: 79,
                country_code: "JP".to_owned(),
            },
            VpngateIpScore {
                provider: VpngateIpProvider::Ffraud,
                score: 81,
                country_code: "US".to_owned(),
            },
        ];
        assert_eq!(
            evaluate_vpngate_admission(&policy, "JP", &scores),
            VpngateAdmissionDecision::Rejected
        );

        let mut any_pass = policy;
        any_pass.risk_decision_policy = VpngateRiskDecisionPolicy::AnyAvailablePass;
        assert_eq!(
            evaluate_vpngate_admission(&any_pass, "JP", &scores),
            VpngateAdmissionDecision::Admitted
        );
    }

    #[test]
    fn vpngate_admission_accepts_one_source_and_does_not_require_country_agreement() {
        let policy = VpngateAdmissionPolicy::default();
        assert_eq!(
            evaluate_vpngate_admission(
                &policy,
                "JP",
                &[VpngateIpScore {
                    provider: VpngateIpProvider::Iplogs,
                    score: 42,
                    country_code: "JP".to_owned(),
                }]
            ),
            VpngateAdmissionDecision::Admitted
        );

        let scores = vec![
            VpngateIpScore {
                provider: VpngateIpProvider::Proxycheck,
                score: 20,
                country_code: "JP".to_owned(),
            },
            VpngateIpScore {
                provider: VpngateIpProvider::Ffraud,
                score: 30,
                country_code: "US".to_owned(),
            },
        ];
        assert_eq!(
            evaluate_vpngate_admission(&policy, "JP", &scores),
            VpngateAdmissionDecision::Admitted
        );
    }

    #[test]
    fn realtime_commands_have_a_small_stable_wire_shape() {
        assert_eq!(
            serde_json::to_string(&AgentRealtimeCommand::Start {
                interval_millis: 1000
            })
            .unwrap(),
            r#"{"type":"start","interval_millis":1000}"#
        );
        assert_eq!(
            serde_json::to_string(&AgentRealtimeCommand::Stop).unwrap(),
            r#"{"type":"stop"}"#
        );
    }

    #[test]
    fn realtime_samples_allow_absent_mux_group_but_require_a_complete_report() {
        let nic_only: AgentRealtimeSample = serde_json::from_value(serde_json::json!({
            "sequence": 1,
            "sampled_at_unix_millis": 10,
            "elapsed_millis": 1000,
            "interface": "eth0",
            "rx_bytes_per_sec": 1,
            "tx_bytes_per_sec": 2,
            "has_gap": false
        }))
        .unwrap();
        assert!(!nic_only.diagnostics_unchanged);
        assert_eq!(nic_only.mux, None);
        assert_eq!(nic_only.vpngate, None);

        let vpngate: VpngateRealtimeReport = serde_json::from_value(serde_json::json!({
            "boot_id": "boot-a",
            "sequence": 3,
            "sampled_at_unix_millis": 10,
            "pools": [{
                "outbound_id": "vpngate-jp", "country_code": "JP", "state": "degraded",
                "reason": "egress_unreachable", "active_slot": 1, "ready_standbys": 0,
                "candidate_count": 16, "consecutive_failures": 1,
                "last_success_age_millis": 5000, "probes": 20, "probe_failures": 2,
                "failovers": 1, "refill_attempts": 2, "refill_failures": 1
            }],
            "backends": [],
            "events": []
        }))
        .unwrap();
        assert_eq!(vpngate.pools[0].state, VpngateRuntimeState::Degraded);
        assert_eq!(vpngate.pools[0].refill_backoff_remaining_millis, 0);

        let report: MuxReport = serde_json::from_value(serde_json::json!({
            "boot_id": "9",
            "sequence": 2,
            "sampled_at_unix_ms": 10,
            "pools": [{
                "pool_id": "11", "pair": "out:app/chain>peer", "role": "dialer", "kind": "tcp",
                "used": true, "draining": false,
                "config": {
                    "concurrency": 8, "prewarm_workers": 1, "reuse_threshold": 2,
                    "max_probing_workers": 1, "probe_interval_ms": 5000,
                    "probe_timeout_ms": 1000, "idle_ttl_ms": 60000,
                    "max_sessions_per_worker": 100, "health_lease_ms": 15000,
                    "confirm_timeout_ms": 4000, "recovery_successes": 2,
                    "session_end_timeout_ms": 10000
                },
                "active_sessions": 1, "available_slots": 7, "ready_workers": 1, "total_workers": 1,
                "dispatches": 3, "active_reuses": 1, "idle_reuses": 1, "demand_dials": 1,
                "rejected_dispatches": 0, "probes": 2, "acks": 2, "timeouts": 0,
                "workers_created_demand": 1, "workers_created_warm": 1,
                "workers_warm_ready": 1, "workers_warm_failed": 0, "workers_closed_idle_ttl": 0,
                "workers_closed_probe": 0, "workers_closed_capacity": 0, "workers_closed_requests": 0,
                "workers_closed_transport": 0, "health_suspects": 3,
                "health_recoveries": 1, "health_draining": 1,
                "health_queue_failures": 2, "health_dial_throttled": 4
            }],
            "workers": [],
            "events": []
        }))
        .unwrap();
        assert_eq!(report.boot_id, "9");
        assert_eq!(report.pools[0].available_slots, 7);
        assert_eq!(report.pools[0].idle_reuses, 1);
        assert_eq!(report.pools[0].config.health_lease_ms, 15000);
        let mut wire = serde_json::to_value(&report).unwrap();
        let worker = serde_json::json!({
            "pool_id": "11", "worker_id": "18446744073709551614",
            "pair": "out:app/chain>peer", "role": "dialer", "kind": "tcp",
            "state": "SUSPECT", "reason": "health_lease_expired", "phase": "active",
            "active_sessions": 1, "affected_sessions": 0, "available_slots": 0,
            "lifetime_sessions": 10, "ack_age_ms": 15000, "rtt_ms": 80,
            "probes": 5, "acks": 4, "timeouts": 1,
            "lease_remaining_ms": 0, "control_queue_depth": 2, "queue_delay_ms": 30
        });
        wire["workers"] = serde_json::json!([worker.clone()]);
        let mut event = worker;
        event["sequence"] = 2.into();
        event["at_unix_ms"] = 10.into();
        event["from"] = "READY".into();
        wire["events"] = serde_json::json!([event]);
        // Agent and Console both decode/re-encode this typed report. Pin every new
        // dimension through both hops instead of silently dropping it in serde.
        let agent: MuxReport = serde_json::from_value(wire.clone()).unwrap();
        let console: MuxReport =
            serde_json::from_value(serde_json::to_value(agent).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(console).unwrap(), wire);

        for pointer in [
            "/pools/0/config/health_lease_ms",
            "/pools/0/health_suspects",
            "/workers/0/lease_remaining_ms",
        ] {
            let mut incomplete = wire.clone();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            incomplete
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<MuxReport>(incomplete).is_err(),
                "missing {pointer} was accepted"
            );
        }
    }

    #[test]
    fn reverse_health_reports_require_current_worker_dimensions() {
        let wire = serde_json::json!({
            "canaries": [],
            "boot_id": "9",
            "sequence": 2,
            "sampled_at_unix_ms": 10,
            "workers": [{
                "active_sessions": 1,
                "affected_sessions": 0,
                "control_queue_depth": 0,
                "queue_delay_ms": 0,
                "scheduler_lag_ms": 0,
                "worker_id": "11",
                "pair": "rev:portal:chain>peer",
                "role": "portal",
                "state": "READY",
                "reason": "validated",
                "ack_age_ms": 10,
                "rtt_ms": 5,
                "probes": 2,
                "acks": 2,
                "timeouts": 0,
                "rejected_dispatches": 0
            }],
            "events": []
        });
        assert!(serde_json::from_value::<ReverseHealthReport>(wire.clone()).is_ok());
        for pointer in [
            "/canaries",
            "/workers/0/active_sessions",
            "/workers/0/scheduler_lag_ms",
        ] {
            let mut incomplete = wire.clone();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            incomplete
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<ReverseHealthReport>(incomplete).is_err(),
                "missing {pointer} was accepted"
            );
        }
    }

    /// The wire shape is the contract. The tagged enum and protocol gate keep an incompatible
    /// parser from silently skipping the certificate. This test pins the tag so a rename cannot
    /// slip in as a serde detail.
    #[test]
    fn the_certificates_variant_serializes_as_a_tagged_enum() {
        let slot = NodeCertificateSlotMaterial {
            certificate_id: "cert-a".to_owned(),
            names: vec!["a.example.net".to_owned()],
            cert_pem: "CERT".to_owned(),
            key_pem: "KEY".to_owned(),
        };
        let material = NodeCertificateMaterial::SelfSigned {
            slots: [slot.clone(), slot],
        };
        let wire =
            serde_json::to_string(&DesiredStateResponse::Certificates(vec![material.clone()]))
                .unwrap();
        assert_eq!(
            wire,
            r#"{"t":"certificates","v":[{"track":"self-signed","slots":[{"certificate_id":"cert-a","names":["a.example.net"],"cert_pem":"CERT","key_pem":"KEY"},{"certificate_id":"cert-a","names":["a.example.net"],"cert_pem":"CERT","key_pem":"KEY"}]}]}"#
        );
        let parsed: DesiredStateResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed, DesiredStateResponse::Certificates(vec![material]));
    }

    #[test]
    fn e2e_probe_security_is_one_explicit_tagged_shape() {
        let current = serde_json::json!({
            "app_id": "app",
            "chain_id": "chain",
            "chain_name": "Chain",
            "ingress_id": "ingress",
            "dial_host": "127.0.0.1",
            "port": 443,
            "uuid": "00000000-0000-0000-0000-000000000000",
            "security": {
                "type": "tls",
                "value": {
                    "server_name": "node.example.net",
                    "pinned_peer_cert_sha256": null,
                    "flow": null
                }
            },
            "xhttp": null,
            "expected_exit_ips": []
        });
        let target: E2eProbeTarget = serde_json::from_value(current.clone()).unwrap();
        assert!(matches!(target.security, E2eProbeSecurity::Tls(_)));

        let mut contradictory = current;
        contradictory
            .as_object_mut()
            .unwrap()
            .insert("reality".to_owned(), serde_json::json!({}));
        assert!(serde_json::from_value::<E2eProbeTarget>(contradictory).is_err());
    }

    #[test]
    fn public_ca_material_has_one_current_certificate() {
        let certificate = NodeCertificateSlotMaterial {
            certificate_id: "cert-current".to_owned(),
            names: vec!["node.example.net".to_owned()],
            cert_pem: "CERT".to_owned(),
            key_pem: "KEY".to_owned(),
        };
        let wire =
            serde_json::to_string(&NodeCertificateMaterial::PublicCa { certificate }).unwrap();
        assert_eq!(
            wire,
            r#"{"track":"public-ca","certificate":{"certificate_id":"cert-current","names":["node.example.net"],"cert_pem":"CERT","key_pem":"KEY"}}"#
        );
    }

    #[test]
    fn host_facts_round_trip_the_complete_current_shape() {
        let host = HostFacts {
            kernel: "6.8.0".to_owned(),
            cpu_model: "Neoverse-N1".to_owned(),
            cores: 2,
            cpu_freq_max_mhz: Some(3000),
            cpu_governor: Some("schedutil".to_owned()),
            cc_algo: "bbr".to_owned(),
            available_cc: vec!["cubic".to_owned(), "bbr".to_owned()],
            default_qdisc: "fq".to_owned(),
            nic_qdisc: "fq".to_owned(),
            nic: "eth0".to_owned(),
            nic_mtu: Some(1500),
            mem_total_bytes: 1024,
            disk_total_bytes: 2048,
            disk_mount: None,
            disk_filesystem: None,
            disk_device: None,
            disk_read_only: None,
            conntrack_max: Some(262_144),
            ephemeral_port_low: None,
            ephemeral_port_high: None,
            ephemeral_port_capacity: None,
            sysctl_managed: true,
            arch: "aarch64".to_owned(),
            os_pretty: "Linux".to_owned(),
            virt: "KVM".to_owned(),
            rmem_max: 4096,
            wmem_max: 4096,
            somaxconn: 4096,
        };
        let parsed: HostFacts =
            serde_json::from_value(serde_json::to_value(&host).unwrap()).unwrap();
        assert_eq!(parsed, host);
    }

    #[test]
    fn load_samples_allow_unavailable_deep_diagnostics() {
        let parsed: LoadSample = serde_json::from_value(serde_json::json!({
            "window_start_unix_secs": 100,
            "window_end_unix_secs": 130,
            "has_gap": false,
            "cpu_user_pct": 1.0,
            "cpu_sys_pct": 2.0,
            "cpu_softirq_pct": 3.0,
            "cpu_peak_pct": 7.0,
            "cpu_steal_pct": 0.0,
            "load1": 0.2,
            "mem_available_bytes": 1024,
            "swap_used_bytes": 0,
            "oom_kills": 0,
            "disk_free_bytes": 2048,
            "disk_inode_free_pct": 99.0,
            "nic_rx_bps": 0,
            "nic_tx_bps": 0,
            "nic_rx_drop": 0,
            "nic_tx_drop": 0,
            "nic_err": 0,
            "conntrack_count": null,
            "uptime_secs": 10
        }))
        .unwrap();
        assert_eq!(parsed.cpu_detail, None);
        assert_eq!(parsed.memory_detail, None);
        assert_eq!(parsed.disk_detail, None);
        assert_eq!(parsed.network_detail, None);
    }

    #[test]
    fn network_detail_represents_unavailable_kernel_counters_as_none() {
        let parsed: NetworkDetailSample = serde_json::from_value(serde_json::json!({
            "tcp_curr_estab": 12,
            "tcp_inuse": 18
        }))
        .unwrap();
        assert_eq!(parsed.tcp_curr_estab, Some(12));
        assert_eq!(parsed.tcp_inuse, Some(18));
        assert_eq!(parsed.tcp_listen_drops, None);
        assert_eq!(parsed.udp_rcvbuf_errors, None);
        assert_eq!(parsed.ephemeral_port_capacity, None);
        assert_eq!(parsed.tcp_ephemeral_top_target_v4, None);
    }

    #[test]
    fn cpu_details_round_trip_explicitly_unavailable_io_pressure() {
        let detail = CpuDetailSample {
            iowait_pct: 1.0,
            load5: 0.2,
            load15: 0.1,
            pressure_some_pct: Some(0.0),
            io_pressure_some_pct: None,
            io_pressure_full_pct: None,
            procs_running: Some(1),
            procs_total: Some(10),
            context_switches_per_sec: Some(100),
            net_rx_softirqs_per_sec: Some(20),
            net_tx_softirqs_per_sec: Some(10),
            throttled_usec: Some(0),
            frequency_mhz: None,
            cores: Vec::new(),
        };
        let parsed: CpuDetailSample =
            serde_json::from_value(serde_json::to_value(&detail).unwrap()).unwrap();
        assert_eq!(parsed, detail);
    }
}

/// A current data-plane snapshot, with bounded recent transitions. Health is not a business canary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseHealthWorker {
    pub active_sessions: u32,
    pub affected_sessions: u32,
    pub control_queue_depth: u32,
    pub queue_delay_ms: i64,
    pub scheduler_lag_ms: i64,
    pub worker_id: String,
    pub pair: String,
    pub role: String,
    pub state: String,
    pub reason: String,
    pub ack_age_ms: i64,
    pub rtt_ms: i64,
    pub probes: u64,
    pub acks: u64,
    pub timeouts: u64,
    pub rejected_dispatches: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseHealthEvent {
    pub sequence: u64,
    pub at_unix_ms: i64,
    pub from: String,
    #[serde(flatten)]
    pub worker: ReverseHealthWorker,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseHealthReport {
    pub canaries: Vec<ReverseCanaryReport>,
    pub boot_id: String,
    pub sequence: u64,
    pub sampled_at_unix_ms: i64,
    pub workers: Vec<ReverseHealthWorker>,
    pub events: Vec<ReverseHealthEvent>,
}

/// Ordinary outbound Mux uses the same report/snapshot/event vocabulary as reverse health. Pool
/// counters are cumulative for one Xray picker instance; the browser derives deltas from pool_id
/// and never asks Xray to reset a data-plane counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxPoolConfigReport {
    pub concurrency: u32,
    pub prewarm_workers: u32,
    pub reuse_threshold: u32,
    pub max_probing_workers: u32,
    pub probe_interval_ms: i64,
    pub probe_timeout_ms: i64,
    pub idle_ttl_ms: i64,
    pub max_sessions_per_worker: u32,
    pub health_lease_ms: i64,
    pub confirm_timeout_ms: i64,
    pub recovery_successes: u32,
    pub session_end_timeout_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxPoolReport {
    pub pool_id: String,
    pub pair: String,
    pub role: String,
    pub kind: String,
    pub used: bool,
    pub draining: bool,
    pub config: MuxPoolConfigReport,
    pub active_sessions: u32,
    pub available_slots: u32,
    pub ready_workers: u32,
    pub total_workers: u32,
    pub dispatches: u64,
    pub active_reuses: u64,
    pub idle_reuses: u64,
    pub demand_dials: u64,
    pub rejected_dispatches: u64,
    pub probes: u64,
    pub acks: u64,
    pub timeouts: u64,
    pub workers_created_demand: u64,
    pub workers_created_warm: u64,
    pub workers_warm_ready: u64,
    pub workers_warm_failed: u64,
    pub workers_closed_idle_ttl: u64,
    pub workers_closed_probe: u64,
    pub workers_closed_capacity: u64,
    pub workers_closed_requests: u64,
    pub workers_closed_transport: u64,
    pub health_suspects: u64,
    pub health_recoveries: u64,
    pub health_draining: u64,
    pub health_queue_failures: u64,
    pub health_dial_throttled: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxWorkerReport {
    pub pool_id: String,
    pub worker_id: String,
    pub pair: String,
    pub role: String,
    pub kind: String,
    pub state: String,
    pub reason: String,
    pub phase: String,
    pub active_sessions: u32,
    pub affected_sessions: u32,
    pub available_slots: u32,
    pub lifetime_sessions: u32,
    pub ack_age_ms: i64,
    pub rtt_ms: i64,
    pub probes: u64,
    pub acks: u64,
    pub timeouts: u64,
    pub lease_remaining_ms: i64,
    pub control_queue_depth: u32,
    pub queue_delay_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxWorkerEvent {
    pub sequence: u64,
    pub at_unix_ms: i64,
    pub from: String,
    #[serde(flatten)]
    pub worker: MuxWorkerReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxReport {
    pub boot_id: String,
    pub sequence: u64,
    pub sampled_at_unix_ms: i64,
    pub pools: Vec<MuxPoolReport>,
    pub workers: Vec<MuxWorkerReport>,
    pub events: Vec<MuxWorkerEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseCanaryReport {
    pub freshness_budget_ms: i64,
    pub pair: String,
    pub state: String,
    pub reason: String,
    pub sampled_at_unix_ms: i64,
    pub latency_ms: i64,
    pub consecutive_successes: u64,
    pub first_ok_unix_ms: i64,
    pub stable_since_unix_ms: i64,
    pub last_failure_unix_ms: i64,
    pub attempts: u64,
    pub failures: u64,
}
