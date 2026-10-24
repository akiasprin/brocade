//! Per-app access quotas with durable byte reservations.
use crate::{AdminContext, AdminRole, Result, StoreError};
use brocade_deployment::user_traffic::*;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

fn require_read(actor: &AdminContext, tenant: &str, user: &str) -> Result<()> {
    actor.require_tenant_access(tenant, "user traffic policy")?;
    if actor.role() == AdminRole::User
        && !actor
            .self_user()
            .is_some_and(|u| u.tenant_id == tenant && u.user_id == user)
    {
        return Err(StoreError::Forbidden(
            "only the current user's traffic policy is visible".into(),
        ));
    }
    Ok(())
}

// Scope is an app id. Quota-suspended access entries remain visible until restored.
const SCOPE_NODES_SQL: &str = "SELECT n.id,n.user_traffic_applied_hash,
    n.user_traffic_policy_seen_at>now()-interval '10 seconds' AS fresh
    FROM nodes n WHERE n.retired_at IS NULL AND
    (($3='access' AND (EXISTS(SELECT 1 FROM grants g JOIN ingresses i ON i.app_id=g.app_id AND i.id=g.ingress_id
        WHERE g.tenant_id=$1 AND g.user_id=$2 AND g.app_id=$4 AND i.node_id=n.id)
      OR EXISTS(SELECT 1 FROM quota_suspensions q JOIN ingresses i ON i.id=q.ingress_id AND i.app_id=q.app_id
        WHERE q.tenant_id=$1 AND q.user_id=$2 AND q.app_id=$4 AND i.node_id=n.id)))
    ) ORDER BY n.id";

pub async fn views(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
) -> Result<Vec<UserTrafficPolicyView>> {
    require_read(actor, tenant, user)?;
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2)",
    )
    .bind(tenant)
    .bind(user)
    .fetch_one(pool)
    .await?
    {
        return Err(StoreError::NotFound("user".into()));
    }
    let scopes: Vec<(String, String)> = sqlx::query_as(
        "SELECT 'access'::text, app_id FROM (
        SELECT app_id FROM grants WHERE tenant_id=$1 AND user_id=$2
        UNION SELECT app_id FROM user_app_quotas WHERE tenant_id=$1 AND user_id=$2
        UNION SELECT app_id FROM quota_suspensions WHERE tenant_id=$1 AND user_id=$2) apps
        WHERE EXISTS(SELECT 1 FROM apps a WHERE a.id=apps.app_id)
        ORDER BY 1,2",
    )
    .bind(tenant)
    .bind(user)
    .fetch_all(pool)
    .await?;
    let mut result = Vec::new();
    for (kind, scope) in scopes {
        let kind: UserTrafficKind = serde_json::from_value(kind.into())?;
        let row=sqlx::query("SELECT u.status='active' AND NOT u.service_suspended AS available,
            user_traffic_usage(u.tenant_id,u.id,$3,$4)::text AS used,
            p.quota_bytes::text AS quota,p.action,p.rate_bps,p.exhausted
            FROM users u LEFT JOIN user_traffic_scopes p ON p.tenant_id=u.tenant_id AND p.user_id=u.id AND p.kind=$3 AND p.scope=$4
            WHERE u.tenant_id=$1 AND u.id=$2")
            .bind(tenant).bind(user).bind(kind.as_str()).bind(&scope).fetch_one(pool).await?;
        let policy = row
            .try_get::<Option<String>, _>("action")?
            .map(|action| -> Result<UserTrafficPolicy> {
                Ok(UserTrafficPolicy {
                    quota_bytes: row.try_get("quota")?,
                    exhausted_action: if action == "throttle" {
                        ExhaustedAction::Throttle {
                            rate_bps: u64::try_from(row.try_get::<i64, _>("rate_bps")?)
                                .map_err(|_| StoreError::InvalidData("invalid rate".into()))?,
                        }
                    } else {
                        ExhaustedAction::Suspend
                    },
                })
            })
            .transpose()?;
        result.push(UserTrafficPolicyView {
            kind,
            app_id: (kind == UserTrafficKind::Access).then_some(scope.clone()),
            policy: policy.clone(),
            used_bytes: row.try_get("used")?,
            exhausted: row
                .try_get::<Option<bool>, _>("exhausted")?
                .unwrap_or(false),
            service_available: row.try_get("available")?,
            unsupported_nodes: unsupported(pool, tenant, user, kind, &scope).await?,
            pending_nodes: if kind == UserTrafficKind::Access
                && policy
                    .as_ref()
                    .is_none_or(|p| matches!(p.exhausted_action, ExhaustedAction::Suspend))
            {
                Vec::new()
            } else {
                pending(pool, tenant, user, kind, &scope).await?
            },
        });
    }
    Ok(result)
}

