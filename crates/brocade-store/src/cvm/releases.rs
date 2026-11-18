//! CVM configuration uses the global revision and deployment lifecycle. The frozen input and
//! native products stay in their own compiler; no Cube runtime state enters brocade-core.
use super::*;
use brocade_deployment::plan::{DeploymentPlan, PlannedAction, PlannedTargetStatus};
use std::collections::BTreeMap;
mod runtime;
pub(crate) use runtime::refresh_runtime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_products: Vec<brocade_cvm_compile::host_network::HostProduct>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain_clients: Vec<ChainClientArtifact>,
    pub instances: Vec<InstanceDesired>,
    pub nic: NicDesired,
    pub entry: EntryDesired,
    pub entry_instances: BTreeMap<String, String>,
    pub publications: Vec<serde_json::Value>,
    pub entry_publications: Vec<serde_json::Value>,
}
fn context(node: &str, revision: i64) -> String {
    format!("cvm-revision:{node}:{revision}")
}
fn has_credentials(config: &NodeConfig) -> bool {
    !config.nic.bindings.is_empty()
        || !config.entry.tunnels.is_empty()
        || !config.chain_clients.is_empty()
}
fn encode(node: &str, revision: i64, config: &NodeConfig) -> Result<String> {
    let json = serde_json::to_string(config)?;
    if has_credentials(config) {
        crate::secrets::seal(&context(node, revision), &json)
    } else {
        Ok(format!("json:{json}"))
    }
}
pub(super) fn decode(node: &str, revision: i64, value: &str) -> Result<NodeConfig> {
    if let Some(json) = value.strip_prefix("json:") {
        let config: NodeConfig = serde_json::from_str(json)?;
        if has_credentials(&config) {
            return Err(invalid("unsealed CVM network credentials"));
        }
        Ok(config)
    } else {
        Ok(serde_json::from_str(&crate::secrets::open(
            &context(node, revision),
            value,
        )?)?)
    }
}
async fn live_tx(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<NodeConfig> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT desired FROM cvm_instances WHERE node_id=$1 AND reserved ORDER BY id LIMIT 256",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    let instances: Vec<InstanceDesired> = rows
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()?;
    let chain_clients = super::chain_egress::freeze_clients(tx, node, &instances).await?;
    let mut nic = nics::bindings::desired_tx(tx, node).await?;
    // Counters and host safety addresses are runtime observations, never revision inputs.
    nic.budgets.clear();
    nic.management_networks.clear();
    let entry = nics::entries::desired::desired_tx(tx, node, true).await?;
    let entry_instances = sqlx::query_as::<_, (String,String)>("SELECT e.id,e.instance_id FROM cvm_entries e JOIN cvm_instances i ON i.id=e.instance_id WHERE (e.provider_node=$1 OR i.node_id=$1) AND NOT e.released ORDER BY e.id")
        .bind(node).fetch_all(&mut **tx).await?.into_iter().collect();
    let publications:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'revision',revision,'admitting',admitting,'deleted',deleted,'spec',spec) FROM cvm_nic_publications WHERE spec->>'provider_node'=$1 ORDER BY id").bind(node).fetch_all(&mut **tx).await?;
    let entry_publications:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'revision',revision,'admitting',admitting,'deleted',deleted,'spec',spec) FROM cvm_entry_publications WHERE spec->>'provider_node'=$1 ORDER BY id").bind(node).fetch_all(&mut **tx).await?;
    let host_products = brocade_cvm_compile::host_network::compile(&nic, &entry, &chain_clients)
        .map_err(invalid)?;
    Ok(NodeConfig {
        host_products,
        chain_clients,
        publications,
        entry_publications,
        instances,
        nic,
        entry,
        entry_instances,
    })
}

