mod admin;
mod agent;
mod agent_release;
mod branding;
mod cert;
mod compile_cache;
mod console;
mod credentials;
mod deployment;
mod distribution;
mod draft;
mod egress_dns;
mod grant_automation;
mod grant_probe;
mod host_tuning;
mod input;
mod lifecycle;
mod load;
mod log_policy;
mod materialize;
mod notifications;
mod pg;
mod ping_probe;
mod probe;
mod provision;
mod public_ip;
mod quota;
mod realtime;
pub mod secrets;
mod serving;
mod settings;
mod subscription_client;
mod traffic;
mod tunnel_probe;
mod usage;
mod user_presence;
mod vpngate;
mod xray_release;

pub use admin::{
    AdminAuthState, AdminContext, AdminInitRequest, AdminInitResult, AdminLoginRequest,
    AdminLoginResult, AdminOperator, AdminRole, AdminSessionAuthentication, AuthenticatedAdmin,
    AuthenticatedUser, ChangeAdminPasswordRequest, CreateAdminOperatorRequest, IssuedAdminSession,
    IssuedAdminToken, IssuedUserDirectLogin, IssuedUserLogin, ResetAdminPasswordResult,
    SetUserPasswordRequest, SetUserPasswordResult, SystemInitRequest, SystemInitResult,
    UserDirectLoginRequest, ADMIN_SESSION_TTL_SECONDS, PUBLIC_OPERATOR_ID,
};
pub use agent::public_route_ip;
pub use agent::{AuthenticatedNode, IssuedNodeToken};
pub use agent_release::{AgentBuildInfo, AgentRelease, AgentReleaseScope};
pub use branding::{BrandingSettings, DEFAULT_SITE_NAME};
pub use brocade_deployment::protocol::{
    BinarySource, CreateDeploymentRequest, CreateDeploymentResult, CreateRollbackRequest,
    DeploymentCommandResult, DeploymentDetail, DeploymentList, DeploymentListItem,
    DeploymentWaveConfirmationRequest, DeploymentWaveConfirmationResult, E2eExitVerdict, E2eProbe,
    E2eProbeHysteria2, E2eProbeReality, E2eProbeRequest, E2eProbeResult, E2eProbeStatus,
    E2eProbeTarget, E2eProbeTargetList, HopLinkList, HopLinkSample, HopLinkView, HostFacts,
    IsolateDeploymentTargetRequest, IsolateNodeRequest, LinkHealth, LinkHealthRequest,
    LinkHealthResult, LinkProbe, LinkProbeRequest, LinkProbeResult, LinkProbeStatus,
    LoadReportRequest, LoadReportResult, LoadSample, NodeDesiredDeployment,
    NodeIsolationCommandResult, NodeLoadList, NodeLoadView, NodePingProbeLatestList,
    NodePingProbeLatestView, NodePingProbeList, NodePingProbeView, PhantunBinaries,
    PingProbeFamily, PingProbeFamilyLatest, PingProbeFamilySeries, PingProbeKind, PingProbePoint,
    PingProbeReportRequest, PingProbeReportResult, PingProbeSample, PingProbeSettings,
    PingProbeSkipReason, PingProbeTarget, PingProbeTargetLatest, PingProbeTargetSeries,
    ProbeTarget, ProbeTargetList, ProbeTransport, RealtimeTelemetryPolicy, ReportTargetResult,
    ReportedNodeState, RouteIpReport, TargetApplyResult, TargetConvergenceReport,
    UpdateRealtimeTelemetryPolicyRequest, UsageCounter, UsageDailyRow, UsageMonthlySummary,
    UsageMonthlyViewRow, UsageNodeBucket, UsageNodeSeries, UsageNodeSeriesList, UsageReportRequest,
    UsageReportResult, UsageSample, UsageSampleList, VpngateAdmissionDecision,
    VpngateAdmissionPolicy, VpngateCandidate, VpngateCountryPolicy, VpngateDesiredPool,
    VpngateDesiredState, VpngateIpIntelligenceAssignment, VpngateIpIntelligenceFailure,
    VpngateIpIntelligenceObservation, VpngateIpIntelligenceReport, VpngateIpNetwork,
    VpngateIpProvider, VpngateIpScore, VpngateManualSwitchCommand, VpngateManualSwitchResult,
    VpngateManualSwitchStatus, VpngateNetworkType, VpngatePoolReport, VpngateProbeAssignment,
    VpngateProbeMode, VpngateProbeReport, VpngateProbeSample, VpngateProbeStatus,
    VpngateProviderRiskRule, VpngateReconcileReport, VpngateRiskDecisionPolicy, VpngateTransport,
    XrayReleaseOffer, XrayReleaseOutcome, XrayReleaseReport,
};
pub use brocade_deployment::protocol::{
    HostNetworkTuning, DEFAULT_NIC_GRO_FLUSH_TIMEOUT_NS, DEFAULT_NIC_NAPI_DEFER_HARD_IRQS,
    MAX_NIC_GRO_FLUSH_TIMEOUT_NS, MAX_NIC_NAPI_DEFER_HARD_IRQS,
};
pub use cert::{
    CertDomain, CertDomainInput, CertGroup, CertificateDnsTarget, CertificateOrder,
    CertificateScanLock, CertificateSigningMethod, GroupCertificate, IssuedCertificate,
    NodeCertificateState, ACME_LETSENCRYPT, ACME_LETSENCRYPT_STAGING,
    DEFAULT_SELF_SIGNED_GROUP_NAME, SELF_SIGNED_DIRECTORY, SELF_SIGNED_INITIAL_POOL_SIZE,
};
pub use console::{
    ArtifactContent, ArtifactIndex, ArtifactIndexEntry, ClashHaitunLink, ClashSubscriptionUsage,
    CompileView, ConsoleEgressDnsPolicy, ConsoleInitialData, ConsoleSnapshot, CreateAppRequest,
    CreateChainRequest, CreateFrontRequest, CreateGrantRequest, CreateIngressRequest,
    CreateRealityIngressRequest, CreateTenantRequest, CreateUserRequest, DeleteFrontRequest,
    DeleteFrontResult, DeleteStepResult, DeploymentVerification, DynamicClashSubscription,
    DynamicSubscriptionFormat, FrontClientConfigState, FrontRouteAnalysisView, HopInRequest,
    HopWireRequest, ModelWriteResult, NodeAgentStateItem, NodeAgentStateList, PruneChainResult,
    PutStepRequest, RedactedModelSnapshot, RedactedSecret, RegisterWarpBindingRequest,
    RegisterWarpBindingResult, RemoveRetiredNodesRequest, RemoveRetiredNodesResult,
    RemoveWarpBindingRequest, RemoveWarpBindingResult, RevisionList, RevisionListItem,
    RotateUserUuidResult, SetUserAppQuotaRequest, SetUserAppQuotaResult, StepAcceptRequest,
    TenantList, TenantListItem, TransportRequest, UpdateNodeRequest, UpdateNodeResult,
    UpdateNodeStatusRequest, UpdateUserProfileRequest, UpdateUserStatusRequest,
    UpdateUserStatusResult, UpdateWarpBindingRequest, UpdateWarpBindingResult, UpsertAppResult,
    UpsertChainResult, UpsertExternalOutboundRequest, UpsertFrontResult, UpsertGrantResult,
    UpsertIngressResult, UpsertTenantResult, UpsertUserResult, UserAccountType, UserAppQuota,
    UserAppQuotaList, UserList, UserListItem, VerifyDeploymentRequest, WarpBindingRemoval,
    WiresRequest,
};
pub use credentials::{
    admin_session_token_hash, admin_token_display_prefix, admin_token_hash,
    enrollment_token_display_prefix, enrollment_token_hash, generate_admin_session_token,
    generate_admin_token, generate_enrollment_token, generate_node_token, generate_reality_keypair,
    generate_reality_short_id, generate_user_direct_login_token, generate_uuid_v4,
    generate_wireguard_keypair, is_reality_short_id, node_token_display_prefix, node_token_hash,
    reality_public_key, user_direct_login_token_hash, RealityKeypair, WireGuardKeypair,
    ADMIN_SESSION_TOKEN_LEN, ADMIN_SESSION_TOKEN_PREFIX, ADMIN_TOKEN_DISPLAY_PREFIX_LEN,
    ADMIN_TOKEN_LEN, ADMIN_TOKEN_PREFIX, ENROLLMENT_TOKEN_DISPLAY_PREFIX_LEN, ENROLLMENT_TOKEN_LEN,
    ENROLLMENT_TOKEN_PREFIX, NODE_TOKEN_BYTES, NODE_TOKEN_DISPLAY_PREFIX_LEN, NODE_TOKEN_HEX_LEN,
    NODE_TOKEN_LEN, NODE_TOKEN_PREFIX, REALITY_SHORT_ID_BYTES, REALITY_SHORT_ID_HEX_LEN,
    USER_DIRECT_LOGIN_TOKEN_LEN, USER_DIRECT_LOGIN_TOKEN_PREFIX, WIREGUARD_KEY_BASE64_LEN,
    WIREGUARD_KEY_BYTES,
};
pub use distribution::DistributionSettings;
pub use draft::{ApplyDraftResult, DraftPreview, ModelOp};
pub use grant_automation::{
    GrantAutomationOutcome, GrantAutomationStatus, GRANTS_AUTOMATION_ACTOR,
};
pub use grant_probe::{
    FrontCombinationProbeMember, FrontCombinationProbePlan, FrontCombinationProbeTarget,
    UserGrantProbePlan, UserGrantProbeTarget,
};
pub use lifecycle::{
    AbandonNodeRequest, NodeLifecyclePhase, NodeLifecycleState, NodeLifecycleTransitionResult,
};
pub use load::{
    LoadSeriesQuery, NodeLoadMetricView, NodeLoadOverviewSeries, NodeLoadOverviewView, NodeNicList,
    NodeNicSample, NodeNicView,
};
pub use log_policy::{
    AgentLogLimitOverrides, AgentLogLimits, AgentLogPolicyView, NodeLogPolicyItem,
    UpdateAgentLogDefaultRequest, UpdateNodeLogPolicyRequest, DEFAULT_AGENT_LOG_MAX_MIB,
    DEFAULT_PHANTUN_LOG_MAX_MIB, DEFAULT_XRAY_LOG_MAX_MIB, MAX_AGENT_LOG_MAX_MIB,
    MIN_AGENT_LOG_MAX_MIB,
};
pub use notifications::{
    ClaimedNotificationDelivery, MachineEventList, MachineEventView, MACHINE_EVENT_RETENTION_DAYS,
};
pub use pg::PgStore;
pub use ping_probe::{
    NodePingProbeColumnarView, PingProbeFamilyColumns, PingProbeTargetColumnarSeries,
};
pub use probe::{
    E2eExitIpIntelligence, E2eProbeItem, E2eProbeSampleSeries, LinkHealthItem, LinkMtuItem,
    LinkMtuView, NodeMtuItem,
};
pub use provision::{
    ProvisionNodeRequest, ProvisionNodeResult, ProvisionedNode, ProvisionedNodeEnrollment,
};
pub use public_ip::{
    NodePublicIpEventView, NodePublicIpHistory, NodePublicIpStateView, PublicIpObservationOutcome,
    RecordPublicIpObservationResult, PUBLIC_IP_EVENT_RETENTION_DAYS,
};
pub use quota::{QuotaEnforcementOutcome, QuotaEnforcementPlan, QuotaGrantChange, QUOTA_ACTOR};
pub use settings::{SettingsSnapshot, UpdateSettingsResult};
pub use subscription_client::{ClientConfigCommitResult, ClientConfigCommitStatus};
pub use traffic::{
    NodeTrafficCycleKind, NodeTrafficItem, NodeTrafficView, UpdateNodeTrafficRequest,
};
pub use tunnel_probe::{
    ClaimedTunnelProbe, TunnelProbeCompletion, TunnelProbeHealth, TunnelProbeJobStatus,
    TunnelProbeList, TunnelProbeListItem, TunnelProbePhase, TunnelProbePoint, TunnelProbePolicy,
    TunnelProbeResultStatus, TunnelProbeRun, TunnelProbeSource, TunnelProbeSummary,
    TunnelProbeTrigger, TunnelProbeView, UpdateTunnelProbePolicy, TUNNEL_PROBE_RETENTION_DAYS,
};
pub use user_presence::{
    UserOnlineSourceAccess, UserOnlineSourceHistory, UserOnlineSourceView, UserPresenceList,
    UserPresenceState, UserPresenceView, USER_ONLINE_SOURCE_HISTORY_LIMIT,
    USER_ONLINE_SOURCE_RETENTION_DAYS,
};
pub use vpngate::{
    RequestVpngatePoolSwitch, UpdateVpngateCatalogSettings, UpdateVpngateIntelligenceCredentials,
    UpdateVpngateIntelligenceNode, UpdateVpngateProbeNode, UpdateVpngateProbeSettings,
    VpngateCatalogStatus, VpngateCountrySummary, VpngateDirectoryFilter, VpngateDirectorySort,
    VpngateIntelligenceCredentialUpdateMode, VpngateIntelligenceCredentials,
    VpngateIntelligenceNodeSelection, VpngateIntelligencePolicy, VpngateIntelligenceRefreshMode,
    VpngateIntelligenceRefreshResult, VpngateIpIntelligenceClaim, VpngateOverview,
    VpngatePoolSwitchRequestView, VpngateProbeNodeAddress, VpngateProbeNodeOrigin,
    VpngateProbeNodeSelection, VpngateReportReceipt, VpngateRuntimeSelection, VpngateRuntimeView,
    VpngateServerInput, VpngateServerPage, VpngateServerPageRequest, VpngateServerView,
    VpngateStaleIntelligencePolicy, VpngateSyncBatch, VpngateSyncClaim, VpngateSyncHistoryPoint,
};
pub use xray_release::{
    CreateXrayReleaseRequest, XrayBuildInfo, XrayRelease, XrayReleaseArtifact,
    XrayReleaseAssignment, XrayReleaseEvent, XrayReleaseList, XrayReleaseStatus,
    XrayReleaseSummary, XrayReleaseTarget, XrayReleaseTargetStatus,
};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug)]
pub enum StoreError {
    Entropy(getrandom::Error),
    Json(serde_json::Error),
    Sqlx(sqlx::Error),
    Migrate(sqlx::migrate::MigrateError),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    Unavailable(String),
    InvalidData(String),
    Unsupported(String),
    PublishBlocked(brocade_core::compile::PublishBlocked),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Entropy(error) => write!(f, "entropy error: {error}"),
            StoreError::Json(error) => write!(f, "json error: {error}"),
            StoreError::Sqlx(error) => write!(f, "database error: {error}"),
            StoreError::Migrate(error) => write!(f, "migration error: {error}"),
            StoreError::NotFound(message) => write!(f, "not found: {message}"),
            StoreError::Unauthorized(message) => write!(f, "unauthorized: {message}"),
            StoreError::Forbidden(message) => write!(f, "forbidden: {message}"),
            StoreError::Conflict(message) => write!(f, "conflict: {message}"),
            StoreError::Unavailable(message) => write!(f, "unavailable: {message}"),
            StoreError::InvalidData(message) => write!(f, "invalid store data: {message}"),
            StoreError::Unsupported(message) => write!(f, "unsupported store operation: {message}"),
            StoreError::PublishBlocked(blocked) => write!(
                f,
                "publish blocked by compiler: {} error(s), {} warning(s)",
                blocked.summary.errors, blocked.summary.warnings
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<getrandom::Error> for StoreError {
    fn from(error: getrandom::Error) -> Self {
        StoreError::Entropy(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::Json(error)
    }
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        StoreError::Sqlx(error)
    }
}

impl From<sqlx::migrate::MigrateError> for StoreError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        StoreError::Migrate(error)
    }
}

impl From<brocade_core::compile::PublishBlocked> for StoreError {
    fn from(error: brocade_core::compile::PublishBlocked) -> Self {
        StoreError::PublishBlocked(error)
    }
}
