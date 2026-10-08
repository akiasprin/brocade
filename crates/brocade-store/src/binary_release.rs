//! Shared audit ledger for Agent and Xray binary releases, not model deployments.
//! The approval freezes scope and digests. Attempts retain their evidence across retries;
//! component-specific executors and health checks remain separate. No historical bytes live here.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{
    AgentReleaseOutcome, AgentReleaseReport, XrayReleaseOutcome, XrayReleaseReport,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

const MAX_NOTE_CHARS: usize = 2_000;
const MAX_ERROR_CHARS: usize = 4_000;
const MAX_VERSION_CHARS: usize = 128;
const MAX_TARGETS: usize = 10_000;
const MAX_ARTIFACTS: usize = 16;
const DISPATCH_LEASE_MINUTES: i32 = 30;
const BINARY_RELEASE_ADVISORY_LOCK: i64 = 0x5852_4159;
const MAX_ATTEMPTS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BinaryComponent {
    Agent,
    Xray,
}

impl BinaryComponent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Xray => "xray",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "agent" => Ok(Self::Agent),
            "xray" => Ok(Self::Xray),
            _ => Err(StoreError::InvalidData(format!(
                "unknown binary component {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryReleaseArtifact {
    pub arch: String,
    pub sha256: String,
}

/// Build facts supplied by the running Console. Store records them but never invents them.
#[derive(Debug, Clone, Copy)]
pub struct BinaryBuildInfo<'a> {
    pub component: BinaryComponent,
    pub build_id: &'a str,
    pub version: &'a str,
    pub artifacts: &'a [BinaryReleaseArtifact],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBinaryReleaseRequest {
    pub idempotency_key: String,
    pub build_id: String,
    pub nodes: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BinaryReleaseStatus {
    Running,
    Halted,
    Succeeded,
    Canceled,
}

impl BinaryReleaseStatus {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "halted" => Ok(Self::Halted),
            "succeeded" => Ok(Self::Succeeded),
            "canceled" => Ok(Self::Canceled),
            other => Err(StoreError::InvalidData(format!(
                "unknown binary release status {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BinaryReleaseTargetStatus {
    Pending,
    Dispatched,
    Succeeded,
    Unverified,
    FailedRecovered,
    FailedDirty,
    Unsupported,
    Canceled,
}

impl BinaryReleaseTargetStatus {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "dispatched" => Ok(Self::Dispatched),
            "succeeded" => Ok(Self::Succeeded),
            "unverified" => Ok(Self::Unverified),
            "failed-recovered" => Ok(Self::FailedRecovered),
            "failed-dirty" => Ok(Self::FailedDirty),
            "unsupported" => Ok(Self::Unsupported),
            "canceled" => Ok(Self::Canceled),
            other => Err(StoreError::InvalidData(format!(
                "unknown binary release target status {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryReleaseTarget {
    pub node_id: String,
    pub status: BinaryReleaseTargetStatus,
    pub attempt: u32,
    pub before_sha256: String,
    pub desired_sha256: Option<String>,
    pub arch: Option<String>,
    pub error: Option<String>,
    pub reported_performed_update: Option<bool>,
    pub reported_service_enabled: Option<bool>,
    pub reported_installed_sha256: Option<String>,
    pub reported_running_sha256: Option<String>,
    /// Failed outcomes and an expired dispatch lease can be retried by an operator. Calculating
    /// this at the database clock keeps the browser from inventing lease state from its own clock.
    pub retryable: bool,
    pub verification: Option<String>,
    pub dispatched_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryReleaseEvent {
    pub id: i64,
    pub kind: String,
    pub node_id: Option<String>,
    pub actor: Option<String>,
    pub detail: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryReleaseAttempt {
    pub node_id: String,
    pub attempt: u32,
    pub status: BinaryReleaseTargetStatus,
    pub verification: Option<String>,
    pub error: Option<String>,
    pub evidence: serde_json::Value,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryRelease {
    pub id: i64,
    pub component: BinaryComponent,
    pub build_id: String,
    pub version: String,
    pub artifacts: Vec<BinaryReleaseArtifact>,
    pub status: BinaryReleaseStatus,
    pub active: bool,
    pub note: Option<String>,
    pub created_at: String,
    pub created_by: String,
    pub halted_at: Option<String>,
    pub finished_at: Option<String>,
    pub targets: Vec<BinaryReleaseTarget>,
    pub events: Vec<BinaryReleaseEvent>,
    pub next_event_before_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryReleaseList {
    pub releases: Vec<BinaryRelease>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinaryReleaseSummary {
    pub id: i64,
    pub component: BinaryComponent,
    pub build_id: String,
    pub version: String,
    pub status: BinaryReleaseStatus,
    pub active: bool,
    pub note: Option<String>,
    pub created_at: String,
    pub created_by: String,
    pub halted_at: Option<String>,
    pub finished_at: Option<String>,
    pub target_count: u64,
    pub succeeded_count: u64,
    pub problem_count: u64,
}

/// Store-side assignment. Console adds the URL because only it knows the public agent origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryReleaseAssignment {
    pub release_id: i64,
    pub attempt: u32,
    pub version: String,
    pub sha256: String,
    pub previous_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryReleaseObservation {
    installed_sha256: String,
    running_sha256: Option<String>,
}

pub async fn list_binary_releases(
    pool: &PgPool,
    component: BinaryComponent,
    limit: u32,
) -> Result<BinaryReleaseList> {
    let ids = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM binary_releases WHERE component = $2 ORDER BY id DESC LIMIT $1",
    )
    .bind(i64::from(limit.clamp(1, 50)))
    .bind(component.as_str())
    .fetch_all(pool)
    .await?;
    let mut releases = Vec::with_capacity(ids.len());
    for id in ids {
        releases.push(load_binary_release(pool, component, id, true).await?);
    }
    Ok(BinaryReleaseList { releases })
}

pub async fn get_binary_release(
    pool: &PgPool,
    component: BinaryComponent,
    id: i64,
) -> Result<BinaryRelease> {
    load_binary_release(pool, component, id, true).await
}

/// Return current target state without the append-only event ledger. This is the shape polled by
/// Console while a rollout is active; audit events are loaded only from the detail endpoint.
pub async fn get_binary_release_overview(
    pool: &PgPool,
    component: BinaryComponent,
    id: i64,
) -> Result<BinaryRelease> {
    load_binary_release(pool, component, id, false).await
}

pub async fn list_binary_release_summaries(
    pool: &PgPool,
    component: BinaryComponent,
    limit: u32,
    before_id: Option<i64>,
) -> Result<Vec<BinaryReleaseSummary>> {
    if before_id.is_some_and(|id| id <= 0) {
        return Err(StoreError::InvalidData(
            "binary release history cursor must be positive".to_owned(),
        ));
    }
    let rows = sqlx::query(
        "WITH page AS MATERIALIZED (
             SELECT * FROM binary_releases
              WHERE component = $3 AND ($2::bigint IS NULL OR id < $2)
              ORDER BY id DESC LIMIT $1
         )
         SELECT r.id, r.component, r.build_id, r.version, r.status, r.active,
                r.note, r.created_at::text AS created_at, r.created_by,
                r.halted_at::text AS halted_at, r.finished_at::text AS finished_at,
                count(t.node_id) AS target_count,
                count(t.node_id) FILTER (WHERE t.status = 'succeeded') AS succeeded_count,
                count(t.node_id) FILTER (
                    WHERE t.status IN (
                        'unverified', 'failed-recovered', 'failed-dirty', 'unsupported'
                    )
                ) AS problem_count
           FROM page r
           LEFT JOIN binary_release_targets t ON t.release_id = r.id
          GROUP BY r.id, r.component, r.build_id, r.version, r.status, r.active,
                   r.note, r.created_at, r.created_by, r.halted_at, r.finished_at
          ORDER BY r.id DESC",
    )
    .bind(i64::from(limit.clamp(1, 50)))
    .bind(before_id)
    .bind(component.as_str())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(BinaryReleaseSummary {
                id: row.try_get("id")?,
                component: BinaryComponent::parse(row.try_get::<String, _>("component")?.as_str())?,
                build_id: row.try_get("build_id")?,
                version: row.try_get("version")?,
                status: BinaryReleaseStatus::parse(row.try_get::<String, _>("status")?.as_str())?,
                active: row.try_get("active")?,
                note: row.try_get("note")?,
                created_at: row.try_get("created_at")?,
                created_by: row.try_get("created_by")?,
                halted_at: row.try_get("halted_at")?,
                finished_at: row.try_get("finished_at")?,
                target_count: u64::try_from(row.try_get::<i64, _>("target_count")?)
                    .map_err(|_| invalid_number("target_count"))?,
                succeeded_count: u64::try_from(row.try_get::<i64, _>("succeeded_count")?)
                    .map_err(|_| invalid_number("succeeded_count"))?,
                problem_count: u64::try_from(row.try_get::<i64, _>("problem_count")?)
                    .map_err(|_| invalid_number("problem_count"))?,
            })
        })
        .collect()
}

pub async fn create_binary_release(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateBinaryReleaseRequest,
    build: BinaryBuildInfo<'_>,
) -> Result<BinaryRelease> {
    require_system_admin(actor)?;
    let component = build.component;
    validate_build(build)?;
    let requested_build_id = request.build_id.trim().to_ascii_lowercase();
    if requested_build_id != build.build_id {
        return Err(StoreError::Conflict(
            "requested binary build is not carried by this Console".to_owned(),
        ));
    }
    let nodes = normalize_nodes(request.nodes);
    if nodes.is_empty() {
        return Err(StoreError::InvalidData(
            "a binary release needs at least one node".to_owned(),
        ));
    }
    if nodes.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "a binary release cannot target more than {MAX_TARGETS} nodes"
        )));
    }
    let note = normalize_text(request.note, MAX_NOTE_CHARS, "binary release note")?;
    let idempotency_key = request.idempotency_key.trim().to_owned();
    if idempotency_key.is_empty() || idempotency_key.chars().count() > 200 {
        return Err(StoreError::InvalidData(
            "binary release idempotency_key must contain 1 to 200 characters".to_owned(),
        ));
    }
    let artifacts = artifacts_json(build.artifacts);

    let mut tx = pool.begin().await?;
    // The partial unique index is the final invariant; this lock turns a concurrent create into a
    // readable conflict instead of a database-specific uniqueness error. It also gives the
    // operator one explicit action per state transition: an active release must be canceled
    // before another can be created, rather than being silently superseded by a second click.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BINARY_RELEASE_ADVISORY_LOCK)
        .execute(&mut *tx)
        .await?;
    if let Some(existing) = sqlx::query(
        "SELECT id, build_id, version, note
           FROM binary_releases WHERE idempotency_key = $1 AND component = $2",
    )
    .bind(&idempotency_key)
    .bind(component.as_str())
    .fetch_optional(&mut *tx)
    .await?
    {
        let existing_id: i64 = existing.try_get("id")?;
        let target_rows =
            sqlx::query("SELECT node_id FROM binary_release_targets WHERE release_id = $1")
                .bind(existing_id)
                .fetch_all(&mut *tx)
                .await?;
        let mut existing_nodes = Vec::with_capacity(target_rows.len());
        for row in target_rows {
            let node_id: String = row.try_get("node_id")?;
            existing_nodes.push(node_id);
        }
        existing_nodes.sort();
        if existing.try_get::<String, _>("build_id")? != requested_build_id
            || existing.try_get::<String, _>("version")? != build.version
            || existing.try_get::<Option<String>, _>("note")? != note
            || existing_nodes != nodes
        {
            return Err(StoreError::Conflict(
                "binary release idempotency_key already belongs to another request".to_owned(),
            ));
        }
        tx.commit().await?;
        return load_binary_release(pool, component, existing_id, true).await;
    }
    if let Some(active) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM binary_releases WHERE active AND component = $1 LIMIT 1",
    )
    .bind(component.as_str())
    .fetch_optional(&mut *tx)
    .await?
    {
        return Err(StoreError::Conflict(format!(
            "binary release {active} is still active; cancel or finish it first"
        )));
    }
    let observations = release_observations(&mut tx, component, &nodes).await?;
    for node_id in &nodes {
        let observation = &observations[node_id];
        if observation.running_sha256.as_deref() != Some(observation.installed_sha256.as_str()) {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} is not currently running its managed Xray"
            )));
        }
        if build
            .artifacts
            .iter()
            .any(|artifact| artifact.sha256 == observation.installed_sha256)
        {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} already has bytes carried by this Console"
            )));
        }
    }

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO binary_releases
                    (idempotency_key, build_id, version, artifacts, component, note, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
          RETURNING id",
    )
    .bind(&idempotency_key)
    .bind(&requested_build_id)
    .bind(build.version)
    .bind(artifacts)
    .bind(component.as_str())
    .bind(&note)
    .bind(actor.operator_id())
    .fetch_one(&mut *tx)
    .await?;

    for node_id in &nodes {
        sqlx::query(
            "INSERT INTO binary_release_targets
                        (release_id, node_id, before_sha256)
                 VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(node_id)
        .bind(&observations[node_id].installed_sha256)
        .execute(&mut *tx)
        .await?;
    }
    insert_event(
        &mut tx,
        id,
        "created",
        None,
        Some(actor.operator_id()),
        serde_json::json!({ "targets": nodes.len() }),
    )
    .await?;
    sqlx::query("INSERT INTO binary_release_attempts (release_id, node_id, attempt) SELECT release_id, node_id, attempt FROM binary_release_targets WHERE release_id = $1")
        .bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    load_binary_release(pool, component, id, true).await
}

