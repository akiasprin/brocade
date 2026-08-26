//! Steps on a chain: writing, deleting, and the cascade a delete brings with it.
//!
//! The cascade is this family's core and the easiest thing to get wrong: deleting a machine
//! also removes the whole subtree downstream of it, clears the rules pointing into that
//! subtree, and then deletes everything left unreachable from the head. Omit any of the three
//! and dangling rows remain in the database while the compiler sees a broken chain.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use brocade_core::model::{Action, HopIn, Rule, Step};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::types::{
    DeleteStepOutcome, DeleteStepResult, HopInRequest, HopWireRequest, PruneChainResult,
    PutStepRequest, UpsertStepResult,
};
use super::{
    chain_tenant_tx, commit_revision, ensure_node_exists_tx, existing_step_accept_uuid,
    insert_revision, lock_control_state, normalize_step_accept, note_or, redacted_value,
    required_text, resolve_hop_security, u64_to_i64,
};
use crate::{AdminContext, Result, StoreError};

pub async fn put_step(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
    request: PutStepRequest,
) -> Result<UpsertStepResult> {
    let note = note_or(request.note.as_deref(), || {
        format!("put step {app_id}/{chain_id}/{node_id}")
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let (step, changed) = put_step_tx(
        &mut tx,
        actor,
        revision_id,
        app_id,
        chain_id,
        node_id,
        request,
    )
    .await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, changed).await?;
    tx.commit().await?;

    Ok(UpsertStepResult {
        revision_id,
        step: redacted_value(step)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn put_step_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    revision_id: u64,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
    request: PutStepRequest,
) -> Result<(Step, bool)> {
    let app_id = required_text(app_id, "app_id")?;
    let chain_id = required_text(chain_id, "chain_id")?;
    let node_id = required_text(node_id, "node_id")?;
    let rules_json = serde_json::to_value(&request.rules)?;
    let chain_tenant = chain_tenant_tx(tx, &app_id, &chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "step")?;
    ensure_node_exists_tx(tx, &node_id).await?;
    let existing_accept_uuid = existing_step_accept_uuid(tx, &chain_id, &node_id).await?;
    let accept = normalize_step_accept(request.accept, existing_accept_uuid, &chain_id, &node_id)?;
    let hop_in = resolve_hop_in(tx, &chain_id, &node_id, request.hop_in.as_ref()).await?;
    // rules is jsonb and `IS DISTINCT FROM` compares parsed values rather than the source text,
    // so neither key order nor whitespace affects the verdict — storing the same rule table
    // again is no change.
    let changed = sqlx::query(
        "INSERT INTO steps (
            chain_id, node_id, accept_uuid, accept_label, rules,
            hop_in_port, hop_in_wire, created_revision
         )
         VALUES ($1, $2, $3::uuid, $4, $5, $6, $7, $8)
         ON CONFLICT (chain_id, node_id) DO UPDATE SET
            accept_uuid = EXCLUDED.accept_uuid,
            accept_label = EXCLUDED.accept_label,
            rules = EXCLUDED.rules,
            hop_in_port = EXCLUDED.hop_in_port,
            hop_in_wire = EXCLUDED.hop_in_wire,
            created_revision = COALESCE(steps.created_revision, EXCLUDED.created_revision)
         WHERE ROW(steps.accept_uuid, steps.accept_label, steps.rules,
                   steps.hop_in_port, steps.hop_in_wire)
            IS DISTINCT FROM
            ROW(EXCLUDED.accept_uuid, EXCLUDED.accept_label, EXCLUDED.rules,
                EXCLUDED.hop_in_port, EXCLUDED.hop_in_wire)",
    )
    .bind(&chain_id)
    .bind(&node_id)
    .bind(accept.as_ref().map(|accept| accept.uuid.as_str()))
    .bind(accept.as_ref().map(|accept| accept.label.as_str()))
    .bind(rules_json)
    .bind(hop_in.as_ref().map(|(port, _)| i32::from(*port)))
    .bind(hop_in.as_ref().map(|(_, security)| security.clone()))
    .bind(u64_to_i64(revision_id, "revision_id")?)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;

    // The copy returned to the caller carries no private key: `hop_in_wire` holds a REALITY
    // private key and this result goes out over HTTP (the `redacted_value` line). The port is
    // given; of the material, only the kind.
    let hop_in = hop_in
        .map(|(port, security)| -> Result<HopIn> {
            Ok(HopIn {
                port,
                security: serde_json::from_value(security).map_err(|error| {
                    StoreError::InvalidData(format!("hop_in_wire 解不开：{error}"))
                })?,
            })
        })
        .transpose()?;

    Ok((
        Step {
            chain: chain_id,
            node: node_id,
            accept,
            hop_in,
            rules: request.rules,
        },
        changed,
    ))
}

/// Remove a machine from a chain — a cascading delete, not one row.
///
/// A steps row is the only source of truth for chain membership (hops, relay ports, and
/// forwarding outbounds all derive from it), so deleting one row alone leaves the removed node
/// alive forever in the compiled artifacts: its port open, its forwarding emitted, the agent
/// deploying it as usual. But deleting rows alone leaves a tail too: other rules still `Forward`
/// at it, compilation reports `relay.no-accept`, and somebody has to fix that by hand after
/// every delete. So a delete must clear the whole dependency:
///
/// A head (with an ingress on it) — has no legitimate semantics. A dangling ingress means
/// compilation reports `chain.no-ingress`, or worse collapses silently into "the ingress exits
/// directly". So deleting a head deletes the whole chain: the chain declaration (the chains
/// row) goes along with steps, ingresses, and grants (which hang off ingresses). front_vias is a
/// RESTRICT foreign key and is cleared explicitly first.
///
/// An ordinary node — removes the whole subtree reachable from it along rule Forwards (itself
/// included), clears every rule entry on the chain pointing into that subtree, and then clears
/// whatever the cleanup left unreachable from the head. What remains is necessarily a clean
/// state — reachable from the head with no dangling references — and compilation is green.
pub async fn delete_step(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
) -> Result<DeleteStepResult> {
    let note = note_or(None, || {
        format!("delete step {app_id}/{chain_id}/{node_id}")
    });
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let outcome = delete_step_tx(&mut tx, actor, revision_id, app_id, chain_id, node_id).await?;
    let revision_id = commit_revision(&mut tx, revision_id, previous, outcome.changed).await?;
    tx.commit().await?;

    Ok(DeleteStepResult {
        revision_id,
        deleted: outcome.changed,
        removed_steps: outcome.removed_steps,
        chain_removed: outcome.chain_removed,
    })
}

pub(crate) async fn delete_step_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    _revision_id: u64,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
) -> Result<DeleteStepOutcome> {
    let app_id = required_text(app_id, "app_id")?;
    let chain_id = required_text(chain_id, "chain_id")?;
    let node_id = required_text(node_id, "node_id")?;
    let chain_tenant = chain_tenant_tx(tx, &app_id, &chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "step")?;

    // Head test: this chain's ingress sits on it. If so, delete the whole chain.
    let is_head = sqlx::query("SELECT 1 FROM ingresses WHERE chain_id = $1 AND node_id = $2")
        .bind(&chain_id)
        .bind(&node_id)
        .fetch_optional(&mut **tx)
        .await?
        .is_some();
    if is_head {
        return delete_whole_chain_tx(tx, actor, &app_id, &chain_id).await;
    }
    delete_subtree_tx(tx, &chain_id, &node_id).await
}

/// Delete a whole chain: removing the chain declaration takes steps, ingresses, and grants with
/// it through CASCADE; front_vias is RESTRICT and must be cleared explicitly. usage_samples has
/// not referenced ingresses by foreign key since 0001's section 0026 (historical usage is a
/// record of fact and does not require the model object to be present), so it is not in the way;
/// e2e_probes hangs off chains and is CASCADE too.
///
/// Two entry points: deleting a head (detected inside delete_step), and the explicit DeleteChain
/// draft operation. It checks permissions itself — both paths require an editor within the
/// tenant.
pub(crate) async fn delete_whole_chain_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    app_id: &str,
    chain_id: &str,
) -> Result<DeleteStepOutcome> {
    let chain_tenant = chain_tenant_tx(tx, app_id, chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "chain")?;

    // Record the chain's member list first — after the CASCADE it cannot be queried.
    let removed_steps = sqlx::query("SELECT node_id FROM steps WHERE chain_id = $1")
        .bind(chain_id)
        .fetch_all(&mut **tx)
        .await?
        .iter()
        .filter_map(|row| row.try_get::<String, _>("node_id").ok())
        .collect::<Vec<_>>();
    let via_rows = sqlx::query(
        "DELETE FROM front_vias
         WHERE ingress_id IN (SELECT id FROM ingresses WHERE chain_id = $1)",
    )
    .bind(chain_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let chain_rows = sqlx::query("DELETE FROM chains WHERE app_id = $1 AND id = $2")
        .bind(app_id)
        .bind(chain_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    Ok(DeleteStepOutcome {
        changed: via_rows + chain_rows > 0,
        removed_steps,
        chain_removed: chain_rows > 0,
    })
}

/// Remove a subtree: delete everything reachable from `node_id` along rule Forwards, clear the
/// rule entries pointing at them, and then clear whatever the cleanup left unreachable from the
/// head.
pub(crate) async fn delete_subtree_tx(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: &str,
    node_id: &str,
) -> Result<DeleteStepOutcome> {
    // Read the chain's current state: node → rule table.
    let rows = sqlx::query("SELECT node_id, rules FROM steps WHERE chain_id = $1")
        .bind(chain_id)
        .fetch_all(&mut **tx)
        .await?;
    let mut steps = Vec::with_capacity(rows.len());
    for row in rows {
        let node = row.try_get::<String, _>("node_id")?;
        let rules = serde_json::from_value::<Vec<Rule>>(row.try_get("rules")?)
            .map_err(|error| StoreError::InvalidData(format!("steps 规则解不开：{error}")))?;
        steps.push((node, rules));
    }
    let mut outcome = DeleteStepOutcome::default();
    let mut changed = false;

    // (1) The deleted node and its subtree (everything reachable along Forwards).
    let subtree = forward_subtree(&steps, node_id);
    outcome.removed_steps.extend(subtree.iter().cloned());
    for member in &subtree {
        let affected = sqlx::query("DELETE FROM steps WHERE chain_id = $1 AND node_id = $2")
            .bind(chain_id)
            .bind(member)
            .execute(&mut **tx)
            .await?
            .rows_affected();
        changed |= affected > 0;
    }

    // (2) Clear the entries in the remaining rule tables that Forward into the subtree.
    // `IS DISTINCT FROM` follows put_step: changed is recorded only where something really
    // changed.
    for (node, rules) in &mut steps {
        if subtree.contains(node) {
            continue;
        }
        let filtered = rules
            .iter()
            .filter(|rule| !forward_to_subtree(rule, &subtree))
            .cloned()
            .collect::<Vec<_>>();
        if filtered.len() == rules.len() {
            continue;
        }
        let affected = sqlx::query(
            "UPDATE steps SET rules = $3
             WHERE chain_id = $1 AND node_id = $2
               AND rules IS DISTINCT FROM $3",
        )
        .bind(chain_id)
        .bind(node.as_str())
        .bind(serde_json::to_value(&filtered)?)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        changed |= affected > 0;
        *rules = filtered;
    }

    // (3) The nodes left unreachable from the head after the cleanup.
    let pruned = prune_unreachable_tx(tx, chain_id).await?;
    changed |= !pruned.is_empty();
    outcome.removed_steps.extend(pruned);

    outcome.changed = changed;
    Ok(outcome)
}

/// Remove the steps on this chain unreachable from the head along Forwards, returning the node
/// ids removed.
///
/// No rule Forwards at them any more (or whoever did is itself disconnected), the compiler does
/// not compile them anyway (`step.unreachable`), and keeping them leaves dangling rows — which
/// stop compilation at `chain.unreachable` and make the whole chain unshippable.
///
/// The test is reachability from the head, not in-degree. In-degree misses cycles: with A→B and
/// B→A neither in-degree is 0, yet neither is reachable from the head. One BFS collects both
/// cases and handles the cascade in passing.
///
/// With no head found (no ingress) it does nothing: better to leave dangling rows than to delete
/// somebody's members on the strength of an incomplete graph.
///
/// Two callers: `delete_subtree_tx`'s cleanup after removing a subtree, and the single cleanup
/// after the console saves a whole rule tree (`prune_chain`). The browser does not compute this
/// itself — it sees a draft of one table at a time while the others are still in their stored
/// state, and deleting against that incomplete picture removes the machine somebody just
/// connected.
pub(crate) async fn prune_unreachable_tx(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: &str,
) -> Result<Vec<String>> {
    // Ordered even though `ingresses.chain_id` is unique, so there is only ever one row to pick.
    // What this function does with the head is delete every step it cannot reach from there, so
    // it must never be able to read a different head than the compiler does. Should the constraint
    // ever come off, an unordered `LIMIT 1` would resume choosing by physical row order.
    let Some(head) =
        sqlx::query("SELECT node_id FROM ingresses WHERE chain_id = $1 ORDER BY id LIMIT 1")
            .bind(chain_id)
            .fetch_optional(&mut **tx)
            .await?
            .and_then(|row| row.try_get::<String, _>("node_id").ok())
    else {
        return Ok(Vec::new());
    };

    let rows = sqlx::query("SELECT node_id, rules FROM steps WHERE chain_id = $1")
        .bind(chain_id)
        .fetch_all(&mut **tx)
        .await?;
    let mut steps = Vec::with_capacity(rows.len());
    for row in rows {
        let node = row.try_get::<String, _>("node_id")?;
        let rules = serde_json::from_value::<Vec<Rule>>(row.try_get("rules")?)
            .map_err(|error| StoreError::InvalidData(format!("steps 规则解不开：{error}")))?;
        steps.push((node, rules));
    }

    let reachable = forward_subtree(&steps, &head);
    let mut removed = Vec::new();
    for (node, _) in &steps {
        if reachable.contains(node) {
            continue;
        }
        let affected = sqlx::query("DELETE FROM steps WHERE chain_id = $1 AND node_id = $2")
            .bind(chain_id)
            .bind(node)
            .execute(&mut **tx)
            .await?
            .rows_affected();
        if affected > 0 {
            removed.push(node.clone());
        }
    }
    Ok(removed)
}

/// Remove the stranded steps on this chain — the cleanup after the console saves a whole rule
/// tree.
///
/// Why this must happen server-side. The test is reachability from the head along Forwards,
/// which needs the chain's complete rule table. In the browser only one table's draft is current
/// at a time while the others remain in their stored state — computed against that incomplete
/// graph, a machine somebody just connected on another table is judged stranded and deleted, and
/// the symptom is compilation reporting `relay.no-accept` after saving (a rule points at it and
/// its step is gone). So once the rule tables have landed one by one, this is called once and
/// the server computes against the real whole.
///
/// A no-op returns the revision number (`commit_revision`), leaving drafts free of a pile of
/// revisions that did nothing.
pub async fn prune_chain(
    pool: &PgPool,
    actor: &AdminContext,
    app_id: &str,
    chain_id: &str,
) -> Result<PruneChainResult> {
    let note = note_or(None, || format!("prune chain {app_id}/{chain_id}"));
    let mut tx = pool.begin().await?;
    let previous = lock_control_state(&mut tx).await?;
    let revision_id = insert_revision(&mut tx, actor.operator_id(), &note).await?;
    let removed_steps = prune_chain_tx(&mut tx, actor, app_id, chain_id).await?;
    let revision_id =
        commit_revision(&mut tx, revision_id, previous, !removed_steps.is_empty()).await?;
    tx.commit().await?;

    Ok(PruneChainResult {
        revision_id,
        removed_steps,
    })
}

pub(crate) async fn prune_chain_tx(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    app_id: &str,
    chain_id: &str,
) -> Result<Vec<String>> {
    let chain_tenant = chain_tenant_tx(tx, app_id, chain_id).await?;
    actor.require_tenant_access(&chain_tenant, "chain")?;
    prune_unreachable_tx(tx, chain_id).await
}

/// The closure reachable from `from` along rule Forwards (`from` itself included).
pub(crate) fn forward_subtree(steps: &[(String, Vec<Rule>)], from: &str) -> BTreeSet<String> {
    let by_node = steps
        .iter()
        .map(|(node, rules)| (node.as_str(), rules))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::from([from.to_owned()]);
    let mut queue = VecDeque::from([from.to_owned()]);
    while let Some(node) = queue.pop_front() {
        if let Some(rules) = by_node.get(node.as_str()) {
            for rule in *rules {
                if let Action::Forward { to, .. } = &rule.action {
                    if seen.insert(to.clone()) {
                        queue.push_back(to.clone());
                    }
                }
            }
        }
    }
    seen
}

pub(crate) fn forward_to_subtree(rule: &Rule, subtree: &BTreeSet<String>) -> bool {
    matches!(&rule.action, Action::Forward { to, .. } if subtree.contains(to))
}

/// Resolve a request's `hop_in` into `(port, transport jsonb)`.
///
/// `None` leaves it alone, keeping what the database holds; `port == 0` turns it off. As with
/// `accept`, "not mentioning the field" and "clearing it" must be two different things, or
/// editing a rule table alone would close the relay port every time.
pub(crate) async fn resolve_hop_in(
    tx: &mut Transaction<'_, Postgres>,
    chain_id: &str,
    node_id: &str,
    wanted: Option<&HopInRequest>,
) -> Result<Option<(u16, Value)>> {
    let Some(wanted) = wanted else {
        let row = sqlx::query(
            "SELECT hop_in_port, hop_in_wire FROM steps WHERE chain_id = $1 AND node_id = $2",
        )
        .bind(chain_id)
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?;
        return Ok(row.and_then(|row| {
            let port: Option<i32> = row.try_get("hop_in_port").ok().flatten();
            let security: Option<Value> = row.try_get("hop_in_wire").ok().flatten();
            match (port, security) {
                (Some(port), Some(security)) => {
                    u16::try_from(port).ok().map(|port| (port, security))
                }
                _ => None,
            }
        }));
    };

    if wanted.port == 0 {
        return Ok(None);
    }

    let security = resolve_hop_security(
        tx,
        chain_id,
        node_id,
        wanted.security.as_ref().unwrap_or(&HopWireRequest::None),
    )
    .await?;
    Ok(Some((wanted.port, security)))
}