/// Topology and client projection edits also freeze their dependent host clients in the
/// same global R. Merely recompiling an old revision during planning is never sufficient.
pub(crate) async fn freeze_chain_dependents(
    tx: &mut Transaction<'_, Postgres>,
    revision: u64,
) -> Result<()> {
    let nodes:Vec<String>=sqlx::query_scalar("SELECT DISTINCT node_id FROM cvm_instances WHERE reserved AND EXISTS(SELECT 1 FROM jsonb_array_elements(desired#>'{spec,routing,routes}') r WHERE r ? 'chain_id') ORDER BY node_id")
        .fetch_all(&mut **tx).await?;
    for node in nodes {
        let previous:Option<(i64,String)>=sqlx::query_as("SELECT revision_id,payload FROM cvm_revision_nodes WHERE node_id=$1 AND revision_id<=$2 ORDER BY revision_id DESC LIMIT 1")
            .bind(&node).bind(revision as i64).fetch_optional(&mut **tx).await?;
        let config = live_tx(tx, &node).await?;
        if let Some((old_revision, payload)) = previous {
            let old = decode(&node, old_revision, &payload)?;
            if old.chain_clients == config.chain_clients {
                continue;
            }
            if old_revision == revision as i64 {
                return Err(invalid("同一全局修订的线路客户端输入不一致"));
            }
        }
        let payload = encode(&node, revision as i64, &config)?;
        let sha = sha256_hex(&serde_json::to_vec(&config)?);
        sqlx::query("INSERT INTO cvm_revision_nodes(revision_id,node_id,sha256,payload) VALUES($1,$2,$3,$4)")
            .bind(revision as i64).bind(&node).bind(sha).bind(payload).execute(&mut **tx).await?;
    }
    Ok(())
}
#[derive(Default)]
pub(crate) struct Scope<'a> {
    pub instances: Vec<&'a str>,
    pub nodes: Vec<&'a str>,
    pub nic_publications: Vec<&'a str>,
    pub entry_publications: Vec<&'a str>,
    pub entries: Vec<&'a str>,
}
impl<'a> Scope<'a> {
    pub fn instance(id: &'a str) -> Self {
        Self {
            instances: vec![id],
            ..Self::default()
        }
    }
    pub fn node(node: &'a str) -> Self {
        Self {
            nodes: vec![node],
            ..Self::default()
        }
    }
}
async fn scope_nodes(tx: &mut Transaction<'_, Postgres>, scope: Scope<'_>) -> Result<Vec<String>> {
    let instances:Vec<String>=sqlx::query_scalar("SELECT id FROM cvm_instances WHERE id=ANY($1) UNION SELECT instance_id FROM cvm_nic_allocations WHERE publication_id=ANY($2) AND NOT released UNION SELECT instance_id FROM cvm_entries WHERE (publication_id=ANY($3) OR id=ANY($4)) AND NOT cleaned")
        .bind(scope.instances).bind(&scope.nic_publications).bind(&scope.entry_publications).bind(scope.entries).fetch_all(&mut **tx).await?;
    let mut nodes:Vec<String>=sqlx::query_scalar("SELECT node_id FROM cvm_instances WHERE id=ANY($1) UNION SELECT provider_node FROM cvm_nic_bindings WHERE instance_id=ANY($1) AND NOT cleaned UNION SELECT consumer_node FROM cvm_nic_bindings WHERE instance_id=ANY($1) AND NOT cleaned UNION SELECT provider_node FROM cvm_entries WHERE instance_id=ANY($1) AND NOT cleaned UNION SELECT spec->>'provider_node' FROM cvm_nic_publications WHERE id=ANY($2) UNION SELECT spec->>'provider_node' FROM cvm_entry_publications WHERE id=ANY($3) UNION SELECT id FROM nodes WHERE id=ANY($4)")
        .bind(instances).bind(scope.nic_publications).bind(scope.entry_publications).bind(scope.nodes).fetch_all(&mut **tx).await?;
    nodes.sort();
    nodes.dedup();
    Ok(nodes)
}

pub(crate) type Before = BTreeMap<String, NodeConfig>;

/// Called under the existing resource lock, before configuration writes. First use adopts the
/// pre-upgrade running intent as the baseline, so merely upgrading Console never unpublishes it.
pub(crate) async fn begin(tx: &mut Transaction<'_, Postgres>, scope: Scope<'_>) -> Result<Before> {
    let nodes = scope_nodes(tx, scope).await?;
    let revision: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(&mut **tx)
            .await?;
    let mut before = BTreeMap::new();
    for node in nodes {
        let config = live_tx(tx, &node).await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_serving_nodes WHERE node_id=$1)")
                .bind(&node)
                .fetch_one(&mut **tx)
                .await?;
        if !exists {
            let payload = encode(&node, revision, &config)?;
            sqlx::query("INSERT INTO cvm_serving_nodes(node_id,revision_id,payload,entry_generation) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(&node).bind(revision).bind(payload).bind(config.entry.generation as i64).execute(&mut **tx).await?;
        }
        before.insert(node, config);
    }
    Ok(before)
}

