//! Operational machine lifecycle.
//!
//! `nodes.retired_at` remains revisioned model intent. This module owns the execution phase and a
//! monotonically increasing epoch which fences immutable deployment targets to the lifecycle in
//! which they were created.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use brocade_deployment::plan::AppliedArtifactState;
use brocade_deployment::protocol::ReportedNodeState;

use crate::{Result, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeLifecyclePhase {
    Active,
    Retiring,
    Retired,
    Abandoned,
}

impl NodeLifecyclePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
            Self::Abandoned => "abandoned",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "retiring" => Ok(Self::Retiring),
            "retired" => Ok(Self::Retired),
            "abandoned" => Ok(Self::Abandoned),
            other => Err(StoreError::InvalidData(format!(
                "unknown node lifecycle phase {other}"
            ))),
        }
    }

    pub fn accepts_business_observations(self) -> bool {
        self == Self::Active
    }

    pub fn accepts_teardown(self) -> bool {
        matches!(self, Self::Active | Self::Retiring)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLifecycleState {
    pub node_id: String,
    pub lifecycle_epoch: u64,
    pub phase: NodeLifecyclePhase,
    pub intent_revision: Option<u64>,
    pub deployment_id: Option<i64>,
    pub requested_at: Option<String>,
    pub completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeLifecycleTransitionResult {
    pub revision_id: u64,
    pub node_id: String,
    pub lifecycle: NodeLifecycleState,
    pub deployment_id: Option<i64>,
    #[serde(default)]
    pub canceled_deployment_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbandonNodeRequest {
    #[serde(default = "default_true")]
    pub unregister_warp: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleAdvance {
    pub lifecycle_epoch: i64,
    pub phase: NodeLifecyclePhase,
}

pub async fn load(pool: &PgPool, node_id: &str) -> Result<NodeLifecycleState> {
    let row = sqlx::query(
        "SELECT n.id AS node_id,
                COALESCE(l.lifecycle_epoch, 0) AS lifecycle_epoch,
                COALESCE(l.phase, CASE WHEN n.retired_at IS NULL THEN 'active' ELSE 'retiring' END) AS phase,
                l.intent_revision,
                l.deployment_id,
                l.requested_at::text AS requested_at,
                l.completed_at::text AS completed_at,
                l.last_error
           FROM nodes n
           LEFT JOIN node_lifecycle_state l ON l.node_id = n.id
          WHERE n.id = $1",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    lifecycle_from_row(&row)
}

fn lifecycle_from_row(row: &sqlx::postgres::PgRow) -> Result<NodeLifecycleState> {
    let epoch: i64 = row.try_get("lifecycle_epoch")?;
    Ok(NodeLifecycleState {
        node_id: row.try_get("node_id")?,
        lifecycle_epoch: u64::try_from(epoch).map_err(|_| {
            StoreError::InvalidData(format!("node lifecycle epoch is negative: {epoch}"))
        })?,
        phase: NodeLifecyclePhase::parse(&row.try_get::<String, _>("phase")?)?,
        intent_revision: row
            .try_get::<Option<i64>, _>("intent_revision")?
            .map(|revision| {
                u64::try_from(revision).map_err(|_| {
                    StoreError::InvalidData(format!(
                        "node lifecycle revision is negative: {revision}"
                    ))
                })
            })
            .transpose()?,
        deployment_id: row.try_get("deployment_id")?,
        requested_at: row.try_get("requested_at")?,
        completed_at: row.try_get("completed_at")?,
        last_error: row.try_get("last_error")?,
    })
}

pub(crate) async fn advance_intent_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    retired: bool,
    revision_id: u64,
) -> Result<LifecycleAdvance> {
    let revision_id = i64::try_from(revision_id)
        .map_err(|_| StoreError::InvalidData("revision id is out of range".to_owned()))?;
    let phase = if retired {
        NodeLifecyclePhase::Retiring
    } else {
        NodeLifecyclePhase::Active
    };
    let row = sqlx::query(
        "INSERT INTO node_lifecycle_state (
             node_id, lifecycle_epoch, phase, intent_revision, deployment_id,
             requested_at, completed_at, last_error, updated_at
         )
         VALUES ($1, 1, $2, $3, NULL,
                 CASE WHEN $2 = 'retiring' THEN now() ELSE NULL END,
                 NULL, NULL, now())
         ON CONFLICT (node_id) DO UPDATE SET
             lifecycle_epoch = node_lifecycle_state.lifecycle_epoch + 1,
             phase = EXCLUDED.phase,
             intent_revision = EXCLUDED.intent_revision,
             deployment_id = NULL,
             requested_at = EXCLUDED.requested_at,
             completed_at = NULL,
             last_error = NULL,
             updated_at = now()
         RETURNING lifecycle_epoch, phase",
    )
    .bind(node_id)
    .bind(phase.as_str())
    .bind(revision_id)
    .fetch_one(&mut **tx)
    .await?;
    let lifecycle_epoch: i64 = row.try_get("lifecycle_epoch")?;
    // Claims are fenced by lifecycle epoch, but closing the old obligations explicitly keeps the
    // debt ledger and deployment settlement truthful after a retire/reactivate transition.
    sqlx::query(
        "WITH canceled AS (
             UPDATE node_convergence_obligations
                SET status = 'canceled',
                    last_error = '节点生命周期代次已变化',
                    settled_at = now()
              WHERE node_id = $1
                AND lifecycle_epoch < $2
                AND status IN (
                    'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
                )
          RETURNING source_deployment_id, node_id
         )
         UPDATE deployment_targets target
            SET status = 'canceled',
                error = '节点生命周期代次已变化'
           FROM canceled
          WHERE target.deployment_id = canceled.source_deployment_id
            AND target.node_id = canceled.node_id
            AND target.status IN ('deferred', 'failed-recovered', 'failed-dirty')",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE deployments d
            SET settlement_status = CASE
                    WHEN EXISTS (
                        SELECT 1 FROM node_convergence_obligations current
                         WHERE current.source_deployment_id = d.id
                           AND current.status IN ('failed-recovered', 'failed-dirty')
                    ) THEN 'uncertain'
                    WHEN EXISTS (
                        SELECT 1 FROM node_convergence_obligations current
                         WHERE current.source_deployment_id = d.id
                           AND current.status IN ('pending', 'dispatched', 'converging')
                    ) THEN 'debt'
                    ELSE 'converged'
                END
          WHERE EXISTS (
              SELECT 1 FROM node_convergence_obligations old
               WHERE old.source_deployment_id = d.id
                 AND old.node_id = $1
                 AND old.lifecycle_epoch < $2
          )",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .execute(&mut **tx)
    .await?;
    Ok(LifecycleAdvance {
        lifecycle_epoch,
        phase: NodeLifecyclePhase::parse(&row.try_get::<String, _>("phase")?)?,
    })
}

pub(crate) async fn current_epoch_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE((SELECT lifecycle_epoch FROM node_lifecycle_state WHERE node_id = $1), 0)",
    )
    .bind(node_id)
    .fetch_one(&mut **tx)
    .await?)
}

