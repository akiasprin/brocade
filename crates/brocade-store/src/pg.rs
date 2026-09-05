use std::{str::FromStr, sync::Arc};

use brocade_core::{
    model::{IpFamily, ModelSettings, ModelSnapshot},
    physical::user::SubscriptionFilter,
};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    Connection, Executor, PgConnection, PgPool,
};

use crate::admin;
use crate::agent_release;
use crate::branding;
use crate::cert;
use crate::console;
use crate::deployment;
use crate::distribution;
use crate::grant_automation;
use crate::grant_probe;
use crate::load;
use crate::ping_probe;
use crate::probe;
use crate::provision;
use crate::quota;
use crate::settings;
use crate::usage;
use crate::{
    agent, materialize, AdminAuthState, AdminContext, AdminInitRequest, AdminInitResult,
    AdminLoginRequest, AdminLoginResult, AdminOperator, ArtifactContent, ArtifactIndex,
    AuthenticatedAdmin, AuthenticatedNode, ChangeAdminPasswordRequest, ClashHaitunLink,
    CompileView, ConsoleSnapshot, CreateAdminOperatorRequest, CreateAppRequest, CreateChainRequest,
    CreateDeploymentRequest, CreateDeploymentResult, CreateFrontRequest, CreateGrantRequest,
    CreateIngressRequest, CreateRollbackRequest, CreateTenantRequest, CreateUserRequest,
    DeleteStepResult, DeploymentCommandResult, DeploymentDetail, DeploymentList,
    DeploymentVerification, DeploymentWaveConfirmationResult, DynamicClashSubscription,
    E2eProbeItem, E2eProbeRequest, E2eProbeResult, E2eProbeTargetList, HopLinkList,
    IssuedAdminToken, IssuedNodeToken, IssuedUserLogin, LinkHealthItem, LinkHealthRequest,
    LinkHealthResult, LinkMtuView, LinkProbeRequest, LinkProbeResult, LoadReportRequest,
    LoadReportResult, LoadSeriesQuery, NodeAgentStateList, NodeDesiredDeployment, NodeLoadList,
    NodeLoadView, NodePingProbeList, NodePingProbeView, PingProbeReportRequest,
    PingProbeReportResult, PingProbeSettings, ProbeTargetList, ProvisionNodeRequest,
    ProvisionNodeResult, PruneChainResult, QuotaEnforcementOutcome, QuotaEnforcementPlan,
    RegisterWarpBindingRequest, RegisterWarpBindingResult, RemoveWarpBindingRequest,
    RemoveWarpBindingResult, ReportTargetResult, ResetAdminPasswordResult, Result, RevisionList,
    RotateUserUuidResult, SetUserAppQuotaRequest, SetUserAppQuotaResult, StoreError,
    TargetConvergenceReport, TenantList, UpdateNodeRequest, UpdateNodeResult, UpdateSettingsResult,
    UpdateUserProfileRequest, UpdateUserStatusRequest, UpdateUserStatusResult,
    UpdateWarpBindingRequest, UpdateWarpBindingResult, UpsertAppResult, UpsertChainResult,
    UpsertFrontResult, UpsertGrantResult, UpsertIngressResult, UpsertTenantResult,
    UpsertUserResult, UsageMonthlySummary, UsageNodeSeriesList, UsageReportRequest,
    UsageReportResult, UsageSampleList, UserAppQuotaList, UserGrantProbePlan, UserList,
    VerifyDeploymentRequest, WarpBindingRemoval,
};
use brocade_deployment::plan::DeploymentKind;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    usage_runtime: Arc<usage::UsageRuntimeState>,
}

/// SQLSTATE for "the database named in the connection string is not on this server".
const INVALID_CATALOG_NAME: &str = "3D000";
/// SQLSTATE for "it already exists" — here, somebody else created it between the probe and the
/// `CREATE`.
const DUPLICATE_DATABASE: &str = "42P04";
/// Where to connect in order to issue `CREATE DATABASE`, which cannot be run from inside the
/// database being created. Every PostgreSQL server has `postgres` unless somebody dropped it, and
/// `template1` cannot be dropped at all, so the pair covers servers that have been tidied up.
const MAINTENANCE_DATABASES: &[&str] = &["postgres", "template1"];

/// The SQLSTATE a failure carries, where it came from the server at all.
fn sqlstate(error: &sqlx::Error) -> Option<String> {
    match error {
        sqlx::Error::Database(error) => error.code().map(|code| code.into_owned()),
        _ => None,
    }
}

