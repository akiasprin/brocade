use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use brocade_deployment::protocol::{
    ConfigXrayForceRetirementCommand, RouteIpReport, XrayUpgradePhase, XrayUpgradeRuntime,
    XrayUpgradeSource,
};

use crate::{
    credentials::{generate_node_token, node_token_display_prefix, node_token_hash},
    NodeLifecyclePhase, Result, StoreError,
};

const MAX_RUNTIME_CLOCK_SKEW_SECS: i64 = 600;

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
    issue_node_token_inner(pool, node_id, false).await
}

/// Rotate a node token for a machine which may have been reinstalled.
///
/// The control plane cannot distinguish a token rotation from a clean operating-system install.
/// In the latter case the old applied-state row is only history: the new machine has no local
/// files or runtime state to replay. Mark the whole managed environment unknown so the next
/// desired-state poll can reconstruct every artifact from the last successful targets.
pub async fn reissue_node_token(pool: &PgPool, node_id: &str) -> Result<IssuedNodeToken> {
    issue_node_token_inner(pool, node_id, true).await
}

async fn issue_node_token_inner(
    pool: &PgPool,
    node_id: &str,
    reconcile_environment: bool,
) -> Result<IssuedNodeToken> {
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
            token_created_at = CASE
                WHEN node_agent_state.token_created_at IS NULL
                    THEN EXCLUDED.token_created_at
                ELSE GREATEST(
                    EXCLUDED.token_created_at,
                    node_agent_state.token_created_at + interval '1 microsecond'
                )
            END,
            token_last_used_at = NULL,
            token_revoked_at = NULL
         RETURNING node_id, token_prefix",
    )
    .bind(node_id)
    .bind(token_hash)
    .bind(token_prefix)
    .fetch_one(&mut *tx)
    .await?;
    if reconcile_environment {
        // Keep acknowledged stops when resetting observations. Legacy narrow targets may have
        // no artifact record for them, and a repeated re-sign must retain the same evidence.
        sqlx::query(
            "UPDATE node_applied_state
                SET phantun_state = 'unknown',
                    phantun_sha256 = NULL,
                    phantun_observed = jsonb_build_object(
                        'previous_state', CASE WHEN phantun_state = 'unknown'
                            THEN phantun_observed->>'previous_state' ELSE phantun_state END,
                        'state', 'unknown',
                        'reason', 'node token re-signed; environment must converge again',
                        'reconcile', 'node-token-resigned'
                    ),
                    wireguard_state = 'unknown',
                    wireguard_sha256 = NULL,
                    wireguard_observed = jsonb_build_object(
                        'previous_state', CASE WHEN wireguard_state = 'unknown'
                            THEN wireguard_observed->>'previous_state' ELSE wireguard_state END,
                        'state', 'unknown',
                        'reason', 'node token re-signed; environment must converge again',
                        'reconcile', 'node-token-resigned'
                    ),
                    xray_state = 'unknown',
                    xray_sha256 = NULL,
                    xray_observed = jsonb_build_object(
                        'previous_state', CASE WHEN xray_state = 'unknown'
                            THEN xray_observed->>'previous_state' ELSE xray_state END,
                        'state', 'unknown',
                        'reason', 'node token re-signed; environment must converge again',
                        'reconcile', 'node-token-resigned'
                    ),
                    hy2_port_hop_state = 'unknown',
                    hy2_port_hop_sha256 = NULL,
                    hy2_port_hop_observed = jsonb_build_object(
                        'previous_state', CASE WHEN hy2_port_hop_state = 'unknown'
                            THEN hy2_port_hop_observed->>'previous_state' ELSE hy2_port_hop_state END,
                        'state', 'unknown',
                        'reason', 'node token re-signed; environment must converge again',
                        'reconcile', 'node-token-resigned'
                    ),
                    grants_state = 'unknown',
                    grants_observed = jsonb_build_object(
                        'previous_state', CASE WHEN grants_state = 'unknown'
                            THEN grants_observed->>'previous_state' ELSE grants_state END,
                        'state', 'unknown',
                        'reason', 'node token re-signed; environment must converge again',
                        'reconcile', 'node-token-resigned'
                    )
              WHERE node_id = $1",
        )
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    }
    crate::notifications::initialize_waiting(&mut tx, node_id).await?;
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
        "WITH authenticated AS MATERIALIZED (
             SELECT agent.node_id, agent.token_prefix, lifecycle.phase
               FROM node_agent_state AS agent
               JOIN node_lifecycle_state AS lifecycle ON lifecycle.node_id = agent.node_id
              WHERE agent.token_hash = $1
                AND agent.token_revoked_at IS NULL
                AND lifecycle.phase IN ('active', 'retiring')
         ), touched AS (
             UPDATE node_agent_state AS agent
                SET token_last_used_at = now()
               FROM authenticated AS auth
              WHERE agent.node_id = auth.node_id
                AND (agent.token_last_used_at IS NULL
                     OR agent.token_last_used_at < now() - interval '1 minute')
          RETURNING agent.node_id
         )
         SELECT node_id, token_prefix, phase FROM authenticated",
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
    let mut tx = pool.begin().await?;
    let phase = sqlx::query_scalar::<_, String>(
        "UPDATE node_agent_state AS agent
            SET last_poll_at = now(),
                agent_version = COALESCE($2, agent.agent_version),
                agent_protocol_version = COALESCE($3, agent.agent_protocol_version)
           FROM node_lifecycle_state AS lifecycle
          WHERE agent.node_id = $1
            AND lifecycle.node_id = agent.node_id
            AND agent.token_hash IS NOT NULL
            AND agent.token_revoked_at IS NULL
      RETURNING lifecycle.phase",
    )
    .bind(node_id)
    .bind(agent_version)
    .bind(protocol_version)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(phase) = phase else {
        return Err(StoreError::Unauthorized(format!(
            "node {node_id} does not have an active node token"
        )));
    };
    // A retiring Agent continues polling until teardown converges, but that is not a machine
    // coming online again. Presence notifications describe active service only.
    if phase != NodeLifecyclePhase::Active.as_str() {
        tx.commit().await?;
        return Ok(());
    }

    let server_now: i64 =
        sqlx::query_scalar("SELECT extract(epoch FROM clock_timestamp())::bigint")
            .fetch_one(&mut *tx)
            .await?;
    let presence = sqlx::query_scalar::<_, String>(
        "SELECT status FROM node_presence_state WHERE node_id = $1 FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    crate::notifications::mark_online_locked(&mut tx, node_id, presence.as_deref(), server_now)
        .await?;
    tx.commit().await?;

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

fn public_ipv4(value: &str) -> Option<String> {
    let ip = value.trim().parse::<Ipv4Addr>().ok()?;
    let [a, b, c, _] = ip.octets();
    let excluded = a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224;
    (!excluded).then(|| ip.to_string())
}

fn public_ipv6(value: &str) -> Option<String> {
    let ip = value.trim().parse::<Ipv6Addr>().ok()?;
    let segments = ip.segments();
    let excluded = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || ip.to_ipv4_mapped().is_some();
    (!excluded).then(|| ip.to_string())
}

pub fn public_route_ip(value: &str) -> Option<IpAddr> {
    match value.trim().parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) => public_ipv4(&ip.to_string()).and(Some(IpAddr::V4(ip))),
        IpAddr::V6(ip) => public_ipv6(&ip.to_string()).and(Some(IpAddr::V6(ip))),
    }
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
/// `agent_version` is not written here. That column's sole writer is `record_node_poll`, taking
/// the User-Agent header; the agent's runtime self-observation stays in `runtime_versions.agent`
/// alongside the xray/phantun/wg fields.
///
/// Both values are the sha256 of the agent's own binary rather than a version number, so what a
/// reader of either column is comparing is a build.
pub async fn record_node_runtime(
    pool: &PgPool,
    node_id: &str,
    report: &brocade_deployment::protocol::NodeRuntimeReport,
) -> Result<()> {
    let server_now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let observed_at = report.observed_at_unix_secs;
    if observed_at <= 0 {
        return Err(StoreError::InvalidData(
            "runtime observed_at must be positive unix seconds".to_owned(),
        ));
    }
    let skew = observed_at.saturating_sub(server_now).abs();
    if skew > MAX_RUNTIME_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "runtime clock skew {skew}s exceeds {MAX_RUNTIME_CLOCK_SKEW_SECS}s; check the node's clock"
        )));
    }
    let versions = serde_json::to_value(&report.versions)?;
    let spool = serde_json::to_value(&report.spool)?;
    let reconcile = report
        .local_reconcile
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;
    let wireguard_health = report
        .wireguard_health
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;
    let xray_upgrade = report
        .xray_upgrade
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

    let mut tx = pool.begin().await?;

    let result = sqlx::query(
        "UPDATE node_agent_state
         SET runtime_versions = $2,
             spool_backlog = $3,
             last_local_reconcile = COALESCE($4, last_local_reconcile),
             wireguard_health = COALESCE($5, wireguard_health),
             geodata_observed = $6,
             xray_upgrade = $9,
             runtime_reported_at = to_timestamp($7),
             online_sources_reported_at = CASE
                 WHEN $8 THEN to_timestamp($7)
                 ELSE online_sources_reported_at
             END
         WHERE node_id = $1
           AND token_hash IS NOT NULL
           AND token_revoked_at IS NULL
           AND (runtime_reported_at IS NULL OR runtime_reported_at <= to_timestamp($7))",
    )
    .bind(node_id)
    .bind(versions)
    .bind(spool)
    .bind(reconcile)
    .bind(wireguard_health)
    .bind(geodata)
    .bind(observed_at as f64)
    .bind(report.online_sources.is_some())
    .bind(xray_upgrade)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                SELECT 1 FROM node_agent_state
                 WHERE node_id = $1
                   AND token_hash IS NOT NULL
                   AND token_revoked_at IS NULL
             )",
        )
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
        if active {
            // A newer runtime snapshot already won. The old report is accepted as a harmless
            // duplicate so its sender does not retry a state that can never become current.
            tx.commit().await?;
            return Ok(());
        }
        return Err(StoreError::Unauthorized(format!(
            "node {node_id} does not have an active node token"
        )));
    }
    if report.xray_upgrade.is_none() {
        // The request is an authorization ledger. Clearing the transient two-generation runtime
        // settles it regardless of whether the old generation drained naturally just before the
        // command arrived or was explicitly terminated; both are safe terminal outcomes.
        sqlx::query(
            "UPDATE config_xray_force_retirements
                SET settled_at = COALESCE(settled_at, now())
              WHERE node_id = $1 AND settled_at IS NULL",
        )
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(online_sources) = report.online_sources.as_deref() {
        crate::user_presence::replace_node_snapshot(&mut tx, node_id, observed_at, online_sources)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Return the only active force-retirement authorization for this node.
///
/// An operator-created authorization wins. Otherwise, a newer configuration work order that is
/// actually eligible for this node authorizes retiring the preceding configuration generation so
/// it cannot block the new release until the old grace window expires. The Agent still treats the
/// result as a hint until its local durable runtime matches the complete tuple; no PID or process
/// identity crosses the control-plane boundary.
pub async fn config_xray_force_retirement_command(
    pool: &PgPool,
    node_id: &str,
) -> Result<Option<ConfigXrayForceRetirementCommand>> {
    let read_active = || async {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT deployment_id, claim_generation
               FROM config_xray_force_retirements
              WHERE node_id = $1 AND settled_at IS NULL",
        )
        .bind(node_id)
        .fetch_optional(pool)
        .await
    };
    let command_from = |(deployment_id, claim_generation): (i64, i64)| {
        Ok(ConfigXrayForceRetirementCommand {
            deployment_id,
            claim_generation: u64::try_from(claim_generation).map_err(|_| {
                StoreError::InvalidData(format!(
                    "node {node_id} force-retirement claim generation is negative"
                ))
            })?,
        })
    };
    if let Some(identity) = read_active().await? {
        return command_from(identity).map(Some);
    }

    let Some(next) = crate::deployment::load_desired_for_node(pool, node_id).await? else {
        return Ok(None);
    };
    let mut tx = pool.begin().await?;
    let next_release = sqlx::query(
        "SELECT kind, active, status
           FROM deployments
          WHERE id = $1
          FOR SHARE",
    )
    .bind(next.deployment_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(next_release) = next_release else {
        return Ok(None);
    };
    let next_kind: String = next_release.try_get("kind")?;
    let next_active: Option<bool> = next_release.try_get("active")?;
    let next_status: String = next_release.try_get("status")?;
    if next_kind != "config"
        || next_active != Some(true)
        || !matches!(next_status.as_str(), "planned" | "running")
    {
        return Ok(None);
    }

    let Some(runtime_value) = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT xray_upgrade
           FROM node_agent_state
          WHERE node_id = $1
            AND runtime_reported_at >= now() - interval '2 minutes'
          FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?
    .flatten() else {
        return Ok(None);
    };
    let runtime: XrayUpgradeRuntime = serde_json::from_value(runtime_value).map_err(|error| {
        StoreError::InvalidData(format!(
            "node {node_id} reported invalid Xray handoff state: {error}"
        ))
    })?;
    let (deployment_id, claim_generation) = match runtime.source {
        XrayUpgradeSource::ConfigDeployment {
            deployment_id,
            claim_generation,
        } if deployment_id != next.deployment_id => (deployment_id, claim_generation),
        _ => return Ok(None),
    };
    if !runtime.old.alive
        || !matches!(
            runtime.phase,
            XrayUpgradePhase::Draining | XrayUpgradePhase::Blocked
        )
        || !runtime
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.alive && candidate.accepting == Some(true))
    {
        return Ok(None);
    }
    let claim_generation = i64::try_from(claim_generation).map_err(|_| {
        StoreError::Conflict("claim generation is out of PostgreSQL bigint range".to_owned())
    })?;
    sqlx::query(
        "UPDATE config_xray_force_retirements
            SET settled_at = COALESCE(settled_at, now())
          WHERE node_id = $1
            AND settled_at IS NULL
            AND (deployment_id, claim_generation) <> ($2, $3)",
    )
    .bind(node_id)
    .bind(deployment_id)
    .bind(claim_generation)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO config_xray_force_retirements (
             deployment_id, node_id, claim_generation, requested_by
         ) VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(deployment_id)
    .bind(node_id)
    .bind(claim_generation)
    .bind(format!("deployment:{}", next.deployment_id))
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT deployment_id, claim_generation
           FROM config_xray_force_retirements
          WHERE node_id = $1 AND settled_at IS NULL",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let command = row.map(command_from).transpose()?;
    tx.commit().await?;
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_route_addresses_are_candidates_for_autofill() {
        assert_eq!(
            public_route_ip("172.93.186.36").unwrap().to_string(),
            "172.93.186.36"
        );
        assert_eq!(
            public_route_ip("2606:4700:4700::1111").unwrap().to_string(),
            "2606:4700:4700::1111"
        );
        for address in [
            "127.0.0.1",
            "10.0.0.8",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "203.0.113.10",
            "::1",
            "fd00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert_eq!(public_route_ip(address), None, "{address}");
        }
    }
}
