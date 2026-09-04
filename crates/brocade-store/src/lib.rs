mod admin;
mod agent;
mod agent_release;
mod branding;
mod cert;
mod console;
mod credentials;
mod deployment;
mod distribution;
mod draft;
mod egress_dns;
mod grant_automation;
mod grant_probe;
mod input;
mod lifecycle;
mod load;
mod log_policy;
mod materialize;
mod pg;
mod ping_probe;
mod probe;
mod provision;
mod quota;
mod realtime;
pub mod secrets;
mod serving;
mod settings;
mod subscription_client;
mod usage;

pub use admin::{
    AdminAuthState, AdminContext, AdminInitRequest, AdminInitResult, AdminLoginRequest,
    AdminLoginResult, AdminOperator, AdminRole, AuthenticatedAdmin, ChangeAdminPasswordRequest,
    CreateAdminOperatorRequest, IssuedAdminSession, IssuedAdminToken, ResetAdminPasswordResult,
    ADMIN_SESSION_TTL_SECONDS, PUBLIC_OPERATOR_ID,
};
pub use agent::{AuthenticatedNode, IssuedNodeToken};
pub use agent_release::{AgentBuildInfo, AgentRelease, AgentReleaseScope};
pub use branding::{BrandingSettings, DEFAULT_SITE_NAME};
pub use brocade_deployment::protocol::{
    BinarySource, CreateDeploymentRequest, CreateDeploymentResult, CreateRollbackRequest,
    DeploymentCommandResult, DeploymentDetail, DeploymentList, DeploymentListItem,
    DeploymentWaveConfirmationRequest, DeploymentWaveConfirmationResult, E2eExitVerdict, E2eProbe,
    E2eProbeHysteria2, E2eProbeReality, E2eProbeRequest, E2eProbeResult, E2eProbeStatus,
    E2eProbeTarget, E2eProbeTargetList, HopLinkList, HopLinkSample, HopLinkView, HostFacts,
    IsolateDeploymentTargetRequest, LinkHealth, LinkHealthRequest, LinkHealthResult, LinkProbe,
    LinkProbeRequest, LinkProbeResult, LinkProbeStatus, LoadReportRequest, LoadReportResult,
    LoadSample, NodeDesiredDeployment, NodeIsolationCommandResult, NodeLoadList, NodeLoadView,
    NodePingProbeList, NodePingProbeView, PhantunBinaries, PingProbePoint, PingProbeReportRequest,
    PingProbeReportResult, PingProbeSample, PingProbeSettings, PingProbeTarget,
    PingProbeTargetSeries, ProbeTarget, ProbeTargetList, ProbeTransport, RealtimeTelemetryPolicy,
    ReportTargetResult, ReportedNodeState, RestoreNodeServiceRequest, RouteIpReport,
    TargetApplyResult, TargetConvergenceReport, UpdateRealtimeTelemetryPolicyRequest, UsageCounter,
    UsageMonthlySummary, UsageMonthlyViewRow, UsageNodeBucket, UsageNodeSeries,
    UsageNodeSeriesList, UsageReportRequest, UsageReportResult, UsageSample, UsageSampleList,
};
pub use cert::{
    CertDomain, CertDomainInput, CertGroup, CertificateDnsTarget, CertificateOrder,
    CertificateScanLock, GroupCertificate, NodeCertificateState, ACME_LETSENCRYPT,
    ACME_LETSENCRYPT_STAGING,
};
pub use console::{
    ArtifactContent, ArtifactIndex, ArtifactIndexEntry, ClashHaitunLink, ClashSubscriptionUsage,
    CompileView, ConsoleEgressDnsPolicy, ConsoleSnapshot, CreateAppRequest, CreateChainRequest,
    CreateFrontRequest, CreateGrantRequest, CreateIngressRequest, CreateRealityIngressRequest,
    CreateTenantRequest, CreateUserRequest, DeleteStepResult, DeploymentVerification,
    DynamicClashSubscription, HopInRequest, HopWireRequest, ModelWriteResult, NodeAgentStateItem,
    NodeAgentStateList, PruneChainResult, PutStepRequest, RedactedModelSnapshot, RedactedSecret,
    RegisterWarpBindingRequest, RegisterWarpBindingResult, RemoveWarpBindingRequest,
    RemoveWarpBindingResult, RevisionList, RevisionListItem, RotateUserUuidResult,
    SetUserAppQuotaRequest, SetUserAppQuotaResult, StepAcceptRequest, TenantList, TenantListItem,
    TransportRequest, UpdateNodeRequest, UpdateNodeResult, UpdateNodeStatusRequest,
    UpdateUserStatusRequest, UpdateUserStatusResult, UpdateWarpBindingRequest,
    UpdateWarpBindingResult, UpsertAppResult, UpsertChainResult, UpsertExternalOutboundRequest,
    UpsertFrontResult, UpsertGrantResult, UpsertIngressResult, UpsertTenantResult,
    UpsertUserResult, UserAppQuota, UserAppQuotaList, UserList, UserListItem,
    VerifyDeploymentRequest, WarpBindingRemoval, WiresRequest,
};
pub use credentials::{
    admin_session_token_hash, admin_token_display_prefix, admin_token_hash,
    enrollment_token_display_prefix, enrollment_token_hash, generate_admin_session_token,
    generate_admin_token, generate_enrollment_token, generate_node_token, generate_reality_keypair,
    generate_reality_short_id, generate_uuid_v4, generate_wireguard_keypair, is_reality_short_id,
    node_token_display_prefix, node_token_hash, reality_public_key, RealityKeypair,
    WireGuardKeypair, ADMIN_SESSION_TOKEN_LEN, ADMIN_SESSION_TOKEN_PREFIX,
    ADMIN_TOKEN_DISPLAY_PREFIX_LEN, ADMIN_TOKEN_LEN, ADMIN_TOKEN_PREFIX,
    ENROLLMENT_TOKEN_DISPLAY_PREFIX_LEN, ENROLLMENT_TOKEN_LEN, ENROLLMENT_TOKEN_PREFIX,
    NODE_TOKEN_BYTES, NODE_TOKEN_DISPLAY_PREFIX_LEN, NODE_TOKEN_HEX_LEN, NODE_TOKEN_LEN,
    NODE_TOKEN_PREFIX, REALITY_SHORT_ID_BYTES, REALITY_SHORT_ID_HEX_LEN, WIREGUARD_KEY_BASE64_LEN,
    WIREGUARD_KEY_BYTES,
};
pub use distribution::DistributionSettings;
pub use draft::{ApplyDraftResult, DraftPreview, ModelOp};
pub use grant_automation::{
    GrantAutomationOutcome, GrantAutomationStatus, GRANTS_AUTOMATION_ACTOR,
};
pub use grant_probe::{UserGrantProbePlan, UserGrantProbeTarget};
pub use lifecycle::{
    AbandonNodeRequest, NodeLifecyclePhase, NodeLifecycleState, NodeLifecycleTransitionResult,
};
pub use log_policy::{
    AgentLogPolicyView, NodeLogPolicyItem, UpdateAgentLogDefaultRequest,
    UpdateNodeLogPolicyRequest, DEFAULT_AGENT_LOG_MAX_MIB, MAX_AGENT_LOG_MAX_MIB,
    MIN_AGENT_LOG_MAX_MIB,
};
pub use pg::PgStore;
pub use probe::{
    E2eProbeItem, E2eProbeSample, LinkHealthItem, LinkMtuItem, LinkMtuView, NodeMtuItem,
};
pub use provision::{
    ProvisionNodeRequest, ProvisionNodeResult, ProvisionedNode, ProvisionedNodeEnrollment,
};
pub use quota::{QuotaEnforcementOutcome, QuotaEnforcementPlan, QuotaGrantChange, QUOTA_ACTOR};
pub use settings::{SettingsSnapshot, UpdateSettingsResult};
pub use subscription_client::{ClientConfigCommitResult, ClientConfigCommitStatus};

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
