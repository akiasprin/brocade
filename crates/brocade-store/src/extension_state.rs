//! Read-only guards remain available when the CVM business module is not compiled.
use crate::{AdminContext, PgStore, Result, StoreError};
use sqlx::{Postgres, Transaction};

pub(crate) async fn ensure_node_released(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<()> {
    let occupied: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cvm_instances WHERE (reserved OR status<>'applied') AND node_id=$1)
         OR EXISTS(SELECT 1 FROM cvm_interfaces f JOIN cvm_instances i ON i.id=f.instance_id WHERE NOT f.detached_confirmed AND i.node_id=$1)
         OR EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE NOT released AND (provider_node=$1 OR consumer_node=$1))
         OR EXISTS(SELECT 1 FROM cvm_user_host_network_quotas WHERE held_states > 0 AND node_id=$1)
         OR EXISTS(SELECT 1 FROM cvm_volumes v JOIN cvm_instances i ON i.id=v.instance_id WHERE v.state<>'released' AND i.node_id=$1)
         OR EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE NOT cleaned AND (provider_node=$1 OR consumer_node=$1))
         OR EXISTS(SELECT 1 FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE NOT e.cleaned AND (e.provider_node=$1 OR i.node_id=$1))
         OR EXISTS(SELECT 1 FROM cvm_nic_resources r WHERE NOT r.released AND (
             r.allocation_id IN (SELECT id FROM cvm_nic_allocations WHERE provider_node=$1 OR consumer_node=$1)
             OR r.entry_id IN (SELECT e.id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE e.provider_node=$1 OR i.node_id=$1)
             OR r.tunnel_id IN (SELECT id FROM cvm_entry_tunnels WHERE entry_node=$1 OR target_node=$1)))
         OR EXISTS(SELECT 1 FROM cvm_entry_tunnels WHERE NOT cleaned AND (entry_node=$1 OR target_node=$1))
         OR EXISTS(SELECT 1 FROM cvm_nic_publications WHERE NOT deleted AND provider_node=$1)
         OR EXISTS(SELECT 1 FROM cvm_entry_publications WHERE NOT deleted AND provider_node=$1)",
    )
    .bind(node)
    .fetch_one(&mut **tx)
    .await?;
    ensure_no_cvm_release(tx, Some(node)).await?;
    if occupied {
        return Err(StoreError::Conflict(
            "机器仍有容器网络或访问入口，请使用容器版完成清理".into(),
        ));
    }
    Ok(())
}

impl PgStore {
    /// Downgrade proof must come from the authority that owns the resource ledger.
    pub async fn ensure_node_cvm_released(&self, node: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        ensure_node_released(&mut tx, node).await?;
        tx.commit().await?;
        Ok(())
    }

    /// A base Console must not abandon resources still requiring CVM convergence.
    pub async fn ensure_cvm_released(&self) -> Result<()> {
        let present: bool =
            sqlx::query_scalar("SELECT to_regclass('public.cvm_instances') IS NOT NULL")
                .fetch_one(&self.pool)
                .await?;
        if !present {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        let occupied: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cvm_instances WHERE reserved OR status<>'applied')
             OR EXISTS(SELECT 1 FROM cvm_volumes WHERE state <> 'released')
             OR EXISTS(SELECT 1 FROM cvm_interfaces WHERE NOT detached_confirmed)
             OR EXISTS(SELECT 1 FROM cvm_nic_resources WHERE NOT released)
             OR EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE NOT released)
             OR EXISTS(SELECT 1 FROM cvm_nic_bindings WHERE NOT cleaned)
             OR EXISTS(SELECT 1 FROM cvm_entries WHERE NOT cleaned)
             OR EXISTS(SELECT 1 FROM cvm_entry_tunnels WHERE NOT cleaned)
             OR EXISTS(SELECT 1 FROM cvm_nic_publications WHERE NOT deleted)
             OR EXISTS(SELECT 1 FROM cvm_entry_publications WHERE NOT deleted)
             OR EXISTS(SELECT 1 FROM cvm_user_host_network_quotas WHERE held_states > 0)
             OR EXISTS(SELECT 1 FROM binary_releases WHERE active AND component='agent' AND artifacts->>'agent_flavor'='cvm')",
        )
        .fetch_one(&mut *tx)
        .await?;
        ensure_no_cvm_release(&mut tx, None).await?;
        if occupied {
            return Err(StoreError::Conflict(
                "CVM 资源仍在使用或等待清理，不能切换为基础版".into(),
            ));
        }
        tx.commit().await?;
        Ok(())
    }
}

