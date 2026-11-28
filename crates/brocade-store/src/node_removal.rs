//! "Remove after retirement completes" as durable server intent.
//!
//! Removal deletes the agent token and teardown work order, so it may only run once retirement is
//! terminal (see `console::remove_retired_nodes`). An operator who wants both should not have to
//! keep the page open until the machine confirms teardown. The request is stored as one `jobs` row
//! for the machine's current retirement intent; the Console worker deletes the machine after the
//! lifecycle reaches `retired` or `abandoned`. Every lifecycle intent change cancels it, so
//! restoring the machine cannot be followed by a stale removal.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    console, AdminContext, NodeLifecyclePhase, NodeLifecycleTransitionResult,
    RemoveRetiredNodesResult, Result, StoreError,
};

/// Also matched literally by the node listing query in `console.rs`.
pub(crate) const KIND: &str = "node-removal";
const ACTOR: &str = "system:node-removal";
/// Backoff grows by 15 seconds per failed attempt; see `process`.
const MAX_ATTEMPTS: i32 = 5;

#[derive(Serialize, Deserialize)]
struct Intent {
    node_id: String,
    /// `node_lifecycle_state.intent_revision` of the retirement this removal belongs to.
    intent_revision: i64,
    requested_by: String,
}

/// Retire the machine if it is still active and record that it should be removed once retirement
/// completes. A machine already retiring, retired or abandoned only gains the removal intent; this
/// never re-plans its teardown, which stays the explicit job of retiring it again.
pub async fn schedule_node_removal(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
) -> Result<NodeLifecycleTransitionResult> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can remove nodes".to_owned(),
        ));
    }
    let lifecycle = crate::lifecycle::load(pool, node_id).await?;
    if lifecycle.phase == NodeLifecyclePhase::Active {
        return crate::deployment::transition_node_status_with_removal(pool, actor, node_id).await;
    }
    let mut tx = pool.begin().await?;
    let revision_id = console::lock_control_state(&mut tx).await?;
    enqueue_tx(&mut tx, node_id, actor.operator_id()).await?;
    tx.commit().await?;
    let lifecycle = crate::lifecycle::load(pool, node_id).await?;
    Ok(NodeLifecycleTransitionResult {
        revision_id,
        node_id: node_id.to_owned(),
        deployment_id: lifecycle.deployment_id,
        lifecycle,
        canceled_deployment_ids: Vec::new(),
    })
}

/// Record removal for the machine's current retirement. The caller holds the model lock
/// (`console::lock_control_state`), which every lifecycle intent change also holds.
pub(crate) async fn enqueue_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    requested_by: &str,
) -> Result<()> {
    let row = sqlx::query(
        "SELECT n.retired_at IS NOT NULL AS retired, l.phase, l.intent_revision
           FROM nodes n
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
          WHERE n.id = $1
          FOR UPDATE OF n",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    let retired: bool = row.try_get("retired")?;
    let phase: Option<String> = row.try_get("phase")?;
    let intent_revision: Option<i64> = row.try_get("intent_revision")?;
    let intent_revision = match (retired, phase.as_deref(), intent_revision) {
        (true, Some("retiring" | "retired" | "abandoned"), Some(revision)) => revision,
        _ => {
            return Err(StoreError::Conflict(format!(
                "node {node_id} is not being retired; retire it before scheduling removal"
            )))
        }
    };
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM jobs
              WHERE kind = $1 AND status IN ('queued', 'running')
                AND payload->>'node_id' = $2
                AND payload->>'intent_revision' = $3::bigint::text
         )",
    )
    .bind(KIND)
    .bind(node_id)
    .bind(intent_revision)
    .fetch_one(&mut **tx)
    .await?;
    if pending {
        return Ok(());
    }
    let intent = Intent {
        node_id: node_id.to_owned(),
        intent_revision,
        requested_by: requested_by.to_owned(),
    };
    sqlx::query("INSERT INTO jobs (kind, status, payload) VALUES ($1, 'queued', $2)")
        .bind(KIND)
        .bind(serde_json::to_value(intent)?)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A retirement or restore replaces the intent a pending removal was recorded for.
