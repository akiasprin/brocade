//! Auditable quota-view resets. Billing ledgers and node counter heads remain unchanged.
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, AdminRole, Result, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCycleResetRequest {
    pub request_id: String,
    pub expected_reset_id: Option<i64>,
    pub expected_period_start: String,
    pub expected_period_end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCycleResetResult {
    pub reset_id: i64,
    pub reset_at: String,
    pub duplicate: bool,
}

pub async fn reset(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    request: UserCycleResetRequest,
) -> Result<UserCycleResetResult> {
    actor.require_tenant_access(tenant, "reset user cycle usage")?;
    if !matches!(
        actor.role(),
        AdminRole::Editor | AdminRole::Publisher | AdminRole::TenantAdmin | AdminRole::SystemAdmin
    ) {
        return Err(StoreError::Forbidden("本期用量归零需要编辑权限".into()));
    }
    if request.request_id.len() != 32
        || request.expected_period_start.len() > 32
        || request.expected_period_end.len() > 32
        || !request
            .request_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || request.expected_reset_id.is_some_and(|id| id <= 0)
    {
        return Err(StoreError::InvalidData("无效的本期归零请求标识".into()));
    }
    let mut tx = pool.begin().await?;
    crate::console::lock_control_state(&mut tx).await?;
    sqlx::query("SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("user".into()))?;
    if let Some(row) = sqlx::query(
        "SELECT id,previous_reset_id,to_char(reset_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS reset_at,
            to_char(period_start AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS period_start,
            to_char(period_end AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS period_end
         FROM user_cycle_resets WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3",
    ).bind(tenant).bind(user).bind(&request.request_id).fetch_optional(&mut *tx).await? {
        if row.try_get::<Option<i64>, _>("previous_reset_id")? != request.expected_reset_id
            || row.try_get::<String, _>("period_start")? != request.expected_period_start
            || row.try_get::<String, _>("period_end")? != request.expected_period_end
        {
            return Err(StoreError::Conflict("归零请求标识已用于其他请求".into()));
        }
        return Ok(UserCycleResetResult {
            reset_id: row.try_get("id")?, reset_at: row.try_get("reset_at")?, duplicate: true,
        });
    }
    let previous: Option<i64> = sqlx::query_scalar(
        "SELECT reset_id FROM user_cycle_reset_state WHERE tenant_id=$1 AND user_id=$2",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    if previous != request.expected_reset_id {
        return Err(StoreError::Conflict(
            "本期用量已被其他操作归零，请刷新后重试".into(),
        ));
    }
    // Capture wall time after the parent lock, rather than the transaction's earlier now().
    let row = sqlx::query(
        "WITH boundary AS (SELECT clock_timestamp() AS at), period AS (
           SELECT cycle.* FROM boundary,users owner CROSS JOIN LATERAL user_traffic_cycle(owner.traffic_reset_day,boundary.at) cycle
           WHERE owner.tenant_id=$1 AND owner.id=$2)
         INSERT INTO user_cycle_resets(tenant_id,user_id,request_id,previous_reset_id,reset_at,baseline_starts_at,actor,period_start,period_end)
         SELECT $1,$2,$3,$4,at,
           (date_trunc('month',at AT TIME ZONE 'Asia/Hong_Kong')-interval '1 month') AT TIME ZONE 'Asia/Hong_Kong',$5,
           starts_at,ends_at
         FROM boundary,period
         WHERE to_char(starts_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')=$6
           AND to_char(ends_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')=$7
         RETURNING id,to_char(reset_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS reset_at",
    ).bind(tenant).bind(user).bind(&request.request_id).bind(previous).bind(actor.operator_id())
        .bind(&request.expected_period_start).bind(&request.expected_period_end)
        .fetch_optional(&mut *tx).await?
        .ok_or_else(|| StoreError::Conflict("账期已变化，请关闭确认框、刷新后重新确认".into()))?;
    let reset_id: i64 = row.try_get("id")?;
    sqlx::query("DELETE FROM user_cycle_reset_state WHERE tenant_id=$1 AND user_id=$2")
        .bind(tenant)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO user_cycle_reset_state(tenant_id,user_id,reset_id) VALUES($1,$2,$3)")
        .bind(tenant)
        .bind(user)
        .bind(reset_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO user_cycle_access_baselines(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
         SELECT s.tenant_id,s.user_id,s.app_id,s.period_start,s.period_end,s.uplink_bytes,s.downlink_bytes
         FROM usage_rollups s JOIN user_cycle_resets event ON event.id=$3
         WHERE s.tenant_id=$1 AND s.user_id=$2 AND s.period_start>=event.baseline_starts_at",
    ).bind(tenant).bind(user).bind(reset_id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO user_cycle_forwarding_baselines(tenant_id,user_id,node_id,rule_id,period_start,uplink_bytes,downlink_bytes)
         SELECT s.tenant_id,s.user_id,s.node_id,s.rule_id,s.period_start,s.uplink_bytes,s.downlink_bytes
         FROM forwarding_usage_rollups s JOIN user_cycle_resets event ON event.id=$3
         WHERE s.tenant_id=$1 AND s.user_id=$2 AND s.period_start>=event.baseline_starts_at",
    ).bind(tenant).bind(user).bind(reset_id).execute(&mut *tx).await?;
    // Existing quota triggers revise throttle policies. Authorization/forwarding workers perform
    // restoration after commit and preserve manual switches and service-validity restrictions.
    sqlx::query("UPDATE user_app_quotas SET exhausted=FALSE WHERE tenant_id=$1 AND user_id=$2 AND exhausted")
        .bind(tenant).bind(user).execute(&mut *tx).await?;
    let result = UserCycleResetResult {
        reset_id,
        reset_at: row.try_get("reset_at")?,
        duplicate: false,
    };
    tx.commit().await?;
    Ok(result)
}