pub async fn cancel_binary_release(
    pool: &PgPool,
    component: BinaryComponent,
    actor: &AdminContext,
    release_id: i64,
) -> Result<BinaryRelease> {
    require_system_admin(actor)?;
    let mut tx = pool.begin().await?;
    let row = lock_release(&mut tx, component, release_id).await?;
    if !row.try_get::<bool, _>("active")? {
        return Err(StoreError::Conflict(format!(
            "binary release {release_id} is already inactive"
        )));
    }
    sqlx::query(
        "UPDATE binary_releases
            SET status = 'canceled', finished_at = now()
          WHERE id = $1",
    )
    .bind(release_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE binary_release_targets
            SET status = 'canceled', finished_at = now()
          WHERE release_id = $1 AND status IN ('pending', 'dispatched')",
    )
    .bind(release_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE binary_release_attempts SET status = 'canceled', finished_at = now() WHERE release_id = $1 AND status IN ('pending', 'dispatched')")
        .bind(release_id).execute(&mut *tx).await?;
    insert_event(
        &mut tx,
        release_id,
        "canceled",
        None,
        Some(actor.operator_id()),
        serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;
    load_binary_release(pool, component, release_id, true).await
}

/// Serialize a lifecycle transition with release creation and close the entire approval when the
/// transitioning machine is one of its targets. A binary replacement is one immutable work order;
/// silently dropping one target would change its requested scope.
pub(crate) async fn cancel_for_node_lifecycle_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    actor: &str,
) -> Result<Option<i64>> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BINARY_RELEASE_ADVISORY_LOCK)
        .execute(&mut **tx)
        .await?;
    let ids = sqlx::query_scalar::<_, i64>(
        "SELECT r.id FROM binary_releases r JOIN binary_release_targets t ON t.release_id = r.id
         WHERE r.active AND t.node_id = $1 ORDER BY r.id FOR UPDATE OF r, t",
    )
    .bind(node_id)
    .fetch_all(&mut **tx)
    .await?;
    for id in &ids {
        sqlx::query(
            "UPDATE binary_releases SET status = 'canceled', finished_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE binary_release_targets SET status = 'canceled', finished_at = now() WHERE release_id = $1 AND status IN ('pending', 'dispatched')").bind(id).execute(&mut **tx).await?;
        sqlx::query("UPDATE binary_release_attempts SET status = 'canceled', finished_at = now() WHERE release_id = $1 AND status IN ('pending', 'dispatched')").bind(id).execute(&mut **tx).await?;
        insert_event(
            tx,
            *id,
            "canceled-for-node-lifecycle",
            Some(node_id),
            Some(actor),
            serde_json::json!({"reason": "target left active lifecycle"}),
        )
        .await?;
    }
    Ok(ids.first().copied())
}

