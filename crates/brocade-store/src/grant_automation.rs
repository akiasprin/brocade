//! Durable automatic releases for permission changes.
//!
//! The model write and the `jobs` row commit in the same transaction.  The worker may be late or
//! the control plane may restart, but there is consequently no state in which a permission changed
//! and nothing remembers that it still has to be shipped.
//!
//! Several queued edits are folded into the newest revision before a deployment exists. A target
//! already handed to an agent is immutable. A still-pending configuration target is deliberately
//! rebased to the newest permissions before the immediate grants order is created; otherwise that
//! future restart would restore its old frozen list and undo the hot update.

use std::collections::BTreeMap;

use brocade_core::model::ModelSnapshot;
use brocade_deployment::plan::{plan_deployment, DesiredGrants};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{deployment, AdminContext, Result, StoreError};

pub const GRANTS_AUTOMATION_ACTOR: &str = "system:grants";
const JOB_KIND: &str = "grants-deployment";
const RETRY_INTERVAL: &str = "5 seconds";
// A stable, application-owned advisory-lock key.  It serializes workers across control-plane
// instances; the per-kind deployment lock remains the final authority against manual releases.
const WORKER_LOCK_KEY: i64 = 0x6272_6f63_6772_616e;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantAutomationOutcome {
    pub merged_jobs: usize,
    pub revision_id: Option<u64>,
    pub deployment_id: Option<i64>,
    pub deferred: Vec<String>,
    pub waiting: Option<String>,
}

/// Read-only health of the durable permission outbox.
///
/// A job is created before a deployment. Exposing only the deployment list therefore hides the
/// exact failure class this status represents: planning may retry forever without ever producing
/// a deployment row. Timestamps stay PostgreSQL text, matching the rest of the console API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantAutomationStatus {
    pub pending_jobs: u64,
    pub retrying_jobs: u64,
    pub failed_jobs: u64,
    pub max_attempts: u64,
    pub latest_revision_id: Option<u64>,
    pub oldest_pending_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_error: Option<String>,
}

pub(crate) async fn enqueue_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
    requested_by: &str,
    note: &str,
) -> Result<()> {
    let revision_id = i64::try_from(revision_id)
        .map_err(|_| StoreError::InvalidData(format!("revision id out of range: {revision_id}")))?;
    sqlx::query(
        "INSERT INTO jobs (kind, status, payload)
         VALUES ($1, 'queued', $2)",
    )
    .bind(JOB_KIND)
    .bind(json!({
        "revision_id": revision_id,
        "requested_by": requested_by,
        "note": note,
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn status(pool: &PgPool) -> Result<GrantAutomationStatus> {
    let row = sqlx::query(
        "SELECT count(*) FILTER (WHERE status IN ('queued', 'running')) AS pending_jobs,
                count(*) FILTER (
                    WHERE status IN ('queued', 'running') AND attempts > 0
                ) AS retrying_jobs,
                count(*) FILTER (WHERE status = 'failed') AS failed_jobs,
                COALESCE((max(attempts) FILTER (
                    WHERE status IN ('queued', 'running')
                ))::bigint, 0::bigint) AS max_attempts,
                max((payload->>'revision_id')::bigint) FILTER (
                    WHERE status IN ('queued', 'running')
                ) AS latest_revision_id,
                (min(created_at) FILTER (
                    WHERE status IN ('queued', 'running')
                ))::text AS oldest_pending_at,
                (max(updated_at) FILTER (
                    WHERE status IN ('queued', 'running') AND attempts > 0
                ))::text AS last_attempt_at,
                (array_agg(last_error ORDER BY updated_at DESC, id DESC) FILTER (
                    WHERE status IN ('queued', 'running') AND last_error IS NOT NULL
                ))[1] AS last_error
         FROM jobs
         WHERE kind = $1",
    )
    .bind(JOB_KIND)
    .fetch_one(pool)
    .await?;
    let nonnegative = |field: &str| -> Result<u64> {
        let value: i64 = row.try_get(field)?;
        u64::try_from(value)
            .map_err(|_| StoreError::InvalidData(format!("jobs.{field} is negative: {value}")))
    };
    let latest_revision_id = row
        .try_get::<Option<i64>, _>("latest_revision_id")?
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                StoreError::InvalidData(format!("grant automation revision is negative: {value}"))
            })
        })
        .transpose()?;
    Ok(GrantAutomationStatus {
        pending_jobs: nonnegative("pending_jobs")?,
        retrying_jobs: nonnegative("retrying_jobs")?,
        failed_jobs: nonnegative("failed_jobs")?,
        max_attempts: nonnegative("max_attempts")?,
        latest_revision_id,
        oldest_pending_at: row.try_get("oldest_pending_at")?,
        last_attempt_at: row.try_get("last_attempt_at")?,
        last_error: row.try_get("last_error")?,
    })
}