pub(crate) async fn cancel_for_intent_change_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE jobs
            SET status = 'canceled', last_error = '机器的退役意图已变化，移出安排已撤销',
                updated_at = now()
          WHERE kind = $1 AND status IN ('queued', 'running')
            AND payload->>'node_id' = $2",
    )
    .bind(KIND)
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The machines left this transaction; their pending removals are fulfilled.
pub(crate) async fn settle_removed_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_ids: &[String],
) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET status = 'succeeded', last_error = NULL, updated_at = now()
          WHERE kind = $1 AND status IN ('queued', 'running')
            AND payload->>'node_id' = ANY($2)",
    )
    .bind(KIND)
    .bind(node_ids)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One bounded attempt per tick. Jobs whose machine is still retiring are not touched, so a
/// machine waiting to come back online consumes no attempts.
pub(crate) async fn process(pool: &PgPool) -> Result<Option<RemoveRetiredNodesResult>> {
    let id = sqlx::query_scalar::<_, i64>(
        "SELECT j.id
           FROM jobs j
           LEFT JOIN nodes n ON n.id = j.payload->>'node_id'
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
          WHERE j.kind = $1 AND j.status = 'queued' AND j.run_after <= now()
            AND (n.id IS NULL
                 OR n.retired_at IS NULL
                 OR l.intent_revision::text IS DISTINCT FROM j.payload->>'intent_revision'
                 OR l.phase IN ('retired', 'abandoned'))
          ORDER BY j.id
          LIMIT 1",
    )
    .bind(KIND)
    .fetch_optional(pool)
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    match process_job(pool, id).await {
        Ok(result) => Ok(result),
        Err(error) => {
            sqlx::query(
                "UPDATE jobs
                    SET attempts = attempts + 1,
                        status = CASE WHEN attempts + 1 >= $2 THEN 'failed' ELSE 'queued' END,
                        run_after = now() + interval '15 seconds' * (attempts + 1),
                        last_error = $3, updated_at = now()
                  WHERE id = $1 AND status = 'queued'",
            )
            .bind(id)
            .bind(MAX_ATTEMPTS)
            .bind(format!("自动移出失败：{error}"))
            .execute(pool)
            .await?;
            Err(error)
        }
    }
}

async fn process_job(pool: &PgPool, id: i64) -> Result<Option<RemoveRetiredNodesResult>> {
    let mut tx = pool.begin().await?;
    // Same order as lifecycle writers: the model lock first, then the job row. Locking the job
    // first would invert the order of a concurrent restore which cancels it.
    let previous = console::lock_control_state(&mut tx).await?;
    let row =
        sqlx::query("SELECT payload FROM jobs WHERE id = $1 AND status = 'queued' FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let intent: Intent = serde_json::from_value(row.try_get("payload")?)?;
    let current = sqlx::query(
        "SELECT n.retired_at IS NOT NULL AS retired, l.phase, l.intent_revision
           FROM nodes n
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
          WHERE n.id = $1",
    )
    .bind(&intent.node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        settle_tx(&mut tx, id, "succeeded", None).await?;
        tx.commit().await?;
        return Ok(None);
    };
    let retired: bool = current.try_get("retired")?;
    let phase: Option<String> = current.try_get("phase")?;
    let intent_revision: Option<i64> = current.try_get("intent_revision")?;
    if !retired || intent_revision != Some(intent.intent_revision) {
        settle_tx(
            &mut tx,
            id,
            "canceled",
            Some("机器的退役意图已变化，移出安排已撤销"),
        )
        .await?;
        tx.commit().await?;
        return Ok(None);
    }
    if !matches!(phase.as_deref(), Some("retired" | "abandoned")) {
        return Ok(None);
    }
    let actor = AdminContext::system_admin(ACTOR);
    let note = format!(
        "remove node {} after retirement completed, requested by {}",
        intent.node_id, intent.requested_by
    );
    let result = console::remove_retired_nodes_tx(
        &mut tx,
        &actor,
        previous,
        std::slice::from_ref(&intent.node_id),
        &note,
    )
    .await?;
    tx.commit().await?;
    Ok(Some(result))
}

async fn settle_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    status: &str,
    reason: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE jobs SET status = $2, last_error = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
