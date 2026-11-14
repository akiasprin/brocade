//! Validated data mirroring and a durable, cross-Console refresh lease.
use super::*;

impl PgStore {
    pub async fn ip_maintenance_asset(
        &self,
        key: &str,
    ) -> Result<Option<(serde_json::Value, i64)>> {
        let row = sqlx::query("SELECT payload,fetched_at FROM ip_maintenance_assets WHERE key=$1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| Ok((row.try_get("payload")?, row.try_get("fetched_at")?)))
            .transpose()
    }
    fn validate_maintenance_asset(key: &str, payload: &serde_json::Value) -> Result<String> {
        let digest = if key == "agents" {
            let agents: Vec<String> = serde_json::from_value(payload.clone())?;
            let mut check = data::MaintenanceData::seed("JP/Default/Tokyo")
                .ok_or_else(|| invalid("养护种子地区缺失"))?;
            check.user_agents = agents;
            check.recompute_digest().map_err(invalid)?;
            check.validate().map_err(invalid)?;
            data::value_digest(&check.user_agents).map_err(invalid)?
        } else {
            let id = key
                .strip_prefix("region:")
                .filter(|id| data::region(id).is_some())
                .ok_or_else(|| invalid("养护数据标识无效"))?;
            let mut check =
                data::MaintenanceData::seed(id).ok_or_else(|| invalid("养护种子地区缺失"))?;
            check.region = serde_json::from_value(payload.clone())?;
            if check.region.id != id {
                return Err(invalid("养护地区包身份不匹配"));
            }
            check.recompute_digest().map_err(invalid)?;
            check.validate().map_err(invalid)?;
            data::value_digest(&check.region).map_err(invalid)?
        };
        Ok(digest)
    }
    pub async fn publish_ip_maintenance_assets(
        &self,
        assets: Vec<(String, serde_json::Value)>,
        commit: &str,
        generation: u64,
    ) -> Result<()> {
        if commit.len() != 40
            || !commit.bytes().all(|b| b.is_ascii_hexdigit())
            || assets.len() > 100
        {
            return Err(invalid("养护数据来源版本无效"));
        }
        let mut validated = Vec::new();
        for (key, payload) in assets {
            let digest = Self::validate_maintenance_asset(&key, &payload)?;
            validated.push((key, payload, digest));
        }
        let mut tx = self.pool.begin().await?;
        let owns:bool=sqlx::query_scalar("SELECT generation=$1 AND lease_until>now() FROM ip_maintenance_refresh WHERE id=TRUE FOR UPDATE").bind(integer(generation)?).fetch_one(&mut *tx).await?;
        if !owns {
            return Err(StoreError::Conflict("数据刷新租约已失效".into()));
        }
        for (key, payload, digest) in validated {
            sqlx::query("INSERT INTO ip_maintenance_assets(key,payload,sha256,source_commit,fetched_at) VALUES($1,$2,$3,$4,extract(epoch FROM now())::bigint) ON CONFLICT(key) DO UPDATE SET payload=EXCLUDED.payload,sha256=EXCLUDED.sha256,source_commit=EXCLUDED.source_commit,fetched_at=EXCLUDED.fetched_at")
                .bind(key).bind(payload).bind(digest).bind(commit).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn ip_maintenance_data(&self, node: &str) -> Result<data::MaintenanceData> {
        let desired = self.ip_maintenance_desired(node).await?;
        let mut result = data::MaintenanceData::seed(&desired.policy.region)
            .ok_or_else(|| invalid("养护地区不存在"))?;
        let key = format!("region:{}", desired.policy.region);
        let rows = sqlx::query(
            "SELECT key,payload,fetched_at FROM ip_maintenance_assets WHERE key=ANY($1)",
        )
        .bind(vec![key.clone(), "agents".to_owned()])
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let payload = row.try_get::<serde_json::Value, _>("payload")?;
            if row.try_get::<String, _>("key")? == key {
                result.region = serde_json::from_value(payload)?;
                result.region_updated_at_unix_secs = row.try_get("fetched_at")?;
            } else {
                result.user_agents = serde_json::from_value(payload)?;
                result.agents_updated_at_unix_secs = row.try_get("fetched_at")?;
            }
        }
        result.recompute_digest().map_err(invalid)?;
        result.validate().map_err(invalid)?;
        Ok(result)
    }
    pub async fn claim_ip_maintenance_refresh(&self) -> Result<Option<(u64, Vec<String>)>> {
        let mut tx = self.pool.begin().await?;
        let claimed=sqlx::query_scalar::<_,i64>("UPDATE ip_maintenance_refresh SET lease_until=now()+interval '1 hour',generation=generation+1 WHERE id=TRUE AND lease_until<now() AND next_run_at<=now() AND EXISTS(SELECT FROM ip_maintenance_policies p JOIN nodes n ON n.id=p.node_id JOIN node_lifecycle_state l ON l.node_id=p.node_id WHERE p.generation>0 AND n.retired_at IS NULL AND l.phase='active') RETURNING generation")
            .fetch_optional(&mut *tx).await?;
        let ids = if claimed.is_some() {
            sqlx::query_scalar("SELECT DISTINCT p.policy->>'region' FROM ip_maintenance_policies p JOIN nodes n ON n.id=p.node_id JOIN node_lifecycle_state l ON l.node_id=p.node_id WHERE p.generation>0 AND n.retired_at IS NULL AND l.phase='active' ORDER BY 1 LIMIT 100")
                .fetch_all(&mut *tx).await?
        } else {
            Vec::new()
        };
        tx.commit().await?;
        claimed
            .map(|generation| {
                Ok((
                    u64::try_from(generation).map_err(|_| invalid("养护更新租约版本无效"))?,
                    ids,
                ))
            })
            .transpose()
    }
    pub async fn finish_ip_maintenance_refresh(&self, generation: u64, failed: bool) -> Result<()> {
        sqlx::query("UPDATE ip_maintenance_refresh SET lease_until='-infinity',last_error=$1,next_run_at=CASE WHEN $2 THEN now()+interval '15 minutes' ELSE ((date_trunc('day',now() AT TIME ZONE 'UTC')+interval '3 hours') AT TIME ZONE 'UTC')+CASE WHEN (now() AT TIME ZONE 'UTC')::time>=time '03:00' THEN interval '1 day' ELSE interval '0' END END WHERE id=TRUE AND generation=$3")
            .bind(if failed{Some("数据源更新未完成，继续上次有效数据")}else{None}).bind(failed).bind(integer(generation)?).execute(&self.pool).await?;
        Ok(())
    }
}
