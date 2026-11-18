//! Operational changes are projected only onto already approved instance lineages. A pending
//! edit of another instance on the same host must not freeze quota resets or physical cleanup.
use super::*;
use std::collections::BTreeSet;

pub(crate) async fn refresh_runtime(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision_id,payload FROM cvm_serving_nodes WHERE node_id=$1 FOR UPDATE",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((revision, payload)) = row else {
        return Ok(());
    };
    let mut serving = decode(node, revision, &payload)?;
    let live = live_tx(tx, node).await?;
    let pending: BTreeSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT instance_id FROM cvm_revision_instances WHERE revision_id>$1",
    )
    .bind(revision)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let pending_bindings: BTreeSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT binding_id FROM cvm_revision_bindings WHERE revision_id>$1",
    )
    .bind(revision)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    let pending_interfaces: BTreeSet<String> = serving
        .nic
        .bindings
        .iter()
        .filter(|b| pending_bindings.contains(&b.id))
        .map(|b| b.interface_id.clone())
        .collect();
    let old_nic = serde_json::to_value(&serving.nic)?;
    let mut instances = Vec::with_capacity(serving.instances.len());
    for mut instance in std::mem::take(&mut serving.instances) {
        if pending.contains(&instance.id) {
            if let Some(current) = live.instances.iter().find(|i| i.id == instance.id) {
                let mut changed = false;
                for device in &mut instance.spec.interfaces {
                    if let Some(nic) = &mut device.allocated {
                        let exceeded = nics::metering::quota_exceeded(
                            tx,
                            &device.id,
                            nic.limits.monthly_bytes,
                        )
                        .await?;
                        if nic.quota_exceeded != exceeded {
                            nic.quota_exceeded = exceeded;
                            changed = true;
                        }
                    }
                }
                if changed {
                    // Keep the approved configuration and the pending user intent as separate
                    // immutable execution generations. The staged branch stays newest so a later
                    // approval cannot replay a generation older than the Agent already executed.
                    let approved_revision = lineage_revision(tx, &instance.id, instance.generation)
                        .await?
                        .unwrap_or(revision);
                    let staged_revision = lineage_revision(tx, &current.id, current.generation)
                        .await?
                        .ok_or_else(|| invalid("pending CVM intent has no revision"))?;
                    instance.generation = current.generation.max(instance.generation);
                    super::super::operations::save_system_version(
                        tx,
                        &mut instance,
                        "network_traffic_quota",
                        false,
                    )
                    .await?;
                    record_lineage(tx, &instance, approved_revision).await?;
                    let mut staged = current.clone();
                    staged.generation = instance.generation;
                    super::super::operations::save_system_version(
                        tx,
                        &mut staged,
                        "network_traffic_quota",
                        true,
                    )
                    .await?;
                    record_lineage(tx, &staged, staged_revision).await?;
                }
            }
            instances.push(instance);
        } else if let Some(current) = live.instances.iter().find(|i| i.id == instance.id) {
            instances.push(current.clone());
        }
    }
    serving.instances = instances;
    let known_interfaces: BTreeSet<String> = serving
        .instances
        .iter()
        .flat_map(|i| i.spec.interfaces.iter().map(|n| n.id.clone()))
        .chain(serving.nic.bindings.iter().map(|b| b.interface_id.clone()))
        .collect();
    let mut bindings = Vec::with_capacity(serving.nic.bindings.len());
    for mut binding in std::mem::take(&mut serving.nic.bindings) {
        if pending_bindings.contains(&binding.id) {
            binding.nic.quota_exceeded = nics::metering::quota_exceeded(
                tx,
                &binding.interface_id,
                binding.nic.limits.monthly_bytes,
            )
            .await?;
            bindings.push(binding);
        } else if let Some(current) = live
            .nic
            .bindings
            .iter()
            .find(|b| b.id == binding.id && b.side == binding.side)
        {
            bindings.push(current.clone());
        }
    }
    serving.nic.bindings = bindings;
    let pending_interface = |id: &str| pending_interfaces.contains(id);
    let mut fences = serving
        .nic
        .fences
        .iter()
        .filter(|f| pending_interface(&f.interface_id))
        .cloned()
        .collect::<Vec<_>>();
    fences.extend(live.nic.fences.into_iter().filter(|f| {
        known_interfaces.contains(&f.interface_id) && !pending_interface(&f.interface_id)
    }));
    serving.nic.fences = fences;
    if serde_json::to_value(&serving.nic)? != old_nic {
        serving.nic.generation = serving
            .nic
            .generation
            .max(live.nic.generation)
            .checked_add(1)
            .ok_or_else(|| invalid("CVM network generation exhausted"))?;
    }
    // Entry topology stays frozen. Its runtime address and revocation overlays are rendered by
    // entry_desired_tx and have their own monotonic generation.
    let payload = encode(node, revision, &serving)?;
    sqlx::query("UPDATE cvm_serving_nodes SET payload=$2 WHERE node_id=$1")
        .bind(node)
        .bind(payload)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(super) async fn lineage_revision(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    generation: u64,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT revision_id FROM cvm_revision_instances WHERE instance_id=$1 AND generation<=$2 ORDER BY generation DESC LIMIT 1")
        .bind(id).bind(generation as i64).fetch_optional(&mut **tx).await?)
}

pub(super) async fn record_lineage(
    tx: &mut Transaction<'_, Postgres>,
    desired: &InstanceDesired,
    revision: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO cvm_revision_instances(instance_id,generation,revision_id) VALUES($1,$2,$3)",
    )
    .bind(&desired.id)
    .bind(desired.generation as i64)
    .bind(revision)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A later global revision may inherit an unchanged instance while its approved quota/runtime
/// generation advances. Never replay the older execution generation captured by that revision.
pub(crate) async fn project_for_approval(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    mut config: NodeConfig,
) -> Result<NodeConfig> {
    let live = live_tx(tx, node).await?;
    for instance in &mut config.instances {
        if let Some(current) = live
            .instances
            .iter()
            .find(|i| i.id == instance.id && i.generation >= instance.generation)
        {
            let revisions:Vec<Option<i64>>=sqlx::query_scalar("SELECT (SELECT revision_id FROM cvm_revision_instances WHERE instance_id=$1 AND generation<=g ORDER BY generation DESC LIMIT 1) FROM unnest($2::bigint[]) g")
                .bind(&instance.id).bind(vec![instance.generation as i64,current.generation as i64]).fetch_all(&mut **tx).await?;
            if revisions[0] == revisions[1] {
                *instance = current.clone();
            }
        }
    }
    for binding in &mut config.nic.bindings {
        if let Some(current) = live.nic.bindings.iter().find(|b| {
            b.id == binding.id && b.side == binding.side && b.generation >= binding.generation
        }) {
            let revisions:Vec<Option<i64>>=sqlx::query_scalar("SELECT (SELECT max(revision_id) FROM cvm_revision_bindings WHERE binding_id=$1 AND generation<=g) FROM unnest($2::bigint[]) g")
                .bind(&binding.id).bind(vec![binding.generation as i64,current.generation as i64]).fetch_all(&mut **tx).await?;
            if revisions[0] == revisions[1] {
                *binding = current.clone();
            }
        }
    }
    Ok(config)
}
