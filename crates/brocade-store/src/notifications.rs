use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

pub const MACHINE_EVENT_RETENTION_DAYS: u32 = 90;
const OFFLINE_AFTER_SECS: i32 = 90;
const DELIVERY_LEASE_SECS: i32 = 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineEventView {
    pub id: i64,
    pub node_id: String,
    pub node_name: String,
    pub event_kind: String,
    pub family: Option<i16>,
    pub previous_value: Option<String>,
    pub current_value: Option<String>,
    pub last_contact_at: Option<String>,
    pub incident_started_at: Option<String>,
    pub metric_value: Option<f32>,
    pub metric_peak_value: Option<f32>,
    pub metric_threshold: Option<f32>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveMachineIncidentView {
    pub event_id: i64,
    pub node_id: String,
    pub node_name: String,
    pub incident_kind: String,
    pub started_at: String,
    pub detected_at: String,
    pub last_observed_at: Option<String>,
    pub current_value: Option<f32>,
    pub peak_value: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineEventList {
    pub retention_days: u32,
    pub latest_event_id: i64,
    pub last_seen_event_id: i64,
    /// Dismisses incident notifications only; public-IP changes ignore this cursor.
    pub cleared_through_event_id: i64,
    pub unread_count: u64,
    pub active: Vec<ActiveMachineIncidentView>,
    pub events: Vec<MachineEventView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimedNotificationDelivery {
    pub delivery_id: i64,
    pub attempt: i32,
    pub event: MachineEventView,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MachineMetricEvent {
    pub incident_started_at_unix_secs: i64,
    pub value: f32,
    pub peak_value: f32,
    pub threshold: f32,
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
    last_contact_at_unix_secs: Option<i64>,
    metric: Option<MachineMetricEvent>,
) -> Result<i64> {
    let (incident_started_at, metric_value, metric_peak_value, metric_threshold) = metric
        .map(|metric| {
            (
                Some(metric.incident_started_at_unix_secs as f64),
                Some(metric.value),
                Some(metric.peak_value),
                Some(metric.threshold),
            )
        })
        .unwrap_or((None, None, None, None));
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO machine_events (
            node_id, event_kind, family, previous_value, current_value,
            last_contact_at, incident_started_at,
            metric_value, metric_peak_value, metric_threshold, occurred_at
         ) VALUES (
            $1, $2, $3, $4, $5, to_timestamp($6), to_timestamp($7),
            $8, $9, $10, to_timestamp($11)
         )
         RETURNING id",
    )
    .bind(node_id)
    .bind(event_kind)
    .bind(family)
    .bind(previous_value)
    .bind(current_value)
    .bind(last_contact_at_unix_secs.map(|value| value as f64))
    .bind(incident_started_at)
    .bind(metric_value)
    .bind(metric_peak_value)
    .bind(metric_threshold)
    .bind(occurred_at_unix_secs as f64)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO notification_deliveries (event_id, channel)
         SELECT $1, 'webhook'
           FROM notification_channels
          WHERE channel = 'webhook' AND enabled",
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
                extract(epoch FROM agent.last_poll_at)::bigint AS last_contact_at,
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
        let last_contact_at: i64 = row.try_get("last_contact_at")?;
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
            Some(last_contact_at),
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

/// Reconcile the optional external channel before the HTTP listener starts accepting events.
/// Disabling keeps the machine event history but makes outstanding legacy work explicitly
/// non-deliverable; reenabling never revives those rows.
pub async fn configure_webhook_channel(pool: &PgPool, enabled: bool) -> Result<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO notification_channels (channel, enabled, changed_at)
         VALUES ('webhook', $1, now())
         ON CONFLICT (channel) DO UPDATE SET
            enabled = EXCLUDED.enabled,
            changed_at = CASE
                WHEN notification_channels.enabled <> EXCLUDED.enabled THEN now()
                ELSE notification_channels.changed_at
            END",
    )
    .bind(enabled)
    .execute(&mut *tx)
    .await?;
    let suppressed = if enabled {
        0
    } else {
        sqlx::query(
            "UPDATE notification_deliveries
                SET status = 'suppressed', lease_owner = NULL, lease_until = NULL,
                    last_error = NULL
              WHERE status IN ('pending', 'delivering')",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
    };
    tx.commit().await?;
    Ok(suppressed)
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
                event.last_contact_at::text AS last_contact_at,
                event.incident_started_at::text AS incident_started_at,
                event.metric_value, event.metric_peak_value, event.metric_threshold,
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
    let (personal_last_seen_event_id, personal_cleared_through_event_id, global_clear_floor) =
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT operator.notification_last_seen_event_id,
                    operator.notification_cleared_through_event_id,
                    (SELECT COALESCE(max(notification_cleared_through_event_id), 0)
                       FROM admin_operators
                      WHERE role = 'system-admin') AS global_clear_floor
               FROM admin_operators operator
              WHERE operator.id = $1",
        )
        .bind(actor.operator_id())
        .fetch_optional(pool)
        .await?
        .unwrap_or_default();
    // A system-admin clear is the fleet-wide floor. Deriving it on reads also reconciles accounts
    // created later and deployments made after the old per-account behavior had already diverged.
    let cleared_through_event_id = personal_cleared_through_event_id.max(global_clear_floor);
    let last_seen_event_id = personal_last_seen_event_id.max(cleared_through_event_id);
    let rows = inbox_events(pool, actor, limit, cleared_through_event_id).await?;
    let (active_rows, summary_row) = if actor.is_global_scope() {
        let active_rows = sqlx::query(
            "SELECT incident.event_id, incident.node_id, incident.node_name,
                    incident.incident_kind, incident.started_at, incident.detected_at,
                    incident.last_observed_at, incident.current_value, incident.peak_value
               FROM (
                    SELECT event.id AS event_id, presence.node_id, node.name AS node_name,
                           'control_plane_offline'::text AS incident_kind,
                           event.last_contact_at::text AS started_at,
                           presence.since_at::text AS detected_at,
                           NULL::text AS last_observed_at,
                           NULL::real AS current_value,
                           NULL::real AS peak_value
                      FROM node_presence_state presence
                      JOIN nodes node ON node.id = presence.node_id
                      JOIN LATERAL (
                           SELECT event.id, event.last_contact_at
                             FROM machine_events event
                            WHERE event.node_id = presence.node_id
                              AND event.event_kind = 'node_offline'
                            ORDER BY event.occurred_at DESC, event.id DESC
                            LIMIT 1
                      ) event ON TRUE
                      LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = presence.node_id
                     WHERE presence.status = 'offline'
                       AND COALESCE(lifecycle.phase, 'active') = 'active'
                    UNION ALL
                    SELECT event.id AS event_id, steal.node_id, node.name AS node_name,
                           'cpu_steal'::text AS incident_kind,
                           steal.active_started_at::text AS started_at,
                           event.occurred_at::text AS detected_at,
                           steal.last_window_end::text AS last_observed_at,
                           steal.current_pct AS current_value,
                           steal.peak_pct AS peak_value
                      FROM node_cpu_steal_state steal
                      JOIN nodes node ON node.id = steal.node_id
                      JOIN LATERAL (
                           SELECT event.id, event.occurred_at
                             FROM machine_events event
                            WHERE event.node_id = steal.node_id
                              AND event.event_kind = 'cpu_steal_started'
                            ORDER BY event.occurred_at DESC, event.id DESC
                            LIMIT 1
                      ) event ON TRUE
                      LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = steal.node_id
                     WHERE steal.status IN ('active', 'recovering')
                       AND COALESCE(lifecycle.phase, 'active') = 'active'
               ) incident
              WHERE incident.event_id > $1
              ORDER BY incident.detected_at DESC, incident.node_id",
        )
        .bind(cleared_through_event_id)
        .fetch_all(pool)
        .await?;
        let summary_row = sqlx::query(
            "SELECT COALESCE(max(id), 0) AS latest_event_id,
                    count(*) FILTER (WHERE id > $1)::bigint AS unread_count
               FROM machine_events",
        )
        .bind(last_seen_event_id)
        .fetch_one(pool)
        .await?;
        (active_rows, summary_row)
    } else {
        let scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        let active_rows = sqlx::query(
            "SELECT incident.event_id, incident.node_id, incident.node_name,
                    incident.incident_kind, incident.started_at, incident.detected_at,
                    incident.last_observed_at, incident.current_value, incident.peak_value
               FROM (
                    SELECT event.id AS event_id, presence.node_id, node.name AS node_name,
                           node.tenant_id,
                           'control_plane_offline'::text AS incident_kind,
                           event.last_contact_at::text AS started_at,
                           presence.since_at::text AS detected_at,
                           NULL::text AS last_observed_at,
                           NULL::real AS current_value,
                           NULL::real AS peak_value
                      FROM node_presence_state presence
                      JOIN nodes node ON node.id = presence.node_id
                      JOIN LATERAL (
                           SELECT event.id, event.last_contact_at
                             FROM machine_events event
                            WHERE event.node_id = presence.node_id
                              AND event.event_kind = 'node_offline'
                            ORDER BY event.occurred_at DESC, event.id DESC
                            LIMIT 1
                      ) event ON TRUE
                      LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = presence.node_id
                     WHERE presence.status = 'offline'
                       AND COALESCE(lifecycle.phase, 'active') = 'active'
                    UNION ALL
                    SELECT event.id AS event_id, steal.node_id, node.name AS node_name,
                           node.tenant_id,
                           'cpu_steal'::text AS incident_kind,
                           steal.active_started_at::text AS started_at,
                           event.occurred_at::text AS detected_at,
                           steal.last_window_end::text AS last_observed_at,
                           steal.current_pct AS current_value,
                           steal.peak_pct AS peak_value
                      FROM node_cpu_steal_state steal
                      JOIN nodes node ON node.id = steal.node_id
                      JOIN LATERAL (
                           SELECT event.id, event.occurred_at
                             FROM machine_events event
                            WHERE event.node_id = steal.node_id
                              AND event.event_kind = 'cpu_steal_started'
                            ORDER BY event.occurred_at DESC, event.id DESC
                            LIMIT 1
                      ) event ON TRUE
                      LEFT JOIN node_lifecycle_state lifecycle ON lifecycle.node_id = steal.node_id
                     WHERE steal.status IN ('active', 'recovering')
                       AND COALESCE(lifecycle.phase, 'active') = 'active'
               ) incident
              WHERE (incident.tenant_id = $1 OR incident.tenant_id LIKE $2 ESCAPE '\\')
                AND incident.event_id > $3
              ORDER BY incident.detected_at DESC, incident.node_id",
        )
        .bind(scope)
        .bind(&pattern)
        .bind(cleared_through_event_id)
        .fetch_all(pool)
        .await?;
        let summary_row = sqlx::query(
            "SELECT COALESCE(max(event.id), 0) AS latest_event_id,
                    count(*) FILTER (WHERE event.id > $3)::bigint AS unread_count
               FROM machine_events event
               JOIN nodes node ON node.id = event.node_id
              WHERE node.tenant_id = $1 OR node.tenant_id LIKE $2 ESCAPE '\\'",
        )
        .bind(scope)
        .bind(&pattern)
        .bind(last_seen_event_id)
        .fetch_one(pool)
        .await?;
        (active_rows, summary_row)
    };
    let unread_count = u64::try_from(summary_row.try_get::<i64, _>("unread_count")?)
        .map_err(|_| StoreError::InvalidData("negative notification unread count".to_owned()))?;
    Ok(MachineEventList {
        retention_days: MACHINE_EVENT_RETENTION_DAYS,
        latest_event_id: summary_row.try_get("latest_event_id")?,
        last_seen_event_id,
        cleared_through_event_id,
        unread_count,
        active: active_rows
            .into_iter()
            .map(|row| {
                Ok(ActiveMachineIncidentView {
                    event_id: row.try_get("event_id")?,
                    node_id: row.try_get("node_id")?,
                    node_name: row.try_get("node_name")?,
                    incident_kind: row.try_get("incident_kind")?,
                    started_at: row.try_get("started_at")?,
                    detected_at: row.try_get("detected_at")?,
                    last_observed_at: row.try_get("last_observed_at")?,
                    current_value: row.try_get("current_value")?,
                    peak_value: row.try_get("peak_value")?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        events: rows
            .into_iter()
            .map(|row| machine_event_from_row(&row))
            .collect::<Result<Vec<_>>>()?,
    })
}

/// Keep the inbox bounded while retaining the opening context of a *new* recovery. Clearing an
/// ongoing outage must not hide its later recovery or invent a zero-duration incident. Public-IP
/// changes are durable notification history within the retention window, so the incident clear
/// cursor never hides them. At most one context event is returned per visible event; callers
/// render by the closing/latest ID.
async fn inbox_events(
    pool: &PgPool,
    actor: &AdminContext,
    limit: i64,
    cleared_through_event_id: i64,
) -> Result<Vec<sqlx::postgres::PgRow>> {
    if !actor.is_global_scope() && actor.tenant_scope().is_none() {
        return Err(StoreError::Forbidden(
            "admin context has no tenant scope".to_owned(),
        ));
    }
    Ok(sqlx::query(
        "WITH visible AS MATERIALIZED (
             SELECT event.id, event.node_id, event.event_kind, event.previous_value
               FROM machine_events event JOIN nodes node ON node.id = event.node_id
              WHERE (event.id > $2 OR event.event_kind = 'public_ip_changed')
                AND ($3 OR node.tenant_id = $4 OR node.tenant_id LIKE $5 ESCAPE '\\')
              ORDER BY event.occurred_at DESC, event.id DESC
              LIMIT $1
         ), event_ids AS (
             SELECT id FROM visible
             UNION
             SELECT opening.id FROM visible recovery
             JOIN LATERAL (
                 SELECT event.id FROM machine_events event
                  WHERE event.node_id = recovery.node_id AND event.id < recovery.id
                    AND event.event_kind = 'node_offline'
                  ORDER BY event.id DESC LIMIT 1
             ) opening ON recovery.event_kind = 'node_online' AND recovery.previous_value = 'offline'
         )
         SELECT event.id, event.node_id, node.name AS node_name, event.event_kind,
                event.family, event.previous_value, event.current_value,
                event.last_contact_at::text AS last_contact_at,
                event.incident_started_at::text AS incident_started_at,
                event.metric_value, event.metric_peak_value, event.metric_threshold,
                event.occurred_at::text AS occurred_at
           FROM event_ids JOIN machine_events event ON event.id = event_ids.id
           JOIN nodes node ON node.id = event.node_id
          ORDER BY event.occurred_at DESC, event.id DESC",
    )
    .bind(limit)
    .bind(cleared_through_event_id)
    .bind(actor.is_global_scope())
    .bind(actor.tenant_scope())
    .bind(actor.tenant_scope_like_pattern())
    .fetch_all(pool)
    .await?)
}

pub async fn mark_read(pool: &PgPool, actor: &AdminContext, through_event_id: i64) -> Result<i64> {
    advance_cursor(pool, actor, through_event_id, false).await
}

pub async fn clear(pool: &PgPool, actor: &AdminContext, through_event_id: i64) -> Result<i64> {
    advance_cursor(pool, actor, through_event_id, true).await
}

async fn advance_cursor(
    pool: &PgPool,
    actor: &AdminContext,
    through_event_id: i64,
    clear: bool,
) -> Result<i64> {
    if through_event_id < 0 {
        return Err(StoreError::InvalidData(
            "notification cursor must not be negative".to_owned(),
        ));
    }
    let latest_visible = if actor.is_global_scope() {
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(max(id), 0) FROM machine_events")
            .fetch_one(pool)
            .await?
    } else {
        let scope = actor
            .tenant_scope()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        let pattern = actor
            .tenant_scope_like_pattern()
            .ok_or_else(|| StoreError::Forbidden("admin context has no tenant scope".to_owned()))?;
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(event.id), 0)
               FROM machine_events event
               JOIN nodes node ON node.id = event.node_id
              WHERE node.tenant_id = $1 OR node.tenant_id LIKE $2 ESCAPE '\\'",
        )
        .bind(scope)
        .bind(pattern)
        .fetch_one(pool)
        .await?
    };
    let bounded = through_event_id.min(latest_visible);
    let cursor = sqlx::query_scalar::<_, i64>(
        "WITH updated AS (
             UPDATE admin_operators
                SET notification_last_seen_event_id = GREATEST(notification_last_seen_event_id, $2),
                    notification_cleared_through_event_id = CASE WHEN $3
                        THEN GREATEST(notification_cleared_through_event_id, $2)
                        ELSE notification_cleared_through_event_id END
              WHERE id = $1 OR ($3 AND $4)
              RETURNING id, notification_last_seen_event_id, notification_cleared_through_event_id
         )
         SELECT CASE WHEN $3 THEN notification_cleared_through_event_id
                     ELSE notification_last_seen_event_id END
           FROM updated
          WHERE id = $1",
    )
    .bind(actor.operator_id())
    .bind(bounded)
    .bind(clear)
    // Clearing is a global acknowledgement only for the system administrator. Read cursors and
    // every non-system account remain personal even when that account has an unscoped read view.
    .bind(actor.is_system_admin())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("operator {}", actor.operator_id())))?;
    Ok(cursor)
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
        last_contact_at: row.try_get("last_contact_at")?,
        incident_started_at: row.try_get("incident_started_at")?,
        metric_value: row.try_get("metric_value")?,
        metric_peak_value: row.try_get("metric_peak_value")?,
        metric_threshold: row.try_get("metric_threshold")?,
        occurred_at: row.try_get("occurred_at")?,
    })
}