/// Queue an automatic release only when the committed model revision actually changes the
/// grants artifact.  Permissions have more inputs than the `grants` table: moving an ingress,
/// retiring its node, deleting a chain, or changing the global REALITY flow all alter what the
/// agent must hold.  Keeping this comparison at the revision boundary means none of those
/// indirect paths has to remember a second side effect.
///
/// An unpublishable endpoint is queued conservatively.  The durable worker records the compile
/// error and a later valid revision is coalesced into it; silently dropping the job would lose a
/// permission change merely because an intermediate draft was incomplete.
pub(crate) async fn enqueue_if_changed_tx(
    tx: &mut Transaction<'_, Postgres>,
    previous_revision: u64,
    revision_id: u64,
) -> Result<()> {
    let previous = crate::materialize::load_snapshot_tx(tx, Some(previous_revision)).await?;
    let current = crate::materialize::load_snapshot_tx(tx, Some(revision_id)).await?;
    let changed = match (compiled_grants(&previous), compiled_grants(&current)) {
        (Some(previous), Some(current)) => previous != current,
        _ => true,
    };
    if !changed {
        return Ok(());
    }

    let row = sqlx::query("SELECT author, note FROM revisions WHERE id = $1")
        .bind(i64::try_from(revision_id).map_err(|_| {
            StoreError::InvalidData(format!("revision id out of range: {revision_id}"))
        })?)
        .fetch_one(&mut **tx)
        .await?;
    let author: String = row.try_get("author")?;
    let note: String = row.try_get("note")?;
    enqueue_tx(tx, revision_id, &author, &note).await
}

fn compiled_grants(snapshot: &ModelSnapshot) -> Option<BTreeMap<String, DesiredGrants>> {
    plan_deployment(snapshot, &[]).ok().map(|plan| {
        plan.targets
            .into_iter()
            .map(|target| (target.node_id, target.desired.grants))
            .collect()
    })
}