pub(crate) async fn attach_deployment_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    lifecycle_epoch: i64,
    deployment_id: i64,
) -> Result<()> {
    let changed = sqlx::query(
        "UPDATE node_lifecycle_state
            SET deployment_id = $3, updated_at = now()
          WHERE node_id = $1 AND lifecycle_epoch = $2 AND phase = 'retiring'",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(StoreError::Conflict(format!(
            "node {node_id} lifecycle changed while attaching retirement deployment"
        )));
    }
    Ok(())
}

pub(crate) fn fully_disabled(observed: &ReportedNodeState) -> bool {
    [
        &observed.phantun,
        &observed.hy2_port_hop,
        &observed.wireguard,
        &observed.xray,
    ]
    .into_iter()
    .all(|state| matches!(state, AppliedArtifactState::Disabled))
}

pub(crate) async fn complete_retirement_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    lifecycle_epoch: i64,
    deployment_id: Option<i64>,
) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE node_lifecycle_state
            SET phase = 'retired', completed_at = now(),
                last_error = NULL, updated_at = now()
          WHERE node_id = $1
            AND lifecycle_epoch = $2
            AND phase = 'retiring'
            AND (deployment_id IS NULL OR deployment_id = $3)",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(deployment_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE node_agent_state
            SET token_revoked_at = COALESCE(token_revoked_at, now())
          WHERE node_id = $1 AND token_hash IS NOT NULL",
    )
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

pub(crate) async fn abandon_tx(tx: &mut Transaction<'_, Postgres>, node_id: &str) -> Result<i64> {
    let lifecycle_epoch = sqlx::query_scalar::<_, i64>(
        "UPDATE node_lifecycle_state
            SET lifecycle_epoch = lifecycle_epoch + 1,
                phase = 'abandoned', deployment_id = NULL,
                completed_at = now(),
                last_error = 'remote teardown was not confirmed', updated_at = now()
          WHERE node_id = $1 AND phase = 'retiring'
        RETURNING lifecycle_epoch",
    )
    .bind(node_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::Unsupported(format!(
            "node {node_id} can only be force-retired while it is retiring"
        ))
    })?;
    sqlx::query(
        "UPDATE node_agent_state
            SET token_revoked_at = COALESCE(token_revoked_at, now())
          WHERE node_id = $1 AND token_hash IS NOT NULL",
    )
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(lifecycle_epoch)
}

pub async fn set_cleanup_error(pool: &PgPool, node_id: &str, error: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE node_lifecycle_state
            SET last_error = $2, updated_at = now()
          WHERE node_id = $1 AND phase IN ('retired', 'abandoned')",
    )
    .bind(node_id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn managed_warp_bindings(pool: &PgPool, node_id: &str) -> Result<Vec<(String, String)>> {
    sqlx::query(
        "SELECT outbound.tenant_id, binding.outbound_id
           FROM external_outbound_bindings binding
           JOIN external_outbounds outbound ON outbound.id = binding.outbound_id
          WHERE binding.node_id = $1 AND outbound.protocol = 'warp'
          ORDER BY outbound.tenant_id, binding.outbound_id",
    )
    .bind(node_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| Ok((row.try_get("tenant_id")?, row.try_get("outbound_id")?)))
    .collect::<Result<Vec<_>>>()
}
