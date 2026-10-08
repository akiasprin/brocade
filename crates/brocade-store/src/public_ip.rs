use std::net::IpAddr;

use brocade_deployment::protocol::{NodePublicIpObservation, PublicIpFamily};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{public_route_ip, AdminContext, Result, StoreError};

const MAX_OBSERVATION_CLOCK_SKEW_SECS: i64 = 600;
const CHANGE_CONFIRMATION_SECS: i64 = 10;
pub const PUBLIC_IP_EVENT_RETENTION_DAYS: u32 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicIpObservationOutcome {
    FirstObserved,
    Unchanged,
    Candidate,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPublicIpObservationResult {
    pub outcome: PublicIpObservationOutcome,
    pub family: PublicIpFamily,
    pub current_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePublicIpStateView {
    pub current_ip: String,
    pub country_code: Option<String>,
    pub since_at: String,
    pub last_seen_at: String,
    pub candidate_ip: Option<String>,
    pub candidate_first_seen_at: Option<String>,
    pub candidate_observations: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePublicIpEventView {
    pub id: i64,
    pub family: PublicIpFamily,
    pub event_kind: String,
    pub previous_ip: Option<String>,
    pub current_ip: String,
    pub previous_country_code: Option<String>,
    pub current_country_code: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePublicIpHistory {
    pub node_id: String,
    pub visible_days: u32,
    pub retention_days: u32,
    pub events: Vec<NodePublicIpEventView>,
}

pub async fn record(
    pool: &PgPool,
    node_id: &str,
    observation: &NodePublicIpObservation,
) -> Result<RecordPublicIpObservationResult> {
    let ip = validate_ip(observation.family, &observation.ip)?;
    let country = normalize_country(observation.country_code.as_deref())?;
    if observation.observed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "public IP observed_at must be a positive unix timestamp".to_owned(),
        ));
    }

    let mut tx = pool.begin().await?;
    // Locking the node makes the first insert race-free and keeps v4/v6 transitions ordered for
    // future notification outbox insertion. The observations themselves still retain separate
    // state rows and never clear one family when the other fails.
    let exists = sqlx::query_scalar::<_, String>("SELECT id FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
    if !exists {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }
    let server_now: i64 =
        sqlx::query_scalar("SELECT extract(epoch FROM clock_timestamp())::bigint")
            .fetch_one(&mut *tx)
            .await?;
    let skew = observation
        .observed_at_unix_secs
        .saturating_sub(server_now)
        .abs();
    if skew > MAX_OBSERVATION_CLOCK_SKEW_SECS {
        return Err(StoreError::InvalidData(format!(
            "public IP observation clock skew {skew}s exceeds {MAX_OBSERVATION_CLOCK_SKEW_SECS}s"
        )));
    }

    let family = observation.family.number();
    let row = sqlx::query(
        "SELECT current_ip, country_code, candidate_ip, candidate_country_code,
                extract(epoch FROM candidate_first_seen_at)::bigint AS candidate_first_seen_at,
                candidate_observations
           FROM node_public_ip_state
          WHERE node_id = $1 AND family = $2
          FOR UPDATE",
    )
    .bind(node_id)
    .bind(family)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(row) = row else {
        sqlx::query(
            "INSERT INTO node_public_ip_state (
                node_id, family, current_ip, country_code, since_at, last_seen_at
             ) VALUES ($1, $2, $3, $4, to_timestamp($5), to_timestamp($5))",
        )
        .bind(node_id)
        .bind(family)
        .bind(&ip)
        .bind(country.as_deref())
        .bind(server_now as f64)
        .execute(&mut *tx)
        .await?;
        insert_event(
            &mut tx,
            node_id,
            family,
            "first_observed",
            None,
            &ip,
            None,
            country.as_deref(),
            server_now,
        )
        .await?;
        tx.commit().await?;
        return Ok(RecordPublicIpObservationResult {
            outcome: PublicIpObservationOutcome::FirstObserved,
            family: observation.family,
            current_ip: ip,
            previous_ip: None,
        });
    };

    let current_ip: String = row.try_get("current_ip")?;
    let current_country: Option<String> = row.try_get("country_code")?;
    if current_ip == ip {
        sqlx::query(
            "UPDATE node_public_ip_state
                SET country_code = COALESCE($3, country_code),
                    last_seen_at = to_timestamp($4),
                    candidate_ip = NULL,
                    candidate_country_code = NULL,
                    candidate_first_seen_at = NULL,
                    candidate_last_seen_at = NULL,
                    candidate_observations = 0
              WHERE node_id = $1 AND family = $2",
        )
        .bind(node_id)
        .bind(family)
        .bind(country.as_deref())
        .bind(server_now as f64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(RecordPublicIpObservationResult {
            outcome: PublicIpObservationOutcome::Unchanged,
            family: observation.family,
            current_ip,
            previous_ip: None,
        });
    }

    let candidate_ip: Option<String> = row.try_get("candidate_ip")?;
    let candidate_first_seen: Option<i64> = row.try_get("candidate_first_seen_at")?;
    let candidate_observations: i32 = row.try_get("candidate_observations")?;
    if candidate_ip.as_deref() == Some(ip.as_str())
        && candidate_observations >= 1
        && candidate_first_seen
            .is_some_and(|first| server_now.saturating_sub(first) >= CHANGE_CONFIRMATION_SECS)
    {
        let candidate_country: Option<String> = row.try_get("candidate_country_code")?;
        let next_country = country.or(candidate_country);
        // The public fact changes only when this confirming sample arrives. Candidate time is
        // solely a debounce boundary; publishing the transition there would backdate history and
        // can place a change before the first-observed event.
        let observed_at = server_now;
        sqlx::query(
            "UPDATE node_public_ip_state
                SET current_ip = $3,
                    country_code = $4,
                    since_at = to_timestamp($5),
                    last_seen_at = to_timestamp($6),
                    candidate_ip = NULL,
                    candidate_country_code = NULL,
                    candidate_first_seen_at = NULL,
                    candidate_last_seen_at = NULL,
                    candidate_observations = 0
              WHERE node_id = $1 AND family = $2",
        )
        .bind(node_id)
        .bind(family)
        .bind(&ip)
        .bind(next_country.as_deref())
        .bind(observed_at as f64)
        .bind(server_now as f64)
        .execute(&mut *tx)
        .await?;
        insert_event(
            &mut tx,
            node_id,
            family,
            "changed",
            Some(&current_ip),
            &ip,
            current_country.as_deref(),
            next_country.as_deref(),
            observed_at,
        )
        .await?;
        crate::notifications::insert_machine_event(
            &mut tx,
            node_id,
            "public_ip_changed",
            Some(family),
            Some(&current_ip),
            Some(&ip),
            observed_at,
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        return Ok(RecordPublicIpObservationResult {
            outcome: PublicIpObservationOutcome::Changed,
            family: observation.family,
            current_ip: ip,
            previous_ip: Some(current_ip),
        });
    }

    if candidate_ip.as_deref() == Some(ip.as_str()) {
        sqlx::query(
            "UPDATE node_public_ip_state
                SET candidate_country_code = COALESCE($3, candidate_country_code),
                    candidate_last_seen_at = to_timestamp($4),
                    candidate_observations = candidate_observations + 1
              WHERE node_id = $1 AND family = $2",
        )
        .bind(node_id)
        .bind(family)
        .bind(country.as_deref())
        .bind(server_now as f64)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE node_public_ip_state
                SET candidate_ip = $3,
                    candidate_country_code = $4,
                    candidate_first_seen_at = to_timestamp($5),
                    candidate_last_seen_at = to_timestamp($5),
                    candidate_observations = 1
              WHERE node_id = $1 AND family = $2",
        )
        .bind(node_id)
        .bind(family)
        .bind(&ip)
        .bind(country.as_deref())
        .bind(server_now as f64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(RecordPublicIpObservationResult {
        outcome: PublicIpObservationOutcome::Candidate,
        family: observation.family,
        current_ip,
        previous_ip: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: &str,
    family: i16,
    event_kind: &str,
    previous_ip: Option<&str>,
    current_ip: &str,
    previous_country: Option<&str>,
    current_country: Option<&str>,
    observed_at: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO node_public_ip_events (
            node_id, family, event_kind, previous_ip, current_ip,
            previous_country_code, current_country_code, observed_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8))",
    )
    .bind(node_id)
    .bind(family)
    .bind(event_kind)
    .bind(previous_ip)
    .bind(current_ip)
    .bind(previous_country)
    .bind(current_country)
    .bind(observed_at as f64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn validate_ip(family: PublicIpFamily, value: &str) -> Result<String> {
    let ip = public_route_ip(value).ok_or_else(|| {
        StoreError::InvalidData("public IP observation is not globally routable".to_owned())
    })?;
    if !matches!(
        (family, ip),
        (PublicIpFamily::V4, IpAddr::V4(_)) | (PublicIpFamily::V6, IpAddr::V6(_))
    ) {
        return Err(StoreError::InvalidData(
            "public IP observation does not match its address family".to_owned(),
        ));
    }
    Ok(ip.to_string())
}

fn normalize_country(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(StoreError::InvalidData(
            "public IP country code must contain two ASCII letters".to_owned(),
        ));
    }
    Ok(Some(value.to_ascii_uppercase()))
}

pub async fn prune(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = retain_days.clamp(1, 3650) as i32;
    let result = sqlx::query(
        "DELETE FROM node_public_ip_events
          WHERE recorded_at < now() - make_interval(days => $1)",
    )
    .bind(retain_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn history(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    visible_days: u32,
) -> Result<NodePublicIpHistory> {
    let visible_days = visible_days.clamp(1, PUBLIC_IP_EVENT_RETENTION_DAYS);
    let visible = if actor.is_system_admin() {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM nodes WHERE id = $1)")
            .bind(node_id)
            .fetch_one(pool)
            .await?
    } else {
        let scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                SELECT 1 FROM nodes
                 WHERE id = $1
                   AND (tenant_id = $2 OR tenant_id LIKE $3 ESCAPE '\\')
             )",
        )
        .bind(node_id)
        .bind(scope)
        .bind(pattern)
        .fetch_one(pool)
        .await?
    };
    if !visible {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }

    let rows = sqlx::query(
        "SELECT id, family, event_kind, previous_ip, current_ip,
                previous_country_code, current_country_code,
                observed_at::text AS observed_at
           FROM node_public_ip_events
          WHERE node_id = $1
            AND observed_at >= now() - make_interval(days => $2)
          ORDER BY observed_at DESC, id DESC
          LIMIT 500",
    )
    .bind(node_id)
    .bind(visible_days as i32)
    .fetch_all(pool)
    .await?;
    let events = rows
        .into_iter()
        .map(|row| {
            let family = match row.try_get::<i16, _>("family")? {
                4 => PublicIpFamily::V4,
                6 => PublicIpFamily::V6,
                value => {
                    return Err(StoreError::InvalidData(format!(
                        "node public IP event has invalid family {value}"
                    )))
                }
            };
            Ok(NodePublicIpEventView {
                id: row.try_get("id")?,
                family,
                event_kind: row.try_get("event_kind")?,
                previous_ip: row.try_get("previous_ip")?,
                current_ip: row.try_get("current_ip")?,
                previous_country_code: row.try_get("previous_country_code")?,
                current_country_code: row.try_get("current_country_code")?,
                observed_at: row.try_get("observed_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(NodePublicIpHistory {
        node_id: node_id.to_owned(),
        visible_days,
        retention_days: PUBLIC_IP_EVENT_RETENTION_DAYS,
        events,
    })
}

#[cfg(test)]
mod tests {
    use brocade_deployment::protocol::PublicIpFamily;

    use super::{normalize_country, validate_ip};

    #[test]
    fn validates_public_address_family() {
        assert_eq!(
            validate_ip(PublicIpFamily::V4, "1.1.1.1").unwrap(),
            "1.1.1.1"
        );
        assert!(validate_ip(PublicIpFamily::V6, "1.1.1.1").is_err());
        assert!(validate_ip(PublicIpFamily::V4, "10.0.0.1").is_err());
    }

    #[test]
    fn normalizes_country_codes() {
        assert_eq!(
            normalize_country(Some("tw")).unwrap().as_deref(),
            Some("TW")
        );
        assert!(normalize_country(Some("TWN")).is_err());
    }
}
