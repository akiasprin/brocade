//! Durable, configurable diagnostic cycles. The row count is bounded by existing machines and chains.
use crate::{AdminContext, AdminRole, PgStore, Result, StoreError};
use brocade_deployment::{
    protocol::{E2eProbeSecurity, E2eProbeTarget},
    unlock_probe::{
        IpFamily, UnlockPathError, UnlockProbeReport, UnlockProbeTarget, UnlockResult,
        CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION, LEGACY_UNLOCK_SERVICES,
        UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION, UNLOCK_DUAL_STACK_PROTOCOL_VERSION,
        UNLOCK_EXTENDED_SERVICES_PROTOCOL_VERSION, UNLOCK_REDUCED_SERVICES_PROTOCOL_VERSION,
        UNLOCK_SERVICES,
    },
};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

mod dns;
mod path;
mod schedule;
pub(crate) use schedule::wake_discovery;
pub use schedule::UnlockDetectionPolicy;
mod summary;
pub use summary::{ChainUnlockProbeItem, ChainUnlockProbePage, ChainUnlockProbeReports};

// Three services per worker, each with an initial attempt and ten bounded retries.
const RETRY_LEASE_SECS: i32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockProbeState {
    Queued,
    Running,
    Complete,
    Expired,
    Obsolete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockProbeTrigger {
    Manual,
    Scheduled,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnlockProbeView {
    pub id: String,
    pub family: Option<IpFamily>,
    pub automatic: bool,
    pub node_id: String,
    pub app_id: Option<String>,
    pub chain_id: Option<String>,
    pub configuration_revision: Option<i64>,
    pub ingress_id: Option<String>,
    pub protocol: Option<String>,
    pub state: UnlockProbeState,
    pub trigger: UnlockProbeTrigger,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub next_run_at: i64,
    pub interval_secs: u64,
    pub retry_at: Option<i64>,
    pub results: Vec<UnlockResult>,
    pub path_error: Option<UnlockPathError>,
    pub previous_results: Vec<UnlockResult>,
    pub previous_completed_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockFamilyMode {
    Split,
    Automatic,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnlockProbeCapabilities {
    pub family_mode: UnlockFamilyMode,
    pub automatic_ready: bool,
    pub ipv6_available: Option<bool>,
    pub ipv6_reason: Option<String>,
    pub detection_mode: crate::UnlockDetectionMode,
    pub interval_hours: u32,
    pub next_run_at: Option<i64>,
}
impl UnlockProbeCapabilities {
    fn new(policy: UnlockDetectionPolicy) -> Self {
        Self {
            family_mode: UnlockFamilyMode::Split,
            automatic_ready: false,
            // Retained for older clients. Host observations are not path capability evidence.
            ipv6_available: None,
            ipv6_reason: None,
            detection_mode: policy.mode,
            interval_hours: policy.interval_hours,
            next_run_at: policy.next_run_at,
        }
    }
}

#[derive(Clone)]
struct Scope {
    key: String,
    family: Option<IpFamily>,
    node: String,
    app: Option<String>,
    chain: Option<String>,
    revision: Option<i64>,
    ingress: Option<String>,
    protocol: Option<String>,
    dns_fingerprint: Option<String>,
}
impl Scope {
    fn with_automatic(mut self) -> Self {
        self.family = None;
        self.key.push_str(":auto");
        self
    }
    fn with_family(mut self, family: Option<IpFamily>) -> Self {
        self.family = family;
        if let Some(family) = family {
            self.key.push_str(match family {
                IpFamily::V4 => ":v4",
                IpFamily::V6 => ":v6",
            });
        }
        self
    }
    fn node(node: &str) -> Self {
        Self {
            key: format!("node:{node}"),
            family: None,
            node: node.into(),
            app: None,
            chain: None,
            revision: None,
            ingress: None,
            protocol: None,
            dns_fingerprint: None,
        }
    }
    fn with_dns(mut self, snapshot: Option<&brocade_core::model::ModelSnapshot>) -> Result<Self> {
        self.dns_fingerprint = snapshot
            .filter(|snapshot| snapshot.nodes.iter().any(|node| node.id == self.node))
            .map(|snapshot| path::dns_fingerprint(snapshot, &self.node))
            .transpose()?;
        Ok(self)
    }
    fn chain(node: &str, target: &E2eProbeTarget, revision: i64) -> Result<Self> {
        let app = target
            .app_id
            .clone()
            .ok_or_else(|| StoreError::InvalidData("chain probe is missing app".into()))?;
        Ok(Self {
            key: format!("chain:{app}/{}", target.chain_id),
            family: None,
            node: node.into(),
            app: Some(app),
            chain: Some(target.chain_id.clone()),
            revision: Some(revision),
            ingress: Some(target.ingress_id.clone()),
            protocol: Some(protocol_name(target).into()),
            dns_fingerprint: None,
        })
    }
}
fn protocol_name(target: &E2eProbeTarget) -> &'static str {
    match target.security {
        E2eProbeSecurity::AnyTls { .. } => "AnyTLS",
        E2eProbeSecurity::Hysteria2(_) => "Hysteria2",
        E2eProbeSecurity::Reality(_) => "VLESS REALITY",
        E2eProbeSecurity::Tls(_) => "VLESS TLS",
        E2eProbeSecurity::VlessEncryption { .. } => "VLESS Encryption",
    }
}
fn decode(row: PgRow) -> Result<UnlockProbeView> {
    let json = |name| -> Result<Vec<UnlockResult>> {
        let mut results: Vec<UnlockResult> = serde_json::from_value(row.try_get(name)?)
            .map_err(|_| StoreError::InvalidData("invalid stored unlock result".into()))?;
        for result in &mut results {
            result.apply_youtube_premium_region_policy();
        }
        Ok(results)
    };
    let path_error: Option<String> = row.try_get("path_error")?;
    Ok(UnlockProbeView {
        id: row.try_get("job_id")?,
        automatic: row.try_get::<String, _>("scope_key")?.ends_with(":auto"),
        family: row
            .try_get::<Option<String>, _>("address_family")?
            .map(|value| serde_json::from_value(serde_json::Value::String(value)))
            .transpose()
            .map_err(|_| StoreError::InvalidData("invalid stored unlock family".into()))?,
        node_id: row.try_get("node_id")?,
        app_id: row.try_get("app_id")?,
        chain_id: row.try_get("chain_id")?,
        configuration_revision: row.try_get("configuration_revision")?,
        ingress_id: row.try_get("ingress_id")?,
        protocol: row.try_get("protocol")?,
        state: serde_json::from_value(serde_json::Value::String(row.try_get("state")?))
            .map_err(|_| StoreError::InvalidData("invalid stored unlock state".into()))?,
        trigger: serde_json::from_value(serde_json::Value::String(row.try_get("trigger")?))
            .map_err(|_| StoreError::InvalidData("invalid stored unlock trigger".into()))?,
        created_at: row.try_get("created_secs")?,
        completed_at: row.try_get("completed_secs")?,
        next_run_at: row.try_get("next_secs")?,
        interval_secs: row.try_get::<i32, _>("interval_secs")? as u64,
        retry_at: if path_error.as_deref() == Some("family_unavailable")
            && row.try_get::<i16, _>("readiness_failures")? < 6
        {
            row.try_get("next_secs")?
        } else {
            None
        },
        results: json("results")?,
        previous_results: json("previous_results")?,
        previous_completed_at: row.try_get("previous_secs")?,
        path_error: path_error
            .map(|value| serde_json::from_value(serde_json::Value::String(value)))
            .transpose()
            .map_err(|_| StoreError::InvalidData("invalid stored unlock path status".into()))?,
    })
}
const SELECT: &str = "SELECT *, extract(epoch FROM created_at)::bigint AS created_secs,
    extract(epoch FROM completed_at)::bigint AS completed_secs, extract(epoch FROM next_run_at)::bigint AS next_secs,
    extract(epoch FROM previous_completed_at)::bigint AS previous_secs,
    (SELECT unlock_detection_interval_hours*3600 FROM control_state WHERE id=TRUE) AS interval_secs FROM unlock_probe_jobs WHERE scope_key=$1";
async fn expire(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("UPDATE unlock_probe_jobs SET state='expired' WHERE state='running' AND claimed_at < now()-make_interval(secs=>CASE WHEN address_family IS NULL AND chain_id IS NULL THEN 240 ELSE $1::int END)")
        .bind(RETRY_LEASE_SECS).execute(&mut **tx).await?;
    Ok(())
}

async fn enqueue(tx: &mut Transaction<'_, Postgres>, scope: &Scope, manual: bool) -> Result<()> {
    let mut readiness_failures: i16 = 0;
    let current = sqlx::query("SELECT state,path_error,readiness_failures,dns_fingerprint,created_at<(SELECT unlock_detection_requested_at FROM control_state WHERE id=TRUE) AS full_refresh, created_at > now()-interval '60 seconds' AS cooldown,
        next_run_at > now() AS future, completed_at IS NOT NULL AS completed, configuration_revision FROM unlock_probe_jobs WHERE scope_key=$1 FOR UPDATE").bind(&scope.key).fetch_optional(&mut **tx).await?;
    if let Some(row) = current {
        let revision: Option<i64> = row.try_get("configuration_revision")?;
        let same = path::matches(
            tx,
            revision,
            scope.revision,
            &scope.node,
            scope.app.as_deref(),
            scope.chain.as_deref(),
        )
        .await?
            && row.try_get::<Option<String>, _>("dns_fingerprint")? == scope.dns_fingerprint;
        let full_refresh: bool = row.try_get("full_refresh")?;
        let state: String = row.try_get("state")?;
        let failures: i16 = row.try_get("readiness_failures")?;
        if !manual
            && same
            && !full_refresh
            && failures < 6
            && row.try_get::<Option<String>, _>("path_error")?.as_deref()
                == Some("family_unavailable")
        {
            readiness_failures = failures;
        }
        if same && matches!(state.as_str(), "queued" | "running") {
            return Ok(());
        }
        if manual && row.try_get::<bool, _>("cooldown")? {
            return Err(StoreError::Conflict("请间隔 60 秒再执行解锁检测".into()));
        }
        let retry_due = failures < 6
            && row.try_get::<Option<String>, _>("path_error")?.as_deref()
                == Some("family_unavailable")
            && !row.try_get::<bool, _>("future")?;
        if !manual && same && !full_refresh && !retry_due && state != "expired" {
            if state == "obsolete" && row.try_get::<bool, _>("completed")? {
                sqlx::query("UPDATE unlock_probe_jobs SET state='complete' WHERE scope_key=$1")
                    .bind(&scope.key)
                    .execute(&mut **tx)
                    .await?;
            }
            return Ok(());
        }
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| StoreError::Unavailable("unlock ID unavailable".into()))?;
    let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    sqlx::query("INSERT INTO unlock_probe_jobs(scope_key,node_id,app_id,chain_id,configuration_revision,ingress_id,protocol,job_id,state,trigger,address_family,readiness_failures,dns_fingerprint,next_run_at)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,'queued',$9,$10,$11,$12,now()+(SELECT make_interval(hours=>unlock_detection_interval_hours) FROM control_state WHERE id=TRUE)) ON CONFLICT(scope_key) DO UPDATE SET
        node_id=EXCLUDED.node_id, configuration_revision=EXCLUDED.configuration_revision, ingress_id=EXCLUDED.ingress_id, protocol=EXCLUDED.protocol,
        job_id=EXCLUDED.job_id,state='queued',trigger=EXCLUDED.trigger,created_at=now(),claimed_at=NULL,completed_at=NULL,
        next_run_at=EXCLUDED.next_run_at,path_error=NULL,readiness_failures=EXCLUDED.readiness_failures,dns_fingerprint=EXCLUDED.dns_fingerprint,
        previous_results=CASE WHEN unlock_probe_jobs.completed_at IS NOT NULL AND unlock_probe_jobs.path_error IS NULL THEN unlock_probe_jobs.results ELSE unlock_probe_jobs.previous_results END,
        previous_completed_at=CASE WHEN unlock_probe_jobs.path_error IS NULL THEN COALESCE(unlock_probe_jobs.completed_at,unlock_probe_jobs.previous_completed_at) ELSE unlock_probe_jobs.previous_completed_at END,results='[]'::jsonb")
        .bind(&scope.key).bind(&scope.node).bind(&scope.app).bind(&scope.chain).bind(scope.revision).bind(&scope.ingress).bind(&scope.protocol).bind(id).bind(if manual {"manual"} else {"scheduled"}).bind(scope.family.map(|family| match family { IpFamily::V4 => "v4", IpFamily::V6 => "v6" })).bind(readiness_failures).bind(&scope.dns_fingerprint).execute(&mut **tx).await?;
    Ok(())
}
async fn serial(pool: &PgPool) -> Result<Transaction<'_, Postgres>> {
    let mut tx = pool.begin().await?;
    // Serialize short claim/enqueue transactions across Console instances, never the actual probe.
    sqlx::query("SELECT pg_advisory_xact_lock(73129,47)")
        .execute(&mut *tx)
        .await?;
    expire(&mut tx).await?;
    Ok(tx)
}

fn require_editor(actor: &AdminContext) -> Result<()> {
    if matches!(
        actor.role(),
        AdminRole::SystemAdmin | AdminRole::TenantAdmin | AdminRole::Publisher | AdminRole::Editor
    ) {
        return Ok(());
    }
    Err(StoreError::Forbidden("解锁检测需要编辑权限".into()))
}

impl PgStore {
    async fn unlock_node_facts(&self, nodes: &[String]) -> Result<Vec<PgRow>> {
        sqlx::query("SELECT n.id,COALESCE(s.last_poll_at>now()-interval '90 seconds' AND s.agent_protocol_version>=$2,false) AS automatic_ready FROM nodes n LEFT JOIN node_agent_state s ON s.node_id=n.id WHERE n.id=ANY($1)")
            .bind(nodes).bind(UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION as i32).fetch_all(self.pool()).await.map_err(Into::into)
    }
    pub async fn node_unlock_capabilities(
        &self,
        actor: &AdminContext,
        node: &str,
    ) -> Result<UnlockProbeCapabilities> {
        self.visible_unlock_node(actor, node).await?;
        Ok(UnlockProbeCapabilities::new(
            schedule::policy(self.pool()).await?,
        ))
    }
    pub async fn chain_unlock_capabilities(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
    ) -> Result<UnlockProbeCapabilities> {
        self.visible_unlock_chain(actor, app, chain).await?;
        let serving = crate::serving::load_subscription_serving_projection(self.pool()).await?;
        serving.ensure_available()?;
        let nodes = serving
            .snapshot
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let facts = self.unlock_node_facts(&nodes).await?;
        Self::chain_unlock_capabilities_from_facts(
            &serving.snapshot,
            &facts,
            app,
            chain,
            schedule::policy(self.pool()).await?,
        )
    }
    fn chain_unlock_capabilities_from_facts(
        snapshot: &brocade_core::model::ModelSnapshot,
        facts: &[PgRow],
        app: &str,
        chain: &str,
        policy: UnlockDetectionPolicy,
    ) -> Result<UnlockProbeCapabilities> {
        let ingress = snapshot
            .apps
            .iter()
            .find(|a| a.id == app)
            .and_then(|a| a.ingresses.iter().find(|i| i.chain == chain));
        let mut caps = UnlockProbeCapabilities::new(policy);
        if let Some(ingress) = ingress {
            // Only transport semantics can prevent propagation of the forced-family identity.
            // Address observations never suppress a real path attempt.
            if crate::probe::unlock_family_probe_nodes(snapshot, &ingress.node, app, chain)?
                .is_none()
            {
                caps.family_mode = UnlockFamilyMode::Automatic;
                caps.automatic_ready = facts
                    .iter()
                    .find(|row| row.get::<String, _>("id") == ingress.node)
                    .map(|row| row.try_get("automatic_ready"))
                    .transpose()?
                    .unwrap_or(false);
            }
        }
        Ok(caps)
    }
    async fn visible_unlock_node(&self, actor: &AdminContext, node: &str) -> Result<()> {
        if self
            .list_node_agent_states(actor)
            .await?
            .nodes
            .iter()
            .any(|item| item.node_id == node)
        {
            return Ok(());
        }
        Err(StoreError::NotFound("node".into()))
    }
    async fn visible_unlock_chain(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
    ) -> Result<()> {
        let tenant: Option<String> =
            sqlx::query_scalar("SELECT tenant_id FROM chains WHERE id=$1 AND app_id=$2")
                .bind(chain)
                .bind(app)
                .fetch_optional(self.pool())
                .await?;
        if tenant.is_some_and(|tenant| actor.can_access_tenant(&tenant)) {
            return Ok(());
        }
        Err(StoreError::NotFound("chain".into()))
    }
    async fn unlock_view(&self, key: &str) -> Result<Option<UnlockProbeView>> {
        sqlx::query("UPDATE unlock_probe_jobs SET state='expired' WHERE scope_key=$1 AND state='running' AND claimed_at<now()-make_interval(secs=>CASE WHEN address_family IS NULL AND chain_id IS NULL THEN 240 ELSE $2::int END)")
                .bind(key).bind(RETRY_LEASE_SECS).execute(self.pool()).await?;
        sqlx::query(SELECT)
            .bind(key)
            .fetch_optional(self.pool())
            .await?
            .map(decode)
            .transpose()
    }

    pub async fn node_unlock_probe(
        &self,
        actor: &AdminContext,
        node: &str,
        start: bool,
    ) -> Result<Option<UnlockProbeView>> {
        self.node_unlock_probe_on_family(actor, node, start, None)
            .await
    }
    pub async fn node_unlock_probe_on_family(
        &self,
        actor: &AdminContext,
        node: &str,
        start: bool,
        family: Option<IpFamily>,
    ) -> Result<Option<UnlockProbeView>> {
        self.visible_unlock_node(actor, node).await?;
        let serving = match crate::serving::load_subscription_serving_projection(self.pool()).await
        {
            Ok(serving) => Some(serving),
            Err(StoreError::Unavailable(_)) => None,
            Err(e) => return Err(e),
        };
        let scope = Scope::node(node)
            .with_dns(serving.as_ref().map(|s| &s.snapshot))?
            .with_family(family);
        if start && family.is_some() {
            let version: Option<i32> = sqlx::query_scalar(
                "SELECT agent_protocol_version FROM node_agent_state WHERE node_id=$1",
            )
            .bind(node)
            .fetch_optional(self.pool())
            .await?
            .flatten();
            if version.is_none_or(|version| version < UNLOCK_DUAL_STACK_PROTOCOL_VERSION as i32) {
                return Err(StoreError::Unsupported(
                    "分族解锁检测需要 Agent 协议 v50".into(),
                ));
            }
        }
        if start {
            require_editor(actor)?;
            let mut tx = serial(self.pool()).await?;
            enqueue(&mut tx, &scope, true).await?;
            tx.commit().await?;
        }
        self.unlock_view(&scope.key).await
    }
    pub async fn chain_unlock_probe(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
        start: bool,
    ) -> Result<Option<UnlockProbeView>> {
        self.chain_unlock_probe_on_family(actor, app, chain, start, None)
            .await
    }
    pub async fn chain_unlock_probe_on_family(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
        start: bool,
        family: Option<IpFamily>,
    ) -> Result<Option<UnlockProbeView>> {
        self.chain_unlock_probe_selected(actor, app, chain, start, family, false)
            .await
    }
    pub async fn chain_unlock_probe_automatic(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
        start: bool,
    ) -> Result<Option<UnlockProbeView>> {
        self.chain_unlock_probe_selected(actor, app, chain, start, None, true)
            .await
    }
    async fn chain_unlock_probe_selected(
        &self,
        actor: &AdminContext,
        app: &str,
        chain: &str,
        start: bool,
        family: Option<IpFamily>,
        automatic: bool,
    ) -> Result<Option<UnlockProbeView>> {
        self.visible_unlock_chain(actor, app, chain).await?;
        let mut key = format!("chain:{app}/{chain}");
        if automatic {
            key.push_str(":auto");
        }
        if let Some(family) = family {
            key.push_str(match family {
                IpFamily::V4 => ":v4",
                IpFamily::V6 => ":v6",
            });
        }
        if start {
            require_editor(actor)?;
            if family.is_some()
                && self
                    .chain_unlock_capabilities(actor, app, chain)
                    .await?
                    .family_mode
                    == UnlockFamilyMode::Automatic
            {
                return Err(StoreError::Unsupported(
                    "线路不支持传递强制地址族，请使用不分族检测".into(),
                ));
            }
            let serving = crate::serving::load_subscription_serving_projection(self.pool()).await?;
            serving.ensure_available()?;
            let ingress = serving
                .snapshot
                .apps
                .iter()
                .find(|a| a.id == app)
                .and_then(|a| a.ingresses.iter().find(|i| i.chain == chain))
                .ok_or_else(|| {
                    StoreError::Conflict("链路尚无已生效入口，请先完成配置发布".into())
                })?;
            let protocol: Option<i32> = sqlx::query_scalar(
                "SELECT agent_protocol_version FROM node_agent_state WHERE node_id=$1",
            )
            .bind(&ingress.node)
            .fetch_optional(self.pool())
            .await?
            .flatten();
            if protocol.is_none_or(|v| {
                v < if automatic {
                    UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION
                } else if family.is_some() {
                    UNLOCK_DUAL_STACK_PROTOCOL_VERSION
                } else {
                    CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION
                } as i32
            }) {
                return Err(StoreError::Unsupported(
                    if automatic {
                        "链路自动检测需要入口 Agent 协议 v52"
                    } else if family.is_some() {
                        "链路分族检测需要入口 Agent 协议 v50"
                    } else {
                        "链路解锁检测需要入口 Agent 协议 v47"
                    }
                    .into(),
                ));
            }
            let targets = crate::probe::e2e_targets_from_snapshot(
                self.pool(),
                &serving.snapshot,
                &ingress.node,
            )
            .await?;
            let target = targets
                .targets
                .iter()
                .find(|t| t.app_id.as_deref() == Some(app) && t.chain_id == chain)
                .ok_or_else(|| StoreError::Unsupported("此入口暂不支持链路解锁检测".into()))?;
            let scope = Scope::chain(&ingress.node, target, serving.topology_revision() as i64)?;
            let scope = if automatic {
                scope.with_automatic()
            } else {
                scope.with_family(family)
            };
            let mut tx = serial(self.pool()).await?;
            enqueue(&mut tx, &scope, true).await?;
            tx.commit().await?;
        }
        self.reconcile_unlock_path(&key).await?;
        self.unlock_view(&key).await
    }

    async fn reconcile_unlock_path(&self, key: &str) -> Result<()> {
        let mut tx = serial(self.pool()).await?;
        let row=sqlx::query("SELECT node_id,app_id,chain_id,configuration_revision,state,completed_at IS NOT NULL AS completed FROM unlock_probe_jobs WHERE scope_key=$1 FOR UPDATE").bind(key).fetch_optional(&mut *tx).await?;
        if let Some(row) = row {
            let current=sqlx::query_scalar::<_,i64>("SELECT topology_revision_id FROM subscription_serving_state WHERE id=TRUE FOR SHARE").fetch_optional(&mut *tx).await?;
            let same = path::matches(
                &mut tx,
                row.try_get("configuration_revision")?,
                current,
                &row.try_get::<String, _>("node_id")?,
                row.try_get::<Option<String>, _>("app_id")?.as_deref(),
                row.try_get::<Option<String>, _>("chain_id")?.as_deref(),
            )
            .await?;
            let state: String = row.try_get("state")?;
            if !same {
                sqlx::query("UPDATE unlock_probe_jobs SET state='obsolete' WHERE scope_key=$1")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            } else if state == "obsolete" && row.try_get::<bool, _>("completed")? {
                sqlx::query("UPDATE unlock_probe_jobs SET state='complete' WHERE scope_key=$1")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Called by the authenticated, active Agent. Scheduling does not depend on a browser visit.
    pub async fn claim_unlock_probe(
        &self,
        node: &str,
        protocol: u32,
    ) -> Result<Option<UnlockProbeTarget>> {
        schedule::advance_cycle(self.pool()).await?;
        let discover=sqlx::query("INSERT INTO unlock_probe_discovery(node_id,next_discovery_at) VALUES($1,now()+interval '60 seconds')
            ON CONFLICT(node_id) DO UPDATE SET next_discovery_at=EXCLUDED.next_discovery_at WHERE unlock_probe_discovery.next_discovery_at<=now() RETURNING node_id")
            .bind(node).fetch_optional(self.pool()).await?.is_some();
        if discover {
            let serving =
                match crate::serving::load_subscription_serving_projection(self.pool()).await {
                    Ok(serving) if serving.ensure_available().is_ok() => Some(serving),
                    Ok(_) | Err(StoreError::Unavailable(_)) => None,
                    Err(error) => return Err(error),
                };
            let policy = schedule::policy(self.pool()).await?;
            let mut scopes =
                vec![Scope::node(node).with_dns(serving.as_ref().map(|s| &s.snapshot))?];
            if protocol >= CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION {
                if let Some(serving) = &serving {
                    let targets = crate::probe::e2e_targets_from_snapshot(
                        self.pool(),
                        &serving.snapshot,
                        node,
                    )
                    .await?;
                    for target in targets.targets {
                        scopes.push(Scope::chain(
                            node,
                            &target,
                            serving.topology_revision() as i64,
                        )?);
                    }
                }
            }
            let facts = if let Some(serving) = &serving {
                let nodes = serving
                    .snapshot
                    .nodes
                    .iter()
                    .map(|node| node.id.clone())
                    .collect::<Vec<_>>();
                self.unlock_node_facts(&nodes).await?
            } else {
                Vec::new()
            };
            let mut scheduled = Vec::new();
            for scope in scopes {
                if protocol >= UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION {
                    if let (Some(app), Some(chain), Some(serving)) =
                        (&scope.app, &scope.chain, &serving)
                    {
                        if Self::chain_unlock_capabilities_from_facts(
                            &serving.snapshot,
                            &facts,
                            app,
                            chain,
                            policy,
                        )?
                        .family_mode
                            == UnlockFamilyMode::Automatic
                        {
                            scheduled.push(scope.with_automatic());
                            continue;
                        }
                    }
                }
                if protocol >= UNLOCK_DUAL_STACK_PROTOCOL_VERSION {
                    scheduled.push(scope.clone().with_family(Some(IpFamily::V4)));
                    scheduled.push(scope.with_family(Some(IpFamily::V6)));
                } else {
                    scheduled.push(scope);
                }
            }
            let mut tx = serial(self.pool()).await?;
            for scope in scheduled {
                enqueue(&mut tx, &scope, false).await?;
            }
            tx.commit().await?;
        }
        let waiting: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM unlock_probe_jobs WHERE node_id=$1 AND state IN ('queued','running') AND ($2 OR chain_id IS NULL) AND ($3 OR address_family IS NULL) AND ($4 OR scope_key NOT LIKE '%:auto'))").bind(node).bind(protocol>=CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION).bind(protocol>=UNLOCK_DUAL_STACK_PROTOCOL_VERSION).bind(protocol>=UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION).fetch_one(self.pool()).await?;
        if !waiting {
            return Ok(None);
        }
        let mut tx = serial(self.pool()).await?;
        let candidate = sqlx::query(
            "SELECT scope_key,job_id,app_id,chain_id,configuration_revision,address_family FROM unlock_probe_jobs
            WHERE node_id=$1 AND state IN ('queued','running') AND ($2 OR chain_id IS NULL) AND ($3 OR address_family IS NULL) AND ($4 OR scope_key NOT LIKE '%:auto')
            ORDER BY (state='running') DESC,created_at,scope_key LIMIT 1 FOR UPDATE",
        )
        .bind(node)
        .bind(protocol >= CHAIN_UNLOCK_PROBE_PROTOCOL_VERSION)
        .bind(protocol >= UNLOCK_DUAL_STACK_PROTOCOL_VERSION)
        .bind(protocol >= UNLOCK_AUTOMATIC_CHAIN_PROTOCOL_VERSION)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = candidate else {
            tx.commit().await?;
            return Ok(None);
        };
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM unlock_probe_jobs WHERE state='running'")
                .fetch_one(&mut *tx)
                .await?;
        let key: String = row.try_get("scope_key")?;
        let already_running: bool =
            sqlx::query_scalar("SELECT state='running' FROM unlock_probe_jobs WHERE scope_key=$1")
                .bind(&key)
                .fetch_one(&mut *tx)
                .await?;
        if running >= 16 && !already_running {
            tx.commit().await?;
            return Ok(None);
        }
        let id: String = row.try_get("job_id")?;
        sqlx::query("UPDATE unlock_probe_jobs SET state='running',claimed_at=COALESCE(claimed_at,now()) WHERE scope_key=$1").bind(&key).execute(&mut *tx).await?;
        tx.commit().await?;
        let chain: Option<String> = row.try_get("chain_id")?;
        let mut target = UnlockProbeTarget {
            probe_id: id,
            services: (protocol >= UNLOCK_EXTENDED_SERVICES_PROTOCOL_VERSION
                && (chain.is_some()
                    || row
                        .try_get::<Option<String>, _>("address_family")?
                        .is_some()))
            .then(|| {
                if protocol >= UNLOCK_REDUCED_SERVICES_PROTOCOL_VERSION {
                    UNLOCK_SERVICES.to_vec()
                } else {
                    // v53 only accepts nine or fifteen services; avoid dispatching
                    // retired checks while its Agent is waiting to upgrade.
                    LEGACY_UNLOCK_SERVICES.to_vec()
                }
            }),
            family: row
                .try_get::<Option<String>, _>("address_family")?
                .map(|value| serde_json::from_value(serde_json::Value::String(value)))
                .transpose()
                .map_err(|_| StoreError::InvalidData("invalid unlock family".into()))?,
            family_ready: None,
            ipv6_egress_check: false,
            chain: None,
            connection_trace: false,
        };
        if let Some(chain) = chain {
            let app: String = row.try_get("app_id")?;
            let revision: i64 = row.try_get("configuration_revision")?;
            let serving = crate::serving::load_subscription_serving_projection(self.pool()).await?;
            serving.ensure_available()?;
            let mut check = self.pool().begin().await?;
            let same = path::matches(
                &mut check,
                Some(revision),
                Some(serving.topology_revision() as i64),
                node,
                Some(&app),
                Some(&chain),
            )
            .await?;
            check.commit().await?;
            if !same {
                self.obsolete_unlock(&key, &target.probe_id).await?;
                return Ok(None);
            }
            if target.family.is_some() {
                let nodes =
                    crate::probe::unlock_family_probe_nodes(&serving.snapshot, node, &app, &chain)?;
                target.family_ready = Some(nodes.is_some());
                if let Some(nodes) = nodes {
                    target.connection_trace=protocol>=brocade_deployment::unlock_probe::UNLOCK_CONNECTION_TRACE_PROTOCOL_VERSION
                        && sqlx::query_scalar::<_,bool>("SELECT count(*)=$2 FROM node_agent_state WHERE node_id=ANY($1) AND last_poll_at>now()-interval '90 seconds' AND runtime_reported_at>now()-interval '90 seconds' AND agent_protocol_version>=$3 AND runtime_versions->>'unlock_connection_trace'='true'")
                        .bind(&nodes).bind(nodes.len()as i64).bind(brocade_deployment::unlock_probe::UNLOCK_CONNECTION_TRACE_PROTOCOL_VERSION as i32).fetch_one(self.pool()).await?;
                }
            }
            target.chain =
                crate::probe::e2e_targets_from_snapshot(self.pool(), &serving.snapshot, node)
                    .await?
                    .targets
                    .into_iter()
                    .find(|t| t.app_id.as_deref() == Some(&app) && t.chain_id == chain);
            if target.chain.is_none() {
                self.obsolete_unlock(&key, &target.probe_id).await?;
                return Ok(None);
            }
        }
        Ok(Some(target))
    }
    async fn obsolete_unlock(&self, key: &str, id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE unlock_probe_jobs SET state='obsolete' WHERE scope_key=$1 AND job_id=$2",
        )
        .bind(key)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
    pub async fn report_unlock_probe(
        &self,
        node: &str,
        mut report: UnlockProbeReport,
    ) -> Result<()> {
        // Only server-side reconciliation may attach a policy explanation.
        for result in &mut report.results {
            result.dns_path = None;
            result.dns_parts.clear();
        }
        if !report.valid() {
            return Err(StoreError::InvalidData("invalid unlock report".into()));
        }
        for result in &mut report.results {
            result.apply_youtube_premium_region_policy();
        }
        report.results.sort_by_key(|r| r.service);
        let mut tx = serial(self.pool()).await?;
        let row=sqlx::query("SELECT state,results,path_error,configuration_revision,app_id,chain_id,address_family,readiness_failures FROM unlock_probe_jobs WHERE node_id=$1 AND job_id=$2 FOR UPDATE").bind(node).bind(&report.probe_id).fetch_optional(&mut *tx).await?.ok_or_else(||StoreError::NotFound("unlock probe".into()))?;
        let expected_family: Option<String> = row.try_get("address_family")?;
        let reported_family = report.family.map(|family| match family {
            IpFamily::V4 => "v4",
            IpFamily::V6 => "v6",
        });
        if expected_family.as_deref() != reported_family {
            return Err(StoreError::InvalidData(
                "unlock report address family mismatch".into(),
            ));
        }
        if report.path_error.is_some() && row.try_get::<Option<String>, _>("chain_id")?.is_none() {
            return Err(StoreError::InvalidData(
                "machine probe cannot report a chain failure".into(),
            ));
        }
        let results = serde_json::to_value(&report.results)
            .map_err(|_| StoreError::InvalidData("invalid unlock report".into()))?;
        let error = report.path_error.map(|code| {
            serde_json::to_value(code)
                .expect("unit enum serializes")
                .as_str()
                .expect("unit enum serializes to string")
                .to_owned()
        });
        let state: String = row.try_get("state")?;
        if state == "complete" && row.try_get::<Option<String>, _>("path_error")? == error {
            let mut stored: Vec<UnlockResult> = serde_json::from_value(row.try_get("results")?)?;
            for result in &mut stored {
                result.dns_path = None;
                result.dns_parts.clear();
                result.apply_youtube_premium_region_policy();
            }
            if serde_json::to_value(stored)? == results {
                tx.commit().await?;
                return Ok(());
            }
        }
        if state != "running" {
            return Err(StoreError::Conflict("unlock probe is not running".into()));
        }
        let revision: Option<i64> = row.try_get("configuration_revision")?;
        if let Some(revision) = revision {
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT topology_revision_id FROM subscription_serving_state WHERE id=TRUE FOR SHARE",
            )
            .fetch_optional(&mut *tx)
            .await?;
            if !path::matches(
                &mut tx,
                Some(revision),
                current,
                node,
                row.try_get::<Option<String>, _>("app_id")?.as_deref(),
                row.try_get::<Option<String>, _>("chain_id")?.as_deref(),
            )
            .await?
            {
                sqlx::query("UPDATE unlock_probe_jobs SET state='obsolete' WHERE job_id=$1")
                    .bind(&report.probe_id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                return Ok(());
            }
            if let (Some(family), None, Some(app), Some(chain)) = (
                report.family,
                report.path_error,
                row.try_get::<Option<String>, _>("app_id")?,
                row.try_get::<Option<String>, _>("chain_id")?,
            ) {
                let snapshot =
                    crate::materialize::load_immutable_snapshot_tx(&mut tx, revision as u64)
                        .await?;
                if let Some(exits) = crate::probe::unlock_dns_exits(&snapshot, node, &app, &chain)?
                {
                    dns::annotate(&snapshot, &exits, family, &mut report.results);
                }
            }
        }
        let results = serde_json::to_value(&report.results)?;
        let failures = if report.path_error == Some(UnlockPathError::FamilyUnavailable) {
            (row.try_get::<i16, _>("readiness_failures")? + 1).min(6)
        } else {
            0
        };
        let retry_secs: i32 =
            match failures {
                1 => 60,
                2 => 120,
                3 => 240,
                4 => 480,
                5 => 900,
                _ => sqlx::query_scalar::<_, i32>(
                    "SELECT unlock_detection_interval_hours*3600 FROM control_state WHERE id=TRUE",
                )
                .fetch_one(&mut *tx)
                .await?,
            };
        sqlx::query("UPDATE unlock_probe_jobs SET state='complete',completed_at=now(),next_run_at=now()+make_interval(secs=>$6::int),results=$3,path_error=$4,readiness_failures=$5 WHERE node_id=$1 AND job_id=$2")
            .bind(node).bind(report.probe_id).bind(results).bind(error).bind(failures).bind(retry_secs).execute(&mut *tx).await?;
        // Keep an already queued batch moving while the real-time channel is connected.
        sqlx::query("SELECT pg_notify('brocade_agent_change',json_build_object('domain','unlock_probe','node',$1::text)::text) WHERE EXISTS(SELECT 1 FROM unlock_probe_jobs WHERE node_id=$1 AND state='queued')")
            .bind(node).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
