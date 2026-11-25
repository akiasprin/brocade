//! Withdraw an instance’s unpublished changes against its approved configuration.
use super::*;
use std::collections::BTreeSet;

impl PgStore {
    pub(super) async fn undo_pending_cvm(
        &self,
        mut tx: Transaction<'_, Postgres>,
        actor: &AdminContext,
        id: &str,
        request: CvmCommand,
        digest: String,
        mut before: releases::Before,
    ) -> Result<CvmMutationResult> {
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut current = from_row(&row)?;
        annotate_pending_entries(&mut tx, std::slice::from_mut(&mut current)).await?;
        require_manager(actor, &current.tenant_id)?;
        if current.operation_kind.as_deref() == Some("assign") {
            return Err(StoreError::Conflict(
                "使用授权不能通过撤回配置恢复，请发布当前变更后另行授权".into(),
            ));
        }
        if current.desired.generation != request.expected_generation {
            return Err(StoreError::Conflict("容器变更已更新，请刷新后重试".into()));
        }
        let Some(revision) = current
            .revision_id
            .filter(|r| current.reserved && *r > current.published_revision_id.unwrap_or(0))
        else {
            return Err(StoreError::Conflict("此容器没有可撤回的未发布变更".into()));
        };
        if let Some(order) = current.blocking_deployment_id {
            return Err(StoreError::Conflict(format!(
                "请先停止发布单 #{order}，然后再次核对哪些容器变更仍可撤回"
            )));
        }
        if current.operation_kind.as_deref() == Some("undo_pending") {
            return Err(StoreError::Conflict(
                "恢复修订已保存，请发布后继续操作".into(),
            ));
        }
        if !matches!(request.change, CvmChange::UndoPending { expected_revision } if expected_revision==revision)
        {
            return Err(StoreError::Conflict(
                "待发布变更已更新，请刷新后再撤回".into(),
            ));
        }
        let revision = revision as i64;
        // The network peer may have started even when the compute host is still waiting.
        let exposed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cvm_revision_nodes r
              JOIN cvm_serving_nodes s ON s.node_id=r.node_id
              WHERE r.revision_id>$2 AND r.revision_id<=$1
                AND r.node_id=ANY($3) AND s.revision_id>=r.revision_id)",
        )
        .bind(revision)
        .bind(current.published_revision_id.unwrap_or(0) as i64)
        .bind(before.keys().cloned().collect::<Vec<_>>())
        .fetch_one(&mut *tx)
        .await?;
        if exposed {
            return Err(StoreError::Conflict(
                "关联机器已收到本次配置，请检查发布回执后恢复".into(),
            ));
        }
        let frozen = before
            .get(&current.node_id)
            .cloned()
            .ok_or_else(|| invalid("容器当前配置缺失"))?;
        let previous = releases::serving_tx(&mut tx, &current.node_id).await?;
        // A repoint can affect the previous host, the new host and an entry
        // provider. Capture every participant before restoring the shared ledger.
        let mut nodes: Vec<String> = sqlx::query_scalar(
            "SELECT node_id FROM cvm_revision_nodes WHERE revision_id=$1 ORDER BY node_id",
        )
        .bind(revision)
        .fetch_all(&mut *tx)
        .await?;
        nodes.extend(before.keys().cloned());
        nodes.sort();
        nodes.dedup();
        let extra = releases::begin(
            &mut tx,
            releases::Scope {
                nodes: nodes.iter().map(String::as_str).collect(),
                ..Default::default()
            },
        )
        .await?;
        for (node, config) in extra {
            before.entry(node).or_insert(config);
        }
        let mut entry_before = previous.clone().unwrap_or_else(|| frozen.clone());
        entry_before.entry_instances.clear();
        entry_before.entry.tunnels.clear();
        let mut entry_after = frozen.clone();
        entry_after.entry_instances.clear();
        entry_after.entry.tunnels.clear();
        for node in &nodes {
            if let Some(prior) = releases::serving_tx(&mut tx, node).await? {
                entry_before.entry_instances.extend(prior.entry_instances);
                entry_before.entry.tunnels.extend(prior.entry.tunnels);
            }
            let staged = before
                .get(node)
                .ok_or_else(|| invalid("关联机器配置缺失"))?;
            entry_after
                .entry_instances
                .extend(staged.entry_instances.clone());
            entry_after
                .entry
                .tunnels
                .extend(staged.entry.tunnels.clone());
        }
        let prior = previous
            .as_ref()
            .and_then(|config| config.instances.iter().find(|item| item.id == id));
        let mut restored = prior.cloned().unwrap_or_else(|| current.desired.clone());
        if let Some(prior) = prior {
            ensure_access_generation(&mut tx, id, prior.generation).await?;
            if prior.power == PowerState::Deleted {
                return Err(StoreError::Conflict(
                    "系统盘或实例已进入删除流程，请检查发布回执".into(),
                ));
            }
        }
        let links_before = previous
            .as_ref()
            .map(|c| c.nic.bindings.as_slice())
            .unwrap_or_default();
        nics::bindings::undo_instance_bindings(&mut tx, id, links_before, &frozen.nic.bindings)
            .await?;
        nics::entries::undo_instance_release(&mut tx, id, &entry_before, &entry_after).await?;
        restored.generation = current
            .desired
            .generation
            .checked_add(1)
            .filter(|generation| *generation <= i64::MAX as u64)
            .ok_or_else(|| invalid("容器版本已满"))?;
        restored.operation_id = new_id("op")?;
        restored.artifacts = None;
        if restored.spec.routing != current.desired.spec.routing {
            restored.spec.routing.revision = current.desired.spec.routing.revision;
            // The route journal can contain an earlier route version hidden by an unpublished
            // operation. Allocate above both cursors rather than reusing a version number.
            let version: Option<i64> = sqlx::query_scalar(
                "SELECT max(revision) FROM cvm_nic_route_versions WHERE instance_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            restored.spec.routing.revision = restored
                .spec
                .routing
                .revision
                .max(version.unwrap_or(0) as u64)
                .checked_add(1)
                .ok_or_else(|| invalid("路由版本已满"))?;
            nics::persist_routes(&mut tx, actor, &restored).await?;
        }
        let keep_interfaces: Vec<&str> = restored
            .spec
            .interfaces
            .iter()
            .map(|nic| nic.id.as_str())
            .collect();
        if prior.is_none() {
            cancel_unmaterialized_instance(&mut tx, id).await?;
            restored.power = PowerState::Deleted;
            restored.delete_data = false;
            restored.detached_interfaces.clear();
        } else {
            let staged = frozen
                .instances
                .iter()
                .find(|item| item.id == id)
                .ok_or_else(|| invalid("待发布容器规格缺失"))?;
            let staged_interfaces: BTreeSet<&str> = staged
                .spec
                .interfaces
                .iter()
                .map(|nic| nic.id.as_str())
                .collect();
            let restored_interfaces: BTreeSet<&str> = keep_interfaces.iter().copied().collect();
            for interface in &restored_interfaces {
                let changed = sqlx::query("UPDATE cvm_interfaces SET desired_attached=true WHERE id=$1 AND instance_id=$2 AND NOT detached_confirmed")
                    .bind(interface).bind(id).execute(&mut *tx).await?.rows_affected();
                if changed != 1 {
                    return Err(StoreError::Conflict(
                        "原网卡已释放，无法撤回这次卸载".into(),
                    ));
                }
            }
            for interface in staged_interfaces.difference(&restored_interfaces) {
                sqlx::query("UPDATE cvm_interfaces SET desired_attached=false,detached_confirmed=true WHERE id=$1 AND instance_id=$2")
                    .bind(interface).bind(id).execute(&mut *tx).await?;
            }
        }
        // Freeze the same restored routes into both host firewalls and Guest artifacts.
        // Restoring only the Guest leaves the staged host deny rules in effect.
        nics::bindings::sync_routes(&mut tx, &restored).await?;
        restored.spec_sha256 = spec_digest(&restored.spec)?;
        save_version(
            &mut tx,
            actor,
            &request.request_id,
            &digest,
            "undo_pending",
            &restored,
        )
        .await?;
        if prior.is_none() {
            sqlx::query("UPDATE cvm_instances SET status='applied',reserved=false WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE cvm_operations SET status='applied' WHERE id=$1")
                .bind(&restored.operation_id)
                .execute(&mut *tx)
                .await?;
        }
        releases::commit(&mut tx, actor, before).await?;
        if prior.is_none() {
            sqlx::query("INSERT INTO cvm_revision_instances(instance_id,generation,revision_id)
                SELECT $1,$2,current_revision FROM control_state WHERE id=TRUE ON CONFLICT DO NOTHING")
                .bind(id).bind(restored.generation as i64).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(CvmMutationResult::existing(
            self.cvm_instance(actor, id).await?,
        ))
    }
}

async fn cancel_unmaterialized_instance(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<()> {
    let dependencies: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cvm_entries WHERE instance_id=$1 AND NOT cleaned)
             OR EXISTS(SELECT 1 FROM cvm_nic_allocations WHERE instance_id=$1 AND NOT released)
             OR EXISTS(SELECT 1 FROM cvm_instances WHERE id=$1 AND report IS NOT NULL)
             OR EXISTS(SELECT 1 FROM cvm_volumes WHERE instance_id=$1 AND state<>'reserved')",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if dependencies {
        return Err(StoreError::Conflict(
            "请先撤回关联入口或网卡变更，再撤回容器创建".into(),
        ));
    }
    sqlx::query("UPDATE cvm_volumes SET state='released' WHERE instance_id=$1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE cvm_interfaces SET desired_attached=false,detached_confirmed=true WHERE instance_id=$1")
        .bind(id).execute(&mut **tx).await?;
    Ok(())
}
