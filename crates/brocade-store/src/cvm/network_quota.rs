//! One automatically managed host budget; no user-specific network allocation.
use super::*;

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

fn identity(node: &str) -> String {
    let digest = sha256_hex(format!("cvm-host-network:{node}").as_bytes());
    // Fixed nonzero hex prefix bounds the host quota identity.
    format!("b{}", &digest[..15])
}

impl PgStore {
    pub async fn cvm_network_quota_desired(&self, node: &str) -> Result<NetworkQuotaDesired> {
        let Some(row) =
            sqlx::query("SELECT revision,capacity FROM cvm_network_quota_hosts WHERE node_id=$1")
                .bind(node)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(NetworkQuotaDesired::default());
        };
        let capacity: NetworkQuotaCapacity = serde_json::from_value(row.try_get("capacity")?)?;
        capacity.validate().map_err(invalid)?;
        let instances:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_instances WHERE node_id=$1 AND reserved AND NOT(desired->>'power'='stopped' AND status='applied' AND coalesce(report->>'network_absent'='true',false)) ORDER BY id LIMIT 256")
            .bind(node).fetch_all(&self.pool).await?;
        let desired = NetworkQuotaDesired {
            assignments: vec![NetworkQuotaAssignment {
                id: identity(node),
                revision: row.try_get::<i64, _>("revision")? as u64,
                limits: NetworkQuotaLimits {
                    max_states: capacity.allocatable(),
                    new_per_second: 100_000,
                    burst: 200_000,
                },
                instances,
            }],
        };
        desired.validate().map_err(invalid)?;
        Ok(desired)
    }
    pub async fn cvm_record_network_quotas(
        &self,
        node: &str,
        reports: Vec<NetworkQuotaReport>,
    ) -> Result<usize> {
        if reports.len() > 512 {
            return Err(invalid("网络容量回执数量超限"));
        }
        let mut accepted = 0;
        for report in reports {
            if report.id != identity(node) {
                continue;
            }
            accepted+=sqlx::query("UPDATE cvm_network_quota_hosts SET report=$3,reported_at=now() WHERE node_id=$1 AND revision=$2")
                .bind(node).bind(report.revision as i64).bind(serde_json::to_value(report)?).execute(&self.pool).await?.rows_affected() as usize;
        }
        Ok(accepted)
    }
}
