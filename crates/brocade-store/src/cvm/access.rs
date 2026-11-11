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
    item.desired.spec.ssh_public_key.clear();
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
        report.runtime_id = None;
        report.volume_id = None;
        report.access_address = None;
        for nic in &mut report.interfaces {
            nic.addresses.clear();
            nic.mac.clear();
        }
    }
}