/// Serialize an operation that invalidates queued revisions with the worker that may turn one of
/// them into a deployment.  `discard_pending_changes` uses the blocking form; once it owns this
/// lock, either the worker's deployment is visible and blocks the discard, or no worker can create
/// one before the queued rows are canceled.
pub(crate) async fn lock_tx(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WORKER_LOCK_KEY)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn cancel_revision_jobs_tx(
    tx: &mut Transaction<'_, Postgres>,
    after_revision: u64,
    through_revision: u64,
) -> Result<()> {
    let after_revision = i64::try_from(after_revision).map_err(|_| {
        StoreError::InvalidData(format!("revision id out of range: {after_revision}"))
    })?;
    let through_revision = i64::try_from(through_revision).map_err(|_| {
        StoreError::InvalidData(format!("revision id out of range: {through_revision}"))
    })?;
    sqlx::query(
        "UPDATE jobs
         SET status = 'canceled',
             last_error = '对应修订已被撤销',
             updated_at = now()
         WHERE kind = $1
           AND status IN ('queued', 'running')
           AND (payload->>'revision_id')::bigint > $2
           AND (payload->>'revision_id')::bigint <= $3",
    )
    .bind(JOB_KIND)
    .bind(after_revision)
    .bind(through_revision)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Try one batch of queued permission work.
///
/// Waiting is a successful worker result: the row remains queued with `last_error` explaining
/// whether a configuration release or another grants release is in front of it.  The timer retries
/// it, while an HTTP-side wake makes the usual path immediate.
pub async fn process_jobs(pool: &PgPool) -> Result<GrantAutomationOutcome> {
    let mut guard = pool.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(WORKER_LOCK_KEY)
        .fetch_one(&mut *guard)
        .await?;
    if !locked {
        guard.rollback().await?;
        return Ok(GrantAutomationOutcome {
            waiting: Some("另一实例正在处理自动授权队列".to_owned()),
            ..Default::default()
        });
    }

    // Keep the transaction (and therefore the cross-instance lock) open while planning and
    // creating through the pool.  The production pool has spare connections; no model row is
    // locked by this guard transaction.
    let outcome = process_jobs_locked(pool).await;
    guard.commit().await?;
    outcome
}

async fn process_jobs_locked(pool: &PgPool) -> Result<GrantAutomationOutcome> {
    let rows = sqlx::query(
        "SELECT id,
                (payload->>'revision_id')::bigint AS revision_id,
                COALESCE(payload->>'note', '') AS note
         FROM jobs
         WHERE kind = $1
           AND status = 'queued'
           AND run_after <= now()
         ORDER BY id",
    )
    .bind(JOB_KIND)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(GrantAutomationOutcome::default());
    }

    let job_ids = rows
        .iter()
        .map(|row| row.try_get::<i64, _>("id"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let latest = rows
        .iter()
        .max_by_key(|row| row.try_get::<i64, _>("revision_id").unwrap_or(i64::MIN))
        .expect("rows is not empty");
    let revision_i64: i64 = latest.try_get("revision_id")?;
    let revision_id = u64::try_from(revision_i64).map_err(|_| {
        StoreError::InvalidData(format!(
            "grant automation job has invalid revision {revision_i64}"
        ))
    })?;
    let latest_note: String = latest.try_get("note")?;

    let note = if job_ids.len() == 1 && !latest_note.trim().is_empty() {
        format!("自动授权：{}", latest_note.trim())
    } else {
        format!("自动授权：合并 {} 次权限变更", job_ids.len())
    };
    let actor = AdminContext::system_admin(GRANTS_AUTOMATION_ACTOR);
    let prepared = match deployment::create_automatic_grants_deployment(
        pool,
        &actor,
        revision_id,
        &note,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            let message = format!("创建自动授权单失败，稍后重试：{error}");
            reschedule(pool, &job_ids, &message).await?;
            return Ok(waiting_outcome(
                job_ids.len(),
                revision_id,
                Vec::new(),
                message,
            ));
        }
    };

    if !prepared.deferred.is_empty() {
        let message = format!(
            "权限与执行中的配置冲突，等待配置：{}",
            prepared.deferred.join("、")
        );
        reschedule(pool, &job_ids, &message).await?;
        return Ok(GrantAutomationOutcome {
            merged_jobs: job_ids.len(),
            revision_id: Some(revision_id),
            deployment_id: prepared.deployment_id,
            deferred: prepared.deferred,
            waiting: Some(message),
        });
    }

    finish(pool, &job_ids, prepared.deployment_id, revision_id).await?;
    Ok(GrantAutomationOutcome {
        merged_jobs: job_ids.len(),
        revision_id: Some(revision_id),
        deployment_id: prepared.deployment_id,
        deferred: Vec::new(),
        waiting: None,
    })
}

fn waiting_outcome(
    merged_jobs: usize,
    revision_id: u64,
    deferred: Vec<String>,
    message: String,
) -> GrantAutomationOutcome {
    GrantAutomationOutcome {
        merged_jobs,
        revision_id: Some(revision_id),
        deployment_id: None,
        deferred,
        waiting: Some(message),
    }
}

async fn reschedule(pool: &PgPool, job_ids: &[i64], error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE jobs
         SET status = 'queued',
             attempts = attempts + 1,
             run_after = now() + $2::interval,
             last_error = $3,
             updated_at = now()
         WHERE id = ANY($1)",
    )
    .bind(job_ids)
    .bind(RETRY_INTERVAL)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

async fn finish(
    pool: &PgPool,
    job_ids: &[i64],
    deployment_id: Option<i64>,
    revision_id: u64,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE jobs
         SET status = 'succeeded',
             attempts = attempts + 1,
             last_error = NULL,
             payload = CASE
                 WHEN $2::bigint IS NULL THEN payload
                 ELSE jsonb_set(payload, '{deployment_id}', to_jsonb($2::bigint), TRUE)
             END,
             updated_at = now()
         WHERE id = ANY($1)",
    )
    .bind(job_ids)
    .bind(deployment_id)
    .execute(&mut *tx)
    .await?;
    if let Some(deployment_id) = deployment_id {
        let activated = sqlx::query_scalar::<_, bool>(
            "SELECT status = 'succeeded' AND activation_status = 'waiting'
               FROM deployments
              WHERE id = $1
              FOR UPDATE",
        )
        .bind(deployment_id)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
        if activated {
            crate::serving::activate_deployment_tx(&mut tx, deployment_id).await?;
        }
    } else {
        crate::serving::activate_permissions_revision_tx(&mut tx, revision_id).await?;
    }
    tx.commit().await?;
    Ok(())
}
