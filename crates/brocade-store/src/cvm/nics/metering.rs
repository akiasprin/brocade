use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmNicUsage {
    pub period: String,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub coverage_gap: bool,
    pub observed_at: u64,
}
pub(super) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    binding: &NicBinding,
    evidence: &NicBindingReport,
) -> Result<()> {
    let Some(sample) = &evidence.traffic else {
        return Ok(());
    };
    if evidence.side != BindingSide::Consumer {
        return Err(invalid("网卡流量只能由消费侧计量"));
    }
    let now = crate::cvm::monitoring::now_millis()?;
    if sample.epoch.len() != 64
        || !sample.epoch.bytes().all(|c| c.is_ascii_hexdigit())
        || sample.observed_at == 0
        || sample.observed_at > now + 300_000
        || sample.uploaded_bytes > i64::MAX as u64
        || sample.downloaded_bytes > i64::MAX as u64
    {
        return Err(invalid("无效的网卡流量采样"));
    }
    let previous:Option<(i64,i64,i64)>=sqlx::query_as("SELECT observed_at,uploaded,downloaded FROM cvm_nic_meter_cursors WHERE interface_id=$1 AND epoch=$2 FOR UPDATE").bind(&binding.interface_id).bind(&sample.epoch).fetch_optional(&mut **tx).await?;
    if previous.is_some_and(|(at, _, _)| at as u64 >= sample.observed_at) {
        return Ok(());
    }
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT max(observed_at) FROM cvm_nic_meter_cursors WHERE interface_id=$1",
    )
    .bind(&binding.interface_id)
    .fetch_one(&mut **tx)
    .await?;
    if previous.is_none() && latest.is_some_and(|at| at as u64 >= sample.observed_at) {
        return Ok(());
    }
    let delta = |current: u64, previous: i64| {
        if current < previous as u64 {
            current
        } else {
            current - previous as u64
        }
    };
    let (uploaded, downloaded, gap) = if let Some((at, up, down)) = previous {
        (delta(sample.uploaded_bytes,up),delta(sample.downloaded_bytes,down),sample.uploaded_bytes<(up as u64)||sample.downloaded_bytes<(down as u64)||sqlx::query_scalar::<_,bool>("SELECT date_trunc('month',to_timestamp($1::bigint::double precision/1000) AT TIME ZONE 'UTC')<>date_trunc('month',to_timestamp($2::bigint::double precision/1000) AT TIME ZONE 'UTC')").bind(at).bind(sample.observed_at as i64).fetch_one(&mut **tx).await?)
    } else {
        let prior: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cvm_nic_meter_cursors WHERE interface_id=$1)",
        )
        .bind(&binding.interface_id)
        .fetch_one(&mut **tx)
        .await?;
        (sample.uploaded_bytes, sample.downloaded_bytes, prior)
    };
    sqlx::query("INSERT INTO cvm_nic_meter_cursors(interface_id,epoch,observed_at,uploaded,downloaded) VALUES($1,$2,$3,$4,$5) ON CONFLICT(interface_id,epoch) DO UPDATE SET observed_at=excluded.observed_at,uploaded=excluded.uploaded,downloaded=excluded.downloaded").bind(&binding.interface_id).bind(&sample.epoch).bind(sample.observed_at as i64).bind(sample.uploaded_bytes as i64).bind(sample.downloaded_bytes as i64).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO cvm_nic_monthly_usage(interface_id,period,uploaded,downloaded,coverage_gap,observed_at) VALUES($1,date_trunc('month',to_timestamp($2::bigint::double precision/1000) AT TIME ZONE 'UTC')::date,$3,$4,$5,$2) ON CONFLICT(interface_id,period) DO UPDATE SET uploaded=least(9223372036854775807::numeric,cvm_nic_monthly_usage.uploaded::numeric+excluded.uploaded)::bigint,downloaded=least(9223372036854775807::numeric,cvm_nic_monthly_usage.downloaded::numeric+excluded.downloaded)::bigint,coverage_gap=cvm_nic_monthly_usage.coverage_gap OR excluded.coverage_gap,observed_at=greatest(cvm_nic_monthly_usage.observed_at,excluded.observed_at)").bind(&binding.interface_id).bind(sample.observed_at as i64).bind(uploaded as i64).bind(downloaded as i64).bind(gap).execute(&mut **tx).await?;
    // Bound counter epochs and retain enough history to reject delayed repeats.
    sqlx::query("DELETE FROM cvm_nic_meter_cursors WHERE interface_id=$1 AND epoch IN(SELECT epoch FROM cvm_nic_meter_cursors WHERE interface_id=$1 ORDER BY observed_at DESC OFFSET 128)").bind(&binding.interface_id).execute(&mut **tx).await?;
    enforce(tx, &binding.instance_id).await
}
pub(super) async fn enforce(tx: &mut Transaction<'_, Postgres>, instance: &str) -> Result<()> {
    let row = sqlx::query(&format!("{INSTANCE_SELECT} WHERE id=$1 FOR UPDATE"))
        .bind(instance)
        .fetch_one(&mut **tx)
        .await?;
    let mut desired = from_row(&row)?.desired;
    let mut changed = false;
    for device in &mut desired.spec.interfaces {
        let Some(nic) = device.allocated.as_mut() else {
            continue;
        };
        let used:i64=sqlx::query_scalar("SELECT least(9223372036854775807::numeric,coalesce(sum(uploaded::numeric+downloaded::numeric),0))::bigint FROM cvm_nic_monthly_usage WHERE interface_id=$1 AND period=date_trunc('month',now() AT TIME ZONE 'UTC')::date").bind(&device.id).fetch_one(&mut **tx).await?;
        let blocked = nic
            .limits
            .monthly_bytes
            .is_some_and(|limit| used as u64 >= limit);
        if nic.quota_exceeded == blocked {
            continue;
        }
        nic.quota_exceeded = blocked;
        changed = true;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM cvm_nic_bindings WHERE interface_id=$1 AND NOT cleaned",
        )
        .bind(&device.id)
        .fetch_all(&mut **tx)
        .await?;
        for id in ids {
            let mut binding = bindings::binding_value(tx, &id).await?;
            binding.nic.quota_exceeded = blocked;
            binding.generation += 1;
            bindings::persist_binding(tx, &binding).await?;
        }
        sqlx::query("UPDATE cvm_nic_allocations SET spec=$2 WHERE id=$1")
            .bind(&nic.allocation_id)
            .bind(serde_json::to_value(&*nic)?)
            .execute(&mut **tx)
            .await?;
    }
    if changed {
        system_network_version(tx, &mut desired, "network_traffic_quota").await?;
    }
    Ok(())
}
impl PgStore {
    pub(super) async fn reset_cvm_nic_period(&self, node: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        lock(&mut tx).await?;
        let instances:Vec<String>=sqlx::query_scalar("SELECT DISTINCT a.instance_id FROM cvm_nic_allocations a JOIN cvm_instances i ON i.id=a.instance_id CROSS JOIN LATERAL jsonb_array_elements(i.desired#>'{spec,interfaces}') n WHERE (a.consumer_node=$1 OR a.provider_node=$1) AND NOT a.released AND n#>>'{allocated,quota_exceeded}'='true' ORDER BY a.instance_id LIMIT 256").bind(node).fetch_all(&mut *tx).await?;
        for instance in instances {
            enforce(&mut tx, &instance).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