pub async fn retry_binary_release_target(
    pool: &PgPool,
    component: BinaryComponent,
    actor: &AdminContext,
    release_id: i64,
    node_id: &str,
    available_build_id: &str,
) -> Result<BinaryRelease> {
    require_system_admin(actor)?;
    let mut tx = pool.begin().await?;
    let row = lock_release(&mut tx, component, release_id).await?;
    ensure_actionable_release(&row, release_id, available_build_id)?;
    let result = sqlx::query(
        "UPDATE binary_release_targets
            SET status = 'pending', attempt = attempt + 1, error = NULL,
                dispatched_at = NULL, finished_at = NULL,
                reported_performed_update = NULL, reported_service_enabled = NULL,
                reported_installed_sha256 = NULL,
                reported_running_sha256 = NULL, verification = NULL,
                arch = CASE WHEN status = 'unsupported' THEN NULL ELSE arch END,
                desired_sha256 = CASE WHEN status = 'unsupported' THEN NULL ELSE desired_sha256 END
          WHERE release_id = $1 AND node_id = $2 AND attempt < 32
            AND (
                status IN ('unverified', 'failed-recovered', 'failed-dirty', 'unsupported')
                OR (
                    status = 'dispatched'
                    AND dispatched_at <= now() - make_interval(mins => $3)
                )
            )",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(DISPATCH_LEASE_MINUTES)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::Conflict(format!(
            "binary release target {release_id}/{node_id} is not retryable; a dispatch lease must be at least {DISPATCH_LEASE_MINUTES} minutes old"
        )));
    }
    sqlx::query(
        "UPDATE binary_release_attempts SET status = 'unverified', finished_at = now(),
        error = COALESCE(error, 'execution lease expired; superseded by an operator retry')
        WHERE release_id = $1 AND node_id = $2 AND status = 'dispatched'",
    )
    .bind(release_id)
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO binary_release_attempts (release_id, node_id, attempt)
        SELECT release_id, node_id, attempt FROM binary_release_targets WHERE release_id = $1 AND node_id = $2")
        .bind(release_id).bind(node_id).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE binary_releases
            SET status = 'running', halted_at = NULL
          WHERE id = $1",
    )
    .bind(release_id)
    .execute(&mut *tx)
    .await?;
    insert_event(
        &mut tx,
        release_id,
        "target-retried",
        Some(node_id),
        Some(actor.operator_id()),
        serde_json::json!({}),
    )
    .await?;
    tx.commit().await?;
    load_binary_release(pool, component, release_id, true).await
}

