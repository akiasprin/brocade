use super::*;

pub(super) const TERMINAL_SESSIONS_USED: &str = "terminal_sessions_used";

impl PgStore {
    pub async fn cvm_record_inventory(&self, node: &str, request: InventoryReport) -> Result<()> {
        request.capabilities.validate().map_err(invalid)?;
        if request.error.as_ref().is_some_and(|e| e.len() > 500) {
            return Err(invalid("CVM inventory error exceeds limit"));
        }
        let mut inventory = serde_json::to_value(&request.capabilities)?;
        // Keep observations out of typed capabilities, which are compiler inputs.
        if let Some(used) = request.terminal_sessions_used {
            if usize::from(used) > brocade_deployment::cvm::terminal::MAX_PER_NODE
                || !request.capabilities.guest_terminal
                || !request.capabilities.guest_terminal_persistent
            {
                return Err(invalid("invalid CVM terminal occupancy"));
            }
            inventory[TERMINAL_SESSIONS_USED] = used.into();
        }
        // An observation outage must not erase the last verified cleanup policy.
        // Freshness/error still deny new allocations, while existing instances can
        // queue deletion using the previously advertised data-retention contract.
        let mut tx = self.pool.begin().await?;
        crate::resource_coordination::lock(&mut tx).await?;
        sqlx::query("INSERT INTO cvm_hosts(node_id,capabilities,error) VALUES($1,$2,$3) ON CONFLICT(node_id) DO UPDATE SET capabilities=CASE WHEN excluded.error IS NULL THEN excluded.capabilities ELSE cvm_hosts.capabilities END,error=excluded.error,observed_at=now()").bind(node).bind(inventory).bind(&request.error).execute(&mut *tx).await?;
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
    /// First compiled page; follow `next_offset` to consume the remaining instances.
    pub async fn cvm_desired(&self, node: &str) -> Result<DesiredState> {
        self.cvm_desired_page(node, 0).await
    }
    pub async fn cvm_desired_page(&self, node: &str, offset: usize) -> Result<DesiredState> {
        let start = offset;
        if start > MAX_INSTANCES_PER_MACHINE {
            return Err(invalid("CVM page cursor exceeds limits"));
        }
        let mut tx = self.pool.begin().await?;
        let serving = releases::serving_tx(&mut tx, node).await?;
        let chain_clients = serving
            .as_ref()
            .map(|s| s.chain_clients.clone())
            .unwrap_or_default();
        let rows: Vec<serde_json::Value> = if let Some(config) = serving {
            config
                .instances
                .into_iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<_, _>>()?
        } else {
            sqlx::query_scalar("SELECT desired FROM cvm_instances WHERE node_id=$1 AND reserved ORDER BY id LIMIT 256").bind(node).fetch_all(&mut *tx).await?
        };
        let total = rows.len();
        let mut instances = Vec::new();
        let mut bytes = 0usize;
        for raw in rows.into_iter().skip(start).take(8) {
            let mut desired: InstanceDesired = serde_json::from_value(raw)?;
            ensure_access_generation(&mut tx, &desired.id, desired.generation).await?;
            desired.artifacts = artifacts::load(&mut tx, &desired.id, desired.generation).await?;
            let size = serde_json::to_vec(&desired)?.len();
            if size > 12 * 1024 * 1024 {
                return Err(invalid(
                    "CVM single-instance artifact response exceeds limits",
                ));
            }
            if bytes + size > 12 * 1024 * 1024 {
                break;
            }
            bytes += size;
            instances.push(desired);
        }
        tx.commit().await?;
        let next = start + instances.len();
        Ok(DesiredState {
            chain_clients,
            instances,
            page_offset: offset,
            next_offset: if next < total { Some(next) } else { None },
        })
    }
}

impl PgStore {
    pub async fn cvm_record_report(&self, node: &str, report: InstanceReport) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        crate::cvm::releases::lock(&mut tx).await?;
        let row = sqlx::query(
            "SELECT desired,reserved,report FROM cvm_instances WHERE id=$1 AND node_id=$2 FOR UPDATE",
        )
        .bind(&report.id)
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound("CVM assignment".into()))?;
        let latest: InstanceDesired = serde_json::from_value(row.try_get("desired")?)?;
        let desired = match releases::serving_tx(&mut tx, node).await? {
            Some(config) => match config.instances.into_iter().find(|i| i.id == report.id) {
                Some(desired) => desired,
                None => return Ok(false),
            },
            None => latest.clone(),
        };
        if report.generation < desired.generation {
            return Ok(false);
        }
        report.validate_for(&desired).map_err(invalid)?;
        if let Some(receipt) = &report.artifacts {
            let bundle = artifacts::load(&mut tx, &desired.id, desired.generation)
                .await?
                .ok_or_else(|| invalid("没有可核对的冻结产物"))?;
            if receipt.compiler != bundle.compiler
                || receipt.input_sha256 != bundle.input_sha256
                || receipt.files.len() != 4
                || receipt.mismatches.len() > 4
                || receipt
                    .files
                    .iter()
                    .map(|f| f.kind)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != 4
                || receipt.files.iter().any(|f| {
                    bundle
                        .file(f.kind)
                        .map_or(true, |file| file.sha256 != f.compiled_sha256)
                        || f.effective_sha256
                            .as_ref()
                            .is_some_and(|sha| !valid_digest(sha))
                        || serde_json::to_vec(&f.bindings).map_or(true, |b| b.len() > 4096)
                })
                || (report.status == ApplyStatus::Applied && !receipt.mismatches.is_empty())
            {
                return Err(invalid("CVM 产物回执与冻结内容不一致"));
            }
        }
        if desired.generation != latest.generation {
            sqlx::query("UPDATE cvm_operations SET status=$2,updated_at=now() WHERE id=$1 AND instance_id=$3 AND generation=$4").bind(&report.operation_id).bind(state_text(report.status)).bind(&report.id).bind(report.generation as i64).execute(&mut *tx).await?;
            sqlx::query("UPDATE cvm_instances SET report=$2 WHERE id=$1")
                .bind(&report.id)
                .bind(serde_json::to_value(&report)?)
                .execute(&mut *tx)
                .await?;
            releases::refresh_runtime(&mut tx, node).await?;
            releases::settle(&mut tx, node).await?;
            tx.commit().await?;
            return Ok(true);
        }
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
        releases::refresh_runtime(&mut tx, node).await?;
        releases::settle(&mut tx, node).await?;
        tx.commit().await?;
        Ok(true)
    }
}
