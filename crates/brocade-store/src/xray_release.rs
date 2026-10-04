//! Auditable, single-stage approval of the Xray binary carried by the running Console.
//!
//! This is deliberately not a model deployment. Xray's configuration belongs to an immutable
//! model revision; the executable that interprets it is operational software. Coupling the two
//! would make a documentation-only runtime upgrade stamp a model revision and would leave idle
//! machines unable to update when no configuration deployment is owed.

use std::collections::{BTreeMap, BTreeSet};

use brocade_deployment::protocol::{XrayReleaseOutcome, XrayReleaseReport};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

const MAX_NOTE_CHARS: usize = 2_000;
const MAX_ERROR_CHARS: usize = 4_000;
const MAX_VERSION_CHARS: usize = 128;
const MAX_TARGETS: usize = 10_000;
const MAX_ARTIFACTS: usize = 16;
const DISPATCH_LEASE_MINUTES: i32 = 30;
const XRAY_RELEASE_ADVISORY_LOCK: i64 = 0x5852_4159;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XrayReleaseArtifact {
    pub arch: String,
    pub sha256: String,
}

/// Build facts supplied by the running Console. Store records them but never invents them.
#[derive(Debug, Clone, Copy)]
pub struct XrayBuildInfo<'a> {
    pub release_id: &'a str,
    pub version: &'a str,
    pub artifacts: &'a [XrayReleaseArtifact],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateXrayReleaseRequest {
    pub idempotency_key: String,
    pub release_id: String,
    pub nodes: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum XrayReleaseStatus {
    Running,
    Halted,
    Succeeded,
    Canceled,
}

