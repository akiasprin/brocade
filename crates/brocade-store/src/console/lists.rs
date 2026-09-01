//! List queries: tenants, machines, revisions, users, quotas. All read-only, each mapping rows
//! into a response type.
use sqlx::{PgPool, Row};

use super::*;
use crate::{AdminContext, Result, StoreError};

pub(crate) async fn list_tenants(pool: &PgPool, actor: &AdminContext) -> Result<TenantList> {
    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT t.id,
                    t.name,
                    t.created_at::text AS created_at,
                    t.created_revision,
                    (SELECT count(*) FROM nodes n WHERE n.tenant_id = t.id) AS node_count,
                    (SELECT count(*) FROM users u WHERE u.tenant_id = t.id) AS user_count,
                    (SELECT count(*) FROM admin_operators o WHERE o.tenant_scope = t.id) AS operator_count
             FROM tenants t
             ORDER BY t.id",
        )
        .fetch_all(pool)
        .await?
    } else {
        let scope = require_actor_tenant_scope(actor)?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(
            "SELECT t.id,
                    t.name,
                    t.created_at::text AS created_at,
                    t.created_revision,
                    (SELECT count(*) FROM nodes n WHERE n.tenant_id = t.id) AS node_count,
                    (SELECT count(*) FROM users u WHERE u.tenant_id = t.id) AS user_count,
                    (SELECT count(*) FROM admin_operators o WHERE o.tenant_scope = t.id) AS operator_count
             FROM tenants t
             WHERE t.id = $1 OR t.id LIKE $2 ESCAPE '\\'
             ORDER BY t.id",
        )
        .bind(scope)
        .bind(pattern)
        .fetch_all(pool)
        .await?
    };

    Ok(TenantList {
        tenants: rows
            .iter()
            .map(tenant_item_from_row)
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn list_node_agent_states(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<NodeAgentStateList> {
    let rows = if actor.is_system_admin() {
        sqlx::query(node_agent_state_sql(false))
            .fetch_all(pool)
            .await?
    } else {
        let scope = require_actor_tenant_scope(actor)?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(node_agent_state_sql(true))
            .bind(scope)
            .bind(pattern)
            .fetch_all(pool)
            .await?
    };

    Ok(NodeAgentStateList {
        nodes: rows
            .iter()
            .map(node_agent_state_from_row)
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn list_revisions(
    pool: &PgPool,
    actor: &AdminContext,
    limit: u32,
) -> Result<RevisionList> {
    let current_revision = crate::materialize::current_revision(pool).await?;
    let limit = i64::from(limit.clamp(1, 200));
    let rows = sqlx::query(
        "SELECT r.id,
                r.created_at::text AS created_at,
                r.author,
                r.note,
                r.status,
                ms.revision_id IS NOT NULL AS has_snapshot
         FROM revisions r
         LEFT JOIN model_snapshots ms
           ON ms.revision_id = r.id
         ORDER BY r.id DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(RevisionList {
        current_revision,
        revisions: rows
            .iter()
            .map(|row| revision_item_from_row(row, current_revision, actor.is_system_admin()))
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn list_users(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: Option<&str>,
    include_disabled: bool,
) -> Result<UserList> {
    let tenant_id = tenant_id.and_then(optional_text);
    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT tenant_id, id, uuid::text AS uuid, status,
                    created_at::text AS created_at, created_revision
             FROM users
             WHERE ($1::text IS NULL OR tenant_id = $1)
               AND ($2::boolean OR status = 'active')
             ORDER BY tenant_id, id",
        )
        .bind(tenant_id)
        .bind(include_disabled)
        .fetch_all(pool)
        .await?
    } else if let Some(tenant_id) = tenant_id {
        actor.require_tenant_access(tenant_id, "user")?;
        sqlx::query(
            "SELECT tenant_id, id, uuid::text AS uuid, status,
                    created_at::text AS created_at, created_revision
             FROM users
             WHERE tenant_id = $1
               AND ($2::boolean OR status = 'active')
             ORDER BY tenant_id, id",
        )
        .bind(tenant_id)
        .bind(include_disabled)
        .fetch_all(pool)
        .await?
    } else {
        let scope = require_actor_tenant_scope(actor)?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant_scope".to_owned()))?;
        sqlx::query(
            "SELECT tenant_id, id, uuid::text AS uuid, status,
                    created_at::text AS created_at, created_revision
             FROM users
             WHERE (tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
               AND ($3::boolean OR status = 'active')
             ORDER BY tenant_id, id",
        )
        .bind(scope)
        .bind(pattern)
        .bind(include_disabled)
        .fetch_all(pool)
        .await?
    };

    Ok(UserList {
        users: rows
            .iter()
            .map(user_item_from_row)
            .collect::<Result<Vec<_>>>()?,
    })
}

// The quota list. Tenant scoping uses the same two-parameter form as usage (usage.rs) — a
// quota and its consumption are a pair, and a user one can see should have both visible.
//
// The JOIN on apps filters orphan rows: quotas have no foreign key to apps (a rollback rebuilds
// apps wholesale), so a quota row survives its view being deleted. Surviving
// is correct — rebuild the view and the quota is still there — but it must not be listed, because
// the UI has no view to hang it on.
pub async fn list_user_app_quotas(
    pool: &PgPool,
    actor: &AdminContext,
    tenant_id: Option<&str>,
) -> Result<UserAppQuotaList> {
    let tenant_id = tenant_id.and_then(optional_text);
    if let Some(tenant_id) = tenant_id {
        actor.require_tenant_access(tenant_id, "quota")?;
    }
    let rows = sqlx::query(
        "SELECT q.tenant_id, q.user_id, q.app_id, q.limit_bytes,
                q.updated_at::text AS updated_at,
                coalesce((
                    SELECT array_agg(s.ingress_id ORDER BY s.ingress_id)
                    FROM quota_suspensions s
                    WHERE s.tenant_id = q.tenant_id
                      AND s.user_id = q.user_id
                      AND s.app_id = q.app_id
                ), ARRAY[]::text[]) AS suspended_ingresses
         FROM user_app_quotas q
         JOIN apps a ON a.id = q.app_id
         WHERE ($1::text IS NULL OR q.tenant_id = $1)
           AND (
                $2::text IS NULL
                OR q.tenant_id = $2
                OR q.tenant_id LIKE $3 ESCAPE '\\'
           )
         ORDER BY q.tenant_id, q.user_id, a.position, a.id",
    )
    .bind(tenant_id)
    .bind(actor.tenant_scope())
    .bind(actor.tenant_scope_like_pattern())
    .fetch_all(pool)
    .await?;

    Ok(UserAppQuotaList {
        quotas: rows
            .iter()
            .map(user_app_quota_from_row)
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn set_user_app_quota(
    pool: &PgPool,
    actor: &AdminContext,
    request: SetUserAppQuotaRequest,
) -> Result<SetUserAppQuotaResult> {
    let tenant_id = required_text(request.tenant_id, "tenant_id")?;
    let user_id = required_text(request.user_id, "user_id")?;
    let app_id = required_text(request.app_id, "app_id")?;
    actor.require_tenant_access(&tenant_id, "quota")?;

    let mut tx = pool.begin().await?;
    ensure_user_exists_tx(&mut tx, &tenant_id, &user_id).await?;
    ensure_app_exists_tx(&mut tx, &app_id).await?;

    let Some(limit_bytes) = request.limit_bytes else {
        sqlx::query(
            "DELETE FROM user_app_quotas WHERE tenant_id = $1 AND user_id = $2 AND app_id = $3",
        )
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&app_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(SetUserAppQuotaResult { quota: None });
    };

    if limit_bytes == 0 {
        return Err(StoreError::InvalidData(
            "limit_bytes 要大于 0；不限额就把这个字段留空".to_owned(),
        ));
    }
    let limit = u64_to_i64(limit_bytes, "limit_bytes")?;

    let row = sqlx::query(
        "INSERT INTO user_app_quotas (tenant_id, user_id, app_id, limit_bytes)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, user_id, app_id) DO UPDATE SET
            limit_bytes = EXCLUDED.limit_bytes,
            updated_at = now()
         RETURNING tenant_id, user_id, app_id, limit_bytes, updated_at::text AS updated_at",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&app_id)
    .bind(limit)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(SetUserAppQuotaResult {
        quota: Some(user_app_quota_from_row(&row)?),
    })
}

pub(crate) fn user_app_quota_from_row(row: &sqlx::postgres::PgRow) -> Result<UserAppQuota> {
    Ok(UserAppQuota {
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        app_id: row.try_get("app_id")?,
        limit_bytes: i64_to_u64(row.try_get("limit_bytes")?, "limit_bytes")?,
        updated_at: row.try_get("updated_at")?,
        // The write path (set_user_app_quota's RETURNING) omits this column: enforcement has
        // not run at that instant, and the suspended state is only accurate a round later. The
        // UI uses the write's return value solely to confirm the write, and refetches the list
        // as usual.
        suspended_ingresses: row.try_get("suspended_ingresses").unwrap_or_default(),
    })
}