pub async fn claim_binary_release(
    pool: &PgPool,
    component: BinaryComponent,
    node_id: &str,
    arch: &str,
    available_build_id: &str,
    receipt_capable: bool,
) -> Result<Option<BinaryReleaseAssignment>> {
    if arch.is_empty()
        || arch.len() > 32
        || !arch
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(StoreError::InvalidData(
            "binary release architecture is invalid".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BINARY_RELEASE_ADVISORY_LOCK)
        .execute(&mut *tx)
        .await?;
    let Some(row) = sqlx::query(
        "SELECT r.id, r.build_id, r.version, r.artifacts,
                t.status, t.attempt, t.before_sha256, t.desired_sha256, t.arch
           FROM binary_releases r
           JOIN binary_release_targets t ON t.release_id = r.id
          WHERE r.active AND r.status = 'running' AND t.node_id = $1 AND r.component = $2
            AND t.status IN ('pending', 'dispatched')
          ORDER BY r.id DESC
          LIMIT 1
          FOR UPDATE OF r, t",
    )
    .bind(node_id)
    .bind(component.as_str())
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.commit().await?;
        return Ok(None);
    };
    let release_id: i64 = row.try_get("id")?;
    if row.try_get::<String, _>("build_id")? != available_build_id {
        tx.commit().await?;
        return Ok(None);
    }
    let artifacts = parse_artifacts(row.try_get("artifacts")?)?;
    let Some(artifact) = artifacts.iter().find(|artifact| artifact.arch == arch) else {
        mark_unsupported_arch(&mut tx, release_id, node_id, arch).await?;
        tx.commit().await?;
        return Ok(None);
    };
    let recorded_arch: Option<String> = row.try_get("arch")?;
    let recorded_sha: Option<String> = row.try_get("desired_sha256")?;
    if recorded_arch
        .as_deref()
        .is_some_and(|recorded| recorded != arch)
        || recorded_sha
            .as_deref()
            .is_some_and(|recorded| recorded != artifact.sha256.as_str())
    {
        mark_unsupported_arch(&mut tx, release_id, node_id, arch).await?;
        tx.commit().await?;
        return Ok(None);
    }
    sqlx::query(
        "UPDATE binary_release_targets
            SET status = 'dispatched', arch = $3, desired_sha256 = $4,
                dispatched_at = COALESCE(dispatched_at, now())
          WHERE release_id = $1 AND node_id = $2",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(arch)
    .bind(&artifact.sha256)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE binary_release_attempts SET status = 'dispatched',
        started_at = COALESCE(started_at, now()),
        receipt_capable = COALESCE(receipt_capable, $3)
        WHERE release_id = $1 AND node_id = $2 AND attempt = $4",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(receipt_capable)
    .bind(row.try_get::<i32, _>("attempt")?)
    .execute(&mut *tx)
    .await?;
    let assignment = BinaryReleaseAssignment {
        release_id,
        attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
            .map_err(|_| invalid_number("attempt"))?,
        version: row.try_get("version")?,
        sha256: artifact.sha256.clone(),
        previous_sha256: row.try_get("before_sha256")?,
    };
    tx.commit().await?;
    Ok(Some(assignment))
}

/// Domain-specific reports share accounting, never their health criteria.
pub async fn report_xray_release(
    pool: &PgPool,
    node_id: &str,
    report: &XrayReleaseReport,
) -> Result<bool> {
    settle_report(pool, node_id, ExecutionReport::Xray(report)).await
}

pub async fn report_agent_release(
    pool: &PgPool,
    node_id: &str,
    report: &AgentReleaseReport,
) -> Result<bool> {
    settle_report(pool, node_id, ExecutionReport::Agent(report)).await
}

#[derive(Serialize)]
#[serde(tag = "component", content = "report", rename_all = "kebab-case")]
enum ExecutionReport<'a> {
    Agent(&'a AgentReleaseReport),
    Xray(&'a XrayReleaseReport),
}

async fn settle_report(pool: &PgPool, node_id: &str, report: ExecutionReport<'_>) -> Result<bool> {
    let (component, release_id, attempt, installed, running, performed, service_enabled, error) =
        match &report {
            ExecutionReport::Agent(r) => (
                BinaryComponent::Agent,
                r.release_id,
                r.attempt,
                r.installed_sha256.as_deref(),
                r.running_sha256.as_deref(),
                r.performed_update,
                None,
                r.error.clone(),
            ),
            ExecutionReport::Xray(r) => (
                BinaryComponent::Xray,
                r.release_id,
                r.attempt,
                r.installed_sha256.as_deref(),
                r.running_sha256.as_deref(),
                r.performed_update,
                Some(r.xray_enabled),
                r.error.clone(),
            ),
        };
    if release_id <= 0
        || attempt == 0
        || attempt > MAX_ATTEMPTS
        || [installed, running]
            .into_iter()
            .flatten()
            .any(|s| !valid_sha256(s))
    {
        return Err(StoreError::InvalidData(
            "invalid binary release report identity or digest".to_owned(),
        ));
    }
    let mut error = normalize_text(error, MAX_ERROR_CHARS, "binary release error")?;
    let mut tx = pool.begin().await?;
    let release = lock_release(&mut tx, component, release_id).await?;
    let row = sqlx::query(
        "SELECT t.status, t.attempt, t.before_sha256, t.desired_sha256,
                a.receipt_capable, t.dispatched_at
           FROM binary_release_targets t JOIN binary_release_attempts a
             ON (a.release_id, a.node_id, a.attempt) = (t.release_id, t.node_id, t.attempt)
          WHERE t.release_id = $1 AND t.node_id = $2 FOR UPDATE OF t, a",
    )
    .bind(release_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("binary release target {release_id}/{node_id}")))?;
    if u32::try_from(row.try_get::<i32, _>("attempt")?).map_err(|_| invalid_number("attempt"))?
        != attempt
    {
        tx.commit().await?;
        return Ok(false);
    }
    let existing: String = row.try_get("status")?;
    let active: bool = release.try_get("active")?;
    let before: String = row.try_get("before_sha256")?;
    let desired: Option<String> = row.try_get("desired_sha256")?;
    let late = !active
        && release.try_get::<String, _>("status")? == "canceled"
        && existing == "canceled"
        && desired.is_some();
    if !active && !late {
        tx.commit().await?;
        return Ok(false);
    }
    if active && !matches!(existing.as_str(), "pending" | "dispatched") {
        tx.commit().await?;
        return Ok(true);
    }
    let desired = desired
        .as_deref()
        .ok_or_else(|| StoreError::Conflict("target has not claimed this attempt".to_owned()))?;
    let receipt_capable: Option<bool> = row.try_get("receipt_capable")?;
    let mut verification = "receipt";
    let status = match &report {
        ExecutionReport::Xray(r) => match r.outcome {
            XrayReleaseOutcome::Succeeded => {
                if installed != Some(desired) || (r.xray_enabled && running != Some(desired)) {
                    return Err(StoreError::InvalidData(
                        "successful Xray report does not identify the approved bytes".to_owned(),
                    ));
                }
                if !performed || before == desired || !r.xray_enabled || running != Some(desired) {
                    error =
                        Some("未证明本次真实替换并启动目标 Xray，不能作为发布成功证据".to_owned());
                    "unverified"
                } else {
                    "succeeded"
                }
            }
            XrayReleaseOutcome::FailedRecovered => {
                if installed != Some(before.as_str())
                    || (r.xray_enabled && running != Some(before.as_str()))
                {
                    return Err(StoreError::InvalidData(
                        "recovered Xray report does not identify the frozen prior bytes".to_owned(),
                    ));
                }
                "failed-recovered"
            }
            XrayReleaseOutcome::FailedDirty => "failed-dirty",
            XrayReleaseOutcome::Unsupported => "unsupported",
        },
        ExecutionReport::Agent(r) => match r.outcome {
            AgentReleaseOutcome::Running => {
                if installed != Some(desired) || running != Some(desired) {
                    return Err(StoreError::InvalidData(
                        "Agent has not restarted into the approved bytes".to_owned(),
                    ));
                }
                // Both facts must have arrived after dispatch, under this node's authenticated
                // identity. A pre-restart response or a stale runtime report cannot settle work.
                let healthy = sqlx::query_scalar::<_, bool>(
                    "SELECT COALESCE(
                        regexp_replace(s.agent_version, '^brocade-agent/', '') = $3
                        AND s.runtime_versions->>'agent' = $3
                        AND s.agent_protocol_version >= $4
                        AND s.last_poll_at >= t.dispatched_at
                        AND s.runtime_reported_at >= t.dispatched_at
                        AND s.last_poll_at >= now() - interval '90 seconds'
                        AND s.runtime_reported_at >= now() - interval '2 minutes', FALSE)
                     FROM node_agent_state s JOIN binary_release_targets t ON t.node_id = s.node_id
                     WHERE t.release_id = $1 AND t.node_id = $2",
                )
                .bind(release_id)
                .bind(node_id)
                .bind(desired)
                .bind(
                    i32::try_from(brocade_deployment::protocol::MIN_AGENT_PROTOCOL_VERSION)
                        .map_err(|_| invalid_number("protocol"))?,
                )
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or(false);
                if !healthy {
                    return Err(StoreError::Conflict(
                        "waiting for fresh target-Agent poll and runtime confirmation".to_owned(),
                    ));
                }
                if receipt_capable == Some(true) && !performed {
                    error = Some("运行摘要已匹配，但本次替换的持久回执缺失".to_owned());
                    "unverified"
                } else {
                    verification = if performed { "receipt" } else { "observed" };
                    "succeeded"
                }
            }
            AgentReleaseOutcome::Failed => {
                if installed == Some(before.as_str()) && running == Some(before.as_str()) {
                    "failed-recovered"
                } else {
                    "failed-dirty"
                }
            }
        },
    };
    let evidence = serde_json::to_value(&report)?;
    sqlx::query(
        "UPDATE binary_release_targets SET status = $3, error = $4,
             reported_performed_update = $5, reported_service_enabled = $6,
             reported_installed_sha256 = $7, reported_running_sha256 = $8,
             verification = $9, finished_at = now()
         WHERE release_id = $1 AND node_id = $2",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(status)
    .bind(&error)
    .bind(performed)
    .bind(service_enabled)
    .bind(installed)
    .bind(running)
    .bind(verification)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE binary_release_attempts SET status = $4, error = $5, verification = $6,
             evidence = $7, finished_at = now()
         WHERE release_id = $1 AND node_id = $2 AND attempt = $3",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(i64::from(attempt))
    .bind(status)
    .bind(&error)
    .bind(verification)
    .bind(&evidence)
    .execute(&mut *tx)
    .await?;
    insert_event(&mut tx, release_id, if late { "target-reported-after-cancel" } else { "target-reported" },
        Some(node_id), None, serde_json::json!({"attempt": attempt, "status": status, "verification": verification, "error": error})).await?;
    if !late {
        let remaining = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM binary_release_targets WHERE release_id = $1 AND status <> 'succeeded')"
        ).bind(release_id).fetch_one(&mut *tx).await?;
        if !remaining {
            sqlx::query("UPDATE binary_releases SET status = 'succeeded', finished_at = now() WHERE id = $1")
                .bind(release_id).execute(&mut *tx).await?;
            insert_event(
                &mut tx,
                release_id,
                "succeeded",
                None,
                None,
                serde_json::json!({}),
            )
            .await?;
        } else if status != "succeeded" {
            sqlx::query(
                "UPDATE binary_releases SET status = 'halted', halted_at = now() WHERE id = $1",
            )
            .bind(release_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

async fn load_binary_release(
    pool: &PgPool,
    component: BinaryComponent,
    id: i64,
    include_events: bool,
) -> Result<BinaryRelease> {
    let row = sqlx::query(
        "SELECT id, component, build_id, version, artifacts, status, active, note,
                created_at::text AS created_at, created_by,
                halted_at::text AS halted_at, finished_at::text AS finished_at
           FROM binary_releases WHERE id = $1 AND component = $2",
    )
    .bind(id)
    .bind(component.as_str())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("binary release {id}")))?;
    let target_rows = sqlx::query(
        "SELECT node_id, status, attempt, before_sha256, desired_sha256, arch, error, verification,
                reported_performed_update, reported_service_enabled,
                reported_installed_sha256, reported_running_sha256,
                (attempt < 32 AND (
                    status IN ('unverified', 'failed-recovered', 'failed-dirty', 'unsupported')
                    OR (
                        status = 'dispatched'
                        AND dispatched_at <= now() - make_interval(mins => $2)
                    )
                )) AS retryable,
                dispatched_at::text AS dispatched_at, finished_at::text AS finished_at
           FROM binary_release_targets WHERE release_id = $1 ORDER BY node_id",
    )
    .bind(id)
    .bind(DISPATCH_LEASE_MINUTES)
    .fetch_all(pool)
    .await?;
    let event_rows = if include_events {
        sqlx::query(
            "SELECT id, kind, node_id, actor, detail, created_at::text AS created_at
               FROM binary_release_events WHERE release_id = $1 ORDER BY id DESC LIMIT 201",
        )
        .bind(id)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };
    let next_event_before_id = if event_rows.len() > 200 {
        Some(event_rows[199].try_get("id")?)
    } else {
        None
    };
    Ok(BinaryRelease {
        id: row.try_get("id")?,
        component: BinaryComponent::parse(row.try_get::<String, _>("component")?.as_str())?,
        build_id: row.try_get("build_id")?,
        version: row.try_get("version")?,
        artifacts: parse_artifacts(row.try_get("artifacts")?)?,
        status: BinaryReleaseStatus::parse(row.try_get::<String, _>("status")?.as_str())?,
        active: row.try_get("active")?,
        note: row.try_get("note")?,
        created_at: row.try_get("created_at")?,
        created_by: row.try_get("created_by")?,
        halted_at: row.try_get("halted_at")?,
        finished_at: row.try_get("finished_at")?,
        targets: target_rows
            .into_iter()
            .map(|row| {
                Ok(BinaryReleaseTarget {
                    node_id: row.try_get("node_id")?,
                    status: BinaryReleaseTargetStatus::parse(
                        row.try_get::<String, _>("status")?.as_str(),
                    )?,
                    attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
                        .map_err(|_| invalid_number("attempt"))?,
                    before_sha256: row.try_get("before_sha256")?,
                    desired_sha256: row.try_get("desired_sha256")?,
                    arch: row.try_get("arch")?,
                    error: row.try_get("error")?,
                    reported_performed_update: row.try_get("reported_performed_update")?,
                    reported_service_enabled: row.try_get("reported_service_enabled")?,
                    reported_installed_sha256: row.try_get("reported_installed_sha256")?,
                    reported_running_sha256: row.try_get("reported_running_sha256")?,
                    retryable: row.try_get("retryable")?,
                    verification: row.try_get("verification")?,
                    dispatched_at: row.try_get("dispatched_at")?,
                    finished_at: row.try_get("finished_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        next_event_before_id,
        events: event_rows
            .into_iter()
            .take(200)
            .map(|row| {
                Ok(BinaryReleaseEvent {
                    id: row.try_get("id")?,
                    kind: row.try_get("kind")?,
                    node_id: row.try_get("node_id")?,
                    actor: row.try_get("actor")?,
                    detail: row.try_get("detail")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn release_attempts(
    pool: &PgPool,
    component: BinaryComponent,
    id: i64,
    node: &str,
) -> Result<Vec<BinaryReleaseAttempt>> {
    let rows = sqlx::query(
        "SELECT a.node_id, a.attempt, a.status, a.verification, a.error, a.evidence,
        a.started_at::text AS started_at, a.finished_at::text AS finished_at
        FROM binary_release_attempts a JOIN binary_releases r ON r.id = a.release_id
        WHERE r.id = $1 AND r.component = $2 AND a.node_id = $3 ORDER BY a.attempt DESC LIMIT 32",
    )
    .bind(id)
    .bind(component.as_str())
    .bind(node)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(BinaryReleaseAttempt {
                node_id: r.try_get("node_id")?,
                attempt: u32::try_from(r.try_get::<i32, _>("attempt")?)
                    .map_err(|_| invalid_number("attempt"))?,
                status: BinaryReleaseTargetStatus::parse(
                    r.try_get::<String, _>("status")?.as_str(),
                )?,
                verification: r.try_get("verification")?,
                error: r.try_get("error")?,
                evidence: r.try_get("evidence")?,
                started_at: r.try_get("started_at")?,
                finished_at: r.try_get("finished_at")?,
            })
        })
        .collect()
}

pub async fn release_events(
    pool: &PgPool,
    component: BinaryComponent,
    id: i64,
    before: i64,
) -> Result<Vec<BinaryReleaseEvent>> {
    if before <= 0 {
        return Err(StoreError::InvalidData(
            "event cursor must be positive".to_owned(),
        ));
    }
    let rows = sqlx::query(
        "SELECT e.id, e.kind, e.node_id, e.actor, e.detail, e.created_at::text AS created_at
        FROM binary_release_events e JOIN binary_releases r ON r.id = e.release_id
        WHERE r.id = $1 AND r.component = $2 AND e.id < $3 ORDER BY e.id DESC LIMIT 201",
    )
    .bind(id)
    .bind(component.as_str())
    .bind(before)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(BinaryReleaseEvent {
                id: r.try_get("id")?,
                kind: r.try_get("kind")?,
                node_id: r.try_get("node_id")?,
                actor: r.try_get("actor")?,
                detail: r.try_get("detail")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect()
}

async fn release_observations(
    tx: &mut Transaction<'_, Postgres>,
    component: BinaryComponent,
    nodes: &[String],
) -> Result<BTreeMap<String, BinaryReleaseObservation>> {
    let rows = sqlx::query(
        "SELECT n.id, lifecycle.phase, state.agent_version,
                state.runtime_versions->>'xray_installed_sha256' AS xray_installed_sha256,
                state.runtime_versions->>'xray_running_sha256' AS xray_running_sha256,
                COALESCE(state.last_poll_at >= now() - interval '90 seconds', FALSE)
                    AS poll_fresh,
                COALESCE(state.runtime_reported_at >= now() - interval '2 minutes', FALSE)
                    AS runtime_fresh
           FROM nodes n
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = n.id
           LEFT JOIN node_agent_state state ON state.node_id = n.id
          WHERE n.id = ANY($1)
          FOR SHARE OF lifecycle",
    )
    .bind(nodes)
    .fetch_all(&mut **tx)
    .await?;
    let mut observations = BTreeMap::new();
    let mut found = BTreeSet::new();
    for row in rows {
        let node_id: String = row.try_get("id")?;
        found.insert(node_id.clone());
        let phase: String = row.try_get("phase")?;
        if phase != "active" {
            return Err(StoreError::InvalidData(format!(
                "binary release target {node_id} is {phase}, not active"
            )));
        }
        if !row.try_get::<bool, _>("poll_fresh")? {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} has not polled desired state in the last 90 seconds"
            )));
        }
        if component == BinaryComponent::Xray && !row.try_get::<bool, _>("runtime_fresh")? {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} has not reported runtime state in the last 2 minutes"
            )));
        }
        let sha: Option<String> = match component {
            BinaryComponent::Xray => row.try_get("xray_installed_sha256")?,
            BinaryComponent::Agent => row
                .try_get::<Option<String>, _>("agent_version")?
                .map(|s| s.strip_prefix("brocade-agent/").unwrap_or(&s).to_owned()),
        };
        let sha = sha.filter(|sha| valid_sha256(sha)).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "node {node_id} has not reported a managed Xray digest; release the supporting Agent first"
            ))
        })?;
        let running: Option<String> = match component {
            BinaryComponent::Xray => row.try_get("xray_running_sha256")?,
            BinaryComponent::Agent => Some(sha.clone()),
        };
        if running
            .as_deref()
            .is_some_and(|running| running != sha.as_str())
        {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} is running Xray bytes that differ from its managed path; reconcile it before release"
            )));
        }
        observations.insert(
            node_id,
            BinaryReleaseObservation {
                installed_sha256: sha,
                running_sha256: running,
            },
        );
    }
    let missing = nodes
        .iter()
        .filter(|node| !found.contains(*node))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(StoreError::NotFound(format!(
            "binary release nodes: {}",
            missing.join(", ")
        )));
    }
    Ok(observations)
}

