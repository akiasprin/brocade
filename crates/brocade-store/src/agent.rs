use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use brocade_deployment::protocol::RouteIpReport;

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

/// Fills only address families which the operator left blank. The write is model state: it gets
/// its own revision and snapshot so subscriptions and future releases see the same address as the
/// node list. Repeated heartbeats and later route changes are intentionally no-ops.
pub async fn autofill_node_public_ips(
    pool: &PgPool,
    node_id: &str,
    report: &RouteIpReport,
) -> Result<Option<u64>> {
    let ipv4 = report.ipv4.as_deref().and_then(public_ipv4);
    let ipv6 = report.ipv6.as_deref().and_then(public_ipv6);
    if ipv4.is_none() && ipv6.is_none() {
        return Ok(None);
    }

    let mut tx = pool.begin().await?;
    let previous = crate::console::lock_control_state(&mut tx).await?;
    let row = sqlx::query("SELECT public_ipv4, public_ipv6 FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    let current_ipv4: Option<String> = row.try_get("public_ipv4")?;
    let current_ipv6: Option<String> = row.try_get("public_ipv6")?;
    let fill_ipv4 = current_ipv4.is_none().then_some(ipv4).flatten();
    let fill_ipv6 = current_ipv6.is_none().then_some(ipv6).flatten();
    if fill_ipv4.is_none() && fill_ipv6.is_none() {
        tx.commit().await?;
        return Ok(None);
    }

    let revision_id = crate::console::insert_revision(
        &mut tx,
        &format!("agent:{node_id}"),
        &format!("auto-detect public IP for {node_id}"),
    )
    .await?;
    sqlx::query(
        "UPDATE nodes
            SET public_ipv4 = COALESCE(public_ipv4, $2),
                public_ipv6 = COALESCE(public_ipv6, $3)
          WHERE id = $1",
    )
    .bind(node_id)
    .bind(fill_ipv4)
    .bind(fill_ipv6)
    .execute(&mut *tx)
    .await?;
    let revision_id = crate::console::commit_revision(&mut tx, revision_id, previous, true).await?;
    tx.commit().await?;
    Ok(Some(revision_id))
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
    let server_now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(pool)
        .await?;
    let observed_at = report.observed_at_unix_secs.unwrap_or(server_now);
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
             wireguard_health = COALESCE($5, wireguard_health),
             geodata_observed = $6,
             runtime_reported_at = to_timestamp($7)
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
    .execute(pool)
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
        .fetch_one(pool)
        .await?;
        if active {
            // A newer runtime snapshot already won. The old report is accepted as a harmless
            // duplicate so its sender does not retry a state that can never become current.
            return Ok(());
        }
        return Err(StoreError::Unauthorized(format!(
            "node {node_id} does not have an active node token"
        )));
    }
    Ok(())
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