/// Atomically commits changed CVM intent with the same revision counter and model snapshot used
/// by every other configuration writer. No new release/version domain is introduced.
pub(crate) async fn commit(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    before: Before,
) -> Result<()> {
    let nodes: Vec<String> = before.keys().cloned().collect();
    let mut changed = Vec::new();
    for node in nodes {
        let current = live_tx(tx, &node).await?;
        if before.get(&node).map(serde_json::to_value).transpose()?
            != Some(serde_json::to_value(&current)?)
        {
            changed.push((node, current));
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    let nodes: Vec<String> = changed.iter().map(|(node, _)| node.clone()).collect();
    let busy:Option<i64>=sqlx::query_scalar("SELECT d.id FROM deployments d JOIN deployment_targets t ON t.deployment_id=d.id JOIN deployment_target_state s ON s.deployment_id=t.deployment_id AND s.node_id=t.node_id WHERE d.kind='config' AND d.active=TRUE AND s.cvm_revision_id IS NOT NULL AND t.node_id=ANY($1) AND t.status IN ('pending','dispatched','converging','failed-recovered','failed-dirty') ORDER BY d.id LIMIT 1").bind(&nodes).fetch_optional(&mut **tx).await?;
    if let Some(id) = busy {
        return Err(StoreError::Conflict(format!(
            "关联机器正在执行配置发布单 #{id}，请先完成或取消该发布单，再保存容器变更"
        )));
    }
    let mut instance_ids = Vec::new();
    for (node, config) in &changed {
        for instance in &config.instances {
            if before.get(node).is_none_or(|old| {
                !old.instances
                    .iter()
                    .any(|i| i.id == instance.id && i.generation == instance.generation)
            }) {
                instance_ids.push(instance.id.clone());
            }
        }
    }
    // Power/restart changes also move the address used by remote entry machines. Freeze those
    // unchanged topologies in this same revision so their new runtime delivery gets a receipt.
    let dependents:Vec<String>=sqlx::query_scalar("SELECT DISTINCT e.provider_node FROM cvm_entries e WHERE e.instance_id=ANY($1) AND NOT e.cleaned").bind(instance_ids).fetch_all(&mut **tx).await?;
    for node in dependents {
        if !changed.iter().any(|(n, _)| n == &node) {
            let config = live_tx(tx, &node).await?;
            changed.push((node, config));
        }
    }

    let previous = crate::console::lock_control_state(tx).await?;
    let revision =
        crate::console::insert_revision(tx, actor.operator_id(), "容器与关联网络配置").await?;
    for (node, config) in changed {
        for instance in &config.instances {
            if before.get(&node).is_none_or(|old| {
                !old.instances
                    .iter()
                    .any(|i| i.id == instance.id && i.generation == instance.generation)
            }) {
                sqlx::query("INSERT INTO cvm_revision_instances(instance_id,generation,revision_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&instance.id).bind(instance.generation as i64).bind(revision as i64).execute(&mut **tx).await?;
            }
        }
        for binding in &config.nic.bindings {
            if before.get(&node).is_none_or(|old| {
                !old.nic
                    .bindings
                    .iter()
                    .any(|b| b.id == binding.id && b.generation == binding.generation)
            }) {
                sqlx::query("INSERT INTO cvm_revision_bindings(binding_id,generation,revision_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&binding.id).bind(binding.generation as i64).bind(revision as i64).execute(&mut **tx).await?;
            }
        }
        let payload = encode(&node, revision as i64, &config)?;
        let sha = sha256_hex(&serde_json::to_vec(&config)?);
        sqlx::query("INSERT INTO cvm_revision_nodes(revision_id,node_id,sha256,payload) VALUES($1,$2,$3,$4)")
            .bind(revision as i64).bind(&node).bind(sha).bind(payload).execute(&mut **tx).await?;
    }
    crate::console::commit_revision(tx, revision, previous, true).await?;
    Ok(())
}

pub(crate) async fn serving_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<Option<NodeConfig>> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT revision_id,payload FROM cvm_serving_nodes WHERE node_id=$1")
            .bind(node)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(r, p)| decode(node, r, &p)).transpose()
}

pub(crate) async fn entry_desired_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<EntryDesired> {
    let Some(config) = serving_tx(tx, node).await? else {
        return nics::entries::desired::desired_tx(tx, node, false).await;
    };
    let mut entry = config.entry;
    nics::entries::validate_serving(tx, &entry).await?;
    let ids: Vec<String> = config.entry_instances.values().cloned().collect();
    let addresses: BTreeMap<String,Option<std::net::Ipv4Addr>> = sqlx::query_as::<_,(String,Option<String>)>("SELECT id,CASE WHEN report->>'power'='running' THEN report->>'access_address' ELSE NULL END FROM cvm_instances WHERE id=ANY($1)")
        .bind(ids).fetch_all(&mut **tx).await?.into_iter().map(|(id,a)| (id,a.and_then(|a| a.parse().ok()))).collect();
    let grants:BTreeMap<String,bool>=sqlx::query_as("SELECT e.id,EXISTS(SELECT 1 FROM jsonb_array_elements(p.spec->'grants') g WHERE g->>'tenant_id'=i.tenant_id AND g->>'user_id'=i.user_id) FROM cvm_entries e JOIN cvm_entry_publications p ON p.id=e.publication_id JOIN cvm_instances i ON i.id=e.instance_id WHERE e.id=ANY($1)")
        .bind(config.entry_instances.keys().cloned().collect::<Vec<_>>()).fetch_all(&mut **tx).await?.into_iter().collect();
    for rule in &mut entry.rules {
        rule.target = rule
            .target
            .filter(|_| grants.get(&rule.id) == Some(&true))
            .and_then(|_| {
                config
                    .entry_instances
                    .get(&rule.id)
                    .and_then(|id| addresses.get(id).copied().flatten())
            });
    }
    entry.deliveries.retain_mut(|delivery| {
        if let Some(address) = addresses
            .get(&delivery.instance_id)
            .copied()
            .flatten()
            .filter(|_| grants.get(&delivery.id) == Some(&true))
        {
            delivery.target = address;
            true
        } else {
            false
        }
    });
    // Keep node-local execution generations monotonic across approvals and target-address changes.
    entry.generation = 0;
    let sha = sha256_hex(&serde_json::to_vec(&entry)?);
    let generation: i64=sqlx::query_scalar("UPDATE cvm_serving_nodes SET entry_generation=CASE WHEN entry_sha256 IS DISTINCT FROM $2 THEN greatest(entry_generation,coalesce((SELECT desired_generation FROM cvm_entry_nodes WHERE node_id=$1),0))+1 ELSE entry_generation END,entry_sha256=$2,entry_ids=$3,tunnel_ids=$4 WHERE node_id=$1 RETURNING entry_generation")
        .bind(node).bind(sha).bind(entry.rules.iter().map(|r|r.id.clone()).collect::<Vec<_>>()).bind(entry.tunnels.iter().map(|t|t.id.clone()).collect::<Vec<_>>()).fetch_one(&mut **tx).await?;
    entry.generation = generation as u64;
    entry.validate().map_err(invalid)?;
    Ok(entry)
}

/// The ordinary planner and CVM compiler remain separate; the release envelope is shared.
pub(crate) async fn augment_plan(pool: &sqlx::PgPool, plan: &mut DeploymentPlan) -> Result<()> {
    let future: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_revision_nodes WHERE revision_id>$1)")
            .bind(plan.revision as i64)
            .fetch_one(pool)
            .await?;
    if future {
        return Err(StoreError::Unsupported(
            "容器变更请使用最新全局修订；历史执行代须通过新的补偿修订发布".into(),
        ));
    }
    let stale:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_serving_nodes s WHERE s.revision_id>$1 AND EXISTS(SELECT 1 FROM cvm_revision_nodes r WHERE r.node_id=s.node_id))").bind(plan.revision as i64).fetch_one(pool).await?;
    if stale {
        return Err(StoreError::Unsupported(
            "历史容器配置不能以旧执行代重新下发；请创建新的补偿修订后发布".into(),
        ));
    }
    let changed: Vec<(String,i64)> = sqlx::query_as("SELECT DISTINCT ON (r.node_id) r.node_id,r.revision_id FROM cvm_revision_nodes r LEFT JOIN cvm_serving_nodes s ON s.node_id=r.node_id WHERE r.revision_id<=$1 ORDER BY r.node_id,r.revision_id DESC")
        .bind(plan.revision as i64).fetch_all(pool).await?;
    let mut dependencies = Vec::new();
    for (node, revision) in changed {
        let serving: Option<(i64, String)> =
            sqlx::query_as("SELECT revision_id,payload FROM cvm_serving_nodes WHERE node_id=$1")
                .bind(&node)
                .fetch_optional(pool)
                .await?;
        if serving.as_ref().is_some_and(|(r, _)| *r == revision) {
            continue;
        }
        let previous = serving.map(|(r, p)| decode(&node, r, &p)).transpose()?;
        if let Some(old) = &previous {
            for binding in &old.nic.bindings {
                dependencies.push((binding.provider_node.clone(), binding.consumer_node.clone()));
            }
            for tunnel in &old.entry.tunnels {
                dependencies.push((tunnel.entry_node.clone(), tunnel.target_node.clone()));
            }
        }
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM cvm_revision_nodes WHERE node_id=$1 AND revision_id=$2",
        )
        .bind(&node)
        .bind(revision)
        .fetch_one(pool)
        .await?;
        let config = decode(&node, revision, &payload)?;
        for binding in &config.nic.bindings {
            dependencies.push((binding.provider_node.clone(), binding.consumer_node.clone()));
        }
        for tunnel in &config.entry.tunnels {
            dependencies.push((tunnel.entry_node.clone(), tunnel.target_node.clone()));
        }
        for client in &config.chain_clients {
            dependencies.push((node.clone(), client.ingress_node.clone()));
        }
        let disruptive = previous.as_ref().is_some_and(|old| {
            old.instances.iter().any(|prior| {
                config
                    .instances
                    .iter()
                    .find(|i| i.id == prior.id)
                    .is_none_or(|next| {
                        prior.power != next.power
                            || prior.restart_epoch != next.restart_epoch
                            || prior.reinstall_epoch != next.reinstall_epoch
                            || prior.spec.resources != next.spec.resources
                            || prior.spec.image != next.spec.image
                            || prior.spec.ssh_public_key != next.spec.ssh_public_key
                    })
            })
        });
        if let Some(target) = plan.targets.iter_mut().find(|t| t.node_id == node) {
            target.actions.push(PlannedAction::SyncCvm);
            if target.status == PlannedTargetStatus::Skipped {
                target.status = PlannedTargetStatus::Pending;
            }
            target.disruptive |= disruptive;
        }
    }
    // Even a tunnel created and withdrawn before its first publication retains physical
    // cleanup reservations. Both ends must receive the withdrawal in the same wave.
    let cleanup_pairs: Vec<(String, String)> = sqlx::query_as(
        "SELECT entry_node,target_node FROM cvm_entry_tunnels WHERE withdrawn AND NOT cleaned",
    )
    .fetch_all(pool)
    .await?;
    dependencies.extend(cleanup_pairs);
    let executing_nodes: std::collections::BTreeSet<_> = plan
        .targets
        .iter()
        .filter(|t| t.status != PlannedTargetStatus::Skipped)
        .map(|t| t.node_id.clone())
        .collect();
    for (a, b) in dependencies {
        if a == b || !executing_nodes.contains(&a) || !executing_nodes.contains(&b) {
            continue;
        }
        for target in &mut plan.targets {
            if target.node_id == a {
                target.prerequisites.push(b.clone());
            }
            if target.node_id == b {
                target.prerequisites.push(a.clone());
            }
            target.prerequisites.sort();
            target.prerequisites.dedup();
        }
    }
    plan.summary = brocade_deployment::plan::summarize_targets(&plan.targets);
    Ok(())
}

