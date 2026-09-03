//! Durable input for user-facing subscriptions.
//!
//! A subscription is rebuilt on every request, but "dynamic" does not mean "read whatever was
//! most recently committed". Configuration and runtime grants are released independently, so the
//! serving model is the topology from the last successful configuration release composed with the
//! permissions from the last successful grants release, then overlays the independently committed
//! client configuration. The three immutable snapshots are the cache; rendered URI/YAML never is.

use std::collections::{BTreeMap, BTreeSet};

use brocade_core::model::ModelSnapshot;
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Result, StoreError};

pub(crate) struct SubscriptionServingProjection {
    pub(crate) snapshot: ModelSnapshot,
    _generation: u64,
    unavailable_reason: Option<String>,
}

impl SubscriptionServingProjection {
    pub(crate) fn ensure_available(&self) -> Result<()> {
        match &self.unavailable_reason {
            Some(reason) => Err(StoreError::Unavailable(reason.clone())),
            None => Ok(()),
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self._generation
    }
}

/// Read all checkpoint pointers and the availability decision from one PostgreSQL statement. A release
/// which commits immediately after this statement is ordered after this request; a release already
/// visible here blocks it. The two snapshots are immutable, so loading them afterwards cannot mix
/// their contents with a newer model.
pub(crate) async fn load_subscription_serving_projection(
    pool: &PgPool,
) -> Result<SubscriptionServingProjection> {
    let row = sqlx::query(
        "SELECT s.topology_revision_id,
                s.permissions_revision_id,
                s.client_snapshot_id,
                s.generation,
                EXISTS (
                    SELECT 1 FROM deployments d
                     WHERE d.active = TRUE
                       AND d.status IN ('planned', 'running', 'halted')
                ) AS deployment_open,
                EXISTS (
                    SELECT 1 FROM jobs j
                     WHERE j.kind = 'grants-deployment'
                       AND j.status IN ('queued', 'running')
                ) AS grants_waiting,
                EXISTS (
                    SELECT 1
                      FROM node_applied_state n
                      JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = n.node_id
                     WHERE lifecycle.phase IN ('active', 'retiring')
                       AND (n.phantun_state = 'dirty'
                        OR n.wireguard_state = 'dirty'
                        OR n.xray_state = 'dirty'
                        OR n.hy2_port_hop_state = 'dirty'
                        OR n.grants_state = 'dirty')
                ) AS runtime_dirty,
                EXISTS (
                    SELECT 1
                      FROM deployments d
                     WHERE (
                         d.status = 'succeeded'
                         OR (
                             d.status IN ('canceled', 'failed')
                             AND EXISTS (
                                 SELECT 1 FROM deployment_targets dt
                                  WHERE dt.deployment_id = d.id
                                    AND dt.status = 'succeeded'
                             )
                         )
                     )
                       AND (
                           (d.kind = 'config'
                            AND d.id > COALESCE(s.topology_deployment_id, 0))
                           OR
                           (d.kind = 'grants'
                            AND d.id > COALESCE(s.permissions_deployment_id, 0))
                       )
                ) AS settlement_gap
           FROM subscription_serving_state s
          WHERE s.id = TRUE",
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        StoreError::Unavailable(
            "订阅尚无已成功发布的 serving 状态；完成首次配置发布后再拉取".to_owned(),
        )
    })?;

    let topology_revision: i64 = row.try_get("topology_revision_id")?;
    let permissions_revision: i64 = row.try_get("permissions_revision_id")?;
    let client_snapshot_id = row
        .try_get::<Option<i64>, _>("client_snapshot_id")?
        .ok_or_else(|| {
            StoreError::Unavailable("订阅客户端配置检查点尚未完成回填；服务暂不可用".to_owned())
        })?;
    let topology = crate::materialize::load_immutable_snapshot(
        pool,
        revision_to_u64("topology_revision_id", topology_revision)?,
    )
    .await?;
    let permissions = crate::materialize::load_immutable_snapshot(
        pool,
        revision_to_u64("permissions_revision_id", permissions_revision)?,
    )
    .await?;
    let client = crate::subscription_client::load_client_snapshot(
        pool,
        revision_to_u64("client_snapshot_id", client_snapshot_id)?,
    )
    .await?;

