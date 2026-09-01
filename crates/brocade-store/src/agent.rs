use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use brocade_deployment::protocol::RouteIpReport;

use crate::{
    credentials::{generate_node_token, node_token_display_prefix, node_token_hash},
    NodeLifecyclePhase, Result, StoreError,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedNodeToken {
    pub node_id: String,
    pub token: String,
    pub token_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedNode {
    pub node_id: String,
    pub token_prefix: Option<String>,
    pub lifecycle_phase: NodeLifecyclePhase,
}

pub async fn issue_node_token(pool: &PgPool, node_id: &str) -> Result<IssuedNodeToken> {
    if node_id.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "node_id must not be empty when issuing a node token".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let phase = sqlx::query_scalar::<_, String>(
        "SELECT lifecycle.phase
           FROM nodes
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = nodes.id
          WHERE nodes.id = $1
          FOR UPDATE OF lifecycle",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    if phase != NodeLifecyclePhase::Active.as_str() {
        return Err(StoreError::Forbidden(format!(
            "node {node_id} is {phase}; it cannot issue an agent token"
        )));
    }

    let token = generate_node_token()?;
    let token_hash = node_token_hash(&token);
    let token_prefix = node_token_display_prefix(&token);
    let row = sqlx::query(
        "INSERT INTO node_agent_state (
            node_id, token_hash, token_prefix, token_created_at,
            token_last_used_at, token_revoked_at
         )
         VALUES ($1, $2, $3, now(), NULL, NULL)
         ON CONFLICT (node_id) DO UPDATE SET
            token_hash = EXCLUDED.token_hash,
            token_prefix = EXCLUDED.token_prefix,
            token_created_at = EXCLUDED.token_created_at,
            token_last_used_at = NULL,
            token_revoked_at = NULL
         RETURNING node_id, token_prefix",
    )
    .bind(node_id)
    .bind(token_hash)
    .bind(token_prefix)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(IssuedNodeToken {
        node_id: row.try_get("node_id")?,
        token,
        token_prefix: row.try_get("token_prefix")?,
    })
}

pub async fn authenticate_node_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<AuthenticatedNode>> {
    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }

    let token_hash = node_token_hash(token);
    let row = sqlx::query(
        "UPDATE node_agent_state AS agent
            SET token_last_used_at = now()
           FROM node_lifecycle_state AS lifecycle
          WHERE agent.token_hash = $1
            AND agent.token_revoked_at IS NULL
            AND lifecycle.node_id = agent.node_id
            AND lifecycle.phase IN ('active', 'retiring')
        RETURNING agent.node_id, agent.token_prefix, lifecycle.phase",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    row.map(|row| {
        Ok(AuthenticatedNode {
            node_id: row.try_get("node_id")?,
            token_prefix: row.try_get("token_prefix")?,
            lifecycle_phase: match row.try_get::<String, _>("phase")?.as_str() {
                "active" => NodeLifecyclePhase::Active,
                "retiring" => NodeLifecyclePhase::Retiring,
                other => {
                    return Err(StoreError::InvalidData(format!(
                        "authenticated node has invalid lifecycle phase {other}"
                    )))
                }
            },
        })
    })
    .transpose()
}

pub async fn revoke_node_token(pool: &PgPool, node_id: &str) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let phase = sqlx::query_scalar::<_, String>(
        "SELECT lifecycle.phase
           FROM nodes
           JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = nodes.id
          WHERE nodes.id = $1
          FOR UPDATE OF lifecycle",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    if phase == NodeLifecyclePhase::Retiring.as_str() {
        return Err(StoreError::Unsupported(format!(
            "node {node_id} is retiring; keep its token until teardown converges or force-retire it"
        )));
    }
    let result = sqlx::query(
        "UPDATE node_agent_state
         SET token_revoked_at = COALESCE(token_revoked_at, now())
         WHERE node_id = $1
           AND token_hash IS NOT NULL",
    )
    .bind(node_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(result.rows_affected() > 0)
}