async fn unsupported(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    kind: UserTrafficKind,
    scope: &str,
) -> Result<Vec<String>> {
    let sql=format!("SELECT n.id FROM ({SCOPE_NODES_SQL}) target JOIN nodes n ON n.id=target.id
        LEFT JOIN node_agent_state agent ON agent.node_id=n.id
        WHERE COALESCE(agent.agent_protocol_version,0)<44 OR NOT n.user_traffic_supported
        OR n.user_traffic_support_at IS NULL OR n.user_traffic_support_at<now()-interval '90 seconds' ORDER BY n.id");
    Ok(sqlx::query_scalar(&sql)
        .bind(tenant)
        .bind(user)
        .bind(kind.as_str())
        .bind(scope)
        .fetch_all(pool)
        .await?)
}

pub async fn update(
    pool: &PgPool,
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    kind: UserTrafficKind,
    app: Option<&str>,
    policy: UserTrafficPolicy,
) -> Result<Vec<UserTrafficPolicyView>> {
    require_read(actor, tenant, user)?;
    if !matches!(
        actor.role(),
        AdminRole::Editor | AdminRole::Publisher | AdminRole::TenantAdmin | AdminRole::SystemAdmin
    ) {
        return Err(StoreError::Forbidden(
            "editing user traffic policy requires edit permission".into(),
        ));
    }
    let scope = match (kind, app) {
        (UserTrafficKind::Access, Some(app)) if !app.is_empty() => app,
        (UserTrafficKind::Forwarding, _) => {
            return Err(StoreError::Unsupported(
                "转发总额度已移除，请设置各机器的转发额度".into(),
            ));
        }
        _ => return Err(StoreError::InvalidData("接入额度必须指定 app".into())),
    };
    policy.validate().map_err(StoreError::InvalidData)?;
    let mut tx = pool.begin().await?;
    crate::console::lock_control_state(&mut tx).await?;
    if sqlx::query("SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
    {
        return Err(StoreError::NotFound("user".into()));
    }
    if kind == UserTrafficKind::Access
        && !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM apps WHERE id=$1)")
            .bind(scope)
            .fetch_one(&mut *tx)
            .await?
    {
        return Err(StoreError::NotFound("app".into()));
    }
    if policy.quota_bytes.is_some()
        && matches!(policy.exhausted_action, ExhaustedAction::Throttle { .. })
    {
        // The user parent lock serializes entry authorization with this capability check.
        let sql=format!("SELECT n.id FROM ({SCOPE_NODES_SQL}) target JOIN nodes n ON n.id=target.id LEFT JOIN node_agent_state agent ON agent.node_id=n.id
            WHERE COALESCE(agent.agent_protocol_version,0)<44 OR NOT n.user_traffic_supported OR n.user_traffic_support_at IS NULL OR n.user_traffic_support_at<now()-interval '90 seconds'");
        let nodes: Vec<String> = sqlx::query_scalar(&sql)
            .bind(tenant)
            .bind(user)
            .bind(kind.as_str())
            .bind(scope)
            .fetch_all(&mut *tx)
            .await?;
        if !nodes.is_empty() {
            return Err(StoreError::Unsupported(
                "启用超额限速前，请升级相关机器的 Agent 和 Xray，并等待用户限速能力确认".into(),
            ));
        }
    }
    let (action, rate) = match policy.exhausted_action {
        ExhaustedAction::Suspend => ("suspend", None),
        ExhaustedAction::Throttle { rate_bps } => ("throttle", Some(rate_bps as i64)),
    };
    let quota = policy
        .quota_bytes
        .as_ref()
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| StoreError::InvalidData("invalid quota".into()))?;
    let changed = if let Some(quota) = quota {
        sqlx::query("INSERT INTO user_app_quotas(tenant_id,user_id,app_id,limit_bytes,exhausted_action,rate_bps) VALUES($1,$2,$3,$4,$5,$6)
                ON CONFLICT(tenant_id,user_id,app_id) DO UPDATE SET limit_bytes=$4,exhausted_action=$5,rate_bps=$6,
                revision=user_app_quotas.revision+1,updated_at=now()
                WHERE (user_app_quotas.limit_bytes,user_app_quotas.exhausted_action,user_app_quotas.rate_bps) IS DISTINCT FROM ($4,$5,$6)")
                .bind(tenant).bind(user).bind(scope).bind(quota).bind(action).bind(rate).execute(&mut *tx).await?.rows_affected()>0
    } else {
        sqlx::query("DELETE FROM user_app_quotas WHERE tenant_id=$1 AND user_id=$2 AND app_id=$3")
            .bind(tenant)
            .bind(user)
            .bind(scope)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0
    };
    if changed {
        sqlx::query("INSERT INTO user_traffic_policy_events(tenant_id,user_id,kind,scope,policy,actor) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(tenant).bind(user).bind(kind.as_str()).bind(scope).bind(serde_json::to_value(&policy)?).bind(actor.operator_id()).execute(&mut *tx).await?;
    }
    if quota.is_some() {
        let nodes: Vec<String> =
            sqlx::query_scalar(&format!("SELECT id FROM ({SCOPE_NODES_SQL}) target"))
                .bind(tenant)
                .bind(user)
                .bind(kind.as_str())
                .bind(scope)
                .fetch_all(&mut *tx)
                .await?;
        for node in nodes {
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM ({ASSIGNMENTS_SQL}) sources"))
                    .bind(&node)
                    .fetch_one(&mut *tx)
                    .await?;
            if count > 4096 {
                return Err(StoreError::Unsupported(
                    "该机器的用户额度来源超过 4096 个，不能启用新的额度".into(),
                ));
            }
        }
    }
    evaluate_one(&mut tx, tenant, user, scope).await?;
    tx.commit().await?;
    views(pool, actor, tenant, user).await
}

async fn evaluate_one(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
    scope: &str,
) -> Result<()> {
    sqlx::query("UPDATE user_app_quotas SET exhausted=user_traffic_usage(tenant_id,user_id,'access',app_id)>=limit_bytes WHERE tenant_id=$1 AND user_id=$2 AND app_id=$3")
        .bind(tenant).bind(user).bind(scope).execute(&mut **tx).await?;
    Ok(())
}
pub async fn enforce(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    crate::console::lock_control_state(&mut tx).await?;
    let rows=sqlx::query("SELECT tenant_id,user_id,scope FROM user_traffic_scopes
        WHERE exhausted IS DISTINCT FROM (quota_bytes IS NOT NULL AND user_traffic_usage(tenant_id,user_id,kind,scope)>=quota_bytes)
        ORDER BY tenant_id,user_id,kind,scope LIMIT 128").fetch_all(&mut *tx).await?;
    for row in &rows {
        let tenant: String = row.try_get("tenant_id")?;
        let user: String = row.try_get("user_id")?;
        let scope: String = row.try_get("scope")?;
        evaluate_one(&mut tx, &tenant, &user, &scope).await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

pub async fn support(pool: &PgPool, node: &str, supported: bool) -> Result<bool> {
    sqlx::query(
        "UPDATE nodes SET user_traffic_supported=$2,user_traffic_support_at=now() WHERE id=$1",
    )
    .bind(node)
    .bind(supported)
    .execute(pool)
    .await?;
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_traffic_scopes p WHERE p.kind='access' AND p.action='throttle' AND p.quota_bytes IS NOT NULL AND
      EXISTS(SELECT 1 FROM ingresses i WHERE i.node_id=$1 AND
        (EXISTS(SELECT 1 FROM grants g WHERE g.tenant_id=p.tenant_id AND g.user_id=p.user_id AND g.app_id=p.scope AND g.ingress_id=i.id AND g.app_id=i.app_id)
        OR EXISTS(SELECT 1 FROM quota_suspensions q WHERE q.tenant_id=p.tenant_id AND q.user_id=p.user_id AND q.app_id=p.scope AND q.ingress_id=i.id AND q.app_id=i.app_id))))")
      .bind(node).fetch_one(pool).await?)
}

pub async fn budget(
    pool: &PgPool,
    node: &str,
    request: UserBudgetRequest,
) -> Result<UserBudgetGrant> {
    if request.bytes == 0
        || request.bytes > MAX_BUDGET_BYTES
        || request.source.len() > 512
        || request.request_id.len() != 32
        || !request.request_id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(StoreError::InvalidData(
            "invalid user budget request".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    // Resolve identity through authoritative ownership, never trust an arbitrary tenant/user from the Agent.
    let (tenant, user, kind, scope) = if let Some(id) = request
        .source
        .strip_prefix("fwd:")
        .and_then(|v| v.parse::<i64>().ok())
    {
        let row=sqlx::query(&format!("SELECT r.tenant_id,r.user_id,
            COALESCE(q.suspended,false) OR (q.limit_bytes IS NOT NULL AND {}>=q.limit_bytes) AS quota_suspended
            FROM forwarding_rules r JOIN forwarding_entitlements e USING(tenant_id,user_id,node_id)
            LEFT JOIN forwarding_node_quotas q USING(tenant_id,user_id,node_id)
            WHERE r.id=$1 AND r.node_id=$2 AND r.deleted_at IS NULL AND r.enabled AND e.enabled", crate::forwarding_quota::CYCLE_USED))
            .bind(id).bind(node).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(UserBudgetGrant::Suspended {
                valid_ms: BUDGET_TTL_MS,
            });
        };
        if row.try_get::<bool, _>("quota_suspended")? {
            return Ok(UserBudgetGrant::Suspended {
                valid_ms: BUDGET_TTL_MS,
            });
        }
        (
            row.try_get::<String, _>("tenant_id")?,
            row.try_get::<String, _>("user_id")?,
            "forwarding",
            String::new(),
        )
    } else if let Some((user, tenant, ingress)) =
        brocade_core::model::parse_grant_label(&request.source)
    {
        let app:Option<String>=sqlx::query_scalar("SELECT g.app_id FROM grants g JOIN ingresses i ON i.app_id=g.app_id AND i.id=g.ingress_id
            WHERE g.tenant_id=$1 AND g.user_id=$2 AND g.ingress_id=$3 AND i.node_id=$4")
            .bind(tenant).bind(user).bind(ingress).bind(node).fetch_optional(&mut *tx).await?;
        let Some(app) = app else {
            return Ok(UserBudgetGrant::Suspended {
                valid_ms: BUDGET_TTL_MS,
            });
        };
        (tenant.to_owned(), user.to_owned(), "access", app)
    } else {
        return Err(StoreError::InvalidData(
            "invalid authenticated traffic source".into(),
        ));
    };
    let owner=sqlx::query("SELECT status='active' AND NOT service_suspended AS available FROM users WHERE tenant_id=$1 AND id=$2 FOR SHARE")
        .bind(&tenant).bind(&user).fetch_one(&mut *tx).await?;
    if !owner.try_get::<bool, _>("available")? {
        return Ok(UserBudgetGrant::Suspended {
            valid_ms: BUDGET_TTL_MS,
        });
    }
    // Older Agents can finish requests for a forwarding source removed from their latest
    // snapshot. Authenticate its owner above, then explicitly release the obsolete policy lease.
    if kind == "forwarding" {
        return Ok(UserBudgetGrant::Unlimited {
            valid_ms: BUDGET_TTL_MS,
            managed: false,
        });
    }
    let row=sqlx::query("SELECT exhausted,action,rate_bps,quota_bytes FROM user_traffic_scopes WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).fetch_optional(&mut *tx).await?;
    let Some(policy) = row else {
        return Ok(UserBudgetGrant::Unlimited {
            valid_ms: BUDGET_TTL_MS,
            managed: false,
        });
    };
    // Existing stop-on-quota access uses the authorization worker. Only the throttle action
    // needs the user budget feature; upgrading Agent alone must not require a new Xray for legacy quotas.
    if kind == "access"
        && policy.try_get::<String, _>("action")? == "suspend"
        && !policy.try_get::<bool, _>("exhausted")?
    {
        return Ok(UserBudgetGrant::Unlimited {
            valid_ms: BUDGET_TTL_MS,
            managed: false,
        });
    }
    if !policy.try_get::<bool, _>("exhausted")? {
        return Ok(UserBudgetGrant::Unlimited {
            valid_ms: BUDGET_TTL_MS,
            managed: policy.try_get::<Option<i64>, _>("quota_bytes")?.is_some(),
        });
    }
    if policy.try_get::<String, _>("action")? == "suspend" {
        return Ok(UserBudgetGrant::Suspended {
            valid_ms: BUDGET_TTL_MS,
        });
    }
    let rate = policy.try_get::<i64, _>("rate_bps")? as u64;
    sqlx::query("INSERT INTO user_traffic_budgets(tenant_id,user_id,kind,scope) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).execute(&mut *tx).await?;
    // A single database row serializes all nodes, Console instances and Xray generations.
    let wait_ms:f64=sqlx::query_scalar("SELECT GREATEST(0,extract(epoch FROM(next_at-clock_timestamp()))*1000)::double precision FROM user_traffic_budgets
        WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 FOR UPDATE")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).fetch_one(&mut *tx).await?;
    sqlx::query("DELETE FROM user_traffic_budget_receipts WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 AND expires_at<=clock_timestamp()")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).execute(&mut *tx).await?;
    let receipt=sqlx::query("SELECT request_id,bytes,rate_bps,GREATEST(0,extract(epoch FROM(expires_at-clock_timestamp()))*1000)::bigint AS valid_ms
        FROM user_traffic_budget_receipts WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 AND node_id=$5 AND request_id=$6")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).bind(node).bind(&request.request_id).fetch_optional(&mut *tx).await?;
    if let Some(receipt) = receipt {
        if receipt.try_get::<String, _>("request_id")? == request.request_id {
            return Ok(UserBudgetGrant::Limited {
                bytes: receipt.try_get::<i64, _>("bytes")? as u64,
                rate_bps: receipt.try_get::<i64, _>("rate_bps")? as u64,
                valid_ms: receipt.try_get::<i64, _>("valid_ms")? as u64,
            });
        }
    }
    sqlx::query(
        "INSERT INTO user_traffic_budget_demand(tenant_id,user_id,kind,scope,node_id) VALUES($1,$2,$3,$4,$5)
        ON CONFLICT(tenant_id,user_id,kind,scope,node_id) DO UPDATE SET seen_at=clock_timestamp()",
    )
    .bind(&tenant)
    .bind(&user)
    .bind(kind).bind(&scope)
    .bind(node)
    .execute(&mut *tx)
    .await?;
    if wait_ms > 0.0 {
        tx.commit().await?;
        return Ok(UserBudgetGrant::Retry {
            after_ms: (wait_ms.ceil() as u64).clamp(1, 1000),
        });
    }
    let next_node:String=sqlx::query_scalar("SELECT node_id FROM user_traffic_budget_demand WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4
        AND seen_at>clock_timestamp()-interval '2 seconds' ORDER BY served_at,node_id LIMIT 1")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).fetch_one(&mut *tx).await?;
    if next_node != node {
        tx.commit().await?;
        return Ok(UserBudgetGrant::Retry { after_ms: 50 });
    }
    let active:i64=sqlx::query_scalar("SELECT count(*) FROM user_traffic_budget_demand WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 AND seen_at>clock_timestamp()-interval '2 seconds'")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).fetch_one(&mut *tx).await?;

    let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM user_traffic_budget_receipts WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 AND node_id=$5")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).bind(node).fetch_one(&mut *tx).await?;
    if receipts >= 8 {
        tx.commit().await?;
        return Ok(UserBudgetGrant::Retry { after_ms: 250 });
    }
    // At most one second of bytes is outstanding globally at issuance; local pacing avoids a one-second write burst.
    let bytes = request.bytes.min((rate / 8 / active.max(1) as u64).max(1));
    let duration_ms = (bytes as f64) * 8000.0 / (rate as f64);
    sqlx::query("UPDATE user_traffic_budgets SET next_at=clock_timestamp()+$5*interval '1 millisecond' WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).bind(duration_ms).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO user_traffic_budget_receipts(tenant_id,user_id,kind,scope,node_id,request_id,bytes,rate_bps,expires_at)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '2 seconds')")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).bind(node).bind(&request.request_id).bind(bytes as i64).bind(rate as i64).execute(&mut *tx).await?;
    sqlx::query("UPDATE user_traffic_budget_demand SET served_at=clock_timestamp() WHERE tenant_id=$1 AND user_id=$2 AND kind=$3 AND scope=$4 AND node_id=$5")
        .bind(&tenant).bind(&user).bind(kind).bind(&scope).bind(node).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(UserBudgetGrant::Limited {
        bytes,
        rate_bps: rate,
        valid_ms: BUDGET_TTL_MS,
    })
}

/// Take the parent before entitlement/rule locks, matching user deletion and policy edits.
pub(crate) async fn lock_user_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
) -> Result<()> {
    sqlx::query("SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR SHARE")
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("user".into()))?;
    Ok(())
}

/// Enabling a new entry must not introduce a machine which can ignore an already configured cap.
pub(crate) async fn check_node_tx(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
    kind: &str,
    scope: Option<&str>,
    node: &str,
) -> Result<()> {
    lock_user_tx(tx, tenant, user).await?;
    let unsupported:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_traffic_scopes p JOIN nodes n ON n.id=$4 LEFT JOIN node_agent_state agent ON agent.node_id=n.id
  WHERE p.tenant_id=$1 AND p.user_id=$2 AND p.kind=$3 AND p.scope=$5 AND p.action='throttle' AND p.quota_bytes IS NOT NULL AND
   (COALESCE(agent.agent_protocol_version,0)<44 OR NOT n.user_traffic_supported OR n.user_traffic_support_at IS NULL OR n.user_traffic_support_at<now()-interval '90 seconds'))")
  .bind(tenant).bind(user).bind(kind).bind(node).bind(scope.unwrap_or("")).fetch_one(&mut **tx).await?;
    if unsupported {
        return Err(StoreError::Unsupported(
            "该用户已设置超额限速，请先升级入口机器的 Agent 和 Xray，并等待能力确认".into(),
        ));
    }
    Ok(())
}

const ASSIGNMENTS_SQL:&str="SELECT g.user_id||'@'||g.tenant_id||'#'||g.ingress_id AS source,
    CASE WHEN u.status<>'active' OR u.service_suspended OR p.suspended THEN 'suspended'
         WHEN p.exhausted AND p.action='throttle' THEN 'limited' ELSE 'unlimited' END AS mode,p.rate_bps,p.quota_bytes IS NOT NULL AS managed,p.revision
    FROM grants g JOIN ingresses i ON i.app_id=g.app_id AND i.id=g.ingress_id
    JOIN users u ON u.tenant_id=g.tenant_id AND u.id=g.user_id
    JOIN user_traffic_scopes p ON p.tenant_id=u.tenant_id AND p.user_id=u.id AND p.kind='access' AND p.scope=g.app_id
    WHERE i.node_id=$1 AND p.action='throttle'
    ORDER BY source LIMIT 4097";

/// Configured app quotas use policy leases. Removing a source releases old Agent policy caches.
pub async fn assignments(pool: &PgPool, node: &str) -> Result<UserTrafficSnapshot> {
    let rows = sqlx::query(ASSIGNMENTS_SQL)
        .bind(node)
        .fetch_all(pool)
        .await?;
    if rows.len() > 4096 {
        return Err(StoreError::Unsupported(
            "user policy source capacity exceeded".into(),
        ));
    }
    let mut sources = rows
        .iter()
        .map(|row| {
            Ok(UserTrafficAssignment {
                source: row.try_get("source")?,
                revision: u64::try_from(row.try_get::<i64, _>("revision")?)
                    .map_err(|_| StoreError::InvalidData("invalid policy revision".into()))?,
                effect: match row.try_get::<String, _>("mode")?.as_str() {
                    "suspended" => UserTrafficEffect::Suspended,
                    "limited" => UserTrafficEffect::Limited {
                        rate_bps: row.try_get::<i64, _>("rate_bps")? as u64,
                    },
                    _ => UserTrafficEffect::Unlimited {
                        managed: row.try_get("managed")?,
                    },
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    sources.sort_by(|a, b| a.source.cmp(&b.source));
    let sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&sources)?));
    Ok(UserTrafficSnapshot { sources, sha256 })
}

async fn pending(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    kind: UserTrafficKind,
    scope: &str,
) -> Result<Vec<String>> {
    let nodes = sqlx::query(SCOPE_NODES_SQL)
        .bind(tenant)
        .bind(user)
        .bind(kind.as_str())
        .bind(scope)
        .fetch_all(pool)
        .await?;
    let mut pending = Vec::new();
    for row in nodes {
        let node: String = row.try_get("id")?;
        let applied: Option<String> = row.try_get("user_traffic_applied_hash")?;
        if !row.try_get::<Option<bool>, _>("fresh")?.unwrap_or(false)
            || applied.as_deref() != Some(assignments(pool, &node).await?.sha256.as_str())
        {
            pending.push(node);
        }
    }
    Ok(pending)
}
pub async fn acknowledge(pool: &PgPool, node: &str, hash: &str) -> Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidData(
            "invalid user policy acknowledgment".into(),
        ));
    }
    if assignments(pool, node).await?.sha256 != hash {
        return Err(StoreError::InvalidData(
            "stale user policy acknowledgment".into(),
        ));
    }
    sqlx::query("UPDATE nodes SET user_traffic_applied_hash=$2,user_traffic_policy_seen_at=now() WHERE id=$1").bind(node).bind(hash).execute(pool).await?;
    Ok(())
}