impl XrayReleaseStatus {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "halted" => Ok(Self::Halted),
            "succeeded" => Ok(Self::Succeeded),
            "canceled" => Ok(Self::Canceled),
            other => Err(StoreError::InvalidData(format!(
                "unknown Xray release status {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum XrayReleaseTargetStatus {
    Pending,
    Dispatched,
    Succeeded,
    Unverified,
    FailedRecovered,
    FailedDirty,
    Unsupported,
    Canceled,
}

impl XrayReleaseTargetStatus {
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
                "unknown Xray release target status {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XrayReleaseTarget {
    pub node_id: String,
    pub status: XrayReleaseTargetStatus,
    pub attempt: u32,
    pub before_sha256: String,
    pub desired_sha256: Option<String>,
    pub arch: Option<String>,
    pub error: Option<String>,
    pub reported_performed_update: Option<bool>,
    pub reported_xray_enabled: Option<bool>,
    pub reported_installed_sha256: Option<String>,
    pub reported_running_sha256: Option<String>,
    /// Failed outcomes and an expired dispatch lease can be retried by an operator. Calculating
    /// this at the database clock keeps the browser from inventing lease state from its own clock.
    pub retryable: bool,
    pub dispatched_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XrayReleaseEvent {
    pub id: i64,
    pub kind: String,
    pub node_id: Option<String>,
    pub actor: Option<String>,
    pub detail: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XrayRelease {
    pub id: i64,
    pub release_id: String,
    pub version: String,
    pub artifacts: Vec<XrayReleaseArtifact>,
    pub status: XrayReleaseStatus,
    pub active: bool,
    pub note: Option<String>,
    pub created_at: String,
    pub created_by: String,
    pub halted_at: Option<String>,
    pub finished_at: Option<String>,
    pub targets: Vec<XrayReleaseTarget>,
    pub events: Vec<XrayReleaseEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XrayReleaseList {
    pub releases: Vec<XrayRelease>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XrayReleaseSummary {
    pub id: i64,
    pub release_id: String,
    pub version: String,
    pub status: XrayReleaseStatus,
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
pub struct XrayReleaseAssignment {
    pub release_id: i64,
    pub attempt: u32,
    pub version: String,
    pub sha256: String,
    pub previous_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct XrayReleaseObservation {
    installed_sha256: String,
    running_sha256: Option<String>,
}

pub async fn list_xray_releases(pool: &PgPool, limit: u32) -> Result<XrayReleaseList> {
    let ids =
        sqlx::query_scalar::<_, i64>("SELECT id FROM xray_releases ORDER BY id DESC LIMIT $1")
            .bind(i64::from(limit.clamp(1, 50)))
            .fetch_all(pool)
            .await?;
    let mut releases = Vec::with_capacity(ids.len());
    for id in ids {
        releases.push(load_xray_release(pool, id, true).await?);
    }
    Ok(XrayReleaseList { releases })
}

pub async fn get_xray_release(pool: &PgPool, id: i64) -> Result<XrayRelease> {
    load_xray_release(pool, id, true).await
}

/// Return current target state without the append-only event ledger. This is the shape polled by
/// Console while a rollout is active; audit events are loaded only from the detail endpoint.
pub async fn get_xray_release_overview(pool: &PgPool, id: i64) -> Result<XrayRelease> {
    load_xray_release(pool, id, false).await
}

pub async fn list_xray_release_summaries(
    pool: &PgPool,
    limit: u32,
    before_id: Option<i64>,
) -> Result<Vec<XrayReleaseSummary>> {
    if before_id.is_some_and(|id| id <= 0) {
        return Err(StoreError::InvalidData(
            "Xray release history cursor must be positive".to_owned(),
        ));
    }
    let rows = sqlx::query(
        "SELECT r.id, r.build_id, r.version, r.status, r.active,
                r.note, r.created_at::text AS created_at, r.created_by,
                r.halted_at::text AS halted_at, r.finished_at::text AS finished_at,
                count(t.node_id) AS target_count,
                count(t.node_id) FILTER (WHERE t.status = 'succeeded') AS succeeded_count,
                count(t.node_id) FILTER (
                    WHERE t.status IN (
                        'unverified', 'failed-recovered', 'failed-dirty', 'unsupported'
                    )
                ) AS problem_count
           FROM xray_releases r
           LEFT JOIN xray_release_targets t ON t.release_id = r.id
          WHERE ($2::bigint IS NULL OR r.id < $2)
          GROUP BY r.id
          ORDER BY r.id DESC
          LIMIT $1",
    )
    .bind(i64::from(limit.clamp(1, 50)))
    .bind(before_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(XrayReleaseSummary {
                id: row.try_get("id")?,
                release_id: row.try_get("build_id")?,
                version: row.try_get("version")?,
                status: XrayReleaseStatus::parse(row.try_get::<String, _>("status")?.as_str())?,
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

pub async fn create_xray_release(
    pool: &PgPool,
    actor: &AdminContext,
    request: CreateXrayReleaseRequest,
    build: XrayBuildInfo<'_>,
) -> Result<XrayRelease> {
    require_system_admin(actor)?;
    validate_build(build)?;
    let release_id = request.release_id.trim().to_ascii_lowercase();
    if release_id != build.release_id {
        return Err(StoreError::Conflict(
            "requested Xray build is not carried by this Console".to_owned(),
        ));
    }
    let nodes = normalize_nodes(request.nodes);
    if nodes.is_empty() {
        return Err(StoreError::InvalidData(
            "an Xray release needs at least one node".to_owned(),
        ));
    }
    if nodes.len() > MAX_TARGETS {
        return Err(StoreError::InvalidData(format!(
            "an Xray release cannot target more than {MAX_TARGETS} nodes"
        )));
    }
    let note = normalize_text(request.note, MAX_NOTE_CHARS, "Xray release note")?;
    let idempotency_key = request.idempotency_key.trim().to_owned();
    if idempotency_key.is_empty() || idempotency_key.chars().count() > 200 {
        return Err(StoreError::InvalidData(
            "Xray release idempotency_key must contain 1 to 200 characters".to_owned(),
        ));
    }
    let artifacts = artifacts_json(build.artifacts);

    let mut tx = pool.begin().await?;
    // The partial unique index is the final invariant; this lock turns a concurrent create into a
    // readable conflict instead of a database-specific uniqueness error. It also gives the
    // operator one explicit action per state transition: an active release must be canceled
    // before another can be created, rather than being silently superseded by a second click.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(XRAY_RELEASE_ADVISORY_LOCK)
        .execute(&mut *tx)
        .await?;
    if let Some(existing) = sqlx::query(
        "SELECT id, build_id, version, note
           FROM xray_releases WHERE idempotency_key = $1",
    )
    .bind(&idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        let existing_id: i64 = existing.try_get("id")?;
        let target_rows =
            sqlx::query("SELECT node_id FROM xray_release_targets WHERE release_id = $1")
                .bind(existing_id)
                .fetch_all(&mut *tx)
                .await?;
        let mut existing_nodes = Vec::with_capacity(target_rows.len());
        for row in target_rows {
            let node_id: String = row.try_get("node_id")?;
            existing_nodes.push(node_id);
        }
        existing_nodes.sort();
        if existing.try_get::<String, _>("build_id")? != release_id
            || existing.try_get::<String, _>("version")? != build.version
            || existing.try_get::<Option<String>, _>("note")? != note
            || existing_nodes != nodes
        {
            return Err(StoreError::Conflict(
                "Xray release idempotency_key already belongs to another request".to_owned(),
            ));
        }
        tx.commit().await?;
        return load_xray_release(pool, existing_id, true).await;
    }
    if let Some(active) =
        sqlx::query_scalar::<_, i64>("SELECT id FROM xray_releases WHERE active LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?
    {
        return Err(StoreError::Conflict(format!(
            "Xray release {active} is still active; cancel or finish it first"
        )));
    }
    let observations = release_observations(&mut tx, &nodes).await?;
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
        "INSERT INTO xray_releases
                    (idempotency_key, build_id, version, artifacts, batch_size, note, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
          RETURNING id",
    )
    .bind(&idempotency_key)
    .bind(&release_id)
    .bind(build.version)
    .bind(artifacts)
    // The column remains for schema compatibility. A single-stage approval has no batch size.
    .bind(1_i32)
    .bind(&note)
    .bind(actor.operator_id())
    .fetch_one(&mut *tx)
    .await?;

    for node_id in &nodes {
        sqlx::query(
            "INSERT INTO xray_release_targets
                        (release_id, node_id, wave, before_sha256)
                 VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(node_id)
        .bind(1_i32)
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
    tx.commit().await?;
    load_xray_release(pool, id, true).await
}

pub async fn cancel_xray_release(
    pool: &PgPool,
    actor: &AdminContext,
    release_id: i64,
) -> Result<XrayRelease> {
    require_system_admin(actor)?;
    let mut tx = pool.begin().await?;
    let row = lock_release(&mut tx, release_id).await?;
    if !row.try_get::<bool, _>("active")? {
        return Err(StoreError::Conflict(format!(
            "Xray release {release_id} is already inactive"
        )));
    }
    sqlx::query(
        "UPDATE xray_releases
            SET active = FALSE, status = 'canceled', finished_at = now()
          WHERE id = $1",
    )
    .bind(release_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE xray_release_targets
            SET status = 'canceled', finished_at = now()
          WHERE release_id = $1 AND status IN ('pending', 'dispatched')",
    )
    .bind(release_id)
    .execute(&mut *tx)
    .await?;
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
    load_xray_release(pool, release_id, true).await
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
        .bind(XRAY_RELEASE_ADVISORY_LOCK)
        .execute(&mut **tx)
        .await?;
    let release_id = sqlx::query_scalar::<_, i64>(
        "SELECT r.id
           FROM xray_releases r
           JOIN xray_release_targets t ON t.release_id = r.id
          WHERE r.active AND t.node_id = $1
          FOR UPDATE OF r, t",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(release_id) = release_id else {
        return Ok(None);
    };
    sqlx::query(
        "UPDATE xray_releases
            SET active = FALSE, status = 'canceled', finished_at = now()
          WHERE id = $1",
    )
    .bind(release_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE xray_release_targets
            SET status = 'canceled', finished_at = now()
          WHERE release_id = $1 AND status IN ('pending', 'dispatched')",
    )
    .bind(release_id)
    .execute(&mut **tx)
    .await?;
    insert_event(
        tx,
        release_id,
        "canceled-for-node-lifecycle",
        Some(node_id),
        Some(actor),
        serde_json::json!({
            "reason": "target left active lifecycle",
        }),
    )
    .await?;
    Ok(Some(release_id))
}

pub async fn retry_xray_release_target(
    pool: &PgPool,
    actor: &AdminContext,
    release_id: i64,
    node_id: &str,
    available_build_id: &str,
) -> Result<XrayRelease> {
    require_system_admin(actor)?;
    let mut tx = pool.begin().await?;
    let row = lock_release(&mut tx, release_id).await?;
    ensure_actionable_release(&row, release_id, available_build_id)?;
    let result = sqlx::query(
        "UPDATE xray_release_targets
            SET status = 'pending', attempt = attempt + 1, error = NULL,
                dispatched_at = NULL, finished_at = NULL,
                reported_performed_update = NULL, reported_xray_enabled = NULL,
                reported_installed_sha256 = NULL,
                reported_running_sha256 = NULL,
                arch = CASE WHEN status = 'unsupported' THEN NULL ELSE arch END,
                desired_sha256 = CASE WHEN status = 'unsupported' THEN NULL ELSE desired_sha256 END
          WHERE release_id = $1 AND node_id = $2
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
            "Xray release target {release_id}/{node_id} is not retryable; a dispatch lease must be at least {DISPATCH_LEASE_MINUTES} minutes old"
        )));
    }
    sqlx::query(
        "UPDATE xray_releases
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
    load_xray_release(pool, release_id, true).await
}

pub async fn claim_xray_release(
    pool: &PgPool,
    node_id: &str,
    arch: &str,
    available_build_id: &str,
) -> Result<Option<XrayReleaseAssignment>> {
    if arch.is_empty()
        || arch.len() > 32
        || !arch
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(StoreError::InvalidData(
            "Xray release architecture is invalid".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query(
        "SELECT r.id, r.build_id, r.version, r.artifacts,
                t.status, t.attempt, t.before_sha256, t.desired_sha256, t.arch
           FROM xray_releases r
           JOIN xray_release_targets t ON t.release_id = r.id
          WHERE r.active AND r.status = 'running' AND t.node_id = $1
            AND t.status IN ('pending', 'dispatched')
          ORDER BY r.id DESC
          LIMIT 1
          FOR UPDATE OF r, t",
    )
    .bind(node_id)
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
        "UPDATE xray_release_targets
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
    let assignment = XrayReleaseAssignment {
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

pub async fn report_xray_release(
    pool: &PgPool,
    node_id: &str,
    report: &XrayReleaseReport,
) -> Result<bool> {
    validate_report(report)?;
    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query(
        "SELECT r.active, r.status AS release_status,
                t.status, t.attempt, t.before_sha256, t.desired_sha256
           FROM xray_releases r
           JOIN xray_release_targets t ON t.release_id = r.id
          WHERE r.id = $1 AND t.node_id = $2
          FOR UPDATE OF r, t",
    )
    .bind(report.release_id)
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Err(StoreError::NotFound(format!(
            "Xray release target {}/{}",
            report.release_id, node_id
        )));
    };
    let attempt =
        u32::try_from(row.try_get::<i32, _>("attempt")?).map_err(|_| invalid_number("attempt"))?;
    if attempt != report.attempt {
        tx.commit().await?;
        return Ok(false);
    }
    let active: bool = row.try_get("active")?;
    let release_status: String = row.try_get("release_status")?;
    let existing: String = row.try_get("status")?;
    let before: String = row.try_get("before_sha256")?;
    let desired: Option<String> = row.try_get("desired_sha256")?;
    // Cancellation closes the offer before marking its targets canceled. An Agent rechecks just
    // before replacement, but cancellation can still commit in the narrow interval after that
    // check. A target with an assigned digest proves it had already claimed the work. Record that
    // late result so the immutable audit does not claim a machine stayed untouched when it did not.
    let late_after_cancel =
        !active && release_status == "canceled" && existing == "canceled" && desired.is_some();
    if !active && !late_after_cancel {
        tx.commit().await?;
        return Ok(false);
    }
    if active && !matches!(existing.as_str(), "pending" | "dispatched") {
        tx.commit().await?;
        return Ok(true);
    }
    if report.outcome == XrayReleaseOutcome::Succeeded {
        let desired = desired.as_deref().ok_or_else(|| {
            StoreError::InvalidData("successful Xray report has no assigned digest".to_owned())
        })?;
        if report.installed_sha256.as_deref() != Some(desired)
            || (report.xray_enabled && report.running_sha256.as_deref() != Some(desired))
        {
            return Err(StoreError::InvalidData(
                "successful Xray report does not identify the released bytes".to_owned(),
            ));
        }
    } else if report.outcome == XrayReleaseOutcome::FailedRecovered
        && (report.installed_sha256.as_deref() != Some(before.as_str())
            || (report.xray_enabled && report.running_sha256.as_deref() != Some(before.as_str())))
    {
        return Err(StoreError::InvalidData(
            "recovered Xray report does not identify the frozen prior bytes".to_owned(),
        ));
    }
    let mut target_status = match report.outcome {
        XrayReleaseOutcome::Succeeded => "succeeded",
        XrayReleaseOutcome::FailedRecovered => "failed-recovered",
        XrayReleaseOutcome::FailedDirty => "failed-dirty",
        XrayReleaseOutcome::Unsupported => "unsupported",
    };
    let mut error = normalize_text(report.error.clone(), MAX_ERROR_CHARS, "Xray release error")?;
    // Every approved target must exercise a real replacement. A no-op or an inactive service
    // proves only that bytes reached disk; it says nothing about whether the candidate can serve
    // this node's real configuration.
    if target_status == "succeeded"
        && (!report.performed_update
            || before == desired.as_deref().unwrap_or_default()
            || !report.xray_enabled
            || report.running_sha256.as_deref() != desired.as_deref())
    {
        target_status = "unverified";
        error = Some(if !report.performed_update {
            "机器没有实际执行本次原子替换，不能把 no-op 当作替换证据".to_owned()
        } else if before == desired.as_deref().unwrap_or_default() {
            "机器在批准前已经是目标字节，没有实际执行替换".to_owned()
        } else if !report.xray_enabled {
            "机器只安装了目标字节，但 Xray 未启用，无法验证真实启动".to_owned()
        } else {
            "机器没有报告目标字节正在运行".to_owned()
        });
    }
    sqlx::query(
        "UPDATE xray_release_targets
            SET status = $3, error = $4,
                reported_performed_update = $5, reported_xray_enabled = $6,
                reported_installed_sha256 = $7, reported_running_sha256 = $8,
                finished_at = now()
          WHERE release_id = $1 AND node_id = $2",
    )
    .bind(report.release_id)
    .bind(node_id)
    .bind(target_status)
    .bind(&error)
    .bind(report.performed_update)
    .bind(report.xray_enabled)
    .bind(&report.installed_sha256)
    .bind(&report.running_sha256)
    .execute(&mut *tx)
    .await?;
    insert_event(
        &mut tx,
        report.release_id,
        if late_after_cancel {
            "target-reported-after-cancel"
        } else {
            "target-reported"
        },
        Some(node_id),
        None,
        serde_json::json!({
            "report": report,
            "classified_status": target_status,
            "classification_error": error,
        }),
    )
    .await?;

    if late_after_cancel {
        tx.commit().await?;
        return Ok(true);
    }

    if target_status == "succeeded" {
        let unfinished = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM xray_release_targets
              WHERE release_id = $1 AND status <> 'succeeded'",
        )
        .bind(report.release_id)
        .fetch_one(&mut *tx)
        .await?;
        if unfinished == 0 {
            sqlx::query(
                "UPDATE xray_releases
                    SET status = 'succeeded', active = FALSE, finished_at = now()
                  WHERE id = $1",
            )
            .bind(report.release_id)
            .execute(&mut *tx)
            .await?;
            insert_event(
                &mut tx,
                report.release_id,
                "succeeded",
                None,
                None,
                serde_json::json!({}),
            )
            .await?;
        } else {
            let failed = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM xray_release_targets
                  WHERE release_id = $1
                    AND status IN (
                        'unverified', 'failed-recovered', 'failed-dirty', 'unsupported'
                    )",
            )
            .bind(report.release_id)
            .fetch_one(&mut *tx)
            .await?;
            if failed != 0 {
                sqlx::query(
                    "UPDATE xray_releases SET status = 'halted', halted_at = now() WHERE id = $1",
                )
                .bind(report.release_id)
                .execute(&mut *tx)
                .await?;
            }
        }
    } else {
        sqlx::query(
            "UPDATE xray_releases
                SET status = 'halted', halted_at = now()
              WHERE id = $1",
        )
        .bind(report.release_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

async fn load_xray_release(pool: &PgPool, id: i64, include_events: bool) -> Result<XrayRelease> {
    let row = sqlx::query(
        "SELECT id, build_id, version, artifacts, status, active, note,
                created_at::text AS created_at, created_by,
                halted_at::text AS halted_at, finished_at::text AS finished_at
           FROM xray_releases WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("Xray release {id}")))?;
    let target_rows = sqlx::query(
        "SELECT node_id, status, attempt, before_sha256, desired_sha256, arch, error,
                reported_performed_update, reported_xray_enabled,
                reported_installed_sha256, reported_running_sha256,
                (
                    status IN ('unverified', 'failed-recovered', 'failed-dirty', 'unsupported')
                    OR (
                        status = 'dispatched'
                        AND dispatched_at <= now() - make_interval(mins => $2)
                    )
                ) AS retryable,
                dispatched_at::text AS dispatched_at, finished_at::text AS finished_at
           FROM xray_release_targets WHERE release_id = $1 ORDER BY node_id",
    )
    .bind(id)
    .bind(DISPATCH_LEASE_MINUTES)
    .fetch_all(pool)
    .await?;
    let event_rows = if include_events {
        sqlx::query(
            "SELECT id, kind, node_id, actor, detail, created_at::text AS created_at
               FROM xray_release_events WHERE release_id = $1 ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };
    Ok(XrayRelease {
        id: row.try_get("id")?,
        release_id: row.try_get("build_id")?,
        version: row.try_get("version")?,
        artifacts: parse_artifacts(row.try_get("artifacts")?)?,
        status: XrayReleaseStatus::parse(row.try_get::<String, _>("status")?.as_str())?,
        active: row.try_get("active")?,
        note: row.try_get("note")?,
        created_at: row.try_get("created_at")?,
        created_by: row.try_get("created_by")?,
        halted_at: row.try_get("halted_at")?,
        finished_at: row.try_get("finished_at")?,
        targets: target_rows
            .into_iter()
            .map(|row| {
                Ok(XrayReleaseTarget {
                    node_id: row.try_get("node_id")?,
                    status: XrayReleaseTargetStatus::parse(
                        row.try_get::<String, _>("status")?.as_str(),
                    )?,
                    attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
                        .map_err(|_| invalid_number("attempt"))?,
                    before_sha256: row.try_get("before_sha256")?,
                    desired_sha256: row.try_get("desired_sha256")?,
                    arch: row.try_get("arch")?,
                    error: row.try_get("error")?,
                    reported_performed_update: row.try_get("reported_performed_update")?,
                    reported_xray_enabled: row.try_get("reported_xray_enabled")?,
                    reported_installed_sha256: row.try_get("reported_installed_sha256")?,
                    reported_running_sha256: row.try_get("reported_running_sha256")?,
                    retryable: row.try_get("retryable")?,
                    dispatched_at: row.try_get("dispatched_at")?,
                    finished_at: row.try_get("finished_at")?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        events: event_rows
            .into_iter()
            .map(|row| {
                Ok(XrayReleaseEvent {
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

async fn release_observations(
    tx: &mut Transaction<'_, Postgres>,
    nodes: &[String],
) -> Result<BTreeMap<String, XrayReleaseObservation>> {
    let rows = sqlx::query(
        "SELECT n.id, lifecycle.phase,
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
                "Xray release target {node_id} is {phase}, not active"
            )));
        }
        if !row.try_get::<bool, _>("poll_fresh")? {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} has not polled desired state in the last 90 seconds"
            )));
        }
        if !row.try_get::<bool, _>("runtime_fresh")? {
            return Err(StoreError::InvalidData(format!(
                "node {node_id} has not reported runtime state in the last 2 minutes"
            )));
        }
        let sha: Option<String> = row.try_get("xray_installed_sha256")?;
        let sha = sha.filter(|sha| valid_sha256(sha)).ok_or_else(|| {
            StoreError::InvalidData(format!(
                "node {node_id} has not reported a managed Xray digest; release the supporting Agent first"
            ))
        })?;
        let running: Option<String> = row.try_get("xray_running_sha256")?;
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
            XrayReleaseObservation {
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
            "Xray release nodes: {}",
            missing.join(", ")
        )));
    }
    Ok(observations)
}

async fn lock_release(
    tx: &mut Transaction<'_, Postgres>,
    release_id: i64,
) -> Result<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT build_id, status, active
           FROM xray_releases WHERE id = $1 FOR UPDATE",
    )
    .bind(release_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("Xray release {release_id}")))
}

fn ensure_actionable_release(
    row: &sqlx::postgres::PgRow,
    release_id: i64,
    available_build_id: &str,
) -> Result<()> {
    if !row.try_get::<bool, _>("active")? {
        return Err(StoreError::Conflict(format!(
            "Xray release {release_id} is inactive"
        )));
    }
    if row.try_get::<String, _>("build_id")? != available_build_id {
        return Err(StoreError::Conflict(
            "this Console no longer carries the Xray build named by the release".to_owned(),
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
    let error = format!("Console carries no released Xray for architecture {arch}");
    sqlx::query(
        "UPDATE xray_release_targets
            SET status = 'unsupported', arch = $3, error = $4, finished_at = now()
          WHERE release_id = $1 AND node_id = $2",
    )
    .bind(release_id)
    .bind(node_id)
    .bind(arch)
    .bind(&error)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE xray_releases SET status = 'halted', halted_at = now() WHERE id = $1")
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
        "INSERT INTO xray_release_events (release_id, kind, node_id, actor, detail)
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

fn artifacts_json(artifacts: &[XrayReleaseArtifact]) -> serde_json::Value {
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

fn parse_artifacts(value: serde_json::Value) -> Result<Vec<XrayReleaseArtifact>> {
    let object = value.as_object().ok_or_else(|| {
        StoreError::InvalidData("Xray release artifacts are not an object".to_owned())
    })?;
    let mut artifacts = object
        .iter()
        .map(|(arch, sha)| {
            let sha256 = sha
                .as_str()
                .filter(|sha| valid_sha256(sha))
                .ok_or_else(|| {
                    StoreError::InvalidData(format!(
                        "Xray release artifact {arch} has an invalid digest"
                    ))
                })?;
            Ok(XrayReleaseArtifact {
                arch: arch.clone(),
                sha256: sha256.to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    artifacts.sort_by(|left, right| left.arch.cmp(&right.arch));
    Ok(artifacts)
}

fn validate_build(build: XrayBuildInfo<'_>) -> Result<()> {
    if !valid_sha256(build.release_id) {
        return Err(StoreError::InvalidData(
            "Xray release id is not a lowercase sha256".to_owned(),
        ));
    }
    if build.version.trim().is_empty()
        || build.version.chars().count() > MAX_VERSION_CHARS
        || build.artifacts.is_empty()
        || build.artifacts.len() > MAX_ARTIFACTS
    {
        return Err(StoreError::InvalidData(
            "Xray build metadata is incomplete".to_owned(),
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
                "invalid Xray artifact metadata for {}",
                artifact.arch
            )));
        }
    }
    Ok(())
}

fn validate_report(report: &XrayReleaseReport) -> Result<()> {
    if report.release_id <= 0 || report.attempt == 0 {
        return Err(StoreError::InvalidData(
            "Xray release report identity is out of range".to_owned(),
        ));
    }
    for (name, value) in [
        ("installed_sha256", report.installed_sha256.as_deref()),
        ("running_sha256", report.running_sha256.as_deref()),
    ] {
        if value.is_some_and(|value| !valid_sha256(value)) {
            return Err(StoreError::InvalidData(format!(
                "Xray release report {name} is invalid"
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
            "only system-admin can manage Xray releases".to_owned(),
        ))
    }
}

fn invalid_number(name: &str) -> StoreError {
    StoreError::InvalidData(format!("Xray release {name} is out of range"))
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