pub async fn record_node_poll(
    pool: &PgPool,
    node_id: &str,
    agent_version: Option<&str>,
    protocol_version: Option<i32>,
) -> Result<()> {
    let result = sqlx::query(
        "UPDATE node_agent_state
         SET last_poll_at = now(),
             agent_version = COALESCE($2, agent_version),
             agent_protocol_version = COALESCE($3, agent_protocol_version)
         WHERE node_id = $1
           AND token_hash IS NOT NULL
           AND token_revoked_at IS NULL",
    )
    .bind(node_id)
    .bind(agent_version)
    .bind(protocol_version)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(StoreError::Unauthorized(format!(
            "node {node_id} does not have an active node token"
        )));
    }

    Ok(())
}

pub async fn record_node_route_ips(
    pool: &PgPool,
    node_id: &str,
    route: &RouteIpReport,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO node_agent_state (node_id, route_ipv4, route_ipv6)
         VALUES ($1, $2, $3)
         ON CONFLICT (node_id) DO UPDATE SET
            route_ipv4 = EXCLUDED.route_ipv4,
            route_ipv6 = EXCLUDED.route_ipv6",
    )
    .bind(node_id)
    .bind(route.ipv4.as_deref())
    .bind(route.ipv6.as_deref())
    .execute(pool)
    .await?;

    Ok(())
}

/// Record one runtime reconcile.
///
/// Separate from `record_node_poll`: a poll is the 15-second heartbeat and only updates a
/// timestamp, while this one is low-frequency (at probing's cadence) and carries what is
/// actually installed on the machine, what it repaired itself, and how much accounting it has
/// piled up undelivered.
///
/// `last_local_reconcile` is overwritten only where the agent actually reported one. Reporting
/// `None` means "no local reconcile ran this round", not "the last one no longer counts" —
/// erasing it deletes the only record of drift there is.
///
/// `agent_version` is not written. That column's sole writer is `record_node_poll` (taking the
/// User-Agent header). A version that wrote it here too existed, and the result was one column
/// written by two paths in two formats — poll wrapped the value in `brocade-agent/…` and this
/// wrote it bare — while poll runs far more often, always writes last, and always wins. So the
/// runtime write was pure waste and left the impression that the column held two formats. What
/// the agent reports about itself stays in `runtime_versions.agent`, alongside the
/// xray/phantun/wg fields, which is where it belongs.
///
/// Both values are the sha256 of the agent's own binary rather than a version number, so what a
/// reader of either column is comparing is a build. Old agents still in the fleet report a
/// hand-written version like `0.1.0`; nothing here rejects those, and the console displays them
/// as they came.
pub async fn record_node_runtime(
    pool: &PgPool,
    node_id: &str,
    report: &brocade_deployment::protocol::NodeRuntimeReport,
) -> Result<()> {
    let versions = serde_json::to_value(&report.versions)?;
    let spool = serde_json::to_value(&report.spool)?;
    let reconcile = report
        .local_reconcile
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;
    // The rule database is the semantic inverse of `last_local_reconcile`: the latter is the
    // last thing that happened and must not be erased by a round in which it did not run,
    // whereas the rule database is the fact of this moment and is overwritten every round.
    // Reporting None (the agent searched every asset directory and found no .dat) writes an
    // empty object — that is itself a conclusion worth displaying and must not be conflated
    // with "never reported", which means the whole report never arrived.
    let geodata = report
        .geodata
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));

    let result = sqlx::query(
        "UPDATE node_agent_state
         SET runtime_versions = $2,
             spool_backlog = $3,
             last_local_reconcile = COALESCE($4, last_local_reconcile),
             geodata_observed = $5,
             runtime_reported_at = now()
         WHERE node_id = $1
           AND token_hash IS NOT NULL
           AND token_revoked_at IS NULL",
    )
    .bind(node_id)
    .bind(versions)
    .bind(spool)
    .bind(reconcile)
    .bind(geodata)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(StoreError::Unauthorized(format!(
            "node {node_id} does not have an active node token"
        )));
    }
    Ok(())
}
