//! Revision numbers are provenance; freshness follows the measured network configuration.
use crate::{compile_cache, materialize, Result, StoreError};
use brocade_core::{artifacts, model::ModelSnapshot};
use sqlx::{Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn nodes(snapshot: &ModelSnapshot, root: &str, chain: &str) -> BTreeSet<String> {
    let steps = snapshot
        .apps
        .iter()
        .flat_map(|a| &a.steps)
        .map(|s| ((s.chain.as_str(), s.node.as_str()), s))
        .collect::<BTreeMap<_, _>>();
    let mut queue = VecDeque::from([(chain.to_owned(), root.to_owned())]);
    let mut seen = BTreeSet::new();
    let mut nodes = BTreeSet::new();
    while let Some((chain, node)) = queue.pop_front() {
        if !seen.insert((chain.clone(), node.clone())) {
            continue;
        }
        nodes.insert(node.clone());
        if let Some(step) = steps.get(&(chain.as_str(), node.as_str())) {
            for rule in &step.rules {
                if let Some(next) = rule.action.forward_ref(&chain) {
                    queue.push_back((next.target_chain.into(), next.target_node.into()));
                }
            }
        }
    }
    nodes
}

pub(super) fn dns_fingerprint(snapshot: &ModelSnapshot, node: &str) -> Result<String> {
    let machine = snapshot
        .nodes
        .iter()
        .find(|n| n.id == node)
        .ok_or_else(|| StoreError::NotFound("probe node".into()))?;
    let mut egress: Vec<_> = snapshot
        .node_egress_dns
        .iter()
        .filter(|p| p.node == node)
        .collect();
    egress.sort_by_key(|p| p.position);
    let bytes = serde_json::to_vec(&(&machine.dns, &machine.domain_strategy, egress))
        .map_err(|_| StoreError::InvalidData("invalid DNS probe configuration".into()))?;
    Ok(brocade_core::hash::sha256_hex(&bytes))
}

pub(super) fn equivalent(
    before: &ModelSnapshot,
    after: &ModelSnapshot,
    node: &str,
    app: &str,
    chain: &str,
) -> Result<bool> {
    let old = compile_cache::compile_incremental(before);
    let new = compile_cache::compile_incremental(after);
    let target = |output| -> Result<_> {
        Ok(compile_cache::project_probe(output, node)
            .map_err(StoreError::from)?
            .targets
            .iter()
            .find(|t| t.app_id.as_deref() == Some(app) && t.chain_id == chain)
            .cloned()
            .map(|mut t| {
                t.chain_name.clear();
                t
            }))
    };
    let old_target = target(&old)?;
    if old_target.is_none() || old_target != target(&new)? {
        return Ok(false);
    }
    let old_nodes = nodes(before, node, chain);
    if old_nodes != nodes(after, node, chain) {
        return Ok(false);
    }
    for node in old_nodes {
        let old = compile_cache::project_node(&old, &node).map_err(StoreError::from)?;
        let new = compile_cache::project_node(&new, &node).map_err(StoreError::from)?;
        // Grants and display metadata are independent of the network path. Compare the
        // actual generated configuration, including all relay and final DNS branches.
        if artifacts::xray::build(&old) != artifacts::xray::build(&new)
            || artifacts::wireguard::build(&old) != artifacts::wireguard::build(&new)
            || artifacts::phantun::build(&old) != artifacts::phantun::build(&new)
            || artifacts::hy2_port_hop::build(&old) != artifacts::hy2_port_hop::build(&new)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn matches(
    tx: &mut Transaction<'_, Postgres>,
    before: Option<i64>,
    after: Option<i64>,
    node: &str,
    app: Option<&str>,
    chain: Option<&str>,
) -> Result<bool> {
    if before == after {
        return Ok(true);
    }
    let (Some(before), Some(after), Some(app), Some(chain)) = (before, after, app, chain) else {
        return Ok(false);
    };
    let revision = |r| {
        u64::try_from(r)
            .map_err(|_| StoreError::InvalidData("invalid unlock snapshot revision".into()))
    };
    let before = materialize::load_immutable_snapshot_tx(tx, revision(before)?).await?;
    let after = materialize::load_immutable_snapshot_tx(tx, revision(after)?).await?;
    equivalent(&before, &after, node, app, chain)
}
