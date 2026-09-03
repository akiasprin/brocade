use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::{
    AppliedArtifactState, AppliedGrantsState, ConfigArtifact, DeploymentKind, DeploymentPlan,
    NodeDesiredState, PlannedAction,
};

/// Wire contract spoken by this agent build. Desired state is withheld from older protocols so
/// an agent never claims work whose fields or actions it cannot interpret.
///
/// Bumped with the `DesiredStateResponse` change: an older agent deserializes the whole
/// response body as a `NodeDesiredDeployment`, and an enum-shaped body fails that outright —
/// which is deliberate. An agent that silently ignored the `certificate` field would read as
/// converged while never writing the file.
pub const AGENT_PROTOCOL_VERSION: u32 = 3;
pub const MIN_AGENT_PROTOCOL_VERSION: u32 = 3;

/// Runtime log-retention bounds shared by the control-plane validator and the agent. MiB is
/// intentional: the values shown to operators map exactly to disk allocation in binary units.
pub const DEFAULT_AGENT_LOG_MAX_MIB: u32 = 100;
pub const MIN_AGENT_LOG_MAX_MIB: u32 = 16;
pub const MAX_AGENT_LOG_MAX_MIB: u32 = 4096;

/// Live telemetry is an operational stream, not another diagnostic or accounting cadence.
/// These are deliberately the only accepted values so the control plane can bound fan-out and
/// memory use while still giving the operator a genuinely live view.
pub const DEFAULT_REALTIME_INTERVAL_SECS: u32 = 1;
pub const REALTIME_INTERVAL_OPTIONS: &[u32] = &[1, 2, 5];

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
    pub sequence: u64,
    pub sampled_at_unix_millis: i64,
    pub elapsed_millis: u32,
    pub interface: String,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub has_gap: bool,
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
    pub wave: u32,
    pub actions: Vec<PlannedAction>,
    /// Immutable ownership map for the Xray counters created by this work order.  It advances
    /// only after the agent has converged the target, so a reading queued before a permission or
    /// topology change is never interpreted through the model that happened to be current when
    /// it was replayed.
    #[serde(default)]
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
    #[serde(default)]
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
    /// A deployment is owed. `certificate` is set when the cert check found drift, so the
    /// deploy path always carries the dependency confirmation with it.
    Deployment {
        deployment: NodeDesiredDeployment,
        certificate: Option<NodeCertificateMaterial>,
    },
    /// No deployment is owed but the certificate is missing or stale.
    Certificate(NodeCertificateMaterial),
}

