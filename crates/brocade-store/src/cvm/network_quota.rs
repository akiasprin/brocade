//! Store is the sole allocator. Observed idle traffic never frees a held grant.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmHostNetworkQuotaChange {
    pub node_id: String,
    pub expected_revision: u64,
    pub budget_revision: u64,
    pub limits: NetworkQuotaLimits,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkQuotaPreviewRequest {
    pub tenant_id: String,
    pub user_id: String,
    pub node_id: String,
    pub limits: NetworkQuotaLimits,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNetworkQuotaPreview {
    pub node_id: String,
    pub revision: u64,
    pub budget_revision: u64,
    pub capacity: NetworkQuotaCapacity,
    pub held_states: u64,
    pub other_allocated: u64,
    pub maximum: u64,
    pub requested: NetworkQuotaLimits,
    pub additional_states: u64,
    pub excess_states: u64,
    pub fits: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmHostNetworkQuota {
    pub tenant_id: String,
    pub user_id: String,
    pub node_id: String,
    pub id: String,
    pub revision: u64,
    pub limits: NetworkQuotaLimits,
    pub held_states: u64,
    pub report: Option<NetworkQuotaReport>,
    pub fresh: bool,
}

fn row_quota(row: &sqlx::postgres::PgRow) -> Result<CvmHostNetworkQuota> {
    Ok(CvmHostNetworkQuota {
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        node_id: row.try_get("node_id")?,
        id: row.try_get("id")?,
        revision: row.try_get::<i64, _>("revision")? as u64,
        limits: serde_json::from_value(row.try_get("limits")?)?,
        held_states: row.try_get::<i64, _>("held_states")? as u64,
        report: row
            .try_get::<Option<serde_json::Value>, _>("report")?
            .map(serde_json::from_value)
            .transpose()?,
        fresh: row.try_get("fresh")?,
    })
}
const SELECT: &str = "SELECT *,coalesce(reported_at>now()-interval '150 seconds',false) AS fresh FROM cvm_user_host_network_quotas";

pub(super) async fn inventory_capacity(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    capacity: &NetworkQuotaCapacity,
) -> Result<()> {
    capacity.validate().map_err(invalid)?;
    sqlx::query("INSERT INTO cvm_network_quota_hosts(node_id,capacity) VALUES($1,$2) ON CONFLICT(node_id) DO UPDATE SET revision=cvm_network_quota_hosts.revision+CASE WHEN cvm_network_quota_hosts.capacity<>excluded.capacity THEN 1 ELSE 0 END,capacity=excluded.capacity")
        .bind(node).bind(serde_json::to_value(capacity)?).execute(&mut **tx).await?;
    Ok(())
}

async fn preview_tx(
    tx: &mut Transaction<'_, Postgres>,
    request: &CvmNetworkQuotaPreviewRequest,
) -> Result<CvmNetworkQuotaPreview> {
    request.limits.validate().map_err(invalid)?;
    if ![&request.tenant_id, &request.user_id, &request.node_id]
        .into_iter()
        .all(|s| valid_id(s))
    {
        return Err(invalid("用户或宿主身份无效"));
    }
    let caps = host_locked(tx, &request.node_id, false).await?;
    let mut capacity = caps.network_quota.ok_or_else(|| {
        StoreError::Unsupported("宿主尚未支持用户网络额度，请先升级节点运行栈".into())
    })?;
    let state = sqlx::query(
        "SELECT revision,capacity FROM cvm_network_quota_hosts WHERE node_id=$1 FOR UPDATE",
    )
    .bind(&request.node_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| invalid("宿主网络容量尚未核验"))?;
    let frozen: NetworkQuotaCapacity = serde_json::from_value(state.try_get("capacity")?)?;
    if frozen != capacity {
        return Err(invalid("宿主网络容量已变化，请重新试算"));
    }
    let guest:i64=sqlx::query_scalar("SELECT coalesce(sum((desired#>>'{spec,resources,memory_bytes}')::bigint),0)::bigint FROM cvm_instances WHERE node_id=$1 AND reserved")
        .bind(&request.node_id).fetch_one(&mut **tx).await?;
    capacity.memory_budget_bytes = capacity.memory_budget_bytes.saturating_sub(guest as u64);
    let memory_states = capacity
        .memory_budget_bytes
        .saturating_sub(capacity.fixed_memory_bytes)
        / u64::from(capacity.bytes_per_state);
    capacity.total_states = capacity
        .total_states
        .min(memory_states.min(u64::from(u32::MAX)) as u32);
    capacity.system_reserved_states = capacity.system_reserved_states.min(capacity.total_states);
    let own=sqlx::query("SELECT revision,held_states FROM cvm_user_host_network_quotas WHERE tenant_id=$1 AND user_id=$2 AND node_id=$3")
        .bind(&request.tenant_id).bind(&request.user_id).bind(&request.node_id).fetch_optional(&mut **tx).await?;
    let other:i64=sqlx::query_scalar("SELECT coalesce(sum(held_states),0)::bigint FROM cvm_user_host_network_quotas WHERE node_id=$1 AND NOT(tenant_id=$2 AND user_id=$3)")
        .bind(&request.node_id).bind(&request.tenant_id).bind(&request.user_id).fetch_one(&mut **tx).await?;
    let held = own
        .as_ref()
        .map(|r| r.try_get::<i64, _>("held_states"))
        .transpose()?
        .unwrap_or(0) as u64;
    let revision = own
        .as_ref()
        .map(|r| r.try_get::<i64, _>("revision"))
        .transpose()?
        .unwrap_or(0) as u64;
    let maximum = u64::from(capacity.allocatable()).saturating_sub(other as u64);
    let requested = u64::from(request.limits.max_states);
    Ok(CvmNetworkQuotaPreview {
        node_id: request.node_id.clone(),
        revision,
        budget_revision: state.try_get::<i64, _>("revision")? as u64,
        capacity,
        held_states: held,
        other_allocated: other as u64,
        maximum,
        requested: request.limits.clone(),
        additional_states: requested.saturating_sub(held),
        excess_states: requested.saturating_sub(maximum),
        fits: requested <= maximum,
    })
}

pub(super) async fn apply_changes(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    quota: &CvmQuota,
) -> Result<()> {
    if quota.host_network_quotas.len() > 64 {
        return Err(invalid("宿主网络额度数量超限"));
    }
    let mut changes = quota.host_network_quotas.iter().collect::<Vec<_>>();
    changes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let mut seen = std::collections::BTreeSet::new();
    for change in changes {
        if !quota.allowed_nodes.contains(&change.node_id) || !seen.insert(&change.node_id) {
            return Err(invalid("网络额度必须属于已授权宿主，且每台宿主只配置一次"));
        }
        let request = CvmNetworkQuotaPreviewRequest {
            tenant_id: quota.tenant_id.clone(),
            user_id: quota.user_id.clone(),
            node_id: change.node_id.clone(),
            limits: change.limits.clone(),
        };
        let p = preview_tx(tx, &request).await?;
        if p.revision != change.expected_revision || p.budget_revision != change.budget_revision {
            return Err(invalid("网络额度或宿主预算已更新，请刷新试算后保存"));
        }
        if !p.fits {
            return Err(invalid(format!(
                "网络额度超出可分配容量 {} 条",
                p.excess_states
            )));
        }
        if p.revision == 0 {
            let groups: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM cvm_user_host_network_quotas WHERE node_id=$1",
            )
            .bind(&change.node_id)
            .fetch_one(&mut **tx)
            .await?;
            if groups >= 512 {
                return Err(invalid("宿主用户网络额度数量已达上限"));
            }
        }
        let identity = format!("{}:{}:{}", quota.tenant_id, quota.user_id, change.node_id);
        let id = &sha256_hex(identity.as_bytes())[..16];
        let revision = p
            .revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| invalid("网络额度版本溢出"))?;
        let held = p.held_states.max(u64::from(change.limits.max_states));
        sqlx::query("INSERT INTO cvm_user_host_network_quotas(tenant_id,user_id,node_id,id,revision,limits,held_states) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant_id,user_id,node_id) DO UPDATE SET revision=excluded.revision,limits=excluded.limits,held_states=excluded.held_states,updated_at=now()")
            .bind(&quota.tenant_id).bind(&quota.user_id).bind(&change.node_id).bind(id).bind(revision as i64)
            .bind(serde_json::to_value(&change.limits)?).bind(held as i64).execute(&mut **tx).await?;
        sqlx::query("UPDATE cvm_network_quota_hosts SET revision=revision+1 WHERE node_id=$1")
            .bind(&change.node_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO cvm_network_quota_events(tenant_id,user_id,node_id,actor_id,policy) VALUES($1,$2,$3,$4,$5)")
            .bind(&quota.tenant_id).bind(&quota.user_id).bind(&change.node_id).bind(actor.operator_id()).bind(serde_json::to_value(change)?).execute(&mut **tx).await?;
    }
    Ok(())
}

pub(super) async fn admission(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
    node: &str,
    caps: &Capabilities,
) -> Result<()> {
    if caps.network_quota.is_none() {
        return Ok(());
    }
    let row=sqlx::query("SELECT revision,limits,report,coalesce(reported_at>now()-interval '150 seconds',false) AS fresh FROM cvm_user_host_network_quotas WHERE tenant_id=$1 AND user_id=$2 AND node_id=$3")
        .bind(tenant).bind(user).bind(node).fetch_optional(&mut **tx).await?.ok_or_else(||invalid("请先分配并生效此用户在宿主上的网络连接额度"))?;
    let limits: NetworkQuotaLimits = serde_json::from_value(row.try_get("limits")?)?;
    let revision = row.try_get::<i64, _>("revision")? as u64;
    let report: Option<NetworkQuotaReport> = row
        .try_get::<Option<serde_json::Value>, _>("report")?
        .map(serde_json::from_value)
        .transpose()?;
    if !row.try_get::<bool, _>("fresh")?
        || limits.max_states == 0
        || !report.is_some_and(|r| r.applied && r.revision == revision && r.error.is_none())
    {
        return Err(invalid("宿主网络额度尚未生效或已暂停新建"));
    }
    Ok(())
}

pub(super) fn limit_compute_memory(caps: &mut Capabilities, held_states: u64) {
    let Some(network) = caps.network_quota.as_ref() else {
        return;
    };
    let network_memory = network.fixed_memory_bytes.saturating_add(
        held_states
            .saturating_add(u64::from(network.system_reserved_states))
            .saturating_mul(u64::from(network.bytes_per_state)),
    );
    let remaining = network.memory_budget_bytes.saturating_sub(network_memory);
    caps.allocatable_memory_bytes = caps.allocatable_memory_bytes.min(remaining);
    if let Some(limits) = &mut caps.resource_limits {
        let maximum =
            caps.allocatable_memory_bytes / limits.memory_step_bytes * limits.memory_step_bytes;
        if maximum < limits.min_memory_bytes {
            caps.resource_limits = None;
            caps.create = false;
            caps.configure = false;
        } else {
            limits.max_memory_bytes = limits.max_memory_bytes.min(maximum)
        }
    }
}

pub(super) async fn compute_memory_budget(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    caps: &mut Capabilities,
) -> Result<()> {
    if caps.network_quota.is_none() {
        return Ok(());
    }
    let held:i64=sqlx::query_scalar("SELECT coalesce(sum(held_states),0)::bigint FROM cvm_user_host_network_quotas WHERE node_id=$1")
        .bind(node).fetch_one(&mut **tx).await?;
    limit_compute_memory(caps, held as u64);
    Ok(())
}

impl PgStore {
    pub async fn cvm_network_quota_preview(
        &self,
        actor: &AdminContext,
        request: CvmNetworkQuotaPreviewRequest,
    ) -> Result<CvmNetworkQuotaPreview> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "只有系统管理员可以分配宿主网络额度".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2)")
                .bind(&request.tenant_id)
                .bind(&request.user_id)
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            return Err(StoreError::NotFound("user".into()));
        }
        let result = preview_tx(&mut tx, &request).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn cvm_host_network_quotas(
        &self,
        actor: &AdminContext,
    ) -> Result<Vec<CvmHostNetworkQuota>> {
        let (tenant, pattern, user) = read_scope(actor)?;
        sqlx::query(&format!(
            "{SELECT} WHERE {READ_SCOPE} ORDER BY node_id,tenant_id,user_id LIMIT 4096"
        ))
        .bind(tenant)
        .bind(pattern)
        .bind(user)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(row_quota)
        .collect()
    }
    pub async fn cvm_network_quota_desired(&self, node: &str) -> Result<NetworkQuotaDesired> {
        let mut desired = NetworkQuotaDesired::default();
        let rows=sqlx::query("SELECT tenant_id,user_id,id,revision,limits FROM cvm_user_host_network_quotas WHERE node_id=$1 ORDER BY id LIMIT 512")
            .bind(node).fetch_all(&self.pool).await?;
        for row in rows {
            let instances:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_instances WHERE node_id=$1 AND tenant_id=$2 AND user_id=$3 AND reserved AND NOT(desired->>'power'='stopped' AND status='applied' AND coalesce(report->>'network_absent'='true',false)) ORDER BY id LIMIT 256")
                .bind(node).bind(row.try_get::<String,_>("tenant_id")?).bind(row.try_get::<String,_>("user_id")?).fetch_all(&self.pool).await?;
            desired.assignments.push(NetworkQuotaAssignment {
                id: row.try_get("id")?,
                revision: row.try_get::<i64, _>("revision")? as u64,
                limits: serde_json::from_value(row.try_get("limits")?)?,
                instances,
            });
        }
        desired.validate().map_err(invalid)?;
        Ok(desired)
    }
    pub async fn cvm_record_network_quotas(
        &self,
        node: &str,
        reports: Vec<NetworkQuotaReport>,
    ) -> Result<usize> {
        if reports.len() > 512 {
            return Err(invalid("网络额度回执数量超限"));
        }
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        sqlx::query("SELECT node_id FROM cvm_network_quota_hosts WHERE node_id=$1 FOR UPDATE")
            .bind(node)
            .fetch_optional(&mut *tx)
            .await?;
        let mut accepted = 0;
        let mut seen = std::collections::BTreeSet::new();
        for report in reports {
            if !seen.insert(report.id.clone())
                || report.error.as_ref().is_some_and(|s| s.len() > 500)
                || report.states > 1048576
                || (report.applied && report.error.is_some())
            {
                return Err(invalid("网络额度回执无效"));
            }
            let Some(row)=sqlx::query("SELECT revision,limits,held_states FROM cvm_user_host_network_quotas WHERE node_id=$1 AND id=$2 FOR UPDATE")
                .bind(node).bind(&report.id).fetch_optional(&mut *tx).await? else {continue};
            if report.revision != row.try_get::<i64, _>("revision")? as u64 {
                continue;
            }
            let limits: NetworkQuotaLimits = serde_json::from_value(row.try_get("limits")?)?;
            let held = row.try_get::<i64, _>("held_states")?;
            let used = report.states;
            let next = if report.applied && used <= u64::from(limits.max_states) {
                i64::from(limits.max_states)
            } else {
                held
            };
            sqlx::query("UPDATE cvm_user_host_network_quotas SET report=$3,reported_at=now(),held_states=$4 WHERE node_id=$1 AND id=$2")
                .bind(node).bind(&report.id).bind(serde_json::to_value(&report)?).bind(next).execute(&mut *tx).await?;
            if next != held {
                sqlx::query(
                    "UPDATE cvm_network_quota_hosts SET revision=revision+1 WHERE node_id=$1",
                )
                .bind(node)
                .execute(&mut *tx)
                .await?;
            }
            accepted += 1;
        }
        tx.commit().await?;
        Ok(accepted)
    }
}
