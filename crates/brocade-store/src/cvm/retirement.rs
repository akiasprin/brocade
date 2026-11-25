//! Forced machine retirement is a control-plane terminal operation.
//!
//! The retired machine cannot prove teardown. Preserve that uncertainty in the instance and
//! resource ledgers, but remove its live CVM intent immediately and publish withdrawals only to
//! surviving, non-isolated peers.

use super::*;
use std::collections::BTreeSet;

const FORCE_RETIREMENT_OPERATION: &str = "force_node_retirement";

pub(crate) async fn force_retire_node(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminContext,
    node: &str,
) -> Result<()> {
    releases::lock(tx).await?;

    let instance_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM cvm_instances WHERE node_id=$1
         UNION
         SELECT instance_id FROM cvm_nic_allocations WHERE provider_node=$1 AND NOT released
         UNION
         SELECT instance_id FROM cvm_entries WHERE provider_node=$1 AND NOT cleaned
         ORDER BY 1",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    let instance_refs = instance_ids.iter().map(String::as_str).collect();
    let before = releases::begin(
        tx,
        releases::Scope {
            instances: instance_refs,
            nodes: vec![node],
            ..Default::default()
        },
    )
    .await?;
    let affected_nodes = before.keys().cloned().collect::<Vec<_>>();

    // A machine can provide remote NICs and entries in addition to hosting containers. Retire
    // those provider relationships before removing hosted instances from the live projection.
    nics::retire_provider(tx, node).await?;
    nics::entries::retire_node(tx, node).await?;

    let rows = sqlx::query(&format!(
        "{INSTANCE_SELECT} WHERE node_id=$1 AND reserved ORDER BY id FOR UPDATE"
    ))
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let instance = from_row(&row)?;
        let mut desired = instance.desired;
        desired.power = PowerState::Deleted;
        desired.delete_data = desired.spec.execution.is_cube();
        desired.spec.routing.routes.clear();
        desired.spec.routing.priorities.clear();
        desired.spec.routing.revision = desired
            .spec
            .routing
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("CVM routing revision exhausted"))?;
        desired.detached_interfaces =
            sqlx::query_scalar("SELECT id FROM cvm_interfaces WHERE instance_id=$1 ORDER BY id")
                .bind(&desired.id)
                .fetch_all(&mut **tx)
                .await?;
        nics::withdraw(tx, &desired.id, None).await?;
        nics::entries::release_instance(tx, &desired.id).await?;
        operations::save_system_version(tx, &mut desired, FORCE_RETIREMENT_OPERATION, true).await?;

        // `reserved=false` removes the instance from live desired state and pending-release UI.
        // `unknown` deliberately does not claim that the unreachable runtime deleted its disk.
        sqlx::query(
            "UPDATE cvm_instances
                SET reserved=false,status='unknown',updated_at=now()
              WHERE id=$1",
        )
        .bind(&desired.id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE cvm_operations SET status='unknown',updated_at=now() WHERE id=$1")
            .bind(&desired.operation_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE cvm_interfaces SET desired_attached=false WHERE instance_id=$1")
            .bind(&desired.id)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "UPDATE cvm_nic_allocations SET retired=true
              WHERE instance_id=$1 AND NOT released",
        )
        .bind(&desired.id)
        .execute(&mut **tx)
        .await?;
    }

    waive_unreachable_binding_peers(tx, node).await?;
    releases::commit(tx, actor, before).await?;

    // Retired and operationally isolated machines cannot execute this transaction. Advance their
    // Console serving baseline to the withdrawal now; when an isolated peer is restored, it sees
    // this empty/withdrawn baseline instead of resurrecting the retired relationship.
    let excluded: BTreeSet<String> = sqlx::query_scalar(
        "SELECT id FROM nodes WHERE id=ANY($1) AND retired_at IS NOT NULL
         UNION
         SELECT node_id FROM node_operational_isolations WHERE node_id=ANY($1)",
    )
    .bind(&affected_nodes)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    for excluded_node in excluded {
        releases::force_activate_latest(tx, &excluded_node).await?;
    }
    Ok(())
}

async fn waive_unreachable_binding_peers(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<()> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT b.id
           FROM cvm_nic_bindings b
          WHERE (b.provider_node=$1 OR b.consumer_node=$1)
            AND b.phase='withdrawing' AND NOT b.cleaned
            AND (
                b.provider_node=b.consumer_node
                OR EXISTS (
                    SELECT 1 FROM nodes peer
                    LEFT JOIN node_operational_isolations isolation ON isolation.node_id=peer.id
                    LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id=peer.id
                    WHERE peer.id=CASE
                        WHEN b.provider_node=$1 THEN b.consumer_node ELSE b.provider_node END
                      AND (peer.retired_at IS NOT NULL
                           OR isolation.node_id IS NOT NULL
                           OR lifecycle.phase IN ('retired','abandoned'))
                )
            )
          ORDER BY b.id",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    if ids.is_empty() {
        return Ok(());
    }
    // Resource claims intentionally stay held when nobody can prove physical cleanup. They no
    // longer represent a live relationship and no longer block this retirement transaction.
    sqlx::query("UPDATE cvm_nic_bindings SET cleaned=true WHERE id=ANY($1)")
        .bind(ids)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
