use super::*;

pub(super) fn state_text(state: ApplyStatus) -> &'static str {
    match state {
        ApplyStatus::Pending => "pending",
        ApplyStatus::Applied => "applied",
        ApplyStatus::Failed => "failed",
        ApplyStatus::Unknown => "unknown",
    }
}

pub(super) fn from_row(row: &sqlx::postgres::PgRow) -> Result<CvmInstance> {
    Ok(CvmInstance {
        revision_id: row
            .try_get::<Option<i64>, _>("revision_id")?
            .map(|r| r as u64),
        published_revision_id: row
            .try_get::<Option<i64>, _>("published_revision_id")?
            .map(|r| r as u64),
        blocking_deployment_id: row.try_get("blocking_deployment_id")?,
        operation_kind: row.try_get("operation_kind")?,
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        node_id: row.try_get("node_id")?,
        desired: serde_json::from_value(row.try_get("desired")?)?,
        status: serde_json::from_value(serde_json::Value::String(row.try_get("status")?))?,
        report: row
            .try_get::<Option<serde_json::Value>, _>("report")?
            .map(serde_json::from_value)
            .transpose()?,
        updated_at: row.try_get("updated")?,
        created_at: row.try_get("created")?,
        reserved: row.try_get("reserved")?,
    })
}

