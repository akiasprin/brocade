//! Read-only guards remain available when the CVM business module is not compiled.
use crate::{PgStore, Result, StoreError};
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
        if occupied {
            return Err(StoreError::Conflict(
                "CVM 资源仍在使用或等待清理，不能切换为基础版".into(),
            ));
        }
        tx.commit().await?;
        Ok(())
    }
}