    let unavailable_reason = if row.try_get::<bool, _>("deployment_open")? {
        Some("发布正在进行，订阅暂不可更新；请等待所有目标收敛".to_owned())
    } else if row.try_get::<bool, _>("grants_waiting")? {
        Some("权限同步仍在排队或执行，订阅暂不可更新".to_owned())
    } else if row.try_get::<bool, _>("runtime_dirty")? {
        Some("运行态存在未确认或 dirty 的机器，订阅暂不可更新".to_owned())
    } else if row.try_get::<bool, _>("settlement_gap")? {
        Some("最近一次发布未形成全机队一致的 serving 状态，等待完整收敛发布".to_owned())
    } else {
        None
    };

    Ok(SubscriptionServingProjection {
        snapshot: crate::subscription_client::compose(topology, &permissions, &client.config)?,
        _generation: revision_to_u64("subscription generation", row.try_get("generation")?)?,
        unavailable_reason,
    })
}

/// Advance the appropriate serving line in the same transaction which turns the deployment into
/// `succeeded`. A partial, halted or canceled release never calls this function and therefore can
/// never leak its revision into a subscription.
pub(crate) async fn activate_deployment_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
) -> Result<()> {
    let deployment = sqlx::query(
        "SELECT kind, revision_id
           FROM deployments
          WHERE id = $1 AND status = 'succeeded'",
    )
    .bind(deployment_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::InvalidData(format!(
            "cannot activate subscription serving state from unfinished deployment {deployment_id}"
        ))
    })?;
    let kind: String = deployment.try_get("kind")?;
    let revision_id: i64 = deployment.try_get("revision_id")?;

    // A tenant-scoped release can be perfectly successful for its own targets while the global
    // fleet still runs more than one topology revision. Such a release is audit history, not a
    // global serving checkpoint. Leaving the previous pointer in place makes `settlement_gap`
    // fail closed until a later release covers every active/retiring machine (succeeded or already
    // converged/skipped).
    if !deployment_covers_serving_fleet_tx(tx, deployment_id, &kind).await? {
        return Ok(());
    }

    match kind.as_str() {
        "config" => {
            // Lock the client head before the serving singleton. This remains a real lock before
            // the first release, where the serving row does not exist yet.
            let client = crate::subscription_client::locked_head_for_revision_tx(
                tx,
                revision_to_u64("deployment revision_id", revision_id)?,
            )
            .await?;
            // A configuration target carries permissions only when it replaces/disables Xray.
            // Pending targets may have been rebased to a newer permission revision; that frozen
            // value is the one which actually landed, not necessarily deployments.revision_id.
            let effective_permissions: Option<i64> = sqlx::query_scalar(
                "SELECT max(
                            CASE
                              WHEN dts.desired_structure->>'grants_revision' ~ '^[0-9]+$'
                              THEN (dts.desired_structure->>'grants_revision')::bigint
                              ELSE $2::bigint
                            END
                        )
                   FROM deployment_target_state dts
                   JOIN deployment_targets dt
                     ON dt.deployment_id = dts.deployment_id
                    AND dt.node_id = dts.node_id
                  WHERE dts.deployment_id = $1
                    AND dt.status = 'succeeded'
                    AND dts.desired_structure->'actions'
                        ?| array['apply-xray', 'disable-xray']",
            )
            .bind(deployment_id)
            .bind(revision_id)
            .fetch_one(&mut **tx)
            .await?;
            let initial_permissions = effective_permissions.unwrap_or(revision_id);
            let existing_permissions = sqlx::query_scalar::<_, i64>(
                "SELECT permissions_revision_id
                   FROM subscription_serving_state
                  WHERE id = TRUE
                  FOR UPDATE",
            )
            .fetch_optional(&mut **tx)
            .await?;
            let composed_permissions = effective_permissions
                .or(existing_permissions)
                .unwrap_or(initial_permissions);
            crate::subscription_client::validate_revision_combination_tx(
                tx,
                revision_to_u64("topology revision_id", revision_id)?,
                revision_to_u64("permissions revision_id", composed_permissions)?,
                &client.config,
                &format!("config deployment {deployment_id}"),
            )
            .await?;
            sqlx::query(
                "INSERT INTO subscription_serving_state (
                    id, topology_revision_id, permissions_revision_id, client_snapshot_id,
                    topology_deployment_id, permissions_deployment_id, generation
                 ) VALUES (TRUE, $2, $3, $5, $1, $4, 1)
                 ON CONFLICT (id) DO UPDATE SET
                    topology_revision_id = EXCLUDED.topology_revision_id,
                    topology_deployment_id = EXCLUDED.topology_deployment_id,
                    client_snapshot_id = EXCLUDED.client_snapshot_id,
                    permissions_revision_id = CASE
                        WHEN $4::bigint IS NULL
                        THEN subscription_serving_state.permissions_revision_id
                        ELSE EXCLUDED.permissions_revision_id
                    END,
                    permissions_deployment_id = CASE
                        WHEN $4::bigint IS NULL
                        THEN subscription_serving_state.permissions_deployment_id
                        ELSE EXCLUDED.permissions_deployment_id
                    END,
                    generation = subscription_serving_state.generation + 1,
                    updated_at = now()",
            )
            .bind(deployment_id)
            .bind(revision_id)
            .bind(initial_permissions)
            .bind(effective_permissions.map(|_| deployment_id))
            .bind(i64::try_from(client.id).map_err(|_| {
                StoreError::InvalidData(format!(
                    "subscription client snapshot id is out of range: {}",
                    client.id
                ))
            })?)
            .execute(&mut **tx)
            .await?;
        }
        "grants" => {
            // A grant-only release cannot establish topology on a fresh installation. Once a
            // configuration checkpoint exists, it advances just the permission half.
            let serving = sqlx::query(
                "SELECT topology_revision_id, client_snapshot_id
                   FROM subscription_serving_state
                  WHERE id = TRUE
                  FOR UPDATE",
            )
            .fetch_optional(&mut **tx)
            .await?;
            let Some(serving) = serving else {
                return Ok(());
            };
            let topology_revision: i64 = serving.try_get("topology_revision_id")?;
            let client_snapshot_id = serving
                .try_get::<Option<i64>, _>("client_snapshot_id")?
                .ok_or_else(|| {
                    StoreError::InvalidData(
                        "cannot advance permissions without a client checkpoint".to_owned(),
                    )
                })?;
            let client = crate::subscription_client::load_client_snapshot_tx(
                tx,
                revision_to_u64("client_snapshot_id", client_snapshot_id)?,
            )
            .await?;
            crate::subscription_client::validate_revision_combination_tx(
                tx,
                revision_to_u64("topology_revision_id", topology_revision)?,
                revision_to_u64("permissions revision_id", revision_id)?,
                &client.config,
                &format!("grants deployment {deployment_id}"),
            )
            .await?;
            sqlx::query(
                "UPDATE subscription_serving_state
                    SET permissions_revision_id = $2,
                        permissions_deployment_id = $1,
                        generation = generation + 1,
                        updated_at = now()
                  WHERE id = TRUE",
            )
            .bind(deployment_id)
            .bind(revision_id)
            .execute(&mut **tx)
            .await?;
        }
        other => {
            return Err(StoreError::InvalidData(format!(
                "deployment {deployment_id} has unknown kind {other}"
            )))
        }
    }
    Ok(())
}

