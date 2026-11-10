//! Authorization, revocation and incarnation checks for persistent Guest terminals.
use super::*;
use brocade_deployment::cvm::terminal::Target;

#[derive(Debug, Serialize)]
pub struct TerminalRuntimePolicy {
    pub enabled: bool,
    pub revision: u64,
    pub revoked_through: u64,
}

impl PgStore {
    pub async fn cvm_terminal_policy(&self, node: &str) -> Result<TerminalRuntimePolicy> {
        let (enabled, revision, revoked): (bool, i64, i64) = sqlx::query_as(
            "SELECT coalesce((SELECT enabled FROM cvm_terminal_policies WHERE node_id=$1),false), coalesce((SELECT revision FROM cvm_terminal_policies WHERE node_id=$1),0), coalesce((SELECT max(revision) FROM cvm_terminal_policy_events WHERE node_id=$1 AND NOT enabled),0)")
            .bind(node).fetch_one(&self.pool).await?;
        Ok(TerminalRuntimePolicy {
            enabled,
            revision: revision
                .try_into()
                .map_err(|_| StoreError::InvalidData("invalid terminal policy revision".into()))?,
            revoked_through: revoked.try_into().map_err(|_| {
                StoreError::InvalidData("invalid terminal revocation revision".into())
            })?,
        })
    }
    pub async fn cvm_terminal_target(
        &self,
        actor: &AdminContext,
        id: &str,
    ) -> Result<(String, Target)> {
        let instance = self.cvm_instance(actor, id).await?;
        require_owner(actor, &instance.tenant_id, &instance.user_id, true)?;
        let allowed: bool = sqlx::query_scalar(
            "SELECT coalesce((SELECT enabled FROM cvm_terminal_policies WHERE node_id=$1),false)",
        )
        .bind(&instance.node_id)
        .fetch_one(&self.pool)
        .await?;
        if !allowed {
            return Err(StoreError::Forbidden("该机器的 WebShell 已关闭".into()));
        }
        let desired = &instance.desired;
        let report = instance
            .report
            .as_ref()
            .ok_or_else(|| StoreError::Unavailable("尚未确认实例运行时，无法连接终端".into()))?;
        if !instance.reserved
            || desired.power != PowerState::Running
            || report.power != Some(PowerState::Running)
            || report.restart_epoch != desired.restart_epoch
            || report.reinstall_epoch != desired.reinstall_epoch
        {
            return Err(StoreError::Conflict(
                "实例正在启停或重装，终端已关闭".into(),
            ));
        }
        let runtime_id = report
            .runtime_id
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| StoreError::Unavailable("实例运行时不可用".into()))?;
        let enabled: bool = sqlx::query_scalar(
            "SELECT coalesce(capabilities->>'guest_terminal'='true' AND capabilities->>'guest_terminal_persistent'='true',false) FROM cvm_hosts WHERE node_id=$1",
        ).bind(&instance.node_id).fetch_optional(&self.pool).await?.unwrap_or(false);
        if !enabled {
            return Err(StoreError::Unsupported(
                "承载机器尚未启用 Guest 终端".into(),
            ));
        }
        Ok((
            instance.node_id,
            Target {
                instance_id: id.to_owned(),
                runtime_id,
                restart_epoch: desired.restart_epoch,
                reinstall_epoch: desired.reinstall_epoch,
            },
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalPolicyUpdate {
    pub enabled: bool,
    pub expected_revision: u64,
}
impl PgStore {
    pub async fn set_cvm_terminal_policy(
        &self,
        actor: &AdminContext,
        node: &str,
        request: TerminalPolicyUpdate,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let tenant:String=sqlx::query_scalar("SELECT n.tenant_id FROM nodes n JOIN cvm_hosts h ON h.node_id=n.id WHERE n.id=$1 AND n.retired_at IS NULL FOR SHARE OF n").bind(node).fetch_optional(&mut *tx).await?.ok_or_else(||StoreError::NotFound("CVM 承载机器".into()))?;
        if !matches!(
            actor.role(),
            AdminRole::SystemAdmin | AdminRole::TenantAdmin
        ) || !(actor.is_global_scope() || actor.can_access_tenant(&tenant))
        {
            return Err(StoreError::Forbidden("无权管理该机器的 WebShell".into()));
        }
        sqlx::query("INSERT INTO cvm_terminal_policies(node_id,actor_id) VALUES($1,$2) ON CONFLICT(node_id) DO NOTHING").bind(node).bind(actor.operator_id()).execute(&mut *tx).await?;
        let (current, revision): (bool, i64) = sqlx::query_as(
            "SELECT enabled,revision FROM cvm_terminal_policies WHERE node_id=$1 FOR UPDATE",
        )
        .bind(node)
        .fetch_one(&mut *tx)
        .await?;
        if revision as u64 != request.expected_revision {
            return Err(StoreError::Conflict(
                "WebShell 设置已被其他操作更新，请刷新后重试".into(),
            ));
        }
        if current != request.enabled {
            let next = revision
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidData("WebShell 策略版本已达到上限".into()))?;
            sqlx::query("UPDATE cvm_terminal_policies SET enabled=$2,revision=$3,actor_id=$4,updated_at=now() WHERE node_id=$1").bind(node).bind(request.enabled).bind(next).bind(actor.operator_id()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO cvm_terminal_policy_events(node_id,revision,enabled,actor_id) VALUES($1,$2,$3,$4)").bind(node).bind(next).bind(request.enabled).bind(actor.operator_id()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