pub(crate) async fn freeze_target(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
    revision: i64,
) -> Result<()> {
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM cvm_revision_nodes WHERE revision_id>$1)",
    )
    .bind(revision)
    .fetch_one(&mut **tx)
    .await?
    {
        return Err(StoreError::Conflict(
            "容器配置已变化，请重新预览最新全局修订".into(),
        ));
    }
    let selected: Option<i64> = sqlx::query_scalar(
        "SELECT max(revision_id) FROM cvm_revision_nodes WHERE node_id=$1 AND revision_id<=$2",
    )
    .bind(node)
    .bind(revision)
    .fetch_one(&mut **tx)
    .await?;
    let serving: Option<i64> =
        sqlx::query_scalar("SELECT revision_id FROM cvm_serving_nodes WHERE node_id=$1")
            .bind(node)
            .fetch_optional(&mut **tx)
            .await?;
    if selected
        .zip(serving)
        .is_some_and(|(selected, serving)| selected < serving)
    {
        return Err(StoreError::Conflict(
            "容器发布基线已变化，请重新预览最新全局修订".into(),
        ));
    }
    sqlx::query("UPDATE deployment_target_state SET cvm_revision_id=$3 WHERE deployment_id=$1 AND node_id=$2").bind(deployment).bind(node).bind(selected).execute(&mut **tx).await?;
    Ok(())
}