// History is fetched for one authorized detail, never multiplied across the list.
pub(super) const INSTANCE_SELECT: &str = "SELECT id,tenant_id,user_id,node_id,desired,status,reserved,report,
    (SELECT r.revision_id FROM cvm_revision_instances r WHERE r.instance_id=cvm_instances.id AND r.generation<=cvm_instances.generation ORDER BY r.generation DESC LIMIT 1) AS revision_id,
    (SELECT s.revision_id FROM cvm_serving_nodes s WHERE s.node_id=cvm_instances.node_id) AS published_revision_id,
    (SELECT o.kind FROM cvm_operations o WHERE o.instance_id=cvm_instances.id AND o.generation=cvm_instances.generation) AS operation_kind,
    (SELECT d.id FROM deployments d JOIN deployment_target_state s ON s.deployment_id=d.id
      JOIN cvm_revision_nodes n ON n.node_id=s.node_id AND n.revision_id=(
        SELECT r.revision_id FROM cvm_revision_instances r WHERE r.instance_id=cvm_instances.id AND r.generation<=cvm_instances.generation ORDER BY r.generation DESC LIMIT 1
      )
      JOIN deployment_targets t ON t.deployment_id=d.id AND t.node_id=s.node_id
      WHERE d.active AND s.cvm_revision_id>=n.revision_id
        AND t.status IN ('pending','dispatched','converging','failed-recovered','failed-dirty')
      ORDER BY d.id DESC LIMIT 1) AS blocking_deployment_id,
    to_char(updated_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated,
    to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created FROM cvm_instances";

impl PgStore {
    pub async fn cvm_workspace(&self, actor: &AdminContext) -> Result<CvmWorkspace> {
        // Scope before applying bounds: another tenant cannot consume this user's page.
        let (tenant, pattern, user) = read_scope(actor)?;
        let mut instances = Vec::new();
        for row in sqlx::query(&format!(
            "{INSTANCE_SELECT} WHERE {READ_SCOPE} ORDER BY reserved DESC,created_at DESC,id LIMIT 512"
        ))
        .bind(&tenant)
        .bind(&pattern)
        .bind(&user)
        .fetch_all(&self.pool)
        .await?
        {
            let mut item = from_row(&row)?;
            redact(actor, &mut item);
            instances.push(item);
        }
        let mut tx = self.pool.begin().await?;
        annotate_pending_entries(&mut tx, &mut instances).await?;
        tx.commit().await?;
        let mut hosts = Vec::new();
        let mut available_hosts = Vec::new();
        for row in sqlx::query("SELECT n.id,n.name,n.tenant_id,h.capabilities,h.error,coalesce(hp.enabled,true) AS host_enabled,coalesce(hp.revision,0) AS host_revision,coalesce(p.enabled,false) AS terminal_enabled,coalesce(p.revision,0) AS terminal_revision,h.observed_at>now()-interval '150 seconds' AND h.error IS NULL AS fresh,to_char(h.observed_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS observed FROM cvm_hosts h JOIN nodes n ON n.id=h.node_id LEFT JOIN cvm_terminal_policies p ON p.node_id=n.id LEFT JOIN cvm_host_policies hp ON hp.node_id=n.id WHERE n.retired_at IS NULL ORDER BY n.name,n.id").fetch_all(&self.pool).await? {
            let node:String=row.try_get("id")?;
            let tenant:String=row.try_get("tenant_id")?;
            let granted=instances.iter().any(|i| i.node_id == node);
            let in_scope=actor.role()!=AdminRole::User && (actor.is_global_scope() || actor.can_access_tenant(&tenant));
            if !granted && !in_scope { continue; }
            let can_manage_host = actor.is_system_admin() && in_scope;
            let can_manage_terminal = in_scope && matches!(actor.role(), AdminRole::SystemAdmin | AdminRole::TenantAdmin);
            let fresh: bool = row.try_get("fresh")?;
            let inventory: serde_json::Value = row.try_get("capabilities")?;
            let terminal_sessions = if can_manage_terminal && fresh {
                inventory[node_state::TERMINAL_SESSIONS_USED].as_u64().map(|used| CvmTerminalSessions {
                    used,
                    limit: brocade_deployment::cvm::terminal::MAX_PER_NODE,
                })
            } else {
                None
            };
            let host = CvmHost {
                host_revision: row.try_get::<i64,_>("host_revision")? as u64,
                can_manage_host,
                terminal_enabled: row.try_get("terminal_enabled")?,
                terminal_revision: row.try_get::<i64,_>("terminal_revision")? as u64,
                can_manage_terminal,
                terminal_sessions,
                node_id: node,
                name: row.try_get("name")?,
                fresh,
                observed_at: row.try_get("observed")?,
                capabilities: serde_json::from_value(inventory)?,
                error: row.try_get("error")?,
            };
            if row.try_get::<bool,_>("host_enabled")? {
                hosts.push(host);
            } else if can_manage_host {
                available_hosts.push(host);
            }
        }
        let node_ids = hosts.iter().map(|h| h.node_id.clone()).collect::<Vec<_>>();
        let mut owners = Vec::new();
        for row in sqlx::query("SELECT tenant_id,id,id AS name FROM users WHERE ($1::text IS NULL OR tenant_id=$1 OR ($3::text IS NULL AND tenant_id LIKE $2 ESCAPE E'\\\\')) AND ($3::text IS NULL OR id=$3) ORDER BY tenant_id,id LIMIT 1024")
            .bind(&tenant).bind(&pattern).bind(&user).fetch_all(&self.pool).await? {
            owners.push(CvmOwner { tenant_id: row.try_get("tenant_id")?, user_id: row.try_get("id")?, name: row.try_get("name")? });
        }
        // Report durable resource usage, including retained volumes and
        // interfaces still awaiting cleanup after compute has been released.
        let usage = sqlx::query(&format!(
            "WITH owners AS (SELECT DISTINCT tenant_id,user_id FROM cvm_instances WHERE user_id IS NOT NULL AND {READ_SCOPE}
             ORDER BY tenant_id,user_id LIMIT 1024), held AS ({COMPUTE_RESERVATIONS})
             SELECT q.tenant_id,q.user_id,
               (SELECT count(*) FROM cvm_instances i WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND i.reserved) AS instances,
               (SELECT coalesce(sum(h.vcpus),0)::bigint FROM held h WHERE h.tenant_id=q.tenant_id AND h.user_id=q.user_id) AS vcpus,
               (SELECT coalesce(sum(h.memory),0)::bigint FROM held h WHERE h.tenant_id=q.tenant_id AND h.user_id=q.user_id) AS memory,
               ((SELECT coalesce(sum(bytes),0)::bigint FROM cvm_volumes v WHERE v.tenant_id=q.tenant_id AND v.user_id=q.user_id AND v.state<>'released') + (SELECT coalesce(sum((desired#>>'{{spec,resources,root_disk_bytes}}')::bigint),0)::bigint FROM cvm_instances i WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND i.reserved AND desired#>>'{{spec,execution,kind}}'='baremetal')) AS disk,
               (SELECT count(*) FROM cvm_interfaces nic JOIN cvm_instances i ON i.id=nic.instance_id WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND (NOT nic.detached_confirmed OR EXISTS(SELECT 1 FROM cvm_nic_allocations a WHERE a.interface_id=nic.id AND NOT a.released AND NOT a.retired))) AS interfaces
             FROM owners q"
        ))
        .bind(&tenant).bind(&pattern).bind(&user).fetch_all(&self.pool).await?
        .into_iter().map(|row| Ok(CvmResourceUsage {
            tenant_id: row.try_get("tenant_id")?, user_id: row.try_get("user_id")?,
            instances: row.try_get::<i64, _>("instances")? as u64,
            vcpus: row.try_get::<i64, _>("vcpus")? as u64,
            memory_bytes: row.try_get::<i64, _>("memory")? as u64,
            disk_bytes: row.try_get::<i64, _>("disk")? as u64,
            interfaces: row.try_get::<i64, _>("interfaces")? as u64,
        })).collect::<Result<Vec<_>>>()?;
        let pending_revision:Option<i64>=sqlx::query_scalar("SELECT max(r.revision_id) FROM cvm_revision_nodes r LEFT JOIN cvm_serving_nodes s ON s.node_id=r.node_id WHERE r.node_id=ANY($1) AND (s.revision_id IS NULL OR r.revision_id>s.revision_id)").bind(&node_ids).fetch_one(&self.pool).await?;
        let tenants=sqlx::query("SELECT id,name FROM tenants WHERE ($1::text IS NULL OR id=$1 OR id LIKE $2 ESCAPE E'\\\\') ORDER BY id LIMIT 1024")
            .bind(&tenant).bind(&pattern).fetch_all(&self.pool).await?.into_iter()
            .map(|row| Ok(CvmTenant {tenant_id:row.try_get("id")?,name:row.try_get("name")?})).collect::<Result<Vec<_>>>()?;
        Ok(CvmWorkspace {
            usage,
            tenants,
            can_publish: matches!(
                actor.role(),
                AdminRole::Publisher | AdminRole::TenantAdmin | AdminRole::SystemAdmin
            ),
            pending_revision_id: pending_revision.map(|r| r as u64),
            available_hosts,
            can_manage_network: actor.is_system_admin(),
            hosts,
            instances,
            owners,
            can_use: matches!(
                actor.role(),
                AdminRole::User | AdminRole::TenantAdmin | AdminRole::SystemAdmin
            ),
            can_write: matches!(
                actor.role(),
                AdminRole::TenantAdmin | AdminRole::SystemAdmin
            ),
            redacted: actor.role() == AdminRole::Readonly,
        })
    }
}

impl PgStore {
    pub async fn cvm_instance(&self, actor: &AdminContext, id: &str) -> Result<CvmInstance> {
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
        let mut value = from_row(&row)?;
        require_owner(actor, &value.tenant_id, value.user(), false)?;
        let mut tx = self.pool.begin().await?;
        annotate_pending_entries(&mut tx, std::slice::from_mut(&mut value)).await?;
        tx.commit().await?;
        redact(actor, &mut value);
        Ok(value)
    }
}

/// A manager-facing summary of the frozen change, containing no runtime or credential data.
#[derive(Serialize)]
pub struct CvmPendingReview {
    revision: u64,
    author: Option<String>,
    saved_at: String,
    fields: Vec<&'static str>,
    entries_added: usize,
    entries_removed: usize,
    nics_added: usize,
    nics_removed: usize,
}

impl PgStore {
    pub async fn cvm_pending_review(
        &self,
        actor: &AdminContext,
        id: &str,
    ) -> Result<CvmPendingReview> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1"))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
        let mut item = from_row(&row)?;
        require_manager(actor, &item.tenant_id)?;
        annotate_pending_entries(&mut tx, std::slice::from_mut(&mut item)).await?;
        let revision = item
            .revision_id
            .filter(|revision| item.reserved && *revision > item.published_revision_id.unwrap_or(0))
            .ok_or_else(|| StoreError::Conflict("容器没有待发布变更".into()))?;
        let after = releases::frozen_revision(&mut tx, &item.node_id, revision as i64).await?;
        let before = releases::serving_tx(&mut tx, &item.node_id).await?;
        let (author, saved_at) = sqlx::query_as(
            "SELECT author,to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM revisions WHERE id=$1",
        ).bind(revision as i64).fetch_one(&mut *tx).await?;
        tx.commit().await?;

        let old = before
            .as_ref()
            .and_then(|config| config.instances.iter().find(|entry| entry.id == id));
        let current = after
            .instances
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or(&item.desired);
        let mut fields = Vec::new();
        if let Some(old) = old {
            if current.power != old.power {
                fields.push("power");
            }
            if current.restart_epoch != old.restart_epoch {
                fields.push("restart");
            }
            if current.reinstall_epoch != old.reinstall_epoch
                || current.spec.image != old.spec.image
            {
                fields.push("image");
            }
            if current.spec.name != old.spec.name {
                fields.push("name");
            }
            if current.spec.resources != old.spec.resources {
                fields.push("resources");
            }
            if current.spec.interfaces != old.spec.interfaces {
                fields.push("interfaces");
            }
            let routing = &current.spec.routing;
            let old_routing = &old.spec.routing;
            if routing.routes != old_routing.routes || routing.priorities != old_routing.priorities
            {
                fields.push("routes");
            }
            if routing.dns != old_routing.dns {
                fields.push("dns");
            }
            if current.spec.login_revision != old.spec.login_revision {
                fields.push("login");
            }
        } else {
            fields.push("create");
        }
        if item.operation_kind.as_deref() == Some("assign") {
            fields.push("access");
        }
        let old_entries = entry_ids(before.as_ref(), id);
        let new_entries = entry_ids(Some(&after), id);
        let old_nics = nic_ids(old);
        let new_nics = nic_ids(Some(current));
        Ok(CvmPendingReview {
            revision,
            author,
            saved_at,
            fields,
            entries_added: new_entries.difference(&old_entries).count(),
            entries_removed: old_entries.difference(&new_entries).count(),
            nics_added: new_nics.difference(&old_nics).count(),
            nics_removed: old_nics.difference(&new_nics).count(),
        })
    }
}

fn entry_ids<'a>(
    config: Option<&'a releases::NodeConfig>,
    id: &str,
) -> std::collections::BTreeSet<&'a str> {
    config
        .into_iter()
        .flat_map(|config| &config.entry_instances)
        .filter_map(|(entry, instance)| (instance == id).then_some(entry.as_str()))
        .collect()
}

fn nic_ids(desired: Option<&InstanceDesired>) -> std::collections::BTreeSet<&str> {
    desired
        .into_iter()
        .flat_map(|instance| &instance.spec.interfaces)
        .filter(|interface| interface.allocated.is_some())
        .map(|interface| interface.id.as_str())
        .collect()
}

/// Entry changes do not need a new Guest execution generation. Associate their
/// latest frozen revision with the affected instance so withdrawal stays discoverable.
pub(super) async fn annotate_pending_entries(
    tx: &mut Transaction<'_, Postgres>,
    instances: &mut [CvmInstance],
) -> Result<()> {
    let nodes = instances
        .iter()
        .map(|i| i.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for node in nodes {
        let latest: Option<i64> = sqlx::query_scalar(
            "SELECT max(r.revision_id) FROM cvm_revision_nodes r LEFT JOIN cvm_serving_nodes s ON s.node_id=r.node_id
             WHERE r.node_id=$1 AND r.revision_id>coalesce(s.revision_id,0)",
        ).bind(&node).fetch_one(&mut **tx).await?;
        let Some(revision) = latest else {
            continue;
        };
        let after = releases::frozen_revision(tx, &node, revision).await?;
        let Some(before) = releases::serving_tx(tx, &node).await? else {
            continue;
        };
        for instance in instances
            .iter_mut()
            .filter(|i| i.node_id == node && i.reserved)
        {
            if instance.revision_id.is_some_and(|r| r >= revision as u64) {
                continue;
            }
            let affected = before
                .entry_instances
                .iter()
                .chain(after.entry_instances.iter())
                .any(|(id, owner)| {
                    owner == &instance.desired.id
                        && before.entry_instances.get(id) != after.entry_instances.get(id)
                });
            if affected {
                instance.revision_id = Some(revision as u64);
                instance.operation_kind = Some("entries".into());
                instance.blocking_deployment_id=sqlx::query_scalar(
                    "SELECT d.id FROM deployments d JOIN deployment_target_state s ON s.deployment_id=d.id
                     WHERE d.active AND s.node_id=$1 AND s.cvm_revision_id>=$2 ORDER BY d.id DESC LIMIT 1",
                ).bind(&node).bind(revision).fetch_optional(&mut **tx).await?;
            }
        }
    }
    Ok(())
}