/// What a node reports about the certificate it actually holds.
///
/// # Why three states rather than an `Option`
///
/// `None` would carry two meanings, that the agent checked and found nothing and that the agent
/// is too old to check, and the two require opposite responses. The first is a fault to report;
/// the second is a machine that predates the feature, where reporting a fault would be incorrect.
/// The default is `Unmanaged` for the same reason `ReportedNodeState::phantun` defaults that way:
/// an older agent must not be classified as having lost something it never held.
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
    /// Not reported. An agent from before this existed, and nothing to conclude from it.
    #[default]
    Unmanaged,
    /// Looked, and there is no certificate on disk. Normal before the first one arrives; a fault
    /// afterwards, and the one this whole report exists to make visible.
    Absent,
    /// The sha256 of `cert.pem` as it is on disk, lowercase hex.
    Present { sha256: String },
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
pub struct NodeCertificateMaterial {
    /// Both names the certificate covers: the wildcard and the bare label. Sent so the agent can
    /// record what it holds without parsing the certificate, and so a person reading the desired
    /// state can see which names are in play.
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
        f.debug_struct("NodeCertificateMaterial")
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
    pub result: TargetApplyResult,
    pub observed_before: ReportedNodeState,
    pub observed_after: ReportedNodeState,
    pub error: Option<String>,
    /// Agent-clock instant immediately before applying the counter namespace change. It is the
    /// lower boundary for a newly authorized label whose first Xray counter starts at zero.
    #[serde(default)]
    pub usage_activated_at_unix_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteIpReport {
    #[serde(default)]
    pub ipv4: Option<String>,
    #[serde(default)]
    pub ipv6: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetApplyResult {
    Applied,
    FailedRecovered,
    FailedDirty,
}

fn unmanaged_state() -> AppliedArtifactState {
    AppliedArtifactState::Unmanaged
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedNodeState {
    /// An older agent does not report this field. The Unmanaged default means the agent does not
    /// manage the artifact rather than that the artifact is absent; the latter would make the
    /// control plane classify it as drift and push an action every round that the agent never
    /// performs.
    #[serde(default = "unmanaged_state")]
    pub phantun: AppliedArtifactState,
    pub wireguard: AppliedArtifactState,
    pub xray: AppliedArtifactState,
    #[serde(default = "unmanaged_state")]
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
    #[serde(default)]
    pub geoip: Option<GeodataFileState>,
    #[serde(default)]
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
    #[serde(default)]
    pub sync_deployment_id: Option<i64>,
    #[serde(default)]
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
    pub active: Option<bool>,
    pub actor: Option<String>,
    // Configuration or grants. The two have to be distinguishable in the list, because they
    // differ in cost by an order of magnitude: restarting xray on three machines and adding one
    // account to a list must not appear the same.
    #[serde(default)]
    pub kind: DeploymentKind,
    pub note: Option<String>,
    // Which version it changed from. Fixed at creation and never recomputed, because rollbacks
    // make a retrospective calculation incorrect. None means this kind has never been deployed
    // successfully.
    #[serde(default)]
    pub base_revision_id: Option<u64>,
    #[serde(default)]
    pub rollback_of_deployment_id: Option<i64>,
    #[serde(default)]
    pub sync_of_deployment_id: Option<i64>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub total_targets: u64,
    pub changed_targets: u64,
    pub skipped_targets: u64,
    pub failed_targets: u64,
    pub disruptive_targets: u64,
    pub max_wave: u32,
    // Waiting on an operator rather than on machines. A destructive wave requires a
    // confirmation before it continues, and the two kinds of pause are otherwise identical in
    // the list: the deployment shows as pushing and the header reports machines pending, with
    // nothing indicating that the operator is the blocking party. Older data deserializes to
    // false.
    #[serde(default)]
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
    pub active: Option<bool>,
    pub actor: Option<String>,
    pub note: Option<String>,
    // The detail page uses it as the baseline for artifact diffs. See the field of the same name
    // on DeploymentListItem.
    #[serde(default)]
    pub base_revision_id: Option<u64>,
    pub warnings: Value,
    pub created_at: String,
    pub started_at: Option<String>,
    pub halted_at: Option<String>,
    pub finished_at: Option<String>,
    pub rollback_of_deployment_id: Option<i64>,
    #[serde(default)]
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
pub struct AgentObservationRequest {
    pub deployment_id: i64,
    pub result: TargetApplyResult,
    pub observed_before: ReportedNodeState,
    pub observed_after: ReportedNodeState,
    pub error: Option<String>,
    #[serde(default)]
    pub route: Option<RouteIpReport>,
    /// See [`TargetConvergenceReport::usage_activated_at_unix_secs`]. Optional for rolling
    /// compatibility with agents that predate first-window accounting.
    #[serde(default)]
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
    #[serde(default)]
    pub agent_instance_id: Option<String>,
    /// Persisted before sampling. Gaps are allowed; reuse and reversal are not.
    #[serde(default)]
    pub sequence: Option<u64>,
    /// The frozen ownership map active when the counters were read.
    #[serde(default)]
    pub usage_generation_id: Option<i64>,
    pub read_at_unix_secs: i64,
    pub xray_started_at_unix_secs: i64,
    /// Boot time plus the serving process's exact start ticks. Unlike the rounded unix second,
    /// this changes for two Xray processes started within the same second.
    #[serde(default)]
    pub xray_epoch: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub agent_instance_id: Option<String>,
    #[serde(default)]
    pub sequence: Option<u64>,
    #[serde(default)]
    pub usage_generation_id: Option<i64>,
    /// True when the control plane returned the durable result of an already committed report.
    #[serde(default)]
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
    #[serde(default)]
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
/// Months are the calendar months of the control plane's local zone (+08), decided server-side and
/// never sent by the UI.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMonthlySummary {
    /// A +08 wall-clock string of the form "2026-08-01 00:00:00" that does not vary with the
    /// database session's timezone. Do not populate it from a timestamptz's text form: under a
    /// session that is not +08 the month is rendered incorrectly.
    pub month_start: String,
    pub month_end: String,
    pub views: Vec<UsageMonthlyViewRow>,
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
    pub reality: E2eProbeReality,
    /// Present for a Hysteria 2 ingress. It supersedes both `reality` and `tls` and lets an older
    /// agent fail only this new target rather than reject the whole work list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hysteria2: Option<E2eProbeHysteria2>,
    /// Present for an AnyTLS ingress. It supersedes both `reality` and `tls` and carries the
    /// certificate name used by the AnyTLS client. The AnyTLS server sends its padding scheme
    /// during the session handshake, so no client-side padding copy is needed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anytls: Option<E2eProbeAnyTls>,
    /// Present when this ingress presents its own certificate, in which case `reality` above is
    /// placeholder data and is ignored.
    ///
    /// Added alongside `reality` rather than replacing it, deliberately: the control plane and
    /// the agents are upgraded at different times, and a machine running an older agent has to
    /// keep probing the chains it already probes rather than fail on a field it cannot parse. An
    /// older agent skips this field and builds a REALITY client for a TLS ingress, which fails,
    /// but only for ingresses that cannot exist on a machine old enough to lack the field, since
    /// presenting a certificate requires the agent that fetches one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<E2eProbeTls>,
    /// Present when the ingress is carried inside HTTP, in which case the probe's client has to
    /// be as well. Absent, the probe dials TCP, which is what it did for every target before this
    /// field existed, and why an XHTTP ingress reported as down while carrying traffic normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp: Option<E2eProbeXhttp>,
    /// Which machines this chain may exit from. The IP the endpoint saw must fall in this set to
    /// agree.
    ///
    /// Empty means the check cannot be made, because every exit is behind NAT. That outcome is
    /// reported explicitly rather than counted as a pass; otherwise a misconfigured chain would
    /// report as healthy only because the check could not run.
    pub expected_exit_ips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeAnyTls {
    /// The name on the machine's certificate. The client verifies it as ordinary TLS.
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eProbeHysteria2 {
    pub server_name: String,
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
    /// family, which disqualifies the machine's other family too — or has no public address at
    /// all. It is its own outcome so that an unrunnable check is not reported as a verified one.
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
// and these three were never sent by it.
//
// They use their own low-frequency endpoint rather than being folded into observations. An
// observation carries a `deployment_id` and exists only during a release, whereas these three
// matter most when nothing is being released: a machine that has not deployed for a month is the
// one most likely to have drifted undetected.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRuntimeReport {
    /// When this snapshot finished being collected on the node. Older agents omit it; the control
    /// plane then falls back to receipt time. New agents supply it so a delayed older snapshot
    /// cannot overwrite runtime state observed later.
    #[serde(default)]
    pub observed_at_unix_secs: Option<i64>,
    pub versions: NodeVersions,
    /// Which certificate this machine is actually holding.
    ///
    /// Here for the same reason as the geodata field below: it is an observation unrelated to a
    /// release. Reported through `ReportedNodeState` instead, a machine that does not deploy for
    /// a month would leave its certificate state unknown for a month, and the certificate is the
    /// only value here with an expiry.
    #[serde(default)]
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
    #[serde(default)]
    pub geodata: Option<GeodataObservation>,
    /// What the last local reconcile did. `None` where none ever ran.
    #[serde(default)]
    pub local_reconcile: Option<LocalReconcileReport>,
    pub spool: SpoolBacklog,
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
    /// The sha256 of the running agent binary, lowercase hex, rather than a version number.
    ///
    /// It was previously `CARGO_PKG_VERSION`, which identified nothing: the workspace version is
    /// not incremented per deployment, so every build since it was last changed reported the same
    /// string. Identifying the exact binary is what both self-update and a fleet rollout require,
    /// so the field carries the value that provides it. The control plane computes its side at
    /// compile time (`brocade-console/build.rs`), so the two compare directly.
    ///
    /// Two other values appear here. `unknown` means the agent could not read `/proc/self/exe`.
    /// A value of the form `0.1.0` comes from an agent predating this change; old agents keep
    /// reporting and their values still have to be interpreted.
    pub agent: String,
    /// The first line of `xray version`. Unreadable does not mean absent: the binary may simply
    /// not be on PATH.
    ///
    /// This field has a specific use: `geodata` auto-update reached xray's main branch on
    /// 2026-04-25 (XTLS/Xray-core#5992), and earlier versions ignore that section without
    /// reporting anything. Without this field, a machine whose .dat never updates and a machine
    /// that cannot reach the download source are indistinguishable in every other field.
    #[serde(default)]
    pub xray: Option<String>,
    #[serde(default)]
    pub phantun: Option<String>,
    /// `wg --version`. Below wireguard-tools 1.0.20200121 there is no `wg syncconf`, which is the
    /// only second-rung remedy that does not interrupt sessions. Without it, every drift
    /// escalates to restarting the interface, which drops every session on that machine.
    #[serde(default)]
    pub wg_tools: Option<String>,
    /// `kernel` or `userspace`.
    ///
    /// `None` means WireGuard is disabled locally, or an older agent could not observe it.
    ///
    /// Kernels 5.6 and above have WireGuard built in; without it `wg-quick` falls back to
    /// `wireguard-go` or `boringtun`. Their `wg show` output is identical while throughput
    /// differs by an order of magnitude, so the backend has to be queried explicitly.
    #[serde(default)]
    pub wg_backend: Option<String>,
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
    #[serde(default)]
    pub error: Option<String>,
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
    /// `/proc/sys/kernel/osrelease`. Nothing reported this previously, while two decisions depend
    /// on it: WireGuard is in-kernel from 5.6, and BBR exists from 4.9. `NodeVersions` could
    /// report kernel versus userspace but not which kernel, so the measurement and the conclusion
    /// were separated.
    pub kernel: String,
    /// Human-readable processor model. New agents read it from `/proc/cpuinfo`; an empty value is
    /// valid for older agents and for architectures whose kernel exposes no model identity.
    #[serde(default)]
    pub cpu_model: String,
    pub cores: u32,
    /// Maximum frequency exposed by cpufreq, in MHz. Virtual machines commonly expose no cpufreq
    /// tree at all; `None` means unsupported, not a zero-frequency processor.
    #[serde(default)]
    pub cpu_freq_max_mhz: Option<u64>,
    /// The common scaling governor across online CPUs. Empty/mixed governors are represented as
    /// `None`; this is a capability detail rather than an alarm.
    #[serde(default)]
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
    /// `None` keeps reports from older agents and hosts with an unreadable sysfs usable.
    #[serde(default)]
    pub nic_mtu: Option<u32>,
    pub mem_total_bytes: u64,
    /// The filesystem holding the state directory, which is where the spool is written.
    pub disk_total_bytes: u64,
    /// Identity of the filesystem whose capacity is reported above. Overlay/container filesystems
    /// do not always have a block device, so every field remains optional independently.
    #[serde(default)]
    pub disk_mount: Option<String>,
    #[serde(default)]
    pub disk_filesystem: Option<String>,
    #[serde(default)]
    pub disk_device: Option<String>,
    #[serde(default)]
    pub disk_read_only: Option<bool>,
    /// `nf_conntrack_max`. `None` means the module is not loaded, which is not a fault: a machine
    /// doing no NAT simply has no such table.
    #[serde(default)]
    pub conntrack_max: Option<u64>,
    /// Kernel-selected anonymous local-port range after subtracting
    /// `ip_local_reserved_ports`. Stored with host facts for explanation; each network sample also
    /// carries the contemporaneous capacity so a later sysctl change cannot rewrite history.
    #[serde(default)]
    pub ephemeral_port_low: Option<u16>,
    #[serde(default)]
    pub ephemeral_port_high: Option<u16>,
    #[serde(default)]
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
    #[serde(default)]
    pub io_pressure_some_pct: Option<f32>,
    #[serde(default)]
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
#[serde(default)]
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
#[serde(default)]
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
    /// Absent on older agents. Missing and zero are intentionally distinct throughout the stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_detail: Option<CpuDetailSample>,

    /// `MemAvailable` rather than `free`. On any machine with a page cache `free` is always
    /// small, so using it reports every healthy machine as low on memory.
    pub mem_available_bytes: u64,
    pub swap_used_bytes: u64,
    /// Absent on older agents. Contains composition, reclaimability and pressure diagnostics.
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
    /// Absent on older agents. Filesystems without a local block-device view still carry an
    /// object whose device-specific fields are `None`, preserving capacity/inode drill-down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_detail: Option<DiskDetailSample>,

    pub nic_rx_bps: u64,
    pub nic_tx_bps: u64,
    /// Drops and errors, differenced. Byte counts are excluded, because usage measures those with
    /// per-label attribution. What usage cannot measure is what was dropped.
    pub nic_rx_drop: u64,
    pub nic_tx_drop: u64,
    pub nic_err: u64,

    #[serde(default)]
    pub conntrack_count: Option<u64>,
    /// Absent on older agents. Socket levels plus differenced TCP/UDP kernel counters.
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
    #[serde(default)]
    pub rss_bytes: Option<u64>,
    #[serde(default)]
    pub cpu_pct: Option<f32>,
    /// Unix seconds. A change means the process restarted, which nothing else reports, even
    /// though usage already tracks xray's start time for its own counter-reset check.
    #[serde(default)]
    pub started_at_unix_secs: Option<i64>,
    #[serde(default)]
    pub fds: Option<u64>,
    /// `RLIMIT_NOFILE`. Reaching it does not terminate xray; it makes xray refuse new
    /// connections, so the process stays up, the logs stay quiet, and users cannot connect.
    #[serde(default)]
    pub fd_limit: Option<u64>,
}

/// One hop's line quality for one window, aggregated from every TCP connection carrying it.
///
/// Keyed `(chain_id, peer_node_id)`, the same key `link_health` uses and for the same reason:
/// the measurement covers this machine's **outbound** leg towards its next hop, and the agent
/// already derives that pair from xray's outbound tags (`out:{app}/{chain}>{to}`, see
/// `probe.rs::hop_of_tag`).
///
/// Not `hop_label`, which was the first attempt and was incorrect. That label is
/// `steps.accept_label`, an **inbound** identity: on the leg hk-01 → sg-02 the label belongs to
/// sg-02 while the measurement is taken on hk-01, so the two never share a `node_id` and joining
/// them aligns rows describing different machines.
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
    #[serde(default)]
    pub btlbw_p50_bps: Option<u64>,
    #[serde(default)]
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
    /// `None` means this machine has never reported, which is distinct from reporting zeros. The
    /// UI has to state that explicitly: a newly enrolled machine, or one running an agent from
    /// before this feature, is not a machine with a fault.
    #[serde(default)]
    pub reported_at_unix_secs: Option<i64>,
    /// The reporting agent's clock minus the control plane's, in seconds, measured when the
    /// latest report arrived. It can only be measured at receipt — `read_at` reads the agent's
    /// clock, which is gone afterwards. Rounds past ±600s are rejected outright, so a stored
    /// value is always inside that range.
    pub clock_skew_secs: Option<i64>,
    #[serde(default)]
    pub host: Option<HostFacts>,
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

    /// The wire shape is the contract. The enum serializes tagged, so an older agent's
    /// `NodeDesiredDeployment` parser fails loudly on it instead of silently skipping the
    /// certificate — which is exactly what the protocol version gate exists to prevent. This
    /// test pins the tag so a rename cannot slip in as a serde detail.
    #[test]
    fn the_certificate_variant_serializes_as_a_tagged_enum() {
        let material = NodeCertificateMaterial {
            names: vec!["*.a.example.net".to_owned(), "a.example.net".to_owned()],
            cert_pem: "CERT".to_owned(),
            key_pem: "KEY".to_owned(),
        };
        let wire =
            serde_json::to_string(&DesiredStateResponse::Certificate(material.clone())).unwrap();
        assert_eq!(
            wire,
            r#"{"t":"certificate","v":{"names":["*.a.example.net","a.example.net"],"cert_pem":"CERT","key_pem":"KEY"}}"#
        );
        let parsed: DesiredStateResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed, DesiredStateResponse::Certificate(material));
    }

    #[test]
    fn host_facts_from_an_older_agent_default_the_cpu_model() {
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
        let mut wire = serde_json::to_value(host).unwrap();
        wire.as_object_mut().unwrap().remove("cpu_model");
        wire.as_object_mut().unwrap().remove("cpu_freq_max_mhz");
        wire.as_object_mut().unwrap().remove("cpu_governor");
        wire.as_object_mut().unwrap().remove("disk_mount");
        wire.as_object_mut().unwrap().remove("disk_filesystem");
        wire.as_object_mut().unwrap().remove("disk_device");
        wire.as_object_mut().unwrap().remove("disk_read_only");
        wire.as_object_mut().unwrap().remove("ephemeral_port_low");
        wire.as_object_mut().unwrap().remove("ephemeral_port_high");
        wire.as_object_mut()
            .unwrap()
            .remove("ephemeral_port_capacity");
        let parsed: HostFacts = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed.cpu_model, "");
        assert_eq!(parsed.cpu_freq_max_mhz, None);
        assert_eq!(parsed.cpu_governor, None);
        assert_eq!(parsed.disk_mount, None);
        assert_eq!(parsed.ephemeral_port_capacity, None);
        assert_eq!(parsed.cores, 2);
    }

    #[test]
    fn load_samples_from_older_agents_have_no_deep_diagnostics() {
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
    fn network_detail_is_forward_compatible_with_partial_kernel_views() {
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
    fn cpu_details_from_the_previous_agent_default_missing_io_pressure() {
        let detail = CpuDetailSample {
            iowait_pct: 1.0,
            load5: 0.2,
            load15: 0.1,
            pressure_some_pct: Some(0.0),
            io_pressure_some_pct: Some(2.0),
            io_pressure_full_pct: Some(1.0),
            procs_running: Some(1),
            procs_total: Some(10),
            context_switches_per_sec: Some(100),
            net_rx_softirqs_per_sec: Some(20),
            net_tx_softirqs_per_sec: Some(10),
            throttled_usec: Some(0),
            frequency_mhz: None,
            cores: Vec::new(),
        };
        let mut wire = serde_json::to_value(detail).unwrap();
        wire.as_object_mut().unwrap().remove("io_pressure_some_pct");
        wire.as_object_mut().unwrap().remove("io_pressure_full_pct");
        let parsed: CpuDetailSample = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed.io_pressure_some_pct, None);
        assert_eq!(parsed.io_pressure_full_pct, None);
    }
}
