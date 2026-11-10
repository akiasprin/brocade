use super::*;

pub(super) fn state_text(state: ApplyStatus) -> &'static str {
    match state {
        ApplyStatus::Pending => "pending",
        ApplyStatus::Applied => "applied",
        ApplyStatus::Failed => "failed",
        ApplyStatus::Unknown => "unknown",
        ApplyStatus::RestartRequired => "restart_required",
    }
}

pub(super) fn from_row(row: &sqlx::postgres::PgRow) -> Result<CvmInstance> {
    Ok(CvmInstance {
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
pub(super) const INSTANCE_SELECT: &str="SELECT id,tenant_id,user_id,node_id,desired,status,reserved,report,to_char(updated_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS updated,to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created FROM cvm_instances";

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
        let mut quotas = Vec::new();
        for row in sqlx::query(&format!(
            "SELECT * FROM cvm_quotas WHERE {READ_SCOPE} ORDER BY tenant_id,user_id LIMIT 1024"
        ))
        .bind(&tenant)
        .bind(&pattern)
        .bind(&user)
        .fetch_all(&self.pool)
        .await?
        {
            quotas.push(quota_row(&row)?);
        }
        let mut hosts = Vec::new();
        let mut available_hosts = Vec::new();
        for row in sqlx::query("SELECT n.id,n.name,n.tenant_id,h.capabilities,h.error,coalesce(hp.enabled,true) AS host_enabled,coalesce(hp.revision,0) AS host_revision,coalesce(p.enabled,false) AS terminal_enabled,coalesce(p.revision,0) AS terminal_revision,h.observed_at>now()-interval '150 seconds' AND h.error IS NULL AS fresh,to_char(h.observed_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS observed FROM cvm_hosts h JOIN nodes n ON n.id=h.node_id LEFT JOIN cvm_terminal_policies p ON p.node_id=n.id LEFT JOIN cvm_host_policies hp ON hp.node_id=n.id WHERE n.retired_at IS NULL ORDER BY n.name,n.id").fetch_all(&self.pool).await? {
            let node:String=row.try_get("id")?;
            let tenant:String=row.try_get("tenant_id")?;
            let granted=quotas.iter().any(|q|q.allowed_nodes.contains(&node));
            let in_scope=actor.role()!=AdminRole::User && (actor.is_global_scope() || actor.can_access_tenant(&tenant));
            if !granted && !in_scope { continue; }
            let can_manage_host = actor.is_system_admin() && in_scope;
            let host = CvmHost {host_revision:row.try_get::<i64,_>("host_revision")? as u64,can_manage_host,terminal_enabled:row.try_get("terminal_enabled")?,terminal_revision:row.try_get::<i64,_>("terminal_revision")? as u64,can_manage_terminal:matches!(actor.role(),AdminRole::SystemAdmin|AdminRole::TenantAdmin)&&(actor.is_global_scope()||actor.can_access_tenant(&tenant)),node_id:node,name:row.try_get("name")?,fresh:row.try_get("fresh")?,observed_at:row.try_get("observed")?,capabilities:serde_json::from_value(row.try_get("capabilities")?)?,error:row.try_get("error")?};
            if row.try_get::<bool,_>("host_enabled")? {
                hosts.push(host);
            } else if can_manage_host {
                available_hosts.push(host);
            }
        }
        let node_ids = hosts.iter().map(|h| h.node_id.clone()).collect::<Vec<_>>();
        let network_holds:Vec<(String,i64)>=sqlx::query_as("SELECT node_id,coalesce(sum(held_states),0)::bigint FROM cvm_user_host_network_quotas WHERE node_id=ANY($1) GROUP BY node_id")
            .bind(&node_ids).fetch_all(&self.pool).await?;
        for host in &mut hosts {
            let held = network_holds
                .iter()
                .find(|(id, _)| id == &host.node_id)
                .map_or(0, |(_, held)| *held as u64);
            network_quota::limit_compute_memory(&mut host.capabilities, held);
        }
        let mut owners = Vec::new();
        for row in sqlx::query("SELECT tenant_id,id,id AS name FROM users WHERE ($1::text IS NULL OR tenant_id=$1 OR ($3::text IS NULL AND tenant_id LIKE $2 ESCAPE E'\\\\')) AND ($3::text IS NULL OR id=$3) ORDER BY tenant_id,id LIMIT 1024")
            .bind(&tenant).bind(&pattern).bind(&user).fetch_all(&self.pool).await? {
            owners.push(CvmOwner { tenant_id: row.try_get("tenant_id")?, user_id: row.try_get("id")?, name: row.try_get("name")? });
        }
        // Match admission's durable ledgers, including retained volumes and
        // interfaces still awaiting cleanup after compute has been released.
        let usage = sqlx::query(&format!(
            "WITH owners AS (SELECT tenant_id,user_id FROM cvm_quotas WHERE {READ_SCOPE}
             ORDER BY tenant_id,user_id LIMIT 1024)
             SELECT q.tenant_id,q.user_id,
               (SELECT count(*) FROM cvm_instances i WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND i.reserved) AS instances,
               (SELECT coalesce(sum((desired#>>'{{spec,resources,vcpus}}')::bigint),0)::bigint FROM cvm_instances i WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND i.reserved) AS vcpus,
               (SELECT coalesce(sum((desired#>>'{{spec,resources,memory_bytes}}')::bigint),0)::bigint FROM cvm_instances i WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND i.reserved) AS memory,
               (SELECT coalesce(sum(bytes),0)::bigint FROM cvm_volumes v WHERE v.tenant_id=q.tenant_id AND v.user_id=q.user_id AND v.state<>'released') AS disk,
               (SELECT count(*) FROM cvm_interfaces nic JOIN cvm_instances i ON i.id=nic.instance_id WHERE i.tenant_id=q.tenant_id AND i.user_id=q.user_id AND (NOT nic.detached_confirmed OR EXISTS(SELECT 1 FROM cvm_nic_allocations a WHERE a.interface_id=nic.id AND NOT a.released AND NOT a.retired))) AS interfaces
             FROM owners q"
        ))
        .bind(&tenant).bind(&pattern).bind(&user).fetch_all(&self.pool).await?
        .into_iter().map(|row| Ok(CvmQuotaUsage {
            tenant_id: row.try_get("tenant_id")?, user_id: row.try_get("user_id")?,
            instances: row.try_get::<i64, _>("instances")? as u64,
            vcpus: row.try_get::<i64, _>("vcpus")? as u64,
            memory_bytes: row.try_get::<i64, _>("memory")? as u64,
            disk_bytes: row.try_get::<i64, _>("disk")? as u64,
            interfaces: row.try_get::<i64, _>("interfaces")? as u64,
        })).collect::<Result<Vec<_>>>()?;
        Ok(CvmWorkspace {
            available_hosts,
            network_quotas: self.cvm_host_network_quotas(actor).await?,
            usage,
            can_manage_network: actor.is_system_admin(),
            hosts,
            instances,
            quotas,
            owners,
            can_manage_quotas: actor.is_system_admin(),
            can_write: matches!(
                actor.role(),
                AdminRole::User | AdminRole::TenantAdmin | AdminRole::SystemAdmin
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
        require_owner(actor, &value.tenant_id, &value.user_id, false)?;
        redact(actor, &mut value);
        Ok(value)
    }
}
