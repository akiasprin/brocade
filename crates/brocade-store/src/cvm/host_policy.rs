//! Console owns host admission independently of Agent capability observations.
use super::*;

#[derive(Debug, Clone, Deserialize)]
pub struct HostPolicyUpdate {
    pub enabled: bool,
    pub expected_revision: u64,
}

impl PgStore {
    pub async fn set_cvm_host_policy(
        &self,
        actor: &AdminContext,
        node: &str,
        request: HostPolicyUpdate,
    ) -> Result<()> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "只有系统管理员可以管理容器宿主身份".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        // Same order as creation, inventory and network cleanup. No admission can
        // race between the resource check and the durable removal decision.
        crate::resource_coordination::lock(&mut tx).await?;
        let row = sqlx::query(
            "SELECT n.tenant_id,h.capabilities FROM nodes n JOIN cvm_hosts h ON h.node_id=n.id WHERE n.id=$1 AND n.retired_at IS NULL FOR UPDATE OF n,h",
        )
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("容器宿主".into()))?;
        let tenant: String = row.try_get("tenant_id")?;
        if !(actor.is_global_scope() || actor.can_access_tenant(&tenant)) {
            return Err(StoreError::Forbidden("无权管理该机器的容器宿主身份".into()));
        }
        sqlx::query("INSERT INTO cvm_host_policies(node_id,actor_id) VALUES($1,$2) ON CONFLICT(node_id) DO NOTHING")
            .bind(node).bind(actor.operator_id()).execute(&mut *tx).await?;
        let (enabled, revision): (bool, i64) = sqlx::query_as(
            "SELECT enabled,revision FROM cvm_host_policies WHERE node_id=$1 FOR UPDATE",
        )
        .bind(node)
        .fetch_one(&mut *tx)
        .await?;
        if request.expected_revision != revision as u64 {
            return Err(StoreError::Conflict(
                "宿主身份已被其他操作更新，请刷新后重试".into(),
            ));
        }
        if enabled != request.enabled {
            if request.enabled {
                let caps: Capabilities = serde_json::from_value(row.try_get("capabilities")?)?;
                if caps.backend.is_empty() {
                    return Err(StoreError::Unsupported("该机器尚未上报容器运行栈".into()));
                }
            } else {
                let occupied: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM cvm_instances i WHERE i.node_id=$1 AND
                        (i.reserved OR EXISTS(SELECT 1 FROM cvm_volumes v WHERE v.instance_id=i.id AND v.state<>'released')
                         OR EXISTS(SELECT 1 FROM cvm_interfaces nic WHERE nic.instance_id=i.id AND NOT nic.detached_confirmed)
                         OR EXISTS(SELECT 1 FROM cvm_entries e WHERE e.instance_id=i.id AND NOT e.cleaned)))
                     OR EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE consumer_node=$1 AND NOT released)
                     OR EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE consumer_node=$1 AND NOT cleaned)
                     OR EXISTS(SELECT 1 FROM cvm_entry_tunnels WHERE target_node=$1 AND NOT cleaned)",
                ).bind(node).fetch_one(&mut *tx).await?;
                if occupied {
                    return Err(StoreError::Conflict(
                        "请先删除该宿主上的实例，并等待系统盘和网络资源清理完成".into(),
                    ));
                }
            }
            let next = revision
                .checked_add(1)
                .ok_or_else(|| invalid("宿主身份版本已达到上限"))?;
            sqlx::query("UPDATE cvm_host_policies SET enabled=$2,revision=$3,actor_id=$4,updated_at=now() WHERE node_id=$1")
                .bind(node).bind(request.enabled).bind(next).bind(actor.operator_id()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO cvm_host_policy_events(node_id,revision,enabled,actor_id) VALUES($1,$2,$3,$4)")
                .bind(node).bind(next).bind(request.enabled).bind(actor.operator_id()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