impl PgStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self {
            pool,
            usage_runtime: Arc::new(usage::UsageRuntimeState::default()),
        })
    }

    /// Create the database named in `database_url` if the server does not have it, returning its
    /// name where one was created and `None` where it was already there.
    ///
    /// # Why the caller is told which
    ///
    /// Creating it silently is the dangerous half of this feature. A typo in `DATABASE_URL` points
    /// at a database that does not exist, which is precisely the condition this function acts on —
    /// so it would create the typo, migrate it, and bring up a console that is empty and entirely
    /// healthy-looking. To whoever is watching, the fleet's records have vanished. Returning the
    /// name instead of a bare `()` is what lets `main.rs` say out loud, on the line above
    /// "listening", which database it just brought into existence; a typo is then one line of
    /// startup output rather than an incident.
    ///
    /// # What it will not do
    ///
    /// Only `3D000` — the server saying it has no such database — is acted on. A wrong password, an
    /// unreachable host, a rejected TLS handshake all propagate untouched. That distinction is the
    /// whole safety of this: those failures are also "cannot connect", and treating them the same
    /// way would have a bad password quietly produce a second, empty database next to the real one.
    pub async fn create_database_if_absent(database_url: &str) -> Result<Option<String>> {
        let options = PgConnectOptions::from_str(database_url)?;

        // Ask the server rather than consult a catalogue: `SELECT FROM pg_database` needs a
        // connection somewhere else first, and the only authority on "can this URL be used" is the
        // URL itself.
        match PgConnection::connect_with(&options).await {
            Ok(connection) => {
                let _ = connection.close().await;
                return Ok(None);
            }
            Err(error) if sqlstate(&error).as_deref() == Some(INVALID_CATALOG_NAME) => {}
            Err(error) => return Err(error.into()),
        }

        // Without a database in the URL, libpq's rule is that it defaults to the role name, and the
        // missing database is therefore one this function was never told the name of. Guessing is
        // worse than stopping.
        let Some(name) = options.get_database().map(str::to_owned) else {
            return Err(StoreError::InvalidData(
                "DATABASE_URL 没写库名，服务器按角色名去找、没找到。\
                 建不出来——要建哪个库这条 URL 里没说。把库名补上，例如 …:5432/brocade。"
                    .to_owned(),
            ));
        };

        // The name goes into DDL, where it cannot be a bind parameter. Doubling the quotes is
        // PostgreSQL's own escape for a quoted identifier, and quoting also keeps a name with
        // capitals or a dash from being folded or rejected.
        let statement = format!("CREATE DATABASE \"{}\"", name.replace('"', "\"\""));

        let mut last_error = None;
        for maintenance in MAINTENANCE_DATABASES {
            let mut connection =
                match PgConnection::connect_with(&options.clone().database(maintenance)).await {
                    Ok(connection) => connection,
                    // This one is missing or not ours to open; try the next. The error is kept in
                    // case none of them works, so what surfaces is a real reason rather than a
                    // sentence about a list.
                    Err(error) => {
                        last_error = Some(error);
                        continue;
                    }
                };
            let created = connection.execute(statement.as_str()).await;
            let _ = connection.close().await;
            return match created {
                Ok(_) => Ok(Some(name)),
                // Two control planes starting at once. The database exists, which is all that was
                // being asked for — and `None` is the truthful answer to "did this call create it",
                // so only the instance that really did will say so.
                Err(error) if sqlstate(&error).as_deref() == Some(DUPLICATE_DATABASE) => Ok(None),
                Err(error) => Err(error.into()),
            };
        }
        Err(last_error
            .expect("MAINTENANCE_DATABASES 非空，循环至少失败一次才会走到这儿")
            .into())
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            usage_runtime: Arc::new(usage::UsageRuntimeState::default()),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<usize> {
        // Keep migrations embedded in the store crate; cargo only refreshes this
        // list when the crate is rebuilt.
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        settings::ensure_anytls_padding_scheme(&self.pool).await?;
        let default_warps = console::ensure_default_warp_outbounds(&self.pool).await?;
        materialize::ensure_current_snapshot(&self.pool).await?;
        crate::subscription_client::ensure_checkpoint(&self.pool).await?;
        Ok(default_warps)
    }

    pub async fn materialize_snapshot(&self, revision: Option<u64>) -> Result<ModelSnapshot> {
        materialize::load_snapshot(&self.pool, revision).await
    }

    pub async fn settings(&self) -> Result<ModelSettings> {
        settings::load_settings(&self.pool).await
    }

    pub async fn settings_snapshot(&self) -> Result<crate::SettingsSnapshot> {
        settings::load_settings_snapshot(&self.pool).await
    }

    /// Read fresh on every call rather than cached on the state. These two are read only when
    /// somebody installs a machine or opens the settings page, so the query costs nothing — and
    /// caching would mean an edit in the console not showing up until a restart, which is the
    /// problem this feature exists to remove.
    pub async fn distribution(&self) -> Result<crate::DistributionSettings> {
        distribution::load_distribution(&self.pool).await
    }

    /// Operational log policy. Unlike model settings, these writes create no revision and are
    /// resolved again on every agent poll.
    pub async fn agent_log_policy(
        &self,
        actor: &AdminContext,
    ) -> Result<crate::AgentLogPolicyView> {
        crate::log_policy::load_agent_log_policy(&self.pool, actor).await
    }

    pub async fn update_agent_log_default(
        &self,
        actor: &AdminContext,
        request: crate::UpdateAgentLogDefaultRequest,
    ) -> Result<()> {
        crate::log_policy::update_agent_log_default(&self.pool, actor, request).await
    }

    pub async fn update_node_log_policy(
        &self,
        actor: &AdminContext,
        node_id: &str,
        request: crate::UpdateNodeLogPolicyRequest,
    ) -> Result<()> {
        crate::log_policy::update_node_log_policy(&self.pool, actor, node_id, request).await
    }

    pub async fn effective_node_log_max_mib(&self, node_id: &str) -> Result<u32> {
        crate::log_policy::effective_node_log_max_mib(&self.pool, node_id).await
    }

    /// Operational live-traffic policy. It is durable, but neither reading nor writing it creates
    /// a model revision; the connected Agent sessions are updated separately by the console.
    pub async fn realtime_telemetry_policy(&self) -> Result<crate::RealtimeTelemetryPolicy> {
        crate::realtime::load_policy(&self.pool).await
    }

    pub async fn update_realtime_telemetry_policy(
        &self,
        actor: &AdminContext,
        request: crate::UpdateRealtimeTelemetryPolicyRequest,
    ) -> Result<crate::RealtimeTelemetryPolicy> {
        crate::realtime::update_policy(&self.pool, actor, request).await
    }

    pub async fn update_distribution(
        &self,
        actor: &AdminContext,
        settings: crate::DistributionSettings,
    ) -> Result<crate::DistributionSettings> {
        distribution::update_distribution(&self.pool, actor, settings).await
    }

    /// Read on every request so a saved name or icon appears without restarting the console.
    pub async fn branding(&self) -> Result<crate::BrandingSettings> {
        branding::load_branding(&self.pool).await
    }

    pub async fn update_branding(
        &self,
        actor: &AdminContext,
        settings: crate::BrandingSettings,
    ) -> Result<crate::BrandingSettings> {
        branding::update_branding(&self.pool, actor, settings).await
    }

    // ── Node certificates ────────────────────────────────────────────────────────────────────
    //
    // Read fresh like the two above, and for a sharper version of the same reason: the issuing
    // worker and the console are looking at a table that changes underneath both of them.

    pub async fn cert_domains(&self) -> Result<Vec<crate::CertDomain>> {
        cert::list_cert_domains(&self.pool).await
    }

    pub async fn upsert_cert_domain(
        &self,
        actor: &AdminContext,
        input: crate::CertDomainInput,
    ) -> Result<crate::CertDomain> {
        cert::upsert_cert_domain(&self.pool, actor, input).await
    }

    /// Decrypted DNS credential, ACME account key and account URL. For the issuing worker; no HTTP
    /// route may return any of the three.
    pub async fn cert_domain_secrets(
        &self,
        domain_id: &str,
    ) -> Result<(Option<String>, Option<String>, Option<String>)> {
        cert::domain_secrets(&self.pool, domain_id).await
    }

    pub async fn save_acme_account(
        &self,
        domain_id: &str,
        account_key_pem: &str,
        account_url: &str,
    ) -> Result<()> {
        cert::save_acme_account(&self.pool, domain_id, account_key_pem, account_url).await
    }

    pub async fn cert_groups(&self, actor: &AdminContext) -> Result<Vec<crate::CertGroup>> {
        cert::list_cert_groups(&self.pool, actor).await
    }

    pub async fn node_certificate_state(
        &self,
        actor: &AdminContext,
    ) -> Result<Vec<crate::NodeCertificateState>> {
        cert::list_node_certificate_state(&self.pool, actor).await
    }

    pub async fn create_cert_label(
        &self,
        actor: &AdminContext,
        domain_id: &str,
        name: &str,
        note: Option<&str>,
    ) -> Result<String> {
        cert::create_cert_label(&self.pool, actor, domain_id, name, note).await
    }

    pub async fn update_cert_label(
        &self,
        actor: &AdminContext,
        id: &str,
        name: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        cert::update_cert_label(&self.pool, actor, id, name, note).await
    }

    pub async fn delete_cert_label(&self, actor: &AdminContext, id: &str) -> Result<()> {
        cert::delete_cert_label(&self.pool, actor, id).await
    }

    /// `None` takes the machine out of every group, which is how a machine that no longer serves
    /// TLS or Hysteria 2 stops holding a certificate — and stops occupying a group's quota.
    pub async fn set_node_cert_label(
        &self,
        actor: &AdminContext,
        node_id: &str,
        label_id: Option<&str>,
    ) -> Result<()> {
        cert::set_node_label(&self.pool, actor, node_id, label_id).await
    }

    pub async fn request_spare_certificate(
        &self,
        actor: &AdminContext,
        label_id: &str,
    ) -> Result<String> {
        cert::request_spare_certificate(&self.pool, actor, label_id).await
    }

    pub async fn promote_certificate(
        &self,
        actor: &AdminContext,
        certificate_id: &str,
    ) -> Result<()> {
        cert::promote_certificate(&self.pool, actor, certificate_id).await
    }

    pub async fn certificate_dns_targets(&self) -> Result<Vec<crate::CertificateDnsTarget>> {
        cert::certificate_dns_targets(&self.pool).await
    }

    pub async fn certificates_due(
        &self,
        retry_after_minutes: i32,
    ) -> Result<Vec<crate::CertificateOrder>> {
        cert::certificates_due(&self.pool, retry_after_minutes).await
    }

    pub async fn try_certificate_scan_lock(&self) -> Result<Option<crate::CertificateScanLock>> {
        cert::try_certificate_scan_lock(&self.pool).await
    }

    pub async fn record_certificate_attempt(&self, certificate_id: &str) -> Result<()> {
        cert::record_certificate_attempt(&self.pool, certificate_id).await
    }

    pub async fn record_certificate(
        &self,
        certificate_id: &str,
        cert_pem: &str,
        key_pem: &str,
        not_after: &str,
        issuer: &str,
    ) -> Result<()> {
        cert::record_certificate(
            &self.pool,
            certificate_id,
            cert_pem,
            key_pem,
            not_after,
            issuer,
        )
        .await
    }

    pub async fn record_certificate_observation(
        &self,
        node_id: &str,
        state: &str,
        sha256: Option<&str>,
    ) -> Result<()> {
        cert::record_observation(&self.pool, node_id, state, sha256).await
    }

    pub async fn record_certificate_observation_at(
        &self,
        node_id: &str,
        state: &str,
        sha256: Option<&str>,
        observed_at_unix_secs: Option<i64>,
    ) -> Result<()> {
        cert::record_observation_at(&self.pool, node_id, state, sha256, observed_at_unix_secs).await
    }

    pub async fn record_certificate_failure(
        &self,
        certificate_id: &str,
        error: &str,
    ) -> Result<()> {
        cert::record_certificate_failure(&self.pool, certificate_id, error).await
    }

    /// The certificate the desired response owes this node, judged against what the node
    /// reports holding. `None` when the node is current on this dimension. This is the one
    /// place a node's private key leaves the database — it goes into that node's desired
    /// state and nowhere else.
    pub async fn cert_delta_for_node(
        &self,
        node_id: &str,
    ) -> Result<Option<brocade_deployment::protocol::NodeCertificateMaterial>> {
        cert::cert_delta_for_node(&self.pool, node_id).await
    }

    /// Read fresh for the same reason as `distribution`, and one more: this one is read on the
    /// agent's own schedule rather than a person's. A cached clearance would keep being handed out
    /// after somebody paused the rollout, which is the one moment it must stop.
    pub async fn agent_release(&self) -> Result<crate::AgentRelease> {
        agent_release::load_agent_release(&self.pool).await
    }

    /// `build` describes the agents the *calling process* carries. It comes from the console's
    /// compile-time constants rather than being read here — store has no business knowing that
    /// crate exists — and it is recorded rather than trusted from the request, so that a release
    /// cannot be filed under a version its bytes have nothing to do with.
    pub async fn update_agent_release(
        &self,
        actor: &AdminContext,
        release: crate::AgentRelease,
        build: crate::AgentBuildInfo<'_>,
    ) -> Result<crate::AgentRelease> {
        agent_release::update_agent_release(&self.pool, actor, release, build).await
    }

    pub async fn redacted_snapshot(
        &self,
        actor: &AdminContext,
        revision: Option<u64>,
    ) -> Result<ConsoleSnapshot> {
        console::redacted_snapshot(&self.pool, actor, revision).await
    }

    pub async fn compile_view(
        &self,
        actor: &AdminContext,
        revision: Option<u64>,
    ) -> Result<CompileView> {
        console::compile_view(&self.pool, actor, revision).await
    }

    /// Commit a draft: a run of edits landing in one write, stamping one revision
    /// (`draft.rs`).
    pub async fn apply_draft(
        &self,
        actor: &AdminContext,
        ops: Vec<crate::ModelOp>,
        note: Option<String>,
    ) -> Result<crate::ApplyDraftResult> {
        crate::draft::apply_ops(&self.pool, actor, ops, note).await
    }

    /// Preview a draft: execute as usual then roll back, returning what it would look like
    /// afterwards, the diagnostics, and the artifact index. Nothing remains in the
    /// database.
    pub async fn preview_draft(
        &self,
        actor: &AdminContext,
        ops: Vec<crate::ModelOp>,
    ) -> Result<crate::DraftPreview> {
        crate::draft::preview_ops(&self.pool, actor, ops).await
    }

    /// The contents of one artifact within a draft.
    pub async fn preview_draft_artifact(
        &self,
        actor: &AdminContext,
        ops: Vec<crate::ModelOp>,
        target_kind: &str,
        target_id: &str,
        artifact_kind: &str,
    ) -> Result<ArtifactContent> {
        crate::draft::preview_artifact(
            &self.pool,
            actor,
            ops,
            target_kind,
            target_id,
            artifact_kind,
        )
        .await
    }

    pub async fn list_tenants(&self, actor: &AdminContext) -> Result<TenantList> {
        console::list_tenants(&self.pool, actor).await
    }

    pub async fn list_node_agent_states(&self, actor: &AdminContext) -> Result<NodeAgentStateList> {
        let mut result = console::list_node_agent_states(&self.pool, actor).await?;
        for node in &mut result.nodes {
            let Some(growing) = self.usage_runtime.growing_unknown_counters(&node.node_id) else {
                continue;
            };
            let Some(serde_json::Value::Object(last_result)) = &mut node.usage_last_result else {
                continue;
            };
            last_result.insert(
                "growing_unknown_counters".to_owned(),
                serde_json::Value::from(growing),
            );
        }
        Ok(result)
    }

    pub async fn list_revisions(&self, actor: &AdminContext, limit: u32) -> Result<RevisionList> {
        console::list_revisions(&self.pool, actor, limit).await
    }

    pub async fn artifact_index(
        &self,
        actor: &AdminContext,
        revision: Option<u64>,
    ) -> Result<ArtifactIndex> {
        console::artifact_index(&self.pool, actor, revision).await
    }

    pub async fn artifact_content(
        &self,
        actor: &AdminContext,
        revision: Option<u64>,
        target_kind: &str,
        target_id: &str,
        artifact_kind: &str,
        filter: SubscriptionFilter,
    ) -> Result<ArtifactContent> {
        console::artifact_content(
            &self.pool,
            actor,
            revision,
            target_kind,
            target_id,
            artifact_kind,
            filter,
        )
        .await
    }

    pub async fn serving_user_artifact_content(
        &self,
        actor: &AdminContext,
        target_id: &str,
        artifact_kind: &str,
        filter: SubscriptionFilter,
    ) -> Result<ArtifactContent> {
        console::serving_user_artifact_content(&self.pool, actor, target_id, artifact_kind, filter)
            .await
    }

    pub async fn clash_subscription_by_uuid(&self, uuid: &str) -> Result<DynamicClashSubscription> {
        console::clash_subscription_by_uuid(&self.pool, uuid).await
    }

    pub async fn clash_subscription_by_uuid_for_family(
        &self,
        uuid: &str,
        family: Option<IpFamily>,
    ) -> Result<DynamicClashSubscription> {
        console::clash_subscription_by_uuid_for_family(&self.pool, uuid, family).await
    }

    pub async fn clash_subscription_by_uuid_filtered(
        &self,
        uuid: &str,
        filter: SubscriptionFilter,
    ) -> Result<DynamicClashSubscription> {
        console::clash_subscription_by_uuid_filtered(&self.pool, uuid, filter).await
    }

    pub async fn clash_subscription_by_haitun_token_for_family(
        &self,
        token: &str,
        family: Option<IpFamily>,
    ) -> Result<DynamicClashSubscription> {
        console::clash_subscription_by_haitun_token_for_family(&self.pool, token, family).await
    }

    pub async fn clash_subscription_by_haitun_token_filtered(
        &self,
        token: &str,
        filter: SubscriptionFilter,
    ) -> Result<DynamicClashSubscription> {
        console::clash_subscription_by_haitun_token_filtered(&self.pool, token, filter).await
    }

    pub async fn clash_subscription_for_user(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<DynamicClashSubscription> {
        console::clash_subscription_for_user(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn user_grant_probe_plan(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<UserGrantProbePlan> {
        grant_probe::user_grant_probe_plan(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn user_grant_probe_generation_matches(&self, expected: u64) -> Result<bool> {
        grant_probe::user_grant_probe_generation_matches(&self.pool, expected).await
    }

    pub async fn clash_haitun_link_for_user(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<ClashHaitunLink>> {
        console::clash_haitun_link_for_user(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn issue_clash_haitun_link(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<ClashHaitunLink> {
        console::issue_clash_haitun_link(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn revoke_clash_haitun_link(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<ClashHaitunLink> {
        console::revoke_clash_haitun_link(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn verify_deployment(
        &self,
        actor: &AdminContext,
        request: VerifyDeploymentRequest,
    ) -> Result<DeploymentVerification> {
        console::verify_deployment(&self.pool, actor, request).await
    }

    pub async fn update_settings(
        &self,
        actor: &AdminContext,
        settings: ModelSettings,
    ) -> Result<UpdateSettingsResult> {
        settings::update_settings(&self.pool, actor, settings, None).await
    }

    pub async fn update_settings_at_revision(
        &self,
        actor: &AdminContext,
        settings: ModelSettings,
        expected_revision: u64,
    ) -> Result<UpdateSettingsResult> {
        settings::update_settings(&self.pool, actor, settings, Some(expected_revision)).await
    }

    pub async fn plan_deployment(
        &self,
        actor: &AdminContext,
        revision_id: u64,
    ) -> Result<brocade_deployment::plan::DeploymentPlan> {
        deployment::plan_deployment(&self.pool, actor, revision_id).await
    }

    pub async fn create_deployment(
        &self,
        actor: &AdminContext,
        request: CreateDeploymentRequest,
    ) -> Result<CreateDeploymentResult> {
        deployment::create_deployment(&self.pool, actor, request).await
    }

    pub async fn create_rollback_deployment(
        &self,
        actor: &AdminContext,
        request: CreateRollbackRequest,
    ) -> Result<CreateDeploymentResult> {
        deployment::create_rollback_deployment(&self.pool, actor, request).await
    }

    pub async fn list_deployments(
        &self,
        actor: &AdminContext,
        limit: u32,
        kind: Option<DeploymentKind>,
    ) -> Result<DeploymentList> {
        deployment::list_deployments(&self.pool, actor, limit, kind).await
    }

    pub async fn deployment_detail(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
        include_content: bool,
    ) -> Result<DeploymentDetail> {
        deployment::deployment_detail(&self.pool, actor, deployment_id, include_content).await
    }

    pub async fn confirm_deployment_wave(
        &self,
        admin: &AdminContext,
        deployment_id: i64,
        wave: u32,
        actor: Option<String>,
    ) -> Result<DeploymentWaveConfirmationResult> {
        deployment::confirm_deployment_wave(&self.pool, admin, deployment_id, wave, actor).await
    }

    pub async fn load_desired_for_node(
        &self,
        node_id: &str,
    ) -> Result<Option<NodeDesiredDeployment>> {
        deployment::load_desired_for_node(&self.pool, node_id).await
    }

    pub async fn claim_desired_for_node(
        &self,
        node_id: &str,
    ) -> Result<Option<NodeDesiredDeployment>> {
        deployment::claim_desired_for_node(&self.pool, node_id).await
    }

    pub async fn report_target_result(
        &self,
        report: TargetConvergenceReport,
    ) -> Result<ReportTargetResult> {
        deployment::report_target_result(&self.pool, report).await
    }

    pub async fn record_node_route_ips(
        &self,
        node_id: &str,
        route: &brocade_deployment::protocol::RouteIpReport,
    ) -> Result<()> {
        agent::record_node_route_ips(&self.pool, node_id, route).await
    }

    pub async fn halt_deployment(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
    ) -> Result<DeploymentCommandResult> {
        deployment::halt_deployment(&self.pool, actor, deployment_id).await
    }

    pub async fn cancel_deployment(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
    ) -> Result<DeploymentCommandResult> {
        deployment::cancel_deployment(&self.pool, actor, deployment_id).await
    }

    pub async fn cancel_deployment_and_rollback(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
    ) -> Result<DeploymentCommandResult> {
        deployment::cancel_deployment_and_rollback(&self.pool, actor, deployment_id).await
    }

    /// Discard the run of edits that were committed but never shipped: the model reverts to
    /// the last successfully released version, producing no deployment.
    pub async fn discard_pending_changes(
        &self,
        actor: &AdminContext,
        revision_id: u64,
    ) -> Result<deployment::DiscardPendingResult> {
        deployment::discard_pending_changes(&self.pool, actor, revision_id).await
    }

    pub async fn retry_target(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
        node_id: &str,
    ) -> Result<ReportTargetResult> {
        deployment::retry_target(&self.pool, actor, deployment_id, node_id).await
    }

    pub async fn isolate_deployment_target(
        &self,
        actor: &AdminContext,
        deployment_id: i64,
        node_id: &str,
        request: brocade_deployment::protocol::IsolateDeploymentTargetRequest,
    ) -> Result<brocade_deployment::protocol::NodeIsolationCommandResult> {
        deployment::isolate_deployment_target(&self.pool, actor, deployment_id, node_id, request)
            .await
    }

    pub async fn restore_node_service(
        &self,
        actor: &AdminContext,
        node_id: &str,
    ) -> Result<brocade_deployment::protocol::NodeIsolationCommandResult> {
        deployment::restore_node_service(&self.pool, actor, node_id).await
    }

    pub async fn provision_node(
        &self,
        actor: &AdminContext,
        request: ProvisionNodeRequest,
    ) -> Result<ProvisionNodeResult> {
        provision::provision_node(&self.pool, actor, request).await
    }

    pub async fn create_tenant(
        &self,
        actor: &AdminContext,
        request: CreateTenantRequest,
    ) -> Result<UpsertTenantResult> {
        console::create_tenant(&self.pool, actor, request).await
    }

    pub async fn update_node_status(
        &self,
        actor: &crate::AdminContext,
        node_id: &str,
        request: crate::console::UpdateNodeStatusRequest,
    ) -> Result<crate::NodeLifecycleTransitionResult> {
        crate::deployment::transition_node_status(&self.pool, actor, node_id, request).await
    }

    pub async fn node_lifecycle(&self, node_id: &str) -> Result<crate::NodeLifecycleState> {
        crate::lifecycle::load(&self.pool, node_id).await
    }

    pub async fn abandon_node(
        &self,
        actor: &AdminContext,
        node_id: &str,
    ) -> Result<crate::NodeLifecycleTransitionResult> {
        crate::deployment::abandon_node(&self.pool, actor, node_id).await
    }

    pub async fn retired_node_warp_bindings(&self, node_id: &str) -> Result<Vec<(String, String)>> {
        crate::lifecycle::managed_warp_bindings(&self.pool, node_id).await
    }

    pub async fn set_node_lifecycle_cleanup_error(
        &self,
        node_id: &str,
        error: Option<&str>,
    ) -> Result<()> {
        crate::lifecycle::set_cleanup_error(&self.pool, node_id, error).await
    }

    pub async fn update_node(
        &self,
        actor: &AdminContext,
        node_id: &str,
        request: UpdateNodeRequest,
    ) -> Result<UpdateNodeResult> {
        console::update_node(&self.pool, actor, node_id, request).await
    }

    pub async fn create_user(
        &self,
        actor: &AdminContext,
        request: CreateUserRequest,
    ) -> Result<UpsertUserResult> {
        console::create_user(&self.pool, actor, request).await
    }

    pub async fn list_users(
        &self,
        actor: &AdminContext,
        tenant_id: Option<&str>,
        include_disabled: bool,
    ) -> Result<UserList> {
        console::list_users(&self.pool, actor, tenant_id, include_disabled).await
    }

    pub async fn rotate_user_uuid(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<RotateUserUuidResult> {
        console::rotate_user_uuid(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn user_profile(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<crate::UserListItem> {
        console::user_profile(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn self_user_profile(&self, actor: &AdminContext) -> Result<crate::UserListItem> {
        console::self_user_profile(&self.pool, actor).await
    }

    pub async fn update_user_profile(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
        request: UpdateUserProfileRequest,
    ) -> Result<crate::UserListItem> {
        console::update_user_profile(&self.pool, actor, tenant_id, user_id, request).await
    }

    pub async fn update_user_status(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
        request: UpdateUserStatusRequest,
    ) -> Result<UpdateUserStatusResult> {
        console::update_user_status(&self.pool, actor, tenant_id, user_id, request).await
    }

    pub async fn upsert_grant(
        &self,
        actor: &AdminContext,
        request: CreateGrantRequest,
    ) -> Result<UpsertGrantResult> {
        console::upsert_grant(&self.pool, actor, request).await
    }

    pub async fn list_user_app_quotas(
        &self,
        actor: &AdminContext,
        tenant_id: Option<&str>,
    ) -> Result<UserAppQuotaList> {
        console::list_user_app_quotas(&self.pool, actor, tenant_id).await
    }

    // Quotas do not enter the model, so this takes no revision parameter and does not touch
    // drafts — looking unlike the neighboring `upsert_*` is deliberate.
    pub async fn set_user_app_quota(
        &self,
        actor: &AdminContext,
        request: SetUserAppQuotaRequest,
    ) -> Result<SetUserAppQuotaResult> {
        console::set_user_app_quota(&self.pool, actor, request).await
    }

    // Quota enforcement. quota.rs fixes the actor as `system:quota` itself — nobody
    // initiated this round, and recording the caller's identity would make the release
    // history look like some admin did it.
    pub async fn enforce_quotas(&self) -> Result<QuotaEnforcementOutcome> {
        quota::enforce_quotas(&self.pool).await
    }

    /// Merge and release the next batch of permission changes recorded in the durable outbox.
    pub async fn process_grant_automation(&self) -> Result<crate::GrantAutomationOutcome> {
        grant_automation::process_jobs(&self.pool).await
    }

    pub async fn grant_automation_status(&self) -> Result<crate::GrantAutomationStatus> {
        grant_automation::status(&self.pool).await
    }

    pub async fn plan_quota_enforcement(&self) -> Result<QuotaEnforcementPlan> {
        quota::plan_quota_enforcement(&self.pool).await
    }

    pub async fn upsert_app(
        &self,
        actor: &AdminContext,
        request: CreateAppRequest,
    ) -> Result<UpsertAppResult> {
        console::upsert_app(&self.pool, actor, request).await
    }

    pub async fn register_warp_binding(
        &self,
        actor: &AdminContext,
        request: RegisterWarpBindingRequest,
    ) -> Result<RegisterWarpBindingResult> {
        console::register_warp_binding(&self.pool, actor, request).await
    }

    pub async fn update_warp_binding(
        &self,
        actor: &AdminContext,
        request: UpdateWarpBindingRequest,
    ) -> Result<UpdateWarpBindingResult> {
        console::update_warp_binding(&self.pool, actor, request).await
    }

    pub async fn prepare_warp_binding_removal(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        outbound_id: &str,
        node_id: &str,
    ) -> Result<WarpBindingRemoval> {
        console::prepare_warp_binding_removal(&self.pool, actor, tenant_id, outbound_id, node_id)
            .await
    }

    pub async fn remove_warp_binding(
        &self,
        actor: &AdminContext,
        request: RemoveWarpBindingRequest,
    ) -> Result<RemoveWarpBindingResult> {
        console::remove_warp_binding(&self.pool, actor, request).await
    }

    pub async fn upsert_chain(
        &self,
        actor: &AdminContext,
        app_id: &str,
        request: CreateChainRequest,
    ) -> Result<UpsertChainResult> {
        console::upsert_chain(&self.pool, actor, app_id, request).await
    }

    pub async fn upsert_front(
        &self,
        actor: &AdminContext,
        app_id: &str,
        request: CreateFrontRequest,
    ) -> Result<UpsertFrontResult> {
        console::upsert_front(&self.pool, actor, app_id, request).await
    }

    pub async fn upsert_ingress(
        &self,
        actor: &AdminContext,
        app_id: &str,
        request: CreateIngressRequest,
    ) -> Result<UpsertIngressResult> {
        console::upsert_ingress(&self.pool, actor, app_id, request).await
    }

    pub async fn delete_step(
        &self,
        actor: &AdminContext,
        app_id: &str,
        chain_id: &str,
        node_id: &str,
    ) -> Result<DeleteStepResult> {
        console::delete_step(&self.pool, actor, app_id, chain_id, node_id).await
    }

    pub async fn prune_chain(
        &self,
        actor: &AdminContext,
        app_id: &str,
        chain_id: &str,
    ) -> Result<PruneChainResult> {
        console::prune_chain(&self.pool, actor, app_id, chain_id).await
    }

    pub async fn issue_node_token(&self, node_id: &str) -> Result<IssuedNodeToken> {
        agent::issue_node_token(&self.pool, node_id).await
    }

    pub async fn redeem_node_enrollment(&self, token: &str) -> Result<IssuedNodeToken> {
        provision::redeem_node_enrollment(&self.pool, token).await
    }

    pub async fn authenticate_node_token(&self, token: &str) -> Result<Option<AuthenticatedNode>> {
        agent::authenticate_node_token(&self.pool, token).await
    }

    pub async fn revoke_node_token(&self, node_id: &str) -> Result<bool> {
        agent::revoke_node_token(&self.pool, node_id).await
    }

    pub async fn record_node_poll(&self, node_id: &str, agent_version: Option<&str>) -> Result<()> {
        agent::record_node_poll(&self.pool, node_id, agent_version, None).await
    }

    pub async fn record_node_poll_with_protocol(
        &self,
        node_id: &str,
        agent_version: Option<&str>,
        protocol_version: Option<i32>,
    ) -> Result<()> {
        agent::record_node_poll(&self.pool, node_id, agent_version, protocol_version).await
    }

    pub async fn record_node_runtime(
        &self,
        node_id: &str,
        report: &brocade_deployment::protocol::NodeRuntimeReport,
    ) -> Result<()> {
        agent::record_node_runtime(&self.pool, node_id, report).await
    }

    pub async fn create_admin_operator(
        &self,
        actor: &AdminContext,
        request: CreateAdminOperatorRequest,
    ) -> Result<AdminOperator> {
        admin::create_admin_operator(&self.pool, actor, request).await
    }

    pub async fn admin_auth_state(&self) -> Result<AdminAuthState> {
        admin::admin_auth_state(&self.pool).await
    }

    pub async fn set_public_access(
        &self,
        actor: &AdminContext,
        enabled: bool,
    ) -> Result<AdminAuthState> {
        admin::set_public_access(&self.pool, actor, enabled).await
    }

    pub async fn init_admin(&self, request: AdminInitRequest) -> Result<AdminInitResult> {
        admin::init_admin(&self.pool, request).await
    }

    pub async fn login_admin(&self, request: AdminLoginRequest) -> Result<AdminLoginResult> {
        admin::login_admin(&self.pool, request).await
    }

    pub async fn list_admin_operators(&self, actor: &AdminContext) -> Result<Vec<AdminOperator>> {
        admin::list_admin_operators(&self.pool, actor).await
    }

    pub async fn issue_user_login(
        &self,
        actor: &AdminContext,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<IssuedUserLogin> {
        admin::issue_user_login(&self.pool, actor, tenant_id, user_id).await
    }

    pub async fn issue_admin_token(
        &self,
        actor: &AdminContext,
        operator_id: &str,
    ) -> Result<IssuedAdminToken> {
        admin::issue_admin_token(&self.pool, actor, operator_id).await
    }

    pub async fn authenticate_admin_token(
        &self,
        token: &str,
    ) -> Result<Option<AuthenticatedAdmin>> {
        admin::authenticate_admin_token(&self.pool, token).await
    }

    pub async fn authenticate_admin_session(
        &self,
        token: &str,
    ) -> Result<Option<AuthenticatedAdmin>> {
        admin::authenticate_admin_session(&self.pool, token).await
    }

    pub async fn revoke_admin_session(&self, token: &str) -> Result<bool> {
        admin::revoke_admin_session(&self.pool, token).await
    }

    pub async fn revoke_admin_token(
        &self,
        actor: &AdminContext,
        operator_id: &str,
    ) -> Result<bool> {
        admin::revoke_admin_token(&self.pool, actor, operator_id).await
    }

    pub async fn reset_admin_password(
        &self,
        actor: &AdminContext,
        operator_id: &str,
    ) -> Result<ResetAdminPasswordResult> {
        admin::reset_admin_password(&self.pool, actor, operator_id).await
    }

    pub async fn change_admin_password(
        &self,
        operator_id: &str,
        keep_session_token: Option<&str>,
        request: ChangeAdminPasswordRequest,
    ) -> Result<u64> {
        admin::change_admin_password(&self.pool, operator_id, keep_session_token, request).await
    }

    pub async fn record_usage_report(
        &self,
        node_id: &str,
        request: UsageReportRequest,
    ) -> Result<UsageReportResult> {
        usage::record_usage_report(&self.pool, &self.usage_runtime, node_id, request).await
    }

    /// Asked once by the agent before each probing round: which endpoints to probe, and how
    /// much to subtract for each.
    pub async fn probe_targets(&self, node_id: &str) -> Result<ProbeTargetList> {
        probe::probe_targets(&self.pool, node_id).await
    }

    pub async fn record_link_health(
        &self,
        node_id: &str,
        request: LinkHealthRequest,
    ) -> Result<LinkHealthResult> {
        probe::record_link_health(&self.pool, node_id, request).await
    }

    /// Link liveness, like MTU, is not partitioned by tenant: a relay link is a fact about the
    /// backbone. The scoping matches `link_mtu_view`, except that this family scopes on one
    /// side by node_id (the reasoning is over there).
    pub async fn link_health(&self, actor: &AdminContext) -> Result<Vec<LinkHealthItem>> {
        probe::link_health_view(&self.pool, actor).await
    }

    /// Asked once by the agent before each end-to-end probing round: which chains to dial on
    /// behalf of as their head, and at which endpoints.
    pub async fn e2e_probe_targets(&self, node_id: &str) -> Result<E2eProbeTargetList> {
        probe::e2e_probe_targets(&self.pool, node_id).await
    }

    pub async fn record_e2e_probe(
        &self,
        node_id: &str,
        request: E2eProbeRequest,
    ) -> Result<E2eProbeResult> {
        probe::record_e2e_probe(&self.pool, node_id, request).await
    }

    /// End-to-end probing is partitioned by tenant: a chain belongs to one, unlike MTU which
    /// is a property of the backbone.
    pub async fn e2e_probes(&self, actor: &AdminContext) -> Result<Vec<E2eProbeItem>> {
        probe::e2e_probe_view(&self.pool, actor).await
    }

    pub async fn record_link_probe(
        &self,
        node_id: &str,
        request: LinkProbeRequest,
    ) -> Result<LinkProbeResult> {
        probe::record_link_probe(&self.pool, node_id, request).await
    }

    /// Probe results are scoped by visibility rather than gated by rank.
    ///
    /// This used to be system-admin only, on the grounds that MTU is a property of the backbone
    /// and the backbone is one global network. That statement is right and the conclusion was
    /// wrong: one global table can be scoped (by node ownership), while gating it costs a
    /// global read-only role — exactly the role that should see it — every last number, when
    /// inspecting links is precisely their job. The scoping lives in `probe::link_mtu_view`,
    /// with the same test as the node list (tenant_scope and its descendants).
    pub async fn link_mtu_view(&self, actor: &AdminContext) -> Result<LinkMtuView> {
        probe::link_mtu_view(&self.pool, actor).await
    }

    pub async fn list_usage_samples(
        &self,
        actor: &AdminContext,
        limit: u32,
        tenant_id: Option<&str>,
        user_id: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<UsageSampleList> {
        usage::list_usage_samples(&self.pool, actor, limit, tenant_id, user_id, node_id).await
    }

    pub async fn list_monthly_usage_summary(
        &self,
        actor: &AdminContext,
    ) -> Result<UsageMonthlySummary> {
        usage::list_monthly_usage_summary(&self.pool, actor).await
    }

    pub async fn prune_usage_readings(&self, retain_days: u32) -> Result<u64> {
        usage::prune_usage_readings(&self.pool, retain_days).await
    }

    pub async fn list_usage_node_series(
        &self,
        actor: &AdminContext,
        window_secs: u32,
        node_id: Option<&str>,
    ) -> Result<UsageNodeSeriesList> {
        usage::list_usage_node_series(&self.pool, actor, window_secs, node_id).await
    }

    // Telemetry. Like quotas, none of this enters the model, so none of these take a revision or
    // touch drafts — they look unlike the neighbouring model writers on purpose.
    pub async fn record_load_report(
        &self,
        node_id: &str,
        request: LoadReportRequest,
    ) -> Result<LoadReportResult> {
        load::record_load_report(&self.pool, node_id, request).await
    }

    pub async fn node_load_view(
        &self,
        actor: &AdminContext,
        node_id: &str,
        selection: LoadSeriesQuery,
        max_samples: u32,
    ) -> Result<NodeLoadView> {
        load::node_load_view(&self.pool, actor, node_id, selection, max_samples).await
    }

    pub async fn list_node_load(
        &self,
        actor: &AdminContext,
        selection: LoadSeriesQuery,
        max_samples_per_node: u32,
    ) -> Result<NodeLoadList> {
        load::list_node_load(&self.pool, actor, selection, max_samples_per_node).await
    }

    pub async fn ping_probe_settings(&self) -> Result<PingProbeSettings> {
        ping_probe::load_settings(&self.pool).await
    }

    pub async fn update_ping_probe_settings(
        &self,
        actor: &AdminContext,
        settings: PingProbeSettings,
    ) -> Result<PingProbeSettings> {
        ping_probe::update_settings(&self.pool, actor, settings).await
    }

    pub async fn record_ping_probe(
        &self,
        node_id: &str,
        request: PingProbeReportRequest,
    ) -> Result<PingProbeReportResult> {
        ping_probe::record_report(&self.pool, node_id, request).await
    }

    pub async fn node_ping_probe_view(
        &self,
        actor: &AdminContext,
        node_id: &str,
        window_secs: u32,
    ) -> Result<NodePingProbeView> {
        ping_probe::node_view(&self.pool, actor, node_id, window_secs).await
    }

    pub async fn list_node_ping_probes(
        &self,
        actor: &AdminContext,
        window_secs: u32,
    ) -> Result<NodePingProbeList> {
        ping_probe::list_nodes(&self.pool, actor, window_secs).await
    }

    pub async fn hop_link_list(
        &self,
        actor: &AdminContext,
        chain_id: Option<&str>,
    ) -> Result<HopLinkList> {
        load::hop_link_list(&self.pool, actor, chain_id).await
    }

    pub async fn prune_load_samples(&self, retain_days: u32) -> Result<u64> {
        load::prune_load_samples(&self.pool, retain_days).await
    }
}