/// Only the existing executable-wave claim calls this. Merely creating or previewing a work
/// order cannot move any of the three CVM desired endpoints.
pub(crate) async fn activate_target(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
) -> Result<()> {
    let row:Option<(i64,String)>=sqlx::query_as("SELECT r.revision_id,r.payload FROM deployment_target_state t JOIN cvm_revision_nodes r ON r.revision_id=t.cvm_revision_id AND r.node_id=t.node_id WHERE t.deployment_id=$1 AND t.node_id=$2").bind(deployment).bind(node).fetch_optional(&mut **tx).await?;
    let Some((revision, payload)) = row else {
        return Ok(());
    };
    let previous: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision_id,payload FROM cvm_serving_nodes WHERE node_id=$1 FOR UPDATE",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?;
    if previous.as_ref().is_some_and(|(r, _)| *r == revision) {
        return Ok(());
    }
    let mut config =
        runtime::project_for_approval(tx, node, decode(node, revision, &payload)?).await?;
    let old = previous.map(|(r, p)| decode(node, r, &p)).transpose()?;
    let old_generation = old.as_ref().map_or(0, |c| c.nic.generation);
    if config.nic.generation > 0
        || old_generation > 0
        || !config.nic.bindings.is_empty()
        || !config.nic.fences.is_empty()
    {
        config.nic.generation = config
            .nic
            .generation
            .max(old_generation)
            .checked_add(1)
            .ok_or_else(|| invalid("CVM network generation exhausted"))?;
    }
    let payload = encode(node, revision, &config)?;
    sqlx::query("INSERT INTO cvm_serving_nodes(node_id,revision_id,payload) VALUES($1,$2,$3) ON CONFLICT(node_id) DO UPDATE SET revision_id=excluded.revision_id,payload=excluded.payload,entry_sha256=NULL")
        .bind(node).bind(revision).bind(payload).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn can_claim(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
    protocol: u32,
) -> Result<bool> {
    let cvm: bool=sqlx::query_scalar("SELECT cvm_revision_id IS NOT NULL FROM deployment_target_state WHERE deployment_id=$1 AND node_id=$2").bind(deployment).bind(node).fetch_one(&mut **tx).await?;
    let chain:bool=sqlx::query_scalar("SELECT coalesce(jsonb_array_length(desired_structure#>'{cvm,chain_clients}'),0)>0 FROM deployment_target_state WHERE deployment_id=$1 AND node_id=$2")
        .bind(deployment).bind(node).fetch_one(&mut **tx).await?;
    Ok(
        (!cvm || protocol >= brocade_deployment::cvm::CVM_PROTOCOL_VERSION)
            && (!chain || protocol >= CHAIN_EGRESS_PROTOCOL_VERSION),
    )
}

pub(crate) async fn artifact_metadata(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    revision: i64,
) -> Result<serde_json::Value> {
    let release_revision = revision;
    let row:Option<(i64,String)>=sqlx::query_as("SELECT revision_id,payload FROM cvm_revision_nodes WHERE node_id=$1 AND revision_id<=$2 ORDER BY revision_id DESC LIMIT 1").bind(node).bind(revision).fetch_optional(&mut **tx).await?;
    let Some((revision, payload)) = row else {
        return Ok(serde_json::json!([]));
    };
    let config = runtime::project_for_approval(tx, node, decode(node, revision, &payload)?).await?;
    let before = serving_tx(tx, node).await?;
    let mut host_products = Vec::new();
    let old_products = before
        .as_ref()
        .map(|b| b.host_products.as_slice())
        .unwrap_or_default();
    for file in &config.host_products {
        let old = old_products.iter().find(|p| p.kind == file.kind);
        if old.is_some_and(|p| p.sha256 == file.sha256) {
            continue;
        }
        host_products.push(serde_json::json!({"kind":file.kind,"sha256":file.sha256,"content":file.content,"redacted":file.redacted,"apply":file.apply,"bindings":file.bindings,"before_sha256":old.map(|p|&p.sha256),"before_content":old.map(|p|&p.content),"before_absent":old.is_none()}));
    }
    for old in old_products
        .iter()
        .filter(|p| !config.host_products.iter().any(|n| n.kind == p.kind))
    {
        host_products.push(serde_json::json!({"kind":old.kind,"absent":true,"before_sha256":old.sha256,"before_content":old.content,"apply":old.apply,"redacted":old.redacted}));
    }
    let mut items = Vec::new();
    for instance in config.instances {
        let previous = before
            .as_ref()
            .and_then(|b| b.instances.iter().find(|i| i.id == instance.id));
        if previous.is_some_and(|i| {
            i.generation == instance.generation && i.operation_id == instance.operation_id
        }) {
            continue;
        }
        let previous_generation = previous.map(|i| i.generation);
        let old = if let Some(previous) = previous {
            artifacts::load(tx, &previous.id, previous.generation).await?
        } else {
            None
        };
        let files = artifacts::load(tx, &instance.id, instance.generation).await?.map(|bundle| bundle.files.into_iter().filter_map(|file| {
            let prior=old.as_ref().and_then(|b|b.file(file.kind).ok());
            if prior.is_some_and(|p|p.sha256==file.sha256) {None} else {Some(serde_json::json!({"kind":file.kind,"sha256":file.sha256,"before_sha256":prior.map(|p|&p.sha256),"before_absent":previous.is_none()}))}
        }).collect::<Vec<_>>()).unwrap_or_default();
        let owner: (String, String) =
            sqlx::query_as("SELECT tenant_id,user_id FROM cvm_instances WHERE id=$1")
                .bind(&instance.id)
                .fetch_one(&mut **tx)
                .await?;
        items.push(serde_json::json!({"id":instance.id,"tenant_id":owner.0,"user_id":owner.1,"name":instance.spec.name,"generation":instance.generation,"previous_generation":previous_generation,"operation_id":instance.operation_id,"power":instance.power,"impact":if previous.is_none(){"create"}else if previous.is_some_and(|p|p.power!=instance.power || p.restart_epoch!=instance.restart_epoch || p.reinstall_epoch!=instance.reinstall_epoch){"power"}else if previous.is_some_and(|p|p.spec.resources!=instance.spec.resources || p.spec.image!=instance.spec.image){"stopped"}else if previous.is_some_and(|p|p.spec.ssh_public_key!=instance.spec.ssh_public_key){"boot_identity"}else{"hot"},"files":files}));
    }
    Ok(
        serde_json::json!({"revision_id":release_revision,"source_revision_id":revision,"instances":items,"host_products":host_products,"network_bindings":config.nic.bindings.len(),"entries":config.entry.rules.len()+config.entry.deliveries.len(),"chain_clients":config.chain_clients.iter().map(|c|serde_json::json!({"instance_id":c.instance_id,"chain_id":c.chain_id,"tenant_id":c.tenant_id,"user_id":c.user_id,"ingress_node":c.ingress_node,"sha256":c.sha256,"redacted":c.redacted})).collect::<Vec<_>>()}),
    )
}

async fn result_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
) -> Result<&'static str> {
    let Some(config) = serving_tx(tx, node).await? else {
        return Ok("succeeded");
    };
    let changed:Vec<String>=sqlx::query_scalar("SELECT item->>'id' FROM deployment_target_state s CROSS JOIN LATERAL jsonb_array_elements(coalesce(s.desired_structure#>'{cvm,instances}','[]'::jsonb)) item WHERE s.deployment_id=$1 AND s.node_id=$2").bind(deployment).bind(node).fetch_all(&mut **tx).await?;
    let mut pending = false;
    for expected in &config.chain_clients {
        let report:Option<serde_json::Value>=sqlx::query_scalar("SELECT report FROM cvm_instances WHERE id=$1 AND node_id=$2 AND updated_at>=(SELECT dispatched_at FROM deployment_target_state WHERE deployment_id=$3 AND node_id=$2)")
            .bind(&expected.instance_id).bind(node).bind(deployment).fetch_optional(&mut **tx).await?.flatten();
        let receipt = report
            .as_ref()
            .and_then(|r| r["chain_egress"].as_array())
            .and_then(|items| {
                items.iter().find(|r| {
                    r["chain_id"] == expected.chain_id
                        && r["sha256"] == expected.sha256
                        && r["guard_sha256"] == sha256_hex(expected.guard.as_bytes())
                })
            });
        if receipt.is_none_or(|r| r["configured"] != true) {
            pending = true;
        }
    }
    for instance in config.instances.iter().filter(|i| changed.contains(&i.id)) {
        let recovered = transport_fell_back(tx, deployment, node, instance).await?;
        if recovered {
            // The fallback is desired; only a later native receipt can prove it. Keep this
            // release failed until a new compensating configuration is approved.
            return Ok("failed-dirty");
        }
        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT i.report FROM cvm_instances i JOIN cvm_operations o ON o.id=$3 WHERE i.id=$1 AND i.node_id=$2 AND o.status=i.report->>'status' AND o.updated_at>=(SELECT dispatched_at FROM deployment_target_state WHERE deployment_id=$4 AND node_id=$2)")
                .bind(&instance.id)
                .bind(node)
                .bind(&instance.operation_id)
                .bind(deployment)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
        let report = raw
            .map(serde_json::from_value::<InstanceReport>)
            .transpose()?;
        match report.filter(|r| {
            r.generation == instance.generation && r.operation_id == instance.operation_id
        }) {
            Some(r) if r.status == ApplyStatus::Applied => {}
            Some(r) if r.status == ApplyStatus::Failed => return Ok("failed-dirty"),
            _ => pending = true,
        }
    }
    if config.nic.generation > 0 {
        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT report FROM cvm_nic_nodes WHERE node_id=$1 AND report_received_at>=(SELECT dispatched_at FROM deployment_target_state WHERE deployment_id=$2 AND node_id=$1)")
                .bind(node)
                .bind(deployment)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
        let report = raw
            .map(serde_json::from_value::<NicNodeReport>)
            .transpose()?;
        if let Some(report) = report.filter(|r| r.generation == config.nic.generation) {
            for binding in &config.nic.bindings {
                match report.bindings.iter().find(|r| {
                    r.id == binding.id
                        && r.side == binding.side
                        && r.generation == binding.generation
                        && r.phase == binding.phase
                }) {
                    Some(r) if r.error.is_some() => return Ok("failed-dirty"),
                    Some(r)
                        if r.configured
                            && binding.phase != BindingPhase::Preparing
                            && binding.phase != BindingPhase::Standby
                            && (binding.phase == BindingPhase::Withdrawing
                                || binding.nic.is_blocked()
                                || r.reachable == Some(true)) => {}
                    _ => pending = true,
                }
            }
        } else {
            pending = true;
        }
    }
    if config.entry.generation > 0 {
        let desired = match entry_desired_tx(tx, node).await {
            Ok(desired) => desired,
            Err(StoreError::Unsupported(_) | StoreError::Conflict(_)) => return Ok("failed-dirty"),
            Err(error) => return Err(error),
        };
        for rule in &config.entry.rules {
            if rule.target.is_some() {
                if let Some(id) = config.entry_instances.get(&rule.id) {
                    if wants_address(tx, id).await?
                        && !desired
                            .rules
                            .iter()
                            .any(|r| r.id == rule.id && r.target.is_some())
                    {
                        pending = true;
                    }
                }
            }
        }
        for delivery in &config.entry.deliveries {
            if wants_address(tx, &delivery.instance_id).await?
                && !desired.deliveries.iter().any(|d| d.id == delivery.id)
            {
                pending = true;
            }
        }

        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT report FROM cvm_entry_nodes WHERE node_id=$1 AND reported_at>=(SELECT dispatched_at FROM deployment_target_state WHERE deployment_id=$2 AND node_id=$1)")
                .bind(node)
                .bind(deployment)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
        match raw
            .map(serde_json::from_value::<EntryNodeReport>)
            .transpose()?
            .filter(|r| r.generation == desired.generation)
        {
            Some(r)
                if r.error.is_some()
                    || r.rules
                        .iter()
                        .chain(&r.deliveries)
                        .any(|r| r.error.is_some())
                    || r.tunnels.iter().any(|t| t.error.is_some()) =>
            {
                return Ok("failed-dirty")
            }
            Some(r)
                if desired
                    .rules
                    .iter()
                    .all(|d| r.rules.iter().any(|a| a.id == d.id && a.configured))
                    && desired
                        .deliveries
                        .iter()
                        .all(|d| r.deliveries.iter().any(|a| a.id == d.id && a.configured))
                    && desired
                        .tunnels
                        .iter()
                        .all(|d| r.tunnels.iter().any(|a| a.id == d.id && a.connected)) => {}
            _ => pending = true,
        }
    }
    Ok(if pending { "converging" } else { "succeeded" })
}