async fn lock_release(
    tx: &mut Transaction<'_, Postgres>,
    component: BinaryComponent,
    release_id: i64,
) -> Result<sqlx::postgres::PgRow> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BINARY_RELEASE_ADVISORY_LOCK)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "SELECT build_id, status, active
           FROM binary_releases WHERE id = $1 AND component = $2 FOR UPDATE",
    )
    .bind(release_id)
    .bind(component.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("binary release {release_id}")))
}

fn ensure_actionable_release(
    row: &sqlx::postgres::PgRow,
    release_id: i64,
    available_build_id: &str,
) -> Result<()> {
    if !row.try_get::<bool, _>("active")? {
        return Err(StoreError::Conflict(format!(
            "binary release {release_id} is inactive"
        )));
    }
    if row.try_get::<String, _>("build_id")? != available_build_id {
        return Err(StoreError::Conflict(
            "this Console no longer carries the binary build named by the release".to_owned(),
        ));
    }
    Ok(())
}

async fn mark_unsupported_arch(
    tx: &mut Transaction<'_, Postgres>,
    release_id: i64,
    node_id: &str,
    arch: &str,
) -> Result<()> {
    let error = format!("Console carries no released binary for architecture {arch}");
    sqlx::query(
        "UPDATE binary_release_targets
            SET status = 'unsupported', arch = $3, error = $4, finished_at = now()
          WHERE release_id = $1 AND node_id = $2",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(arch)
    .bind(&error)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE binary_release_attempts SET status = 'unsupported', error = $3, finished_at = now()
        WHERE release_id = $1 AND node_id = $2 AND attempt = (SELECT attempt FROM binary_release_targets WHERE release_id = $1 AND node_id = $2)")
        .bind(release_id).bind(node_id).bind(&error).execute(&mut **tx).await?;
    sqlx::query("UPDATE binary_releases SET status = 'halted', halted_at = now() WHERE id = $1")
        .bind(release_id)
        .execute(&mut **tx)
        .await?;
    insert_event(
        tx,
        release_id,
        "target-unsupported",
        Some(node_id),
        None,
        serde_json::json!({ "arch": arch, "error": error }),
    )
    .await
}

async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    release_id: i64,
    kind: &str,
    node_id: Option<&str>,
    actor: Option<&str>,
    detail: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO binary_release_events (release_id, kind, node_id, actor, detail)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(release_id)
    .bind(kind)
    .bind(node_id)
    .bind(actor)
    .bind(detail)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn artifacts_json(artifacts: &[BinaryReleaseArtifact]) -> serde_json::Value {
    serde_json::Value::Object(
        artifacts
            .iter()
            .map(|artifact| {
                (
                    artifact.arch.clone(),
                    serde_json::Value::String(artifact.sha256.clone()),
                )
            })
            .collect(),
    )
}

fn parse_artifacts(value: serde_json::Value) -> Result<Vec<BinaryReleaseArtifact>> {
    let object = value.as_object().ok_or_else(|| {
        StoreError::InvalidData("binary release artifacts are not an object".to_owned())
    })?;
    let mut artifacts = object
        .iter()
        .map(|(arch, sha)| {
            let sha256 = sha
                .as_str()
                .filter(|sha| valid_sha256(sha))
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "binary release artifact {arch} has an invalid digest"
                    ))
                })?;
            Ok(BinaryReleaseArtifact {
                arch: arch.clone(),
                sha256: sha256.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    artifacts.sort_by(|left, right| left.arch.cmp(&right.arch));
    Ok(artifacts)
}

fn validate_build(build: BinaryBuildInfo<'_>) -> Result<()> {
    if !valid_sha256(build.build_id) {
        return Err(StoreError::InvalidData(
            "binary release id is not a lowercase sha256".to_owned(),
        ));
    }
    if build.version.trim().is_empty()
        || build.version.chars().count() > MAX_VERSION_CHARS
        || build.artifacts.is_empty()
        || build.artifacts.len() > MAX_ARTIFACTS
    {
        return Err(StoreError::InvalidData(
            "binary build metadata is incomplete".to_owned(),
        ));
    }
    let mut arches = BTreeSet::new();
    for artifact in build.artifacts {
        if artifact.arch.trim().is_empty()
            || !artifact
                .arch
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            || !valid_sha256(&artifact.sha256)
            || !arches.insert(&artifact.arch)
        {
            return Err(StoreError::InvalidData(format!(
                "invalid binary artifact metadata for {}",
                artifact.arch
            )));
        }
    }
    Ok(())
}

fn normalize_nodes(nodes: Vec<String>) -> Vec<String> {
    nodes
        .into_iter()
        .map(|node| node.trim().to_owned())
        .filter(|node| !node.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalize_text(value: Option<String>, max: usize, name: &str) -> Result<Option<String>> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if value
        .as_ref()
        .is_some_and(|value| value.chars().count() > max)
    {
        return Err(StoreError::InvalidData(format!(
            "{name} exceeds {max} characters"
        )));
    }
    Ok(value)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn require_system_admin(actor: &AdminContext) -> Result<()> {
    if actor.is_system_admin() {
        Ok(())
    } else {
        Err(StoreError::Forbidden(
            "only system-admin can manage binary releases".to_owned(),
        ))
    }
}

fn invalid_number(name: &str) -> StoreError {
    StoreError::InvalidData(format!("binary release {name} is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_selection_is_trimmed_sorted_and_deduplicated() {
        assert_eq!(
            normalize_nodes(vec![" b ".to_owned(), "a".to_owned(), "b".to_owned()]),
            vec!["a", "b"]
        );
    }

    #[test]
    fn artifact_map_is_read_back_in_architecture_order() {
        let value = serde_json::json!({
            "x86_64": "a".repeat(64),
            "aarch64": "b".repeat(64),
        });
        let parsed = parse_artifacts(value).unwrap();
        assert_eq!(parsed[0].arch, "aarch64");
        assert_eq!(parsed[1].arch, "x86_64");
    }

    #[test]
    fn uppercase_or_short_digests_are_not_release_identity() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
    }
}
