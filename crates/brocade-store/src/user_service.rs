//! Operational user billing periods and scheduled service access. The scheduler persists its
//! result before freezing a revision, so compiling an immutable snapshot never depends on time.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{console, forwarding, AdminContext, Result, StoreError, UserListItem};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserServiceState {
    #[default]
    Active,
    Pending,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserServicePolicyWrite {
    pub traffic_reset_day: u8,
    pub service_starts_at: Option<String>,
    pub service_expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCyclePeriod {
    pub tenant_id: String,
    pub user_id: String,
    pub starts_at: String,
    pub ends_at: String,
    pub reset_id: Option<i64>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCycleUsage {
    pub periods: Vec<UserCyclePeriod>,
    pub views: Vec<crate::UsageMonthlyViewRow>,
    pub forwarding: Vec<brocade_deployment::protocol::UsageForwardingMonthlyRow>,
    pub traffic_policies: Vec<brocade_deployment::user_traffic::UserTrafficStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserServicePolicyPreview {
    pub starts_at: String,
    pub ends_at: String,
    pub access_used_bytes: u64,
    pub forwarding_used_bytes: u64,
    pub service_state: UserServiceState,
}

async fn validate(pool: &PgPool, policy: &UserServicePolicyWrite) -> Result<()> {
    if !(1..=31).contains(&policy.traffic_reset_day) {
        return Err(StoreError::InvalidData("流量重置日需在 1–31 日之间".into()));
    }
    for value in [&policy.service_starts_at, &policy.service_expires_at]
        .into_iter()
        .flatten()
    {
        // Requiring an explicit RFC3339 offset prevents the database/session timezone from
        // silently changing an operator's intended instant. PostgreSQL validates the calendar.
        let valid: bool = sqlx::query_scalar(
            "SELECT $1 ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$'
                    AND pg_input_is_valid($1, 'timestamp with time zone')",
        ).bind(value).fetch_one(pool).await?;
        if !valid {
            return Err(StoreError::InvalidData(
                "服务时间需为带时区的有效日期时间".into(),
            ));
        }
    }
    let valid: bool = sqlx::query_scalar(
        "SELECT $1::timestamptz IS NULL OR $2::timestamptz IS NULL OR $2::timestamptz > $1::timestamptz",
    ).bind(&policy.service_starts_at).bind(&policy.service_expires_at).fetch_one(pool).await?;
    if !valid {
        return Err(StoreError::InvalidData("到期时间必须晚于生效时间".into()));
    }
    Ok(())
}

pub async fn preview(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    policy: &UserServicePolicyWrite,
) -> Result<UserServicePolicyPreview> {
    actor.require_tenant_access(tenant, "user service policy")?;
    validate(pool, policy).await?;
    let row = sqlx::query(
        "SELECT to_char(c.starts_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS starts_at,
                to_char(c.ends_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS ends_at,
                user_service_state($4::timestamptz, $5::timestamptz, now()) AS service_state,
                (SELECT coalesce(sum(s.uplink_bytes+s.downlink_bytes),0)::bigint FROM user_cycle_access_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s) AS access_used,
                (SELECT coalesce(sum(s.uplink_bytes+s.downlink_bytes),0)::bigint FROM user_cycle_forwarding_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s) AS forwarding_used,
                CASE u.traffic_accounting_mode WHEN 'double' THEN 2 ELSE 1 END AS multiplier
         FROM users u CROSS JOIN user_traffic_cycle($3, now()) c WHERE u.tenant_id=$1 AND u.id=$2",
    ).bind(tenant).bind(user).bind(i32::from(policy.traffic_reset_day))
        .bind(&policy.service_starts_at).bind(&policy.service_expires_at)
        .fetch_optional(pool).await?.ok_or_else(|| StoreError::NotFound("user".into()))?;
    let multiplier = u64::try_from(row.try_get::<i32, _>("multiplier")?)
        .map_err(|_| StoreError::InvalidData("invalid accounting multiplier".into()))?;
    let bytes = |field| -> Result<u64> {
        u64::try_from(row.try_get::<i64, _>(field)?)
            .map(|value| value.saturating_mul(multiplier))
            .map_err(|_| StoreError::InvalidData("negative cycle usage".into()))
    };
    Ok(UserServicePolicyPreview {
        starts_at: row.try_get("starts_at")?,
        ends_at: row.try_get("ends_at")?,
        access_used_bytes: bytes("access_used")?,
        forwarding_used_bytes: bytes("forwarding_used")?,
        service_state: serde_json::from_value(row.try_get::<String, _>("service_state")?.into())?,
    })
}

pub async fn update(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    policy: UserServicePolicyWrite,
) -> Result<UserListItem> {
    actor.require_tenant_access(tenant, "user service policy")?;
    validate(pool, &policy).await?;
    let mut tx = pool.begin().await?;
    let previous = console::lock_control_state(&mut tx).await?;
    let before: bool = sqlx::query_scalar(
        "SELECT service_suspended FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound("user".into()))?;
    let row = sqlx::query(
        "UPDATE users SET traffic_reset_day=$3, service_starts_at=$4::timestamptz,
                service_expires_at=$5::timestamptz,
                service_suspended=user_service_state($4::timestamptz,$5::timestamptz,now()) <> 'active'
         WHERE tenant_id=$1 AND id=$2 AND
           (traffic_reset_day,service_starts_at,service_expires_at,service_suspended) IS DISTINCT FROM
           ($3,$4::timestamptz,$5::timestamptz,user_service_state($4::timestamptz,$5::timestamptz,now()) <> 'active')
         RETURNING service_suspended",
    ).bind(tenant).bind(user).bind(i32::from(policy.traffic_reset_day))
        .bind(&policy.service_starts_at).bind(&policy.service_expires_at).fetch_optional(&mut *tx).await?;
    if let Some(row) = row {
        sqlx::query("INSERT INTO user_service_policy_events(tenant_id,user_id,policy,actor) VALUES($1,$2,$3,$4)")
            .bind(tenant).bind(user).bind(serde_json::to_value(&policy)?).bind(actor.operator_id())
            .execute(&mut *tx).await?;
        if before != row.try_get::<bool, _>("service_suspended")? {
            bump_forwarding_tx(&mut tx, &[(tenant.to_owned(), user.to_owned())]).await?;
            let revision =
                console::insert_revision(&mut tx, actor.operator_id(), "更新用户服务有效期")
                    .await?;
            console::commit_revision(&mut tx, revision, previous, true).await?;
        }
    }
    tx.commit().await?;
    console::user_profile(pool, actor, tenant, user).await
}

/// Bounded and serialized with policy/model writes. Restarting or multiple Console instances
/// cannot lose a transition or overwrite a manual enable/disable decision.
pub async fn enforce(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let previous = console::lock_control_state(&mut tx).await?;
    let rows = sqlx::query(
        "SELECT tenant_id,id FROM users WHERE service_suspended IS DISTINCT FROM
           (user_service_state(service_starts_at,service_expires_at,now()) <> 'active')
         ORDER BY tenant_id,id LIMIT 128 FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    let users = rows
        .iter()
        .map(|row| Ok((row.try_get("tenant_id")?, row.try_get("id")?)))
        .collect::<Result<Vec<(String, String)>>>()?;
    for (tenant, user) in &users {
        sqlx::query("UPDATE users SET service_suspended=user_service_state(service_starts_at,service_expires_at,now()) <> 'active'
                     WHERE tenant_id=$1 AND id=$2")
            .bind(tenant).bind(user).execute(&mut *tx).await?;
    }
    bump_forwarding_tx(&mut tx, &users).await?;
    let revision =
        console::insert_revision(&mut tx, "system:user-service", "用户服务生效或到期").await?;
    console::commit_revision(&mut tx, revision, previous, true).await?;
    tx.commit().await?;
    Ok(users.len())
}

async fn bump_forwarding_tx(
    tx: &mut Transaction<'_, Postgres>,
    users: &[(String, String)],
) -> Result<()> {
    // Acquire machine locks in the same bytewise order as rule moves and quota enforcement.
    let tenants = users
        .iter()
        .map(|(tenant, _)| tenant.clone())
        .collect::<Vec<_>>();
    let ids = users
        .iter()
        .map(|(_, user)| user.clone())
        .collect::<Vec<_>>();
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT e.node_id COLLATE \"C\" FROM forwarding_entitlements e
         JOIN unnest($1::text[],$2::text[]) AS owner(tenant,user_id) ON e.tenant_id=owner.tenant AND e.user_id=owner.user_id
         ORDER BY 1",
    ).bind(tenants).bind(ids).fetch_all(&mut **tx).await?;
    for node in nodes {
        forwarding::bump_node_generation(tx, &node).await?;
    }
    Ok(())
}

pub async fn cycle_usage(pool: &PgPool, actor: &AdminContext) -> Result<UserCycleUsage> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let scope = actor.tenant_scope();
    let pattern = actor.tenant_scope_like_pattern();
    let rows=sqlx::query("SELECT u.tenant_id,u.id AS user_id,
            to_char(c.starts_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS starts_at,
            to_char(c.ends_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS ends_at,
            state.reset_id,
            CASE WHEN event.reset_at>=c.starts_at AND event.reset_at<c.ends_at
                 THEN to_char(event.reset_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') END AS reset_at
        FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
        LEFT JOIN user_cycle_reset_state state ON state.tenant_id=u.tenant_id AND state.user_id=u.id
        LEFT JOIN user_cycle_resets event ON event.id=state.reset_id
        WHERE ($1::text IS NULL OR u.tenant_id=$1 OR u.tenant_id LIKE $2 ESCAPE '\\') ORDER BY u.tenant_id,u.id")
        .bind(scope).bind(&pattern).fetch_all(&mut *tx).await?;
    let periods = rows
        .iter()
        .map(|r| {
            Ok(UserCyclePeriod {
                tenant_id: r.try_get("tenant_id")?,
                user_id: r.try_get("user_id")?,
                starts_at: r.try_get("starts_at")?,
                ends_at: r.try_get("ends_at")?,
                reset_id: r.try_get("reset_id")?,
                reset_at: r.try_get("reset_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = sqlx::query(
        "SELECT u.tenant_id,u.id AS user_id,s.app_id,s.uplink_bytes,s.downlink_bytes,s.has_gap
        FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
        CROSS JOIN LATERAL user_cycle_access_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s
        WHERE ($1::text IS NULL OR u.tenant_id=$1 OR u.tenant_id LIKE $2 ESCAPE '\\')
        ORDER BY u.tenant_id,u.id,s.app_id",
    )
    .bind(scope)
    .bind(&pattern)
    .fetch_all(&mut *tx)
    .await?;
    let views = rows
        .iter()
        .map(|r| {
            Ok(crate::UsageMonthlyViewRow {
                tenant_id: r.try_get("tenant_id")?,
                user_id: r.try_get("user_id")?,
                app_id: r.try_get("app_id")?,
                uplink_bytes: bytes(r, "uplink_bytes")?,
                downlink_bytes: bytes(r, "downlink_bytes")?,
                has_gap: r.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = sqlx::query(
        "SELECT u.tenant_id,u.id AS user_id,s.node_id,s.rule_id,r.name AS rule_name,
            s.uplink_bytes,s.downlink_bytes,s.has_gap
        FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c
        CROSS JOIN LATERAL user_cycle_forwarding_usage(u.tenant_id,u.id,c.starts_at,c.ends_at) s
        JOIN forwarding_rules r ON r.id=s.rule_id
        WHERE ($1::text IS NULL OR u.tenant_id=$1 OR u.tenant_id LIKE $2 ESCAPE '\\')
        ORDER BY u.tenant_id,u.id,s.node_id,s.rule_id",
    )
    .bind(scope)
    .bind(&pattern)
    .fetch_all(&mut *tx)
    .await?;
    let forwarding = rows
        .iter()
        .map(|r| {
            Ok(brocade_deployment::protocol::UsageForwardingMonthlyRow {
                tenant_id: r.try_get("tenant_id")?,
                user_id: r.try_get("user_id")?,
                node_id: r.try_get("node_id")?,
                rule_id: r.try_get("rule_id")?,
                rule_name: r.try_get("rule_name")?,
                uplink_bytes: bytes(r, "uplink_bytes")?,
                downlink_bytes: bytes(r, "downlink_bytes")?,
                has_gap: r.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let rows=sqlx::query("SELECT p.tenant_id,p.user_id,p.kind,p.scope,p.quota_bytes,
    user_traffic_usage(p.tenant_id,p.user_id,p.kind,p.scope)::text AS used_bytes,p.exhausted,p.suspended,p.rate_bps,p.base_rate_bps,p.action,
    u.status='active' AND NOT u.service_suspended AS available
    FROM user_traffic_scopes p JOIN users u ON u.tenant_id=p.tenant_id AND u.id=p.user_id WHERE ($1::text IS NULL OR p.tenant_id=$1 OR p.tenant_id LIKE $2 ESCAPE '\\') ORDER BY p.tenant_id,p.user_id,p.kind,p.scope")
    .bind(scope).bind(&pattern).fetch_all(&mut *tx).await?;
    let traffic_policies = rows
        .iter()
        .map(
            |row| -> Result<brocade_deployment::user_traffic::UserTrafficStatus> {
                let policy = crate::user_traffic::policy_from_row(row)?;
                let effect = policy.effect(row.try_get("exhausted")?, row.try_get("available")?);
                Ok(brocade_deployment::user_traffic::UserTrafficStatus {
                    tenant_id: row.try_get("tenant_id")?,
                    user_id: row.try_get("user_id")?,
                    kind: serde_json::from_value(row.try_get::<String, _>("kind")?.into())?,
                    app_id: (row.try_get::<String, _>("kind")? == "access")
                        .then(|| row.try_get::<String, _>("scope"))
                        .transpose()?,
                    quota_bytes: policy.quota_bytes,
                    used_bytes: row.try_get("used_bytes")?,
                    exhausted: row.try_get("exhausted")?,
                    suspended: matches!(
                        effect,
                        brocade_deployment::user_traffic::UserTrafficEffect::Suspended
                    ),
                    rate_bps: match effect {
                        brocade_deployment::user_traffic::UserTrafficEffect::Limited {
                            rate_bps,
                        } => Some(rate_bps),
                        _ => None,
                    },
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;

    tx.commit().await?;
    Ok(UserCycleUsage {
        periods,
        views,
        forwarding,
        traffic_policies,
    })
}

fn bytes(row: &sqlx::postgres::PgRow, field: &str) -> Result<u64> {
    u64::try_from(row.try_get::<i64, _>(field)?)
        .map_err(|_| StoreError::InvalidData("negative cycle usage".into()))
}
