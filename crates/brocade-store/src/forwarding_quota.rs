//! Machine-local forwarding quotas. A quota is an overlay, never a rewrite of rule.enabled.
//! The overlay, generation and durable job commit together. Only an exact successful Agent
//! reconciliation completes a job; offline nodes and failed removals must stay pending.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{forwarding, AdminContext, ForwardingOverview, Result, StoreError};

const WORKER_LOCK: i64 = 0x6272_6f63_6671_756f;
// Both the editor and enforcer use the same per-user cycle and durable attribution, including deleted rules.
pub(crate) const CYCLE_USED: &str =
    "((SELECT COALESCE(sum(s.uplink_bytes+s.downlink_bytes),0)::bigint
 FROM users owner CROSS JOIN LATERAL user_traffic_cycle(owner.traffic_reset_day,now()) cycle
 JOIN forwarding_usage_rollups s ON s.tenant_id=owner.tenant_id AND s.user_id=owner.id
 WHERE owner.tenant_id=e.tenant_id AND owner.id=e.user_id AND s.node_id=e.node_id
 AND s.period_start >= cycle.starts_at AND s.period_start < cycle.ends_at)
 * COALESCE((SELECT CASE u.traffic_accounting_mode WHEN 'double' THEN 2 ELSE 1 END
             FROM users u WHERE u.tenant_id=e.tenant_id AND u.id=e.user_id),1))";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardingQuotaWrite {
    pub limit_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardingQuotaJob {
    pub id: i64,
    pub node_id: String,
    pub suspended: bool,
    pub generation: i64,
    pub status: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardingQuotaJobPage {
    pub jobs: Vec<ForwardingQuotaJob>,
    pub next_before_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForwardingQuotaOutcome {
    pub suspended: usize,
    pub restored: usize,
}

pub async fn set_quota(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: &str,
    user_id: &str,
    node_id: &str,
    write: ForwardingQuotaWrite,
) -> Result<ForwardingOverview> {
    forwarding::require_entitlement_admin(actor, tenant_id)?;
    let limit = match write.limit_bytes {
        None => None,
        Some(value) => Some(i64::try_from(value).ok().filter(|n| *n > 0).ok_or_else(|| {
            StoreError::InvalidData(
                "forwarding quota must be a positive signed 64-bit byte count, or null for unlimited".to_owned(),
            )
        })?),
    };
    let mut tx = pool.begin().await?;
    crate::user_traffic::lock_user_tx(&mut tx, tenant_id, user_id).await?;
    let found = sqlx::query("SELECT 1 FROM forwarding_entitlements WHERE tenant_id=$1 AND user_id=$2 AND node_id=$3 FOR UPDATE")
        .bind(tenant_id).bind(user_id).bind(node_id).fetch_optional(&mut *tx).await?;
    if found.is_none() {
        return Err(StoreError::NotFound("forwarding entitlement".to_owned()));
    }
    sqlx::query(
        "INSERT INTO forwarding_node_quotas(tenant_id,user_id,node_id,limit_bytes,updated_by)
        VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,user_id,node_id) DO UPDATE SET
        limit_bytes=EXCLUDED.limit_bytes,updated_by=EXCLUDED.updated_by,updated_at=now()",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(node_id)
    .bind(limit)
    .bind(actor.operator_id())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    forwarding::overview(pool, actor, tenant_id, user_id).await
}

pub async fn enforce(pool: &PgPool) -> Result<ForwardingQuotaOutcome> {
    let mut tx = pool.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(WORKER_LOCK)
        .fetch_one(&mut *tx)
        .await?;
    if !locked {
        return Ok(ForwardingQuotaOutcome::default());
    }
    // A bounded batch also bounds locks and the number of outbox entries per timer round.
    // Bytewise node order matches rule moves, which can hold multiple generation locks.
    let rows = sqlx::query(&format!("SELECT q.tenant_id,q.user_id,q.node_id,q.limit_bytes,q.suspended,
        {CYCLE_USED} AS used_bytes FROM forwarding_node_quotas q
        JOIN forwarding_entitlements e USING(tenant_id,user_id,node_id)
        WHERE q.suspended IS DISTINCT FROM (q.limit_bytes IS NOT NULL AND {CYCLE_USED}>=q.limit_bytes)
        ORDER BY q.node_id COLLATE \"C\",q.tenant_id,q.user_id LIMIT 128 FOR UPDATE OF q"))
        .fetch_all(&mut *tx).await?;
    let mut outcome = ForwardingQuotaOutcome::default();
    for row in rows {
        let tenant: String = row.try_get("tenant_id")?;
        let user: String = row.try_get("user_id")?;
        let node: String = row.try_get("node_id")?;
        let limit: Option<i64> = row.try_get("limit_bytes")?;
        let used: i64 = row.try_get("used_bytes")?;
        let suspended = limit.is_some_and(|limit| used >= limit);
        let generation = forwarding::bump_node_generation(&mut tx, &node).await?;
        cancel_jobs_tx(&mut tx, &tenant, &user, &node).await?;
        let job_id: i64 = sqlx::query_scalar("INSERT INTO jobs(kind,status,payload) VALUES('forwarding-quota','queued',$1) RETURNING id")
            .bind(json!({"tenant_id":tenant,"user_id":user,"node_id":node,"generation":generation,
                "suspended":suspended,"limit_bytes":limit,"used_bytes":used,"requested_by":"system:forwarding-quota",
                "note":if suspended {"转发本期配额耗尽，暂停该用户在该机器上的转发"} else {"转发本期配额恢复，仅恢复仍手动启用的规则"}}))
            .fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE forwarding_node_quotas SET suspended=$4,last_job_id=$5 WHERE tenant_id=$1 AND user_id=$2 AND node_id=$3")
            .bind(&tenant).bind(&user).bind(&node).bind(suspended).bind(job_id).execute(&mut *tx).await?;
        if suspended {
            outcome.suspended += 1;
        } else {
            outcome.restored += 1;
        }
    }
    // Removing an entitlement/user cancels its operational intent, never its historical audit.
    sqlx::query("UPDATE jobs j SET status='canceled',last_error='配额或转发授权已移除',updated_at=now()
        WHERE j.id IN (SELECT j.id FROM jobs j WHERE j.kind='forwarding-quota' AND j.status IN('queued','running')
          AND NOT EXISTS(SELECT 1 FROM forwarding_node_quotas q WHERE q.last_job_id=j.id) ORDER BY j.id LIMIT 128)")
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(outcome)
}

pub(crate) async fn cancel_jobs_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
    node: &str,
) -> Result<()> {
    sqlx::query("UPDATE jobs SET status='canceled',updated_at=now(),last_error='已由更新的配额意图取代或授权已移除'
        WHERE kind='forwarding-quota' AND status IN('queued','running')
        AND payload->>'tenant_id'=$1 AND payload->>'user_id'=$2 AND payload->>'node_id'=$3")
        .bind(tenant).bind(user).bind(node).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn dispatched_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    generation: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET status='running',attempts=attempts+1,updated_at=now()
        WHERE kind='forwarding-quota' AND status='queued' AND payload->>'node_id'=$1
        AND (payload->>'generation')::bigint <= $2",
    )
    .bind(node)
    .bind(generation)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn acknowledged_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    generation: i64,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE jobs j SET status=CASE WHEN $3::text IS NULL THEN 'succeeded' ELSE 'running' END,
        last_error=$3,updated_at=now() WHERE j.kind='forwarding-quota' AND j.status IN('queued','running')
        AND j.payload->>'node_id'=$1 AND (j.payload->>'generation')::bigint <= $2
        AND EXISTS(SELECT 1 FROM forwarding_node_quotas q WHERE q.last_job_id=j.id)")
        .bind(node).bind(generation).bind(error).execute(&mut **tx).await?;
    Ok(())
}

pub async fn jobs(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    node: &str,
    before: Option<i64>,
) -> Result<ForwardingQuotaJobPage> {
    forwarding::require_user_access(actor, tenant, user)?;
    if before.is_some_and(|id| id <= 0) {
        return Err(StoreError::InvalidData(
            "before_id must be positive".to_owned(),
        ));
    }
    let rows = sqlx::query(
        "SELECT id,status,last_error,created_at::text,updated_at::text,
        payload->>'node_id' AS node_id,(payload->>'generation')::bigint AS generation,
        (payload->>'suspended')::boolean AS suspended FROM jobs WHERE kind='forwarding-quota'
        AND payload->>'tenant_id'=$1 AND payload->>'user_id'=$2 AND payload->>'node_id'=$3
        AND ($4::bigint IS NULL OR id<$4) ORDER BY id DESC LIMIT 21",
    )
    .bind(tenant)
    .bind(user)
    .bind(node)
    .bind(before)
    .fetch_all(pool)
    .await?;
    let mut jobs = rows
        .iter()
        .map(|row| {
            Ok(ForwardingQuotaJob {
                id: row.try_get("id")?,
                node_id: row.try_get("node_id")?,
                suspended: row.try_get("suspended")?,
                generation: row.try_get("generation")?,
                status: row.try_get("status")?,
                last_error: row.try_get("last_error")?,
                created_at: row.try_get("created_at")?,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let next_before_id = if jobs.len() > 20 {
        jobs.truncate(20);
        jobs.last().map(|j| j.id)
    } else {
        None
    };
    Ok(ForwardingQuotaJobPage {
        jobs,
        next_before_id,
    })
}
