//! Operational machine lifecycle.
//!
//! `nodes.retired_at` remains revisioned model intent. This module owns the execution phase and a
//! monotonically increasing epoch which fences immutable deployment targets to the lifecycle in
//! which they were created.

use serde::{Deserialize, Serialize};
use serde_json::json;
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
    pub completed_by: Option<String>,
    pub reason: Option<String>,
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
    pub reason: String,
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
                l.completed_by,
                l.reason,
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
        completed_by: row.try_get("completed_by")?,
        reason: row.try_get("reason")?,
        last_error: row.try_get("last_error")?,
    })
}

pub(crate) async fn advance_intent_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    retired: bool,
    revision_id: u64,
    actor: &str,
    reason: &str,
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
             requested_at, completed_at, completed_by, reason, last_error, updated_at
         )
         VALUES ($1, 1, $2, $3, NULL,
                 CASE WHEN $2 = 'retiring' THEN now() ELSE NULL END,
                 NULL, NULL, $4, NULL, now())
         ON CONFLICT (node_id) DO UPDATE SET
             lifecycle_epoch = node_lifecycle_state.lifecycle_epoch + 1,
             phase = EXCLUDED.phase,
             intent_revision = EXCLUDED.intent_revision,
             deployment_id = NULL,
             requested_at = EXCLUDED.requested_at,
             completed_at = NULL,
             completed_by = NULL,
             reason = EXCLUDED.reason,
             last_error = NULL,
             updated_at = now()
         RETURNING lifecycle_epoch, phase",
    )
    .bind(node_id)
    .bind(phase.as_str())
    .bind(revision_id)
    .bind(reason)
    .fetch_one(&mut **tx)
    .await?;
    let lifecycle_epoch: i64 = row.try_get("lifecycle_epoch")?;
    let stored_phase = NodeLifecyclePhase::parse(&row.try_get::<String, _>("phase")?)?;
    let event = if retired {
        "retirement-requested"
    } else {
        "node-reactivated"
    };
    sqlx::query(
        "INSERT INTO node_lifecycle_events (
             node_id, lifecycle_epoch, event, revision_id, actor, reason, details
         )
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb)
         ON CONFLICT (node_id, lifecycle_epoch, event) DO NOTHING",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(event)
    .bind(revision_id)
    .bind(actor)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(LifecycleAdvance {
        lifecycle_epoch,
        phase: stored_phase,
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
    actor: &str,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE node_lifecycle_state
            SET phase = 'retired', completed_at = now(), completed_by = $4,
                last_error = NULL, updated_at = now()
          WHERE node_id = $1
            AND lifecycle_epoch = $2
            AND phase = 'retiring'
            AND (deployment_id IS NULL OR deployment_id = $3)
        RETURNING intent_revision",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(deployment_id)
    .bind(actor)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let revision_id: Option<i64> = row.try_get("intent_revision")?;
    sqlx::query(
        "INSERT INTO node_lifecycle_events (
             node_id, lifecycle_epoch, event, revision_id, deployment_id, actor, details
         )
         VALUES ($1, $2, 'retirement-converged', $3, $4, $5, $6)
         ON CONFLICT (node_id, lifecycle_epoch, event) DO NOTHING",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(revision_id)
    .bind(deployment_id)
    .bind(actor)
    .bind(json!({ "all_artifacts_disabled": true }))
    .execute(&mut **tx)
    .await?;
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

pub(crate) async fn abandon_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    actor: &str,
    reason: &str,
) -> Result<i64> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(StoreError::InvalidData(
            "force retirement reason must not be empty".to_owned(),
        ));
    }
    let row = sqlx::query(
        "UPDATE node_lifecycle_state
            SET lifecycle_epoch = lifecycle_epoch + 1,
                phase = 'abandoned', deployment_id = NULL,
                completed_at = now(), completed_by = $2, reason = $3,
                last_error = 'remote teardown was not confirmed', updated_at = now()
          WHERE node_id = $1 AND phase = 'retiring'
        RETURNING lifecycle_epoch, intent_revision",
    )
    .bind(node_id)
    .bind(actor)
    .bind(reason)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        StoreError::Unsupported(format!(
            "node {node_id} can only be force-retired while it is retiring"
        ))
    })?;
    let lifecycle_epoch: i64 = row.try_get("lifecycle_epoch")?;
    let revision_id: Option<i64> = row.try_get("intent_revision")?;
    sqlx::query(
        "INSERT INTO node_lifecycle_events (
             node_id, lifecycle_epoch, event, revision_id, actor, reason,
             details
         )
         VALUES ($1, $2, 'retirement-abandoned', $3, $4, $5,
                 '{\"remote_teardown_confirmed\":false}'::jsonb)
         ON CONFLICT (node_id, lifecycle_epoch, event) DO NOTHING",
    )
    .bind(node_id)
    .bind(lifecycle_epoch)
    .bind(revision_id)
    .bind(actor)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
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
