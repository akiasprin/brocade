use super::*;

pub(super) fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}

pub(super) fn view_allowed(actor: &AdminContext, tenant: &str, user: &str) -> bool {
    if actor.role() == AdminRole::User {
        return actor
            .self_user()
            .is_some_and(|u| u.tenant_id == tenant && u.user_id == user);
    }
    actor.is_global_scope() || actor.can_access_tenant(tenant)
}

pub(super) fn require_owner(
    actor: &AdminContext,
    tenant: &str,
    user: &str,
    write: bool,
) -> Result<()> {
    if !view_allowed(actor, tenant, user)
        || (write
            && !matches!(
                actor.role(),
                AdminRole::User | AdminRole::SystemAdmin | AdminRole::TenantAdmin
            ))
    {
        return Err(StoreError::Forbidden(
            "CVM is outside your authorized scope".into(),
        ));
    }
    Ok(())
}

pub(super) fn require_manager(actor: &AdminContext, tenant: &str) -> Result<()> {
    if !matches!(
        actor.role(),
        AdminRole::SystemAdmin | AdminRole::TenantAdmin
    ) || !(actor.is_global_scope() || actor.can_access_tenant(tenant))
    {
        return Err(StoreError::Forbidden(
            "只有作用域内管理员可以管理容器".into(),
        ));
    }
    Ok(())
}

pub(super) async fn require_host_manager(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    node: &str,
) -> Result<()> {
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM nodes WHERE id=$1 AND retired_at IS NULL")
            .bind(node)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("宿主".into()))?;
    require_manager(actor, &tenant)
}

pub(super) async fn check_assignee(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: Option<&str>,
) -> Result<()> {
    if let Some(user) = user {
        let active: bool = sqlx::query_scalar(
            "SELECT status='active' FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("user".into()))?;
        if !active {
            return Err(StoreError::Forbidden("用户已停用".into()));
        }
    }
    Ok(())
}

pub(super) const READ_SCOPE: &str = "($1::text IS NULL OR tenant_id=$1 OR ($3::text IS NULL AND tenant_id LIKE $2 ESCAPE E'\\\\')) AND ($3::text IS NULL OR user_id=$3)";

pub(super) fn read_scope(
    actor: &AdminContext,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    if actor.role() == AdminRole::User {
        let user = actor
            .self_user()
            .ok_or_else(|| StoreError::Forbidden("CVM user identity is missing".into()))?;
        return Ok((
            Some(user.tenant_id.clone()),
            None,
            Some(user.user_id.clone()),
        ));
    }
    Ok((
        actor.tenant_scope().map(str::to_owned),
        actor.tenant_scope_like_pattern(),
        None,
    ))
}

pub(super) fn redact(actor: &AdminContext, item: &mut CvmInstance) {
    if actor.role() != AdminRole::Readonly {
        return;
    }
    item.desired.artifacts = None;
    item.desired.spec.legacy_ssh_public_key = None;
    for nic in &mut item.desired.spec.interfaces {
        if let Some(allocated) = &mut nic.allocated {
            allocated.mac.clear();
            allocated.ipv4 = None;
            allocated.ipv6 = None;
            allocated.public_ipv4 = None;
            allocated.public_ipv6 = None;
            allocated.allocation_id.clear();
        }
    }
    if let Some(report) = &mut item.report {
        report.artifacts = None;
        report.runtime_id = None;
        report.volume_id = None;
        report.access_address = None;
        for nic in &mut report.interfaces {
            nic.addresses.clear();
            nic.mac.clear();
        }
    }
}

/// Historical configuration cannot restore an earlier user's execution identity.
pub(super) async fn ensure_access_generation(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    generation: u64,
) -> Result<()> {
    let stale: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_instance_access_events WHERE instance_id=$1 AND generation>$2)")
        .bind(id).bind(generation as i64).fetch_one(&mut **tx).await?;
    if stale {
        return Err(StoreError::Conflict(
            "容器使用授权已更新，请先发布当前授权变更；旧执行版本不可恢复访问身份".into(),
        ));
    }
    Ok(())
}