async fn ensure_no_cvm_release(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
) -> Result<()> {
    // Preflight also runs against published schema 0007 before migration 0008 is applied.
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('public.cvm_revision_nodes') IS NOT NULL")
            .fetch_one(&mut **tx)
            .await?;
    if !present {
        return Ok(());
    }
    let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deployments d JOIN deployment_targets t ON t.deployment_id=d.id JOIN deployment_target_state s ON s.deployment_id=t.deployment_id AND s.node_id=t.node_id WHERE d.active=TRUE AND s.cvm_revision_id IS NOT NULL AND ($1::text IS NULL OR t.node_id=$1) AND t.status IN ('pending','dispatched','converging','failed-recovered','failed-dirty'))").bind(node).fetch_one(&mut **tx).await?;
    let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_revision_nodes r LEFT JOIN cvm_serving_nodes s ON s.node_id=r.node_id WHERE ($1::text IS NULL OR r.node_id=$1) AND r.revision_id>COALESCE(s.revision_id,0))").bind(node).fetch_one(&mut **tx).await?;
    if busy || pending {
        return Err(StoreError::Conflict(
            "容器配置尚未发布或发布尚未结束，不能切换为基础版".into(),
        ));
    }
    Ok(())
}

/// Model-only rollback cannot rewind an independent resource ledger or its serving pointer.
pub(crate) async fn ensure_model_rollback_safe(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    revision: u64,
) -> Result<()> {
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('public.cvm_revision_nodes') IS NOT NULL")
            .fetch_one(&mut **tx)
            .await?;
    if !present {
        return Ok(());
    }
    let incompatible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deployment_target_state WHERE deployment_id=$1 AND cvm_revision_id IS NOT NULL) OR EXISTS(SELECT 1 FROM cvm_revision_nodes WHERE revision_id>$2)")
        .bind(deployment).bind(i64::try_from(revision).map_err(|_| StoreError::InvalidData("revision exceeds i64".into()))?).fetch_one(&mut **tx).await?;
    if incompatible {
        return Err(StoreError::Unsupported("混合发布包含容器或网络资源，不能只回滚机器模型；请单独取消发布，并通过容器配置创建新的补偿修订后发布".into()));
    }
    Ok(())
}

// Historical mixed releases remain scoped even in a base Console.
pub(crate) fn present_cvm_artifacts(
    actor: &AdminContext,
    targets: &mut [brocade_deployment::protocol::DeploymentTargetDetail],
) {
    for target in targets {
        let Some(instances) = target
            .desired_structure
            .get_mut("cvm")
            .and_then(|c| c.get_mut("instances"))
            .and_then(|v| v.as_array_mut())
        else {
            continue;
        };
        instances.retain(|instance| {
            actor.is_system_admin()
                || match (instance["tenant_id"].as_str(), instance["user_id"].as_str()) {
                    (Some(t), Some(u)) => {
                        if actor.role() == crate::AdminRole::User {
                            actor
                                .self_user()
                                .is_some_and(|me| me.tenant_id == t && me.user_id == u)
                        } else {
                            actor.is_global_scope() || actor.can_access_tenant(t)
                        }
                    }
                    _ => false,
                }
        });
        let visible: std::collections::BTreeSet<String> = instances
            .iter()
            .filter_map(|i| i["id"].as_str().map(str::to_owned))
            .collect();
        if let Some(cvm) = target.desired_structure.get_mut("cvm") {
            if let Some(clients) = cvm.get_mut("chain_clients").and_then(|v| v.as_array_mut()) {
                clients.retain(|client| {
                    actor.is_system_admin()
                        || match (client["tenant_id"].as_str(), client["user_id"].as_str()) {
                            (Some(t), Some(u)) if actor.role() == crate::AdminRole::User => actor
                                .self_user()
                                .is_some_and(|me| me.tenant_id == t && me.user_id == u),
                            (Some(t), Some(_)) => {
                                actor.is_global_scope() || actor.can_access_tenant(t)
                            }
                            _ => false,
                        }
                });
            }
            // Host tables span owners. Instance owners retain their own four-file products.
            if !actor.is_system_admin() {
                cvm.as_object_mut().map(|o| o.remove("host_products"));
            }
        }
        if let Some(receipts) = target
            .observed_after
            .as_mut()
            .and_then(|v| v.get_mut("cvm"))
            .and_then(|v| v.get_mut("instances"))
            .and_then(|v| v.as_array_mut())
        {
            receipts.retain(|r| r["id"].as_str().is_some_and(|id| visible.contains(id)));
        }
    }
}