/// A permission job can discover that every running Xray already has the desired allow-list and
/// therefore need no deployment row. Finishing that durable job is still a serving transition:
/// advance the permission snapshot in the same transaction that marks the job complete. If no
/// topology has ever shipped, the update intentionally affects no row and the subscription stays
/// unavailable until the first configuration release.
pub(crate) async fn activate_permissions_revision_tx(
    tx: &mut Transaction<'_, Postgres>,
    revision_id: u64,
) -> Result<()> {
    let revision_id_i64 = i64::try_from(revision_id).map_err(|_| {
        StoreError::InvalidData(format!(
            "subscription permissions revision is out of range: {revision_id}"
        ))
    })?;
    let serving = sqlx::query(
        "SELECT topology_revision_id, client_snapshot_id
           FROM subscription_serving_state
          WHERE id = TRUE
          FOR UPDATE",
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(serving) = serving else {
        return Ok(());
    };
    let topology_revision: i64 = serving.try_get("topology_revision_id")?;
    let client_snapshot_id = serving
        .try_get::<Option<i64>, _>("client_snapshot_id")?
        .ok_or_else(|| {
            StoreError::InvalidData(
                "cannot advance permissions without a client checkpoint".to_owned(),
            )
        })?;
    let client = crate::subscription_client::load_client_snapshot_tx(
        tx,
        revision_to_u64("client_snapshot_id", client_snapshot_id)?,
    )
    .await?;
    crate::subscription_client::validate_revision_combination_tx(
        tx,
        revision_to_u64("topology_revision_id", topology_revision)?,
        revision_id,
        &client.config,
        "permission job without deployment",
    )
    .await?;
    sqlx::query(
        "UPDATE subscription_serving_state
            SET permissions_revision_id = $1,
                generation = generation + 1,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(revision_id_i64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn deployment_covers_serving_fleet_tx(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: i64,
    kind: &str,
) -> Result<bool> {
    // A grants order deliberately contains only machines whose running client list differs.
    // Requiring a target row for every Xray machine therefore confuses "already matched" with
    // "was outside the release" and leaves subscriptions unavailable after every ordinary
    // one-machine permission edit. The durable automation job is the global proof instead: it is
    // marked succeeded with this deployment id only after planning the whole fleet found no
    // deferred machine. A partial order created while another config target is in flight has no
    // such job reference and remains fail-closed.
    if kind == "grants" {
        return Ok(sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                   FROM jobs
                  WHERE kind = 'grants-deployment'
                    AND status = 'succeeded'
                    AND payload->>'deployment_id' ~ '^[0-9]+$'
                    AND (payload->>'deployment_id')::bigint = $1
             )",
        )
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await?);
    }

    Ok(sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1
               FROM node_lifecycle_state lifecycle
              WHERE lifecycle.phase IN ('active', 'retiring')
                AND NOT EXISTS (
                    SELECT 1
                      FROM deployment_targets dt
                     WHERE dt.deployment_id = $1
                       AND dt.node_id = lifecycle.node_id
                       AND dt.status IN ('succeeded', 'skipped')
                )
         )",
    )
    .bind(deployment_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Combine the independently released state dimensions. This is also used by automatic grant
/// planning; keeping one function prevents subscriptions and the Agent allow-list from developing
/// different ideas of which grant belongs to the running topology. Flow stays with `topology` and
/// therefore changes only through a configuration release.
pub(crate) fn permission_projection(
    mut topology: ModelSnapshot,
    permissions: &ModelSnapshot,
) -> ModelSnapshot {
    topology.revision = topology.revision.max(permissions.revision);
    topology.users = permissions.users.clone();
    let latest_apps = permissions
        .apps
        .iter()
        .map(|app| (app.id.as_str(), app))
        .collect::<BTreeMap<_, _>>();
    for app in &mut topology.apps {
        let ingress_ids = app
            .ingresses
            .iter()
            .map(|ingress| ingress.id.clone())
            .collect::<BTreeSet<_>>();
        if let Some(latest) = latest_apps.get(app.id.as_str()) {
            app.grants = latest
                .grants
                .iter()
                .filter(|grant| ingress_ids.contains(&grant.ingress))
                .cloned()
                .collect();
        } else {
            app.grants.clear();
        }
    }
    topology
}

fn revision_to_u64(field: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidData(format!("{field} is negative: {value}")))
}
