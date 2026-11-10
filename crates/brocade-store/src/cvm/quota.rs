use super::*;

pub(super) async fn quota_locked(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    user: &str,
) -> Result<CvmQuota> {
    let row = sqlx::query("SELECT * FROM cvm_quotas WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE")
        .bind(tenant)
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| StoreError::Forbidden("该用户尚未获得 CVM 创建额度".into()))?;
    quota_row(&row)
}

pub(super) fn quota_row(row: &sqlx::postgres::PgRow) -> Result<CvmQuota> {
    Ok(CvmQuota {
        host_network_quotas: vec![],
        tenant_id: row.try_get("tenant_id")?,
        user_id: row.try_get("user_id")?,
        enabled: row.try_get("enabled")?,
        allowed_nodes: serde_json::from_value(row.try_get("allowed_nodes")?)?,
        max_instances: row.try_get::<i32, _>("max_instances")? as u32,
        max_vcpus: row.try_get::<i64, _>("max_vcpus")? as u64,
        max_memory_bytes: row.try_get::<i64, _>("max_memory_bytes")? as u64,
        max_disk_bytes: row.try_get::<i64, _>("max_disk_bytes")? as u64,
        max_interfaces: row.try_get::<i32, _>("max_interfaces")? as u32,
    })
}

pub(super) async fn admission(
    tx: &mut Transaction<'_, Postgres>,
    quota: &CvmQuota,
    node: &str,
    resources: Option<&Resources>,
    extra_interfaces: u64,
) -> Result<()> {
    if !quota.enabled || !quota.allowed_nodes.iter().any(|n| n == node) {
        return Err(StoreError::Forbidden("未授权在这台机器上使用 CVM".into()));
    }
    let usage=sqlx::query("SELECT count(*)::bigint AS instances,coalesce(sum((desired#>>'{spec,resources,vcpus}')::bigint),0)::bigint AS cpu,coalesce(sum((desired#>>'{spec,resources,memory_bytes}')::bigint),0)::bigint AS memory FROM cvm_instances WHERE tenant_id=$1 AND user_id=$2 AND reserved").bind(&quota.tenant_id).bind(&quota.user_id).fetch_one(&mut **tx).await?;
    let disks:i64=sqlx::query_scalar("SELECT coalesce(sum(bytes),0)::bigint FROM cvm_volumes WHERE tenant_id=$1 AND user_id=$2 AND state<>'released'").bind(&quota.tenant_id).bind(&quota.user_id).fetch_one(&mut **tx).await?;
    let nics:i64=sqlx::query_scalar("SELECT count(*) FROM cvm_interfaces nic JOIN cvm_instances i ON i.id=nic.instance_id WHERE i.tenant_id=$1 AND i.user_id=$2 AND (NOT nic.detached_confirmed OR EXISTS(SELECT 1 FROM cvm_nic_allocations a WHERE a.interface_id=nic.id AND NOT a.released AND NOT a.retired))").bind(&quota.tenant_id).bind(&quota.user_id).fetch_one(&mut **tx).await?;
    let add = |a: i64, b: u64, limit: u64| {
        a >= 0 && (a as u64).checked_add(b).is_some_and(|v| v <= limit)
    };
    if !add(
        usage.try_get("instances")?,
        u64::from(resources.is_some()),
        u64::from(quota.max_instances),
    ) || !add(
        usage.try_get("cpu")?,
        resources.map_or(0, |p| u64::from(p.vcpus)),
        quota.max_vcpus,
    ) || !add(
        usage.try_get("memory")?,
        resources.map_or(0, |p| p.memory_bytes),
        quota.max_memory_bytes,
    ) || !add(
        disks,
        resources.map_or(0, |p| p.root_disk_bytes),
        quota.max_disk_bytes,
    ) || !add(nics, extra_interfaces, u64::from(quota.max_interfaces))
    {
        return Err(invalid("CVM 实例、计算、磁盘或网卡额度不足"));
    }
    Ok(())
}

pub(super) async fn host_locked(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    recovery: bool,
) -> Result<Capabilities> {
    let row=sqlx::query("SELECT h.capabilities,h.observed_at>now()-interval '150 seconds' AS fresh,h.error,n.retired_at IS NULL AS active FROM cvm_hosts h JOIN nodes n ON n.id=h.node_id LEFT JOIN cvm_terminal_policies p ON p.node_id=n.id WHERE h.node_id=$1 FOR UPDATE OF h,n").bind(node).fetch_optional(&mut **tx).await?.ok_or_else(||invalid("机器尚未上报 CVM 执行能力"))?;
    if !recovery
        && (!row.try_get::<bool, _>("fresh")?
            || !row.try_get::<bool, _>("active")?
            || row.try_get::<Option<String>, _>("error")?.is_some())
    {
        return Err(invalid("机器能力已过期或机器已退役"));
    }
    if !recovery {
        let enabled: bool = sqlx::query_scalar(
            "SELECT coalesce((SELECT enabled FROM cvm_host_policies WHERE node_id=$1),true)",
        )
        .bind(node)
        .fetch_one(&mut **tx)
        .await?;
        if !enabled {
            return Err(StoreError::Conflict(
                "该机器已移除容器宿主身份，请先重新添加宿主".into(),
            ));
        }
    }
    let mut caps: Capabilities = serde_json::from_value(row.try_get("capabilities")?)?;
    caps.validate().map_err(invalid)?;
    network_quota::compute_memory_budget(tx, node, &mut caps).await?;
    Ok(caps)
}

