//! Request and response types for the console interfaces. Pure data with no logic — and
//! simultaneously the HTTP wire format (brocade-console serdes these structs directly), so
//! renaming a field is an interface change.
use std::net::IpAddr;

use serde::{de::Deserializer, Deserialize, Serialize};
use serde_json::Value;

use brocade_core::model::{
    AnyTls, Chain, DestMatch, Dns, DomainStrategy, EgressDnsResolution, ExternalOutboundProtocol,
    ExternalOutboundSecurity, Front, FrontStrategy, Grant, Hysteria2, IngressGuard, NodeConnection,
    Projection, RealityFallbackLimits, RealityFallbackMode, Rule, User, WgTransport, Xhttp,
};
use brocade_deployment::plan::{PlanSummary, PlannedTarget};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedSecret {
    pub redacted: bool,
}

pub type RedactedModelSnapshot = Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleSnapshot {
    pub snapshot: RedactedModelSnapshot,
    /// Machine-owned DNS policies, repeated beside the redacted model for the console's
    /// narrow TypeScript snapshot declaration. The canonical copy also lives in ModelSnapshot so
    /// drafts, historical revisions and rollback retain policies with no current chain reference.
    pub node_egress_dns: Vec<ConsoleEgressDnsPolicy>,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEgressDnsPolicy {
    pub node: String,
    pub position: u32,
    pub selector: DestMatch,
    pub resolution: EgressDnsResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompileView {
    pub revision: u64,
    pub summary: brocade_core::diagnostic::DiagnosticSummary,
    pub diagnostics: Vec<brocade_core::diagnostic::Diagnostic>,
    pub system: Value,
    pub apps: Value,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantList {
    pub tenants: Vec<TenantListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantListItem {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub created_revision: Option<u64>,
    pub node_count: u64,
    pub user_count: u64,
    pub operator_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAgentStateList {
    pub nodes: Vec<NodeAgentStateItem>,
}

/// An empty JSON object counts as absent.
///
/// The `runtime_versions`, `spool_backlog`, and `geodata_observed` columns are all
/// `NOT NULL DEFAULT '{}'`, so a newly enrolled machine is born with an empty object, while an
/// older agent leaves NULL. The two mean the same thing — never reported — and the UI should
/// not draw them two different ways.
pub(crate) fn non_empty_json(
    value: Option<Option<serde_json::Value>>,
) -> Option<serde_json::Value> {
    let value = value.flatten()?;
    match &value {
        serde_json::Value::Object(map) if map.is_empty() => None,
        serde_json::Value::Null => None,
        _ => Some(value),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAgentStateItem {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    pub public_ipv4: Option<String>,
    /// ISO alpha-2 country derived by the control plane from `public_ipv4` and its cached
    /// geoip.dat. It is response decoration, not model state, so the store initializes it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ipv4_country: Option<String>,
    pub public_ipv6: Option<String>,
    pub public_ipv4_nat: bool,
    pub public_ipv6_nat: bool,
    pub route_ipv4: Option<String>,
    pub route_ipv6: Option<String>,
    pub token_prefix: Option<String>,
    pub token_created_at: Option<String>,
    pub token_last_used_at: Option<String>,
    pub token_revoked_at: Option<String>,
    pub agent_version: Option<String>,
    /// Explicit wire compatibility, separate from the binary identity. `None` identifies an old
    /// agent which predates protocol negotiation and is being offered a rescue self-update.
    pub agent_protocol_version: Option<i32>,
    /// Runtime observations. `None` means this machine has never reported (an older
    /// agent, or freshly enrolled and not yet due) — which must stay distinct from "reported
    /// zero": the UI shows an em dash, not 0, or the machine most worth worrying about displays
    /// as the healthiest.
    pub runtime_versions: Option<serde_json::Value>,
    pub spool_backlog: Option<serde_json::Value>,
    pub last_local_reconcile: Option<serde_json::Value>,
    pub wireguard_health: Option<serde_json::Value>,
    pub runtime_reported_at: Option<String>,
    /// On-disk observations of the two `.dat` files. In the same row as `runtime_versions`
    /// (`node_agent_state`) — it is a periodic observation, not an artifact of a release; hung
    /// off releases, a machine that does not ship for a month leaves its rule-database state
    /// unknown for a month.
    pub geodata_observed: Option<serde_json::Value>,
    pub last_poll_at: Option<String>,
    pub last_usage_report_at: Option<String>,
    /// Exact result of the last committed idempotent usage round. Empty means the machine has
    /// never spoken protocol v3. The store may decorate the response with process-local findings;
    /// those fields are never written back to this JSON column.
    pub usage_last_result: Option<serde_json::Value>,
    pub usage_generation_id: Option<i64>,
    pub xray_started_at: Option<String>,
    /// `udp` or `fake_tcp`: how others dial this machine's wg port.
    pub wg_transport_kind: String,
    /// The TCP port under fake TCP; empty under `udp`.
    pub wg_fake_tcp_port: Option<u16>,
    /// The wg0 MTU this machine set for itself; `None` takes the global default.
    pub mtu: Option<u16>,
    /// What this machine set for itself out of the connection policy — each field `None`
    /// where it takes `settings.connection`. Not the resolved values: the detail view has
    /// to distinguish "this machine says 600" from "the fleet says 600", or an operator
    /// clearing a field cannot tell whether anything changed.
    pub connection: NodeConnection,
    /// Whether it is on the backbone. `false` generates no wg config and enters no `Link`; it
    /// can still relay as long as some chain opened a relay port on it, going over public
    /// addresses in that case (SystemNode in ir/system.rs).
    pub overlay: bool,
    /// This machine is permitted to exit. It is the test the compiler applies when appending a
    /// default action at a chain's end: a permitted end node gets `Egress`, one that is not gets
    /// `Block` plus a `step.no-egress` report.
    ///
    /// Not the same as "this chain exits here" — that looks for an actual `Egress` in the rule
    /// table (`exit_nodes` in physical/probe.rs). A relay cleared for egress may still only
    /// forward.
    ///
    /// Like `overlay` this is a model field rather than an observation the agent reports: the
    /// console reads the compiled view when editing this value (the one that goes through draft
    /// preview), and this copy is the fallback while compilation has not returned.
    pub egress_allowed: bool,
    /// This machine's resolver, and what it does with the answers. Both are model fields
    /// rather than agent observations, carried here so the detail view can show and edit
    /// them — until now neither was exposed anywhere but the provisioning form, which is
    /// why they could be set once and never changed.
    pub dns: Dns,
    pub domain_strategy: DomainStrategy,
    /// The decommission time. Non-null means retired; the machine remains in the plan, with a
    /// desired state of all four configuration artifacts off.
    pub retired_at: Option<String>,
    /// Operational progress of that intent. Unlike retired_at this is not part of model snapshots.
    pub lifecycle_phase: String,
    pub lifecycle_epoch: u64,
    pub lifecycle_deployment_id: Option<i64>,
    pub lifecycle_completed_at: Option<String>,
    pub lifecycle_last_error: Option<String>,
    pub applied: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactIndex {
    pub revision: u64,
    pub artifacts: Vec<ArtifactIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactIndexEntry {
    pub target_kind: String,
    pub target_id: String,
    pub artifact_kind: String,
    pub state: String,
    pub sha256: Option<String>,
    pub byte_len: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactContent {
    pub revision: u64,
    pub target_kind: String,
    pub target_id: String,
    pub artifact_kind: String,
    pub state: String,
    pub sha256: Option<String>,
    pub byte_len: Option<u64>,
    pub content: Option<String>,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionList {
    pub current_revision: u64,
    pub revisions: Vec<RevisionListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionListItem {
    pub id: u64,
    pub created_at: String,
    pub author: Option<String>,
    pub note: Option<String>,
    pub status: String,
    pub current: bool,
    pub has_snapshot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserList {
    pub users: Vec<UserListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserListItem {
    pub tenant_id: String,
    pub id: String,
    pub uuid: String,
    pub status: String,
    pub created_at: String,
    pub created_revision: Option<u64>,
}

/// A Clash subscription compiled from the current model for one active user. It is deliberately
/// a response value rather than a stored entity: the public endpoint builds one on every GET.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicClashSubscription {
    pub tenant_id: String,
    pub user_id: String,
    pub uuid: String,
    pub revision: u64,
    pub content: String,
    pub usage: ClashSubscriptionUsage,
}

/// Revocable bearer for the deliberately small Clash document consumed by Haitun's test bot.
/// The token is stored because an operator must be able to reopen the dialog and copy the same
/// active URL; unlike generated YAML, this operational grant is durable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClashHaitunLink {
    pub tenant_id: String,
    pub user_id: String,
    pub token: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClashSubscriptionUsage {
    pub upload_bytes: u64,
    pub download_bytes: u64,
    /// Present only when every app represented by the subscription has a quota. One unlimited
    /// app makes the combined subscription unlimited, so inventing a total would be misleading.
    pub total_bytes: Option<u64>,
    pub remaining_bytes: Option<u64>,
    pub reset_at: String,
    pub has_gap: bool,
}

// Per user × view traffic quotas. Calendar months, reset at the start of each.
// The granularity matches usage's monthly rollup grouping key, one quota row per consumption
// row.
//
// It stamps no revision: quotas affect no artifact, and changing one alters not a byte of what
// ships. So these entry points write the pool directly, entering
// neither drafts nor compilation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAppQuotaList {
    pub quotas: Vec<UserAppQuota>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAppQuota {
    pub tenant_id: String,
    pub user_id: String,
    pub app_id: String,
    pub limit_bytes: u64,
    pub updated_at: String,
    // The ingresses under this view revoked by quota enforcement. The UI has to tell this apart
    // from "never granted" — after a revocation those grant rows are gone, and without stating
    // why, the row reads "not yet granted", which is a lie.
    #[serde(default)]
    pub suspended_ingresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetUserAppQuotaRequest {
    pub tenant_id: String,
    pub user_id: String,
    pub app_id: String,
    // None cancels the quota (deleting the row), the same interaction as the machine MTU field
    // where clearing returns to the default. Using 0 to cancel would put "limited to 0 bytes"
    // and "unlimited" on one value, which the UI cannot ask apart.
    #[serde(default)]
    pub limit_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetUserAppQuotaResult {
    pub quota: Option<UserAppQuota>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentVerification {
    pub revision_id: u64,
    pub converged: bool,
    pub summary: PlanSummary,
    pub targets: Vec<PlannedTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_lifecycle: Option<crate::lifecycle::NodeLifecycleState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyDeploymentRequest {
    #[serde(default)]
    pub revision_id: Option<u64>,
    #[serde(default)]
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWriteResult {
    pub revision_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantRequest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertTenantResult {
    pub revision_id: u64,
    pub tenant: TenantListItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub tenant_id: String,
    pub id: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserStatusRequest {
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertUserResult {
    pub revision_id: u64,
    pub user: User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateUserUuidResult {
    pub revision_id: u64,
    pub user: User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateUserStatusResult {
    pub revision_id: u64,
    pub user: UserListItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGrantRequest {
    pub app_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub ingress_id: String,
    pub enabled: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertGrantResult {
    pub revision_id: u64,
    pub grant: Grant,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAppRequest {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertAppResult {
    pub revision_id: u64,
    pub app: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertExternalOutboundRequest {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol: ExternalOutboundProtocol,
    pub security: ExternalOutboundSecurity,
    #[serde(default)]
    pub note: Option<String>,
}

/// A completed provider registration ready to be committed as one machine binding.
///
/// This is intentionally not the public HTTP request: the console creates the key locally and
/// obtains the remaining fields from Cloudflare before handing the complete, verified record to
/// the store. Draft preview never constructs this type and therefore cannot perform registration
/// as a side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterWarpBindingRequest {
    pub outbound_id: String,
    pub node_id: String,
    pub device_id: String,
    pub account_id: String,
    pub access_token: String,
    pub private_key: String,
    pub peer_public_key: String,
    pub local_addresses: Vec<String>,
    pub reserved: Vec<u8>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterWarpBindingResult {
    pub revision_id: u64,
    /// Redacted machine binding. Its private key has already been removed.
    pub binding: Value,
}

/// Replaces the optional route overrides for one already-registered WARP machine identity.
/// `None` means inherit that field from the logical tunnel. Fields inherit independently except
/// `allowed_ips` and `domain_strategy`, which form one address-policy override and move together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateWarpBindingRequest {
    pub tenant_id: String,
    pub outbound_id: String,
    pub node_id: String,
    #[serde(default)]
    pub endpoint_address: Option<String>,
    #[serde(default)]
    pub endpoint_port: Option<u16>,
    #[serde(default)]
    pub mtu: Option<u16>,
    #[serde(default)]
    pub keep_alive: Option<u16>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
    #[serde(default)]
    pub no_kernel_tun: Option<bool>,
    #[serde(default)]
    pub domain_strategy: Option<String>,
    #[serde(default)]
    pub workers: Option<u16>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateWarpBindingResult {
    pub revision_id: u64,
    /// Safe machine binding fields only; provider token and WireGuard private key are absent.
    pub binding: Value,
}

/// Provider identity needed for the irreversible half of removing one machine registration.
///
/// This type deliberately has no serde implementation: the access token crosses only the
/// store/console process boundary and must never become an HTTP response by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct WarpBindingRemoval {
    pub device_id: String,
    pub access_token: String,
}

impl std::fmt::Debug for WarpBindingRemoval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WarpBindingRemoval")
            .field("device_id", &self.device_id)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

/// Commits the local half after Cloudflare has confirmed that the device is gone.
/// `expected_device_id` closes the small provider/database race: a delayed response may remove
/// only the identity it prepared, never a different registration later attached to the same node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWarpBindingRequest {
    pub tenant_id: String,
    pub outbound_id: String,
    pub node_id: String,
    pub expected_device_id: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWarpBindingResult {
    pub revision_id: u64,
    pub node_id: String,
    pub device_id: String,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChainRequest {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    #[serde(default)]
    pub subscription_country: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertChainResult {
    pub revision_id: u64,
    pub chain: Chain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFrontRequest {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub strategy: FrontStrategy,
    pub via: Vec<String>,
    #[serde(default)]
    pub external_via: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertFrontResult {
    pub revision_id: u64,
    pub front: Front,
}

/// The wire shape a caller asks for.
///
/// A near-mirror of [`Transport`](brocade_core::model::Transport) rather than the type itself,
/// for the same reason [`CreateRealityIngressRequest`] is not `Reality`: the REALITY keys are
/// generated here and never accepted from a caller, so the request carries the overrides and
/// this carries the shape.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TransportRequest {
    #[default]
    VlessReality,
    VlessRealityXhttp {
        xhttp: Xhttp,
    },
    VlessTls,
    VlessTlsXhttp {
        xhttp: Xhttp,
    },
}

impl<'de> Deserialize<'de> for TransportRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("transport 必须是对象"))?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("transport.kind 缺失"))?;
        let xhttp = || {
            object
                .get("xhttp")
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("transport.xhttp 缺失"))
                .and_then(|value| serde_json::from_value(value).map_err(serde::de::Error::custom))
        };
        match kind {
            "vless-reality" => Ok(Self::VlessReality),
            "vless-reality-xhttp" => Ok(Self::VlessRealityXhttp { xhttp: xhttp()? }),
            // `fingerprint` and `alpn` were briefly accepted here. The custom decoder
            // deliberately ignores those legacy keys so a stale browser can still save, while
            // the typed request and every new response stop advertising them as managed state.
            "vless-tls" => Ok(Self::VlessTls),
            "vless-tls-xhttp" => Ok(Self::VlessTlsXhttp { xhttp: xhttp()? }),
            other => Err(serde::de::Error::custom(format!(
                "未知的 transport.kind: {other}"
            ))),
        }
    }
}

/// What a caller asks an ingress to serve: either wire, or both.
///
/// Mirrors [`IngressWires`](brocade_core::model::IngressWires) rather than being it, for the same
/// reason [`TransportRequest`] mirrors `Transport`: the REALITY keys are generated here and never
/// accepted from a caller. "At least one" is checked in the deserializer, so no handler below has
/// to remember it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WiresRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vless: Option<TransportRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anytls: Option<AnyTls>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hysteria2: Option<Hysteria2>,
}

impl Default for WiresRequest {
    /// A bare create with no `wires` gets what every ingress was before there was a choice.
    fn default() -> Self {
        Self {
            vless: Some(TransportRequest::default()),
            anytls: None,
            hysteria2: None,
        }
    }
}

impl<'de> Deserialize<'de> for WiresRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            vless: Option<TransportRequest>,
            #[serde(default)]
            anytls: Option<AnyTls>,
            #[serde(default)]
            hysteria2: Option<Hysteria2>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.vless.is_none() && wire.anytls.is_none() && wire.hysteria2.is_none() {
            return Err(serde::de::Error::custom(
                "接入面至少要有一条线：wires.vless、wires.anytls 和 wires.hysteria2 不能都空着",
            ));
        }
        Ok(Self {
            vless: wire.vless,
            anytls: wire.anytls,
            hysteria2: wire.hysteria2,
        })
    }
}

impl WiresRequest {
    /// How the TCP half is spelled in storage, or `None` where there is none.
    pub fn vless_kind(&self) -> Option<&'static str> {
        self.vless.as_ref().map(TransportRequest::kind)
    }

    pub fn xhttp(&self) -> Option<&Xhttp> {
        self.vless.as_ref().and_then(TransportRequest::xhttp)
    }
}

impl TransportRequest {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::VlessReality => "vless-reality",
            Self::VlessRealityXhttp { .. } => "vless-reality-xhttp",
            Self::VlessTls => "vless-tls",
            Self::VlessTlsXhttp { .. } => "vless-tls-xhttp",
        }
    }

    pub fn xhttp(&self) -> Option<&Xhttp> {
        match self {
            Self::VlessReality | Self::VlessTls => None,
            Self::VlessRealityXhttp { xhttp } | Self::VlessTlsXhttp { xhttp } => Some(xhttp),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateIngressRequest {
    pub id: String,
    pub chain_id: String,
    pub node_id: String,
    pub bind: IpAddr,
    pub port: u16,
    #[serde(default)]
    pub front_id: Option<String>,
    pub reality: CreateRealityIngressRequest,
    /// The whole shape of the wire. Absent means the plain shape, which is what every ingress
    /// was before this field existed — so an older client's request keeps meaning what it meant.
    #[serde(default)]
    pub wires: WiresRequest,
    /// The outward projection. Like the other fields in this request it overwrites wholesale:
    /// absent means neither family is projected.
    ///
    /// A family's "no projection" is expressed by its whole absence (or `null`), not by an empty
    /// host. An empty host is turned back by `ingress.projection-blank` — an empty string left
    /// in the database can never afterwards be told apart from a half-filled one.
    #[serde(default)]
    pub projection: Projection,
    /// What this entrance refuses to carry. Absent means the four defaults on — a caller that does
    /// not mention it gets the protected shape rather than the open one, which is the only way
    /// round that is safe when the field is forgotten.
    #[serde(default)]
    pub guard: IngressGuard,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRealityIngressRequest {
    /// Explicit provenance of the fallback target. Optional only for callers predating this
    /// field; the write path infers global/custom from the submitted site in that case.
    #[serde(default)]
    pub fallback_mode: Option<RealityFallbackMode>,
    /// Optional for old callers, which are placed on the balanced preset when written.
    #[serde(default)]
    pub fallback_limits: Option<RealityFallbackLimits>,
    /// Whether the fallback may reach only the borrowed name. Absent means on, which is also what
    /// the column defaults to: a caller predating this field is protected rather than exposed.
    #[serde(default)]
    pub fallback_guard: Option<bool>,
    /// Blank takes the site from the global settings (settings.reality_site). A value written
    /// here overrides it for this ingress — different ingresses borrowing different sites is a
    /// deliberately retained capability.
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub server_names: Vec<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertIngressResult {
    pub revision_id: u64,
    pub ingress: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutStepRequest {
    #[serde(default)]
    pub accept: Option<StepAcceptRequest>,
    /// This chain's relay port on this machine. `None` leaves it alone (the same meaning as
    /// `accept`); turning it off requires an explicit `{"port":0}` — 0 is invalid in the port
    /// range anyway and is borrowed to mean "off".
    #[serde(default)]
    pub hop_in: Option<HopInRequest>,
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub note: Option<String>,
}

/// A relay port's request shape. Keys are not supplied by the caller, on the same line as
/// `HopWireRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HopInRequest {
    /// The listening port. `0` turns off this chain's relay port on this machine.
    ///
    /// Where it is only ever dialed over the overlay nobody cares what it is, yet one must still
    /// be supplied — it has to be stable (argued on `brocade_core::model::HopIn::port`). The UI
    /// picks a free one.
    pub port: u16,
    #[serde(default)]
    pub security: Option<HopWireRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepAcceptRequest {
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteStepResult {
    pub revision_id: u64,
    /// True only where something was really deleted. Deleting a step the database does not have
    /// is a no-op, the revision number falls back (the rollback logic in `commit_revision`), and
    /// `deleted` is false.
    pub deleted: bool,
    /// The node ids of steps removed by the cascade (including the deleted machine, the subtree
    /// reachable beneath it, and whatever the cleanup left unreachable from the head). Deleting
    /// the head yields the chain's whole member list.
    pub removed_steps: Vec<String>,
    /// The head was deleted: the whole chain (ingress, grants, chain declaration) went with
    /// it.
    pub chain_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneChainResult {
    pub revision_id: u64,
    /// The node ids of the steps removed. Empty means the chain had no stranded ones to begin
    /// with, and the revision number falls back (the rollback logic in `commit_revision`).
    pub removed_steps: Vec<String>,
}

/// A cascading delete's result as written, used inside the transaction. `changed` feeds
/// `commit_revision`.
#[derive(Debug, Default)]
pub(crate) struct DeleteStepOutcome {
    pub changed: bool,
    pub removed_steps: Vec<String>,
    pub chain_removed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateNodeRequest {
    #[serde(default)]
    pub tenant_id: Option<String>,
    // Many fields, and most callers change only one or two of them, hence Default — so that
    // tests and internal call sites need not copy out a dozen Nones.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub public_ipv4: Option<String>,
    #[serde(default)]
    pub public_ipv6: Option<String>,
    #[serde(default)]
    pub public_ipv4_nat: Option<bool>,
    #[serde(default)]
    pub public_ipv6_nat: Option<bool>,
    #[serde(default)]
    pub wg_listen_port: Option<u16>,
    #[serde(default)]
    pub api_port: Option<u16>,
    #[serde(default)]
    pub overlay: Option<bool>,
    #[serde(default)]
    pub egress_allowed: Option<bool>,
    #[serde(default)]
    pub dns: Option<Dns>,
    /// Absent means "this request did not mention it", as everywhere else on this form.
    #[serde(default)]
    pub domain_strategy: Option<DomainStrategy>,
    /// This machine's wg0 MTU. Passing 0 clears it (returning to the `settings.overlay.mtu`
    /// default).
    ///
    /// 0 rather than null signals the clear because `Option<u16>` cannot express both "not
    /// changing it this time" and "changing it to empty" in JSON — the same problem
    /// `public_ipv4` solves with `is_some()` as a switch, except that 0 is invalid in this
    /// range anyway (validated at 1000–9000) and can simply be borrowed.
    #[serde(default)]
    pub mtu: Option<u16>,
    /// This machine's connection policy overrides, submitted whole.
    ///
    /// One `Option` around the four rather than four separate ones, and that is what lets
    /// this form clear a field at all: absent means "this request did not mention the
    /// policy", present means "these four are the policy now", and a `null` inside it is a
    /// field going back to the global default. Four loose `Option<u32>`s could not say the
    /// last of those — the same bind that `mtu` solves with 0, except that 0 is a legal
    /// buffer size here (xray reads it as "no buffer") and cannot be borrowed as a signal.
    ///
    /// It matches how the card is edited: the four are shown and saved together, so
    /// submitting them together loses nothing.
    #[serde(default)]
    pub connection: Option<NodeConnection>,
    /// How others dial this wg port. `{"t":"udp"}` or
    /// `{"t":"fake_tcp","v":{"port":39743}}`. It only needs touching where an upstream sealed
    /// inbound UDP; see `WgTransport`.
    #[serde(default)]
    pub wg_transport: Option<WgTransport>,
    #[serde(default)]
    pub note: Option<String>,
}

/// A relay port's wire format as a request: it names only the kind, and the caller supplies
/// no keys.
///
/// Keeping it apart from the model's `HopWire` is deliberate. Letting a caller send keys
/// directly would require the console to display the private key before the site could be
/// edited — breaking the `redacted_value` line that private keys do not leave over HTTP (the
/// same line as node private keys not being returned from a provision response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum HopWireRequest {
    None,
    /// VLESS Encryption. The X25519 key pair is generated server-side.
    Encryption,
    /// REALITY. The key pair and short_id are generated server-side while the borrowed site
    /// comes from the operator — as with ingresses, the site is a business decision and the keys
    /// are not.
    Reality {
        dest: String,
        #[serde(default)]
        server_names: Vec<String>,
        #[serde(default)]
        fingerprint: Option<String>,
    },
    /// Shadowsocks 2022. The key is generated server-side like the other material: it is a
    /// secret with no business meaning, and a caller that supplies one is a caller that can
    /// supply a weak one.
    Shadowsocks2022,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNodeResult {
    pub revision_id: u64,
    pub node: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warp_removal_debug_never_prints_the_provider_token() {
        let removal = WarpBindingRemoval {
            device_id: "device-1".to_owned(),
            access_token: "provider-secret".to_owned(),
        };
        let debug = format!("{removal:?}");
        assert!(debug.contains("device-1"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("provider-secret"));
    }

    #[test]
    fn hysteria2_wires_request_has_a_typed_nested_payload() {
        let request: WiresRequest = serde_json::from_value(serde_json::json!({
            "hysteria2": {
                "bandwidth": { "up": "20 mbps", "down": "100 mbps" },
                "congestion": "brutal",
                "obfs": { "kind": "salamander", "password": "secret" },
                "masquerade": { "kind": "proxy", "url": "https://cover.example.net/" }
            }
        }))
        .unwrap();
        let Some(hysteria2) = request.hysteria2 else {
            panic!("wires 里应当有 hysteria2 那一半")
        };
        assert_eq!(hysteria2.bandwidth.up.as_deref(), Some("20 mbps"));
        assert!(matches!(
            hysteria2.obfs,
            Some(brocade_core::model::HysteriaObfs::Salamander { ref password })
                if password == "secret"
        ));

        let missing = serde_json::from_value::<TransportRequest>(serde_json::json!({
            "kind": "hysteria2"
        }));
        assert!(
            missing.is_err(),
            "missing settings must not silently use defaults"
        );
    }

    #[test]
    fn stale_tls_client_overrides_are_ignored_by_the_request_decoder() {
        let request: TransportRequest = serde_json::from_value(serde_json::json!({
            "kind": "vless-tls-xhttp",
            "fingerprint": "none",
            "alpn": "http1",
            "xhttp": { "path": "/probe" }
        }))
        .unwrap();

        let TransportRequest::VlessTlsXhttp { xhttp } = request else {
            panic!("expected TLS + XHTTP")
        };
        assert_eq!(xhttp.path, "/probe");
    }
}
