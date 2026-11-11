use super::*;

impl PgStore {
    pub async fn cvm_record_inventory(&self, node: &str, request: InventoryReport) -> Result<()> {
        request.capabilities.validate().map_err(invalid)?;
        if request.error.as_ref().is_some_and(|e| e.len() > 500) {
            return Err(invalid("CVM inventory error exceeds limit"));
        }
        // An observation outage must not erase the last verified cleanup policy.
        // Freshness/error still deny new allocations, while existing instances can
        // queue deletion using the previously advertised data-retention contract.
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        sqlx::query("INSERT INTO cvm_hosts(node_id,capabilities,error) VALUES($1,$2,$3) ON CONFLICT(node_id) DO UPDATE SET capabilities=CASE WHEN excluded.error IS NULL THEN excluded.capabilities ELSE cvm_hosts.capabilities END,error=excluded.error,observed_at=now()").bind(node).bind(serde_json::to_value(&request.capabilities)?).bind(&request.error).execute(&mut *tx).await?;
        if request.error.is_none() {
            if let Some(capacity) = &request.capabilities.network_quota {
                network_quota::inventory_capacity(&mut tx, node, capacity).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

impl PgStore {
    pub async fn cvm_desired(&self, node: &str) -> Result<DesiredState> {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT desired FROM cvm_instances WHERE node_id=$1 AND reserved ORDER BY id LIMIT 256",
        )
        .bind(node)
        .fetch_all(&self.pool)
        .await?;
        Ok(DesiredState {
            instances: rows
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<_, _>>()?,
        })
    }
}

impl PgStore {
    pub async fn cvm_record_report(&self, node: &str, report: InstanceReport) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        let row = sqlx::query(
            "SELECT desired,reserved,report FROM cvm_instances WHERE id=$1 AND node_id=$2 FOR UPDATE",
        )
        .bind(&report.id)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("CVM assignment".into()))?;
        let desired: InstanceDesired = serde_json::from_value(row.try_get("desired")?)?;
        if report.generation < desired.generation {
            return Ok(false);
        }
        report.validate_for(&desired).map_err(invalid)?;
        if !row.try_get::<bool, _>("reserved")? && report.status != ApplyStatus::Applied {
            return Ok(false);
        }
        if report.status == ApplyStatus::Applied {
            let removed:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_interfaces WHERE instance_id=$1 AND NOT desired_attached AND NOT detached_confirmed").bind(&report.id).fetch_all(&mut *tx).await?;
            // Whole-instance deletion already proves all network devices absent.
            // A superseded detach does not require a second per-device receipt.
            if desired.power != PowerState::Deleted
                && removed
                    .iter()
                    .any(|id| !report.detached_interfaces.contains(id))
            {
                return Err(invalid("网卡卸载缺少实际设备移除回执"));
            }
            for id in removed {
                sqlx::query("UPDATE cvm_interfaces SET detached_confirmed=true WHERE id=$1 AND instance_id=$2").bind(id).bind(&report.id).execute(&mut *tx).await?;
            }
            if desired.power == PowerState::Deleted {
                sqlx::query("UPDATE cvm_instances SET reserved=false WHERE id=$1")
                    .bind(&report.id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE cvm_interfaces SET desired_attached=false,detached_confirmed=true WHERE instance_id=$1").bind(&report.id).execute(&mut *tx).await?;
                sqlx::query("UPDATE cvm_volumes SET state=$2 WHERE instance_id=$1")
                    .bind(&report.id)
                    .bind(if report.volume_deleted {
                        "released"
                    } else {
                        "retained"
                    })
                    .execute(&mut *tx)
                    .await?;
            } else {
                sqlx::query(
                    "UPDATE cvm_volumes SET state='present',backend_id=$2 WHERE instance_id=$1",
                )
                .bind(&report.id)
                .bind(&report.volume_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        // Entries deliver to the reported primary address; a change (restart, stop) must
        // reach every entry machine and the host in the same transaction.
        let previous: Option<InstanceReport> = row
            .try_get::<Option<serde_json::Value>, _>("report")?
            .map(serde_json::from_value)
            .transpose()?;
        let delivery = |r: Option<&InstanceReport>| r.map(|r| (r.power, r.access_address.clone()));
        if delivery(previous.as_ref()) != delivery(Some(&report)) {
            nics::entries::wake_instance(&mut tx, &report.id, node).await?;
        }
        let stored_report = serde_json::to_value(&report)?;
        sqlx::query("UPDATE cvm_instances SET status=$2,report=$3,updated_at=now() WHERE id=$1")
            .bind(&report.id)
            .bind(state_text(report.status))
            .bind(stored_report)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE cvm_operations SET status=$2,updated_at=now() WHERE id=$1 AND instance_id=$3 AND generation=$4").bind(&report.operation_id).bind(state_text(report.status)).bind(&report.id).bind(report.generation as i64).execute(&mut *tx).await?;
        if report.status == ApplyStatus::Applied {
            nics::release_confirmed(&mut tx, &report.id).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}
