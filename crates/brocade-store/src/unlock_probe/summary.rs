use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct ChainUnlockProbeItem {
    pub app_id: String,
    pub chain_id: String,
    pub capabilities: UnlockProbeCapabilities,
    pub probes: ChainUnlockProbeReports,
}

#[derive(Debug, Default, Serialize)]
pub struct ChainUnlockProbeReports {
    pub v4: Option<UnlockProbeView>,
    pub v6: Option<UnlockProbeView>,
    #[serde(rename = "auto")]
    pub automatic: Option<UnlockProbeView>,
}

#[derive(Debug, Serialize)]
pub struct ChainUnlockProbePage {
    pub chains: Vec<ChainUnlockProbeItem>,
    pub next_cursor: Option<String>,
}

impl PgStore {
    /// A bounded read of visible chains. It never discovers, enqueues or resets diagnostics.
    /// Serving and node facts are loaded once per page; job freshness uses the same path
    /// comparison as the detail endpoint, with each immutable revision loaded only once.
    pub async fn chain_unlock_probe_page(
        &self,
        actor: &AdminContext,
        after: Option<&str>,
        limit: u32,
    ) -> Result<ChainUnlockProbePage> {
        if !(1..=100).contains(&limit) || after.is_some_and(|value| value.len() > 256) {
            return Err(StoreError::InvalidData("invalid unlock page".into()));
        }
        let mut chains = sqlx::query(
            "SELECT app_id,id FROM chains WHERE
            ($1 OR tenant_id=$2 OR tenant_id LIKE $3 ESCAPE '\\') AND ($4::text IS NULL OR id>$4)
            ORDER BY id LIMIT $5",
        )
        .bind(actor.is_system_admin())
        .bind(actor.tenant_scope())
        .bind(actor.tenant_scope_like_pattern())
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(self.pool())
        .await?;
        let next_cursor = if chains.len() > limit as usize {
            chains.truncate(limit as usize);
            chains.last().map(|row| row.get("id"))
        } else {
            None
        };
        if chains.is_empty() {
            return Ok(ChainUnlockProbePage {
                chains: vec![],
                next_cursor,
            });
        }
        let mut tx = serial(self.pool()).await?;
        // Serialize with reports and publication while comparing provenance. No probe credentials
        // or generated client configurations cross this read boundary.
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT topology_revision_id FROM subscription_serving_state WHERE id=TRUE FOR SHARE",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let serving = crate::serving::load_subscription_serving_projection(self.pool()).await?;
        serving.ensure_available()?;
        let nodes = serving
            .snapshot
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let facts = self.unlock_node_facts(&nodes).await?;
        let policy = super::schedule::policy(self.pool()).await?;
        let mut items = Vec::with_capacity(chains.len());
        let mut keys = Vec::with_capacity(chains.len() * 3);
        for row in chains {
            let app: String = row.try_get("app_id")?;
            let chain: String = row.try_get("id")?;
            let capabilities = Self::chain_unlock_capabilities_from_facts(
                &serving.snapshot,
                &facts,
                &app,
                &chain,
                policy,
            )?;
            for suffix in ["v4", "v6", "auto"] {
                keys.push(format!("chain:{app}/{chain}:{suffix}"));
            }
            items.push(ChainUnlockProbeItem {
                app_id: app,
                chain_id: chain,
                capabilities,
                probes: ChainUnlockProbeReports::default(),
            });
        }
        let rows = sqlx::query("SELECT *, extract(epoch FROM created_at)::bigint AS created_secs,
            extract(epoch FROM completed_at)::bigint AS completed_secs, extract(epoch FROM next_run_at)::bigint AS next_secs,
            extract(epoch FROM previous_completed_at)::bigint AS previous_secs, (SELECT unlock_detection_interval_hours*3600 FROM control_state WHERE id=TRUE) AS interval_secs FROM unlock_probe_jobs WHERE scope_key=ANY($1) FOR UPDATE")
            .bind(&keys).fetch_all(&mut *tx).await?;
        let mut snapshots = BTreeMap::new();
        let mut obsolete = Vec::new();
        let mut complete = Vec::new();
        let mut probes = BTreeMap::new();
        for row in rows {
            let key: String = row.try_get("scope_key")?;
            let mut view = decode(row)?;
            let same = if view.configuration_revision == revision {
                true
            } else if let (Some(before), Some(after)) = (view.configuration_revision, revision) {
                for id in [before, after] {
                    if let std::collections::btree_map::Entry::Vacant(entry) = snapshots.entry(id) {
                        let id = u64::try_from(id).map_err(|_| {
                            StoreError::InvalidData("invalid unlock snapshot revision".into())
                        })?;
                        entry.insert(
                            crate::materialize::load_immutable_snapshot_tx(&mut tx, id).await?,
                        );
                    }
                }
                path::equivalent(
                    &snapshots[&before],
                    &snapshots[&after],
                    &view.node_id,
                    view.app_id
                        .as_deref()
                        .ok_or_else(|| StoreError::InvalidData("missing unlock app".into()))?,
                    view.chain_id
                        .as_deref()
                        .ok_or_else(|| StoreError::InvalidData("missing unlock chain".into()))?,
                )?
            } else {
                false
            };
            if !same {
                view.state = UnlockProbeState::Obsolete;
                obsolete.push(key.clone());
            } else if view.state == UnlockProbeState::Obsolete && view.completed_at.is_some() {
                view.state = UnlockProbeState::Complete;
                complete.push(key.clone());
            }
            probes.insert(key, view);
        }
        for (state, keys) in [("obsolete", obsolete), ("complete", complete)] {
            if !keys.is_empty() {
                sqlx::query("UPDATE unlock_probe_jobs SET state=$1 WHERE scope_key=ANY($2)")
                    .bind(state)
                    .bind(keys)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        for item in &mut items {
            let key = format!("chain:{}/{}", item.app_id, item.chain_id);
            item.probes = ChainUnlockProbeReports {
                v4: probes.remove(&format!("{key}:v4")),
                v6: probes.remove(&format!("{key}:v6")),
                automatic: probes.remove(&format!("{key}:auto")),
            };
        }
        Ok(ChainUnlockProbePage {
            chains: items,
            next_cursor,
        })
    }
}
