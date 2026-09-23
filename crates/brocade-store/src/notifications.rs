use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

pub const MACHINE_EVENT_RETENTION_DAYS: u32 = 90;
const OFFLINE_AFTER_SECS: i32 = 90;
const DELIVERY_LEASE_SECS: i32 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEventView {
    pub id: i64,
    pub node_id: String,
    pub node_name: String,
    pub event_kind: String,
    pub family: Option<i16>,
    pub previous_value: Option<String>,
    pub current_value: Option<String>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEventList {
    pub retention_days: u32,
    pub events: Vec<MachineEventView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimedNotificationDelivery {
    pub delivery_id: i64,
    pub attempt: i32,
    pub event: MachineEventView,
}

pub(crate) async fn initialize_waiting(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO node_presence_state (node_id, status)
         VALUES ($1, 'waiting')
         ON CONFLICT (node_id) DO NOTHING",
    )
    .bind(node_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_machine_event(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    event_kind: &str,
    family: Option<i16>,
    previous_value: Option<&str>,
    current_value: Option<&str>,
    occurred_at_unix_secs: i64,
) -> Result<i64> {
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO machine_events (
            node_id, event_kind, family, previous_value, current_value, occurred_at
         ) VALUES ($1, $2, $3, $4, $5, to_timestamp($6))
         RETURNING id",
    )
    .bind(node_id)
    .bind(event_kind)
    .bind(family)
    .bind(previous_value)
    .bind(current_value)
    .bind(occurred_at_unix_secs as f64)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO notification_deliveries (event_id, channel)
         VALUES ($1, 'webhook')",
    )
    .bind(event_id)
    .execute(&mut **tx)
    .await?;
    Ok(event_id)
}

/// Mark online machines offline after six missed desired-state polls. The Agent currently polls
/// every 15 seconds, so 90 seconds tolerates transient connection failures without delaying a
/// genuine outage notification for minutes.
pub async fn reconcile_offline(pool: &PgPool) -> Result<u64> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query(
        "SELECT presence.node_id,
                extract(epoch FROM clock_timestamp())::bigint AS occurred_at
           FROM node_presence_state presence
           JOIN node_agent_state agent ON agent.node_id = presence.node_id
           LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = presence.node_id
          WHERE presence.status = 'online'
            AND agent.last_poll_at < now() - make_interval(secs => $1)
            AND COALESCE(lifecycle.phase, 'active') = 'active'
          ORDER BY presence.node_id
          FOR UPDATE OF presence SKIP LOCKED",
    )
    .bind(OFFLINE_AFTER_SECS)
    .fetch_all(&mut *tx)
    .await?;
    for row in &rows {
        let node_id: String = row.try_get("node_id")?;
        let occurred_at: i64 = row.try_get("occurred_at")?;
        sqlx::query(
            "UPDATE node_presence_state
                SET status = 'offline', since_at = to_timestamp($2)
              WHERE node_id = $1",
        )
        .bind(&node_id)
        .bind(occurred_at as f64)
        .execute(&mut *tx)
        .await?;
        insert_machine_event(
            &mut tx,
            &node_id,
            "node_offline",
            None,
            Some("online"),
            Some("offline"),
            occurred_at,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

pub async fn claim_delivery(
    pool: &PgPool,
    owner: &str,
) -> Result<Option<ClaimedNotificationDelivery>> {
    if owner.trim().is_empty() {
        return Err(StoreError::InvalidData(
            "notification delivery owner must not be empty".to_owned(),
        ));
    }
    let row = sqlx::query(
        "WITH candidate AS (
             SELECT delivery.id
               FROM notification_deliveries delivery
              WHERE (delivery.status = 'pending' AND delivery.next_attempt_at <= now())
                 OR (delivery.status = 'delivering' AND delivery.lease_until <= now())
              ORDER BY delivery.next_attempt_at, delivery.id
              FOR UPDATE SKIP LOCKED
              LIMIT 1
         ), claimed AS (
             UPDATE notification_deliveries delivery
                SET status = 'delivering',
                    attempts = delivery.attempts + 1,
                    lease_owner = $1,
                    lease_until = now() + make_interval(secs => $2),
                    last_error = NULL
               FROM candidate
              WHERE delivery.id = candidate.id
          RETURNING delivery.id, delivery.event_id, delivery.attempts
         )
         SELECT claimed.id AS delivery_id, claimed.attempts,
                event.id, event.node_id, node.name AS node_name, event.event_kind,
                event.family, event.previous_value, event.current_value,
                event.occurred_at::text AS occurred_at
           FROM claimed
           JOIN machine_events event ON event.id = claimed.event_id
           JOIN nodes node ON node.id = event.node_id",
    )
    .bind(owner)
    .bind(DELIVERY_LEASE_SECS)
    .fetch_optional(pool)
    .await?;
    row.map(machine_delivery_from_row).transpose()
}

pub async fn complete_delivery(
    pool: &PgPool,
    delivery_id: i64,
    owner: &str,
    attempt: i32,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE notification_deliveries
            SET status = 'delivered', delivered_at = now(),
                lease_owner = NULL, lease_until = NULL, last_error = NULL
          WHERE id = $1 AND status = 'delivering'
            AND lease_owner = $2 AND attempts = $3",
    )
    .bind(delivery_id)
    .bind(owner)
    .bind(attempt)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn fail_delivery(
    pool: &PgPool,
    delivery_id: i64,
    owner: &str,
    attempt: i32,
    error: &str,
) -> Result<bool> {
    let exponent = u32::try_from(attempt.saturating_sub(1))
        .unwrap_or_default()
        .min(10);
    let retry_secs = 2_i32.saturating_pow(exponent).clamp(2, 3600);
    let error = error.chars().take(512).collect::<String>();
    let result = sqlx::query(
        "UPDATE notification_deliveries
            SET status = 'pending',
                next_attempt_at = now() + make_interval(secs => $4),
                lease_owner = NULL, lease_until = NULL, last_error = $5
          WHERE id = $1 AND status = 'delivering'
            AND lease_owner = $2 AND attempts = $3",
    )
    .bind(delivery_id)
    .bind(owner)
    .bind(attempt)
    .bind(retry_secs)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn list(pool: &PgPool, actor: &AdminContext, limit: u32) -> Result<MachineEventList> {
    let limit = i64::from(limit.clamp(1, 200));
    let rows = if actor.is_system_admin() {
        sqlx::query(
            "SELECT event.id, event.node_id, node.name AS node_name, event.event_kind,
                    event.family, event.previous_value, event.current_value,
                    event.occurred_at::text AS occurred_at
               FROM machine_events event
               JOIN nodes node ON node.id = event.node_id
              ORDER BY event.occurred_at DESC, event.id DESC
              LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        let scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        sqlx::query(
            "SELECT event.id, event.node_id, node.name AS node_name, event.event_kind,
                    event.family, event.previous_value, event.current_value,
                    event.occurred_at::text AS occurred_at
               FROM machine_events event
               JOIN nodes node ON node.id = event.node_id
              WHERE node.tenant_id = $1 OR node.tenant_id LIKE $2 ESCAPE '\\'
              ORDER BY event.occurred_at DESC, event.id DESC
              LIMIT $3",
        )
        .bind(scope)
        .bind(pattern)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };
    Ok(MachineEventList {
        retention_days: MACHINE_EVENT_RETENTION_DAYS,
        events: rows
            .into_iter()
            .map(|row| machine_event_from_row(&row))
            .collect::<Result<Vec<_>>>()?,
    })
}

pub async fn prune(pool: &PgPool, retain_days: u32) -> Result<u64> {
    let retain_days = retain_days.clamp(1, 3650) as i32;
    let result = sqlx::query(
        "DELETE FROM machine_events
          WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(retain_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

fn machine_delivery_from_row(row: sqlx::postgres::PgRow) -> Result<ClaimedNotificationDelivery> {
    Ok(ClaimedNotificationDelivery {
        delivery_id: row.try_get("delivery_id")?,
        attempt: row.try_get("attempts")?,
        event: machine_event_from_row(&row)?,
    })
}

fn machine_event_from_row(row: &sqlx::postgres::PgRow) -> Result<MachineEventView> {
    Ok(MachineEventView {
        id: row.try_get("id")?,
        node_id: row.try_get("node_id")?,
        node_name: row.try_get("node_name")?,
        event_kind: row.try_get("event_kind")?,
        family: row.try_get("family")?,
        previous_value: row.try_get("previous_value")?,
        current_value: row.try_get("current_value")?,
        occurred_at: row.try_get("occurred_at")?,
    })
}