pub(crate) async fn machine_result(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
    status: &'static str,
) -> Result<&'static str> {
    if status != "succeeded" {
        return Ok(status);
    }
    let cvm:bool=sqlx::query_scalar("UPDATE deployment_target_state SET cvm_machine_ready=true WHERE deployment_id=$1 AND node_id=$2 RETURNING cvm_revision_id IS NOT NULL").bind(deployment).bind(node).fetch_one(&mut **tx).await?;
    if cvm {
        activate_target(tx, deployment, node).await?;
        result_tx(tx, deployment, node).await
    } else {
        Ok(status)
    }
}

pub(crate) async fn settle(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<()> {
    // CVM reports take the common resource lock before this function; ordinary reports take it
    // before deployment locks as well. This preserves the single global lock order.
    let rows:Vec<(i64,String)>=sqlx::query_as("SELECT d.id,d.status FROM deployments d JOIN deployment_targets t ON t.deployment_id=d.id JOIN deployment_target_state s ON s.deployment_id=t.deployment_id AND s.node_id=t.node_id JOIN cvm_serving_nodes c ON c.node_id=t.node_id AND c.revision_id=s.cvm_revision_id WHERE t.node_id=$1 AND d.active=TRUE AND d.status IN ('planned','running','halted') AND t.status IN ('dispatched','converging') AND s.cvm_machine_ready FOR UPDATE OF d,t,s")
        .bind(node).fetch_all(&mut **tx).await?;
    for (deployment, status) in rows {
        let result = result_tx(tx, deployment, node).await?;
        if result == "converging" {
            continue;
        }
        sqlx::query("UPDATE deployment_targets SET status=$3,error=CASE WHEN $3='failed-dirty' THEN '容器或关联网络执行失败，请查看网络日志和产物回执' ELSE error END WHERE deployment_id=$1 AND node_id=$2").bind(deployment).bind(node).bind(result).execute(&mut **tx).await?;
        record_evidence(tx, deployment, node, result).await?;
        crate::deployment::refresh_deployment_status_after_report(
            tx,
            deployment,
            result,
            status == "halted",
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn lock(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    crate::deployment::lock_deployment_isolation_tx(tx).await?;
    crate::resource_coordination::lock(tx).await
}

async fn wants_address(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<bool> {
    let Some((node, raw)) = sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT node_id,desired FROM cvm_instances WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(false);
    };
    let desired = if let Some(config) = serving_tx(tx, &node).await? {
        config.instances.into_iter().find(|i| i.id == id)
    } else {
        Some(serde_json::from_value(raw)?)
    };
    Ok(desired.is_some_and(|i| i.power == PowerState::Running))
}

/// Freeze receipt identity alongside the existing ordinary observation, so a later operation
/// cannot replace the evidence shown by a historical mixed release.
pub(crate) async fn record_evidence(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
    status: &str,
) -> Result<()> {
    let enabled:bool=sqlx::query_scalar("SELECT cvm_revision_id IS NOT NULL FROM deployment_target_state WHERE deployment_id=$1 AND node_id=$2").bind(deployment).bind(node).fetch_one(&mut **tx).await?;
    if !enabled {
        return Ok(());
    }
    let Some(config) = serving_tx(tx, node).await? else {
        return Ok(());
    };
    #[derive(Deserialize)]
    struct FrozenInstance {
        id: String,
        generation: u64,
        operation_id: String,
    }
    let frozen:serde_json::Value=sqlx::query_scalar("SELECT coalesce(desired_structure#>'{cvm,instances}','[]'::jsonb) FROM deployment_target_state WHERE deployment_id=$1 AND node_id=$2")
        .bind(deployment).bind(node).fetch_one(&mut **tx).await?;
    let frozen: Vec<FrozenInstance> = serde_json::from_value(frozen)?;
    let mut instances = Vec::new();
    for expected in frozen {
        let report: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT i.report FROM cvm_instances i JOIN cvm_operations o ON o.id=i.report->>'operation_id' AND o.instance_id=i.id AND o.generation=(i.report->>'generation')::bigint AND o.status=i.report->>'status' WHERE i.id=$1 AND i.node_id=$2 AND o.updated_at>=(SELECT dispatched_at FROM deployment_target_state WHERE deployment_id=$3 AND node_id=$2)")
                .bind(&expected.id).bind(node).bind(deployment)
                .fetch_optional(&mut **tx)
                .await?
                .flatten();
        if let Some(report) = report {
            let actual: InstanceReport = serde_json::from_value(report.clone())?;
            let approved = config.instances.iter().find(|i| i.id == expected.id);
            let matches = if let Some(approved) = approved {
                actual.generation == approved.generation
                    && actual.operation_id == approved.operation_id
            } else {
                // Applied deletion removes the instance from the serving set before network
                // cleanup completes. Its frozen target still owns this terminal evidence.
                let exact = actual.generation == expected.generation
                    && actual.operation_id == expected.operation_id;
                let same_lineage = if exact {
                    true
                } else {
                    let expected_revision =
                        runtime::lineage_revision(tx, &expected.id, expected.generation).await?;
                    expected_revision.is_some()
                        && expected_revision
                            == runtime::lineage_revision(tx, &expected.id, actual.generation)
                                .await?
                };
                actual.power == Some(PowerState::Deleted)
                    && actual.status == ApplyStatus::Applied
                    && same_lineage
            };
            if matches {
                instances.push(serde_json::json!({"id":expected.id,"generation":actual.generation,"operation_id":actual.operation_id,"status":report["status"],"artifacts":report["artifacts"]}));
            }
        }
    }
    let entry: Option<i64> =
        sqlx::query_scalar("SELECT entry_generation FROM cvm_serving_nodes WHERE node_id=$1")
            .bind(node)
            .fetch_optional(&mut **tx)
            .await?;
    let receipt = serde_json::json!({"instances":instances,"nic_generation":config.nic.generation,"entry_generation":entry});
    sqlx::query("UPDATE deployment_target_state SET observed_after=jsonb_set(coalesce(observed_after,'{}'::jsonb),'{cvm}',$3),verdict=coalesce(verdict,'{}'::jsonb)||jsonb_build_object('cvm_pending',false,'cvm_status',$4::text,'target_status',$4::text) WHERE deployment_id=$1 AND node_id=$2")
        .bind(deployment).bind(node).bind(receipt).bind(status).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn prepare_retry(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
) -> Result<()> {
    if let Some(mut config) = serving_tx(tx, node).await? {
        for instance in &config.instances {
            let recovered = transport_fell_back(tx, deployment, node, instance).await?;
            if recovered {
                return Err(StoreError::Unsupported(
                    "承载切换已回退，请在容器页重新配置，并发布新的全局修订".into(),
                ));
            }
        }
        // The asynchronous executor can return a cached failure while a retry runs. Give the
        // retry a fresh immutable execution identity without changing R or the ordinary machine.
        for instance in &mut config.instances {
            let latest: serde_json::Value =
                sqlx::query_scalar("SELECT desired FROM cvm_instances WHERE id=$1 FOR UPDATE")
                    .bind(&instance.id)
                    .fetch_one(&mut **tx)
                    .await?;
            let latest: InstanceDesired = serde_json::from_value(latest)?;
            let revision = runtime::lineage_revision(tx, &instance.id, instance.generation)
                .await?
                .ok_or_else(|| invalid("CVM retry has no approved lineage"))?;
            let pending_revision =
                runtime::lineage_revision(tx, &latest.id, latest.generation).await?;
            let pending = pending_revision != Some(revision);
            instance.generation = instance.generation.max(latest.generation);
            super::operations::save_system_version(tx, instance, "configuration_retry", !pending)
                .await?;
            runtime::record_lineage(tx, instance, revision).await?;
            if pending {
                let mut staged = latest;
                staged.generation = instance.generation;
                super::operations::save_system_version(
                    tx,
                    &mut staged,
                    "configuration_retry",
                    true,
                )
                .await?;
                runtime::record_lineage(
                    tx,
                    &staged,
                    pending_revision.ok_or_else(|| invalid("pending CVM retry has no lineage"))?,
                )
                .await?;
            }
        }
        let revision: i64 =
            sqlx::query_scalar("SELECT revision_id FROM cvm_serving_nodes WHERE node_id=$1")
                .bind(node)
                .fetch_one(&mut **tx)
                .await?;
        let payload = encode(node, revision, &config)?;
        sqlx::query("UPDATE cvm_serving_nodes SET payload=$2 WHERE node_id=$1")
            .bind(node)
            .bind(payload)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn transport_fell_back(
    tx: &mut Transaction<'_, Postgres>,
    deployment: i64,
    node: &str,
    instance: &InstanceDesired,
) -> Result<bool> {
    // Later quota/authorization executions inherit a fallback. Inspect the immutable
    // operation interval owned by this release, rather than only the current operation kind.
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cvm_operations o JOIN deployment_target_state s ON s.deployment_id=$2 AND s.node_id=$3 CROSS JOIN LATERAL jsonb_array_elements(s.desired_structure#>'{cvm,instances}') item WHERE o.instance_id=$1 AND o.kind='transport_recovery' AND item->>'id'=o.instance_id AND o.generation>(item->>'generation')::bigint AND o.generation<=$4)")
        .bind(&instance.id).bind(deployment).bind(node).bind(instance.generation as i64).fetch_one(&mut **tx).await?)
}