impl PgStore {
    pub async fn set_cvm_quota(&self, actor: &AdminContext, request: CvmQuota) -> Result<()> {
        if !actor.is_system_admin() {
            return Err(StoreError::Forbidden(
                "只有系统管理员可以分配 CVM 能力".into(),
            ));
        }
        if request.allowed_nodes.len() > 64
            || request.allowed_nodes.iter().any(|x| !valid_id(x))
            || request.max_instances > 256
            || request.max_interfaces > 4096
            || [
                request.max_vcpus,
                request.max_memory_bytes,
                request.max_disk_bytes,
            ]
            .iter()
            .any(|x| *x > i64::MAX as u64)
        {
            return Err(invalid("CVM 额度或机器范围无效"));
        }
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        sqlx::query("SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
            .bind(&request.tenant_id)
            .bind(&request.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| StoreError::NotFound("user".into()))?;
        for node in &request.allowed_nodes {
            if !sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1 AND retired_at IS NULL)",
            )
            .bind(node)
            .fetch_one(&mut *tx)
            .await?
            {
                return Err(invalid("授权机器不存在或已退役"));
            }
        }
        let active_nodes:Vec<String>=sqlx::query_scalar("SELECT DISTINCT i.node_id FROM cvm_instances i JOIN cvm_volumes v ON v.instance_id=i.id WHERE i.tenant_id=$1 AND i.user_id=$2 AND (i.reserved OR v.state<>'released')").bind(&request.tenant_id).bind(&request.user_id).fetch_all(&mut *tx).await?;
        let network_nodes:Vec<String>=sqlx::query_scalar("SELECT q.node_id FROM cvm_user_host_network_quotas q JOIN nodes n ON n.id=q.node_id WHERE q.tenant_id=$1 AND q.user_id=$2 AND q.held_states>0 AND n.retired_at IS NULL")
            .bind(&request.tenant_id).bind(&request.user_id).fetch_all(&mut *tx).await?;
        if network_nodes
            .iter()
            .any(|node| !request.enabled || !request.allowed_nodes.contains(node))
        {
            return Err(invalid(
                "请先将受影响宿主的网络额度设为 0，并等待节点确认释放，再缩小授权范围",
            ));
        }
        if active_nodes
            .iter()
            .any(|node| !request.enabled || !request.allowed_nodes.contains(node))
        {
            return Err(invalid("先删除并完成受影响实例的清理，再缩小机器授权范围"));
        }
        if let Some(node) = active_nodes.first() {
            admission(&mut tx, &request, node, None, 0).await?;
        }
        let held_disks:i64=sqlx::query_scalar("SELECT coalesce(sum(bytes),0)::bigint FROM cvm_volumes WHERE tenant_id=$1 AND user_id=$2 AND state<>'released'").bind(&request.tenant_id).bind(&request.user_id).fetch_one(&mut *tx).await?;
        if held_disks as u64 > request.max_disk_bytes {
            return Err(invalid("磁盘额度不能小于已保留的系统盘"));
        }
        sqlx::query("INSERT INTO cvm_quotas(tenant_id,user_id,enabled,allowed_nodes,max_instances,max_vcpus,max_memory_bytes,max_disk_bytes,max_interfaces) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(tenant_id,user_id) DO UPDATE SET enabled=excluded.enabled,allowed_nodes=excluded.allowed_nodes,max_instances=excluded.max_instances,max_vcpus=excluded.max_vcpus,max_memory_bytes=excluded.max_memory_bytes,max_disk_bytes=excluded.max_disk_bytes,max_interfaces=excluded.max_interfaces").bind(&request.tenant_id).bind(&request.user_id).bind(request.enabled).bind(serde_json::to_value(&request.allowed_nodes)?).bind(request.max_instances as i32).bind(request.max_vcpus as i64).bind(request.max_memory_bytes as i64).bind(request.max_disk_bytes as i64).bind(request.max_interfaces as i32).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO cvm_quota_changes(tenant_id,user_id,actor_id,policy) VALUES($1,$2,$3,$4)",
        )
        .bind(&request.tenant_id)
        .bind(&request.user_id)
        .bind(actor.operator_id())
        .bind(serde_json::to_value(&request)?)
        .execute(&mut *tx)
        .await?;
        network_quota::apply_changes(&mut tx, actor, &request).await?;
        tx.commit().await?;
        Ok(())
    }
}
