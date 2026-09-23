//! Per-node physical interface accounting and reset policy.
//!
//! Agent readings are absolute logical counters. This layer accepts only monotonic readings from
//! the same meter epoch, converts them to idempotent deltas, and keeps those deltas by UTC day.
//! Reset dates and calibrations are presentation/accounting anchors; neither asks an Agent to zero
//! its durable meter, so policy edits and control-plane outages cannot break continuity.

use brocade_deployment::protocol::NodeTrafficReading;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeTrafficCycleKind {
    Monthly,
    Yearly,
}

impl NodeTrafficCycleKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficItem {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    pub cycle_kind: NodeTrafficCycleKind,
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    pub period_start_unix_secs: i64,
    pub period_end_unix_secs: i64,
    /// Decimal strings keep byte totals exact in JavaScript beyond Number.MAX_SAFE_INTEGER.
    pub rx_bytes: String,
    pub tx_bytes: String,
    pub total_bytes: String,
    pub interface: Option<String>,
    pub tracking_started_at_unix_secs: Option<i64>,
    pub last_reported_at_unix_secs: Option<i64>,
    pub calibrated_at_unix_secs: Option<i64>,
    pub last_gap_at_unix_secs: Option<i64>,
    pub last_gap_reason: Option<String>,
    pub has_gap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficView {
    pub nodes: Vec<NodeTrafficItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNodeTrafficRequest {
    pub cycle_kind: NodeTrafficCycleKind,
    /// Required for yearly and forbidden for monthly.
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    /// When present, append a total-only calibration at the server's current UTC time.
    #[serde(default)]
    pub calibrated_total_bytes: Option<String>,
}

fn valid_policy(request: &UpdateNodeTrafficRequest) -> Result<()> {
    if !(1..=31).contains(&request.reset_day) {
        return Err(StoreError::InvalidData(
            "流量重置日期需在 1–31 日之间".to_owned(),
        ));
    }
    match (&request.cycle_kind, request.reset_month) {
        (NodeTrafficCycleKind::Monthly, None) => Ok(()),
        (NodeTrafficCycleKind::Yearly, Some(1..=12)) => Ok(()),
        (NodeTrafficCycleKind::Monthly, Some(_)) => {
            Err(StoreError::InvalidData("月度重置不能设置月份".to_owned()))
        }
        (NodeTrafficCycleKind::Yearly, _) => {
            Err(StoreError::InvalidData("年度重置需设置 1–12 月".to_owned()))
        }
    }
}

fn parse_decimal_u64(value: &str, what: &str) -> Result<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(StoreError::InvalidData(format!(
            "{what}必须是非负整数字节数"
        )));
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| StoreError::InvalidData(format!("{what}超出 u64 范围")))
}

fn valid_meter_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn valid_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.trim() == value
        && !value.contains('/')
        && value != "."
        && value != ".."
}

fn validate_reading(reading: &NodeTrafficReading) -> Result<()> {
    if !valid_meter_id(&reading.meter_id) {
        return Err(StoreError::InvalidData(
            "traffic meter id has an invalid shape".to_owned(),
        ));
    }
    if reading.sequence == 0 {
        return Err(StoreError::InvalidData(
            "traffic sequence must be positive".to_owned(),
        ));
    }
    if !valid_interface(&reading.interface) || !valid_boot_id(&reading.boot_id) {
        return Err(StoreError::InvalidData(
            "traffic interface or boot id has an invalid shape".to_owned(),
        ));
    }
    Ok(())
}

fn crosses_unobserved_utc_day(previous: i64, current: i64) -> bool {
    current.saturating_sub(previous) > 120
        && current.div_euclid(86_400) != previous.div_euclid(86_400)
}

struct PreviousReading {
    meter_id: String,
    sequence: u64,
    rx_bytes: u64,
    tx_bytes: u64,
    discontinuities: u64,
    interface: String,
    boot_id: String,
    last_reported_at_unix_secs: i64,
}

fn parse_u64_column(row: &sqlx::postgres::PgRow, name: &str) -> Result<u64> {
    parse_decimal_u64(&row.try_get::<String, _>(name)?, name)
}

/// Accept one absolute Agent meter reading. Duplicate or delayed sequences are harmless no-ops.
pub async fn record_node_traffic(
    pool: &PgPool,
    node_id: &str,
    observed_at_unix_secs: i64,
    reading: &NodeTrafficReading,
) -> Result<()> {
    validate_reading(reading)?;
    if observed_at_unix_secs <= 0 {
        return Err(StoreError::InvalidData(
            "traffic observed_at must be positive unix seconds".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT meter_id, sequence::text AS sequence, interface, boot_id,
                agent_rx_bytes::text AS agent_rx_bytes,
                agent_tx_bytes::text AS agent_tx_bytes,
                discontinuities::text AS discontinuities,
                extract(epoch FROM last_reported_at)::bigint AS last_reported_at_unix_secs
           FROM node_traffic_state
          WHERE node_id = $1
          FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *tx)
    .await?;
    let previous = row
        .as_ref()
        .map(|row| -> Result<PreviousReading> {
            Ok(PreviousReading {
                meter_id: row.try_get("meter_id")?,
                sequence: parse_u64_column(row, "sequence")?,
                rx_bytes: parse_u64_column(row, "agent_rx_bytes")?,
                tx_bytes: parse_u64_column(row, "agent_tx_bytes")?,
                discontinuities: parse_u64_column(row, "discontinuities")?,
                interface: row.try_get("interface")?,
                boot_id: row.try_get("boot_id")?,
                last_reported_at_unix_secs: row.try_get("last_reported_at_unix_secs")?,
            })
        })
        .transpose()?;

    if previous.as_ref().is_some_and(|old| {
        observed_at_unix_secs < old.last_reported_at_unix_secs
            || (old.meter_id == reading.meter_id && reading.sequence <= old.sequence)
    }) {
        tx.commit().await?;
        return Ok(());
    }

    let (rx_delta, tx_delta, mut gap_reason) = match &previous {
        None => (
            0,
            0,
            (reading.discontinuities > 0).then_some("agent-discontinuity"),
        ),
        Some(old) if old.meter_id != reading.meter_id => (0, 0, Some("meter-replaced")),
        Some(old) => {
            let rx = reading.rx_bytes.checked_sub(old.rx_bytes);
            let tx = reading.tx_bytes.checked_sub(old.tx_bytes);
            let regressed = rx.is_none() || tx.is_none();
            let discontinuity_changed = reading.discontinuities != old.discontinuities;
            (
                rx.unwrap_or(0),
                tx.unwrap_or(0),
                if regressed {
                    Some("counter-regressed")
                } else if old.boot_id != reading.boot_id {
                    Some("machine-reboot")
                } else if old.interface != reading.interface {
                    Some("interface-changed")
                } else if discontinuity_changed {
                    Some("agent-discontinuity")
                } else {
                    None
                },
            )
        }
    };
    // Absolute totals bridge an ordinary control-plane outage exactly. If delivery was absent
    // across a UTC midnight, however, the delta cannot be split around a reset that may live on
    // that date. Keep the bytes (discarding known traffic would be worse) and make the ambiguity
    // explicit so a calibration can establish the authoritative current-period total.
    if gap_reason.is_none()
        && previous.as_ref().is_some_and(|old| {
            crosses_unobserved_utc_day(old.last_reported_at_unix_secs, observed_at_unix_secs)
        })
    {
        gap_reason = Some("report-gap");
    }

    if previous.is_none() {
        sqlx::query(
            "INSERT INTO node_traffic_state (
                node_id, meter_id, sequence, interface, boot_id,
                agent_rx_bytes, agent_tx_bytes, accounted_rx_bytes, accounted_tx_bytes,
                discontinuities, tracking_started_at, last_reported_at
             ) VALUES (
                $1, $2, $3::numeric, $4, $5,
                $6::numeric, $7::numeric, 0, 0,
                $8::numeric, to_timestamp($9), to_timestamp($9)
             )",
        )
        .bind(node_id)
        .bind(&reading.meter_id)
        .bind(reading.sequence.to_string())
        .bind(&reading.interface)
        .bind(&reading.boot_id)
        .bind(reading.rx_bytes.to_string())
        .bind(reading.tx_bytes.to_string())
        .bind(reading.discontinuities.to_string())
        .bind(observed_at_unix_secs as f64)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE node_traffic_state
                SET meter_id = $2,
                    sequence = $3::numeric,
                    interface = $4,
                    boot_id = $5,
                    agent_rx_bytes = $6::numeric,
                    agent_tx_bytes = $7::numeric,
                    accounted_rx_bytes = accounted_rx_bytes + $8::numeric,
                    accounted_tx_bytes = accounted_tx_bytes + $9::numeric,
                    discontinuities = $10::numeric,
                    last_reported_at = to_timestamp($11)
              WHERE node_id = $1",
        )
        .bind(node_id)
        .bind(&reading.meter_id)
        .bind(reading.sequence.to_string())
        .bind(&reading.interface)
        .bind(&reading.boot_id)
        .bind(reading.rx_bytes.to_string())
        .bind(reading.tx_bytes.to_string())
        .bind(rx_delta.to_string())
        .bind(tx_delta.to_string())
        .bind(reading.discontinuities.to_string())
        .bind(observed_at_unix_secs as f64)
        .execute(&mut *tx)
        .await?;
    }

    if rx_delta > 0 || tx_delta > 0 {
        sqlx::query(
            "INSERT INTO node_traffic_daily (node_id, day, rx_bytes, tx_bytes)
             VALUES ($1, (to_timestamp($2) AT TIME ZONE 'UTC')::date, $3::numeric, $4::numeric)
             ON CONFLICT (node_id, day) DO UPDATE SET
                rx_bytes = node_traffic_daily.rx_bytes + EXCLUDED.rx_bytes,
                tx_bytes = node_traffic_daily.tx_bytes + EXCLUDED.tx_bytes",
        )
        .bind(node_id)
        .bind(observed_at_unix_secs as f64)
        .bind(rx_delta.to_string())
        .bind(tx_delta.to_string())
        .execute(&mut *tx)
        .await?;
    }
    if let Some(reason) = gap_reason {
        insert_gap(&mut tx, node_id, observed_at_unix_secs, reason).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn insert_gap(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    observed_at_unix_secs: i64,
    reason: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO node_traffic_gaps (node_id, occurred_at, reason)
         VALUES ($1, to_timestamp($2), $3)",
    )
    .bind(node_id)
    .bind(observed_at_unix_secs as f64)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn update_node_traffic(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: UpdateNodeTrafficRequest,
) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update node traffic accounting".to_owned(),
        ));
    }
    valid_policy(&request)?;
    let calibration = request
        .calibrated_total_bytes
        .as_deref()
        .map(|value| parse_decimal_u64(value, "当前流量校准值"))
        .transpose()?;
    let mut tx = pool.begin().await?;
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM nodes WHERE id = $1)")
        .bind(node_id)
        .fetch_one(&mut *tx)
        .await?;
    if !exists {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }
    sqlx::query(
        "INSERT INTO node_traffic_policy (
            node_id, cycle_kind, reset_month, reset_day, updated_at, updated_by
         ) VALUES ($1, $2, $3, $4, now(), $5)
         ON CONFLICT (node_id) DO UPDATE SET
            cycle_kind = EXCLUDED.cycle_kind,
            reset_month = EXCLUDED.reset_month,
            reset_day = EXCLUDED.reset_day,
            updated_at = EXCLUDED.updated_at,
            updated_by = EXCLUDED.updated_by",
    )
    .bind(node_id)
    .bind(request.cycle_kind.as_str())
    .bind(request.reset_month.map(i32::from))
    .bind(i32::from(request.reset_day))
    .bind(actor.operator_id())
    .execute(&mut *tx)
    .await?;

    if let Some(target) = calibration {
        sqlx::query(
            "INSERT INTO node_traffic_calibrations (
                node_id, target_total_bytes, accounted_total_bytes, calibrated_by
             )
             SELECT $1, $2::numeric,
                    COALESCE(accounted_rx_bytes + accounted_tx_bytes, 0), $3
               FROM (SELECT 1) AS singleton
               LEFT JOIN node_traffic_state ON node_id = $1",
        )
        .bind(node_id)
        .bind(target.to_string())
        .bind(actor.operator_id())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Load current-cycle totals. Days that do not exist in a month clamp to that month's last day;
/// e.g. a reset on day 31 happens on February 28 (or 29) and returns to day 31 in March.
pub async fn load_node_traffic(pool: &PgPool, actor: &AdminContext) -> Result<NodeTrafficView> {
    let rows = sqlx::query(NODE_TRAFFIC_VIEW_SQL)
        .bind(actor.tenant_scope())
        .bind(actor.tenant_scope_like_pattern())
        .fetch_all(pool)
        .await?;
    let nodes = rows
        .into_iter()
        .map(|row| {
            let cycle = match row.try_get::<String, _>("cycle_kind")?.as_str() {
                "monthly" => NodeTrafficCycleKind::Monthly,
                "yearly" => NodeTrafficCycleKind::Yearly,
                other => {
                    return Err(StoreError::InvalidData(format!(
                        "invalid node traffic cycle {other}"
                    )))
                }
            };
            Ok(NodeTrafficItem {
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                name: row.try_get("name")?,
                cycle_kind: cycle,
                reset_month: row
                    .try_get::<Option<i32>, _>("reset_month")?
                    .map(|value| {
                        u8::try_from(value).map_err(|_| {
                            StoreError::InvalidData("traffic reset month is invalid".to_owned())
                        })
                    })
                    .transpose()?,
                reset_day: u8::try_from(row.try_get::<i32, _>("reset_day")?).map_err(|_| {
                    StoreError::InvalidData("traffic reset day is invalid".to_owned())
                })?,
                period_start_unix_secs: row.try_get("period_start_unix_secs")?,
                period_end_unix_secs: row.try_get("period_end_unix_secs")?,
                rx_bytes: row.try_get("rx_bytes")?,
                tx_bytes: row.try_get("tx_bytes")?,
                total_bytes: row.try_get("total_bytes")?,
                interface: row.try_get("interface")?,
                tracking_started_at_unix_secs: row.try_get("tracking_started_at_unix_secs")?,
                last_reported_at_unix_secs: row.try_get("last_reported_at_unix_secs")?,
                calibrated_at_unix_secs: row.try_get("calibrated_at_unix_secs")?,
                last_gap_at_unix_secs: row.try_get("last_gap_at_unix_secs")?,
                last_gap_reason: row.try_get("last_gap_reason")?,
                has_gap: row.try_get("has_gap")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(NodeTrafficView { nodes })
}

const NODE_TRAFFIC_VIEW_SQL: &str = r#"
WITH policies AS (
    SELECT n.id AS node_id, n.tenant_id, n.name,
           COALESCE(p.cycle_kind, 'monthly') AS cycle_kind,
           p.reset_month,
           COALESCE(p.reset_day, 1) AS reset_day,
           (now() AT TIME ZONE 'UTC')::date AS today
      FROM nodes AS n
      LEFT JOIN node_traffic_policy AS p ON p.node_id = n.id
     WHERE ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\')
), anchors AS (
    SELECT policies.*,
           CASE WHEN cycle_kind = 'monthly' THEN
                make_date(
                    extract(year FROM today)::integer,
                    extract(month FROM today)::integer,
                    LEAST(reset_day, extract(day FROM date_trunc('month', today::timestamp)
                          + interval '1 month - 1 day')::integer)
                )
           ELSE
                make_date(
                    extract(year FROM today)::integer,
                    reset_month,
                    LEAST(reset_day, extract(day FROM date_trunc('month',
                          make_date(extract(year FROM today)::integer, reset_month, 1)::timestamp)
                          + interval '1 month - 1 day')::integer)
                )
           END AS this_anchor
      FROM policies
), bases AS (
    SELECT anchors.*,
           CASE WHEN this_anchor <= today THEN
                CASE WHEN cycle_kind = 'monthly'
                     THEN date_trunc('month', today::timestamp)::date
                     ELSE make_date(extract(year FROM today)::integer, reset_month, 1)
                END
           ELSE
                CASE WHEN cycle_kind = 'monthly'
                     THEN (date_trunc('month', today::timestamp) - interval '1 month')::date
                     ELSE make_date(extract(year FROM today)::integer - 1, reset_month, 1)
                END
           END AS period_base
      FROM anchors
), periods AS (
    SELECT bases.*,
           make_date(
               extract(year FROM period_base)::integer,
               extract(month FROM period_base)::integer,
               LEAST(reset_day, extract(day FROM date_trunc('month', period_base::timestamp)
                     + interval '1 month - 1 day')::integer)
           ) AS period_start,
           CASE WHEN cycle_kind = 'monthly'
                THEN (period_base + interval '1 month')::date
                ELSE (period_base + interval '1 year')::date
           END AS next_base
      FROM bases
), bounds AS (
    SELECT periods.*,
           make_date(
               extract(year FROM next_base)::integer,
               extract(month FROM next_base)::integer,
               LEAST(reset_day, extract(day FROM date_trunc('month', next_base::timestamp)
                     + interval '1 month - 1 day')::integer)
           ) AS period_end
      FROM periods
)
SELECT b.node_id, b.tenant_id, b.name, b.cycle_kind, b.reset_month, b.reset_day,
       extract(epoch FROM b.period_start::timestamp AT TIME ZONE 'UTC')::bigint
           AS period_start_unix_secs,
       extract(epoch FROM b.period_end::timestamp AT TIME ZONE 'UTC')::bigint
           AS period_end_unix_secs,
       daily.rx_bytes::text AS rx_bytes,
       daily.tx_bytes::text AS tx_bytes,
       (CASE WHEN calibration.target_total_bytes IS NOT NULL
             THEN calibration.target_total_bytes
                  + GREATEST(COALESCE(state.accounted_rx_bytes + state.accounted_tx_bytes, 0)
                             - calibration.accounted_total_bytes, 0)
             ELSE daily.rx_bytes + daily.tx_bytes
        END)::text AS total_bytes,
       state.interface,
       extract(epoch FROM state.tracking_started_at)::bigint AS tracking_started_at_unix_secs,
       extract(epoch FROM state.last_reported_at)::bigint AS last_reported_at_unix_secs,
       extract(epoch FROM calibration.calibrated_at)::bigint AS calibrated_at_unix_secs,
       extract(epoch FROM latest_gap.occurred_at)::bigint AS last_gap_at_unix_secs,
       latest_gap.reason AS last_gap_reason,
       CASE WHEN state.node_id IS NULL THEN FALSE
            ELSE state.tracking_started_at > GREATEST(
                     b.period_start::timestamp AT TIME ZONE 'UTC',
                     COALESCE(calibration.calibrated_at,
                              b.period_start::timestamp AT TIME ZONE 'UTC'))
                 OR latest_gap.reason IS NOT NULL
       END AS has_gap
  FROM bounds AS b
  LEFT JOIN node_traffic_state AS state ON state.node_id = b.node_id
  LEFT JOIN LATERAL (
      SELECT COALESCE(sum(day.rx_bytes), 0) AS rx_bytes,
             COALESCE(sum(day.tx_bytes), 0) AS tx_bytes
        FROM node_traffic_daily AS day
       WHERE day.node_id = b.node_id
         AND day.day >= b.period_start
         AND day.day < b.period_end
  ) AS daily ON TRUE
  LEFT JOIN LATERAL (
      SELECT item.calibrated_at, item.target_total_bytes, item.accounted_total_bytes
        FROM node_traffic_calibrations AS item
       WHERE item.node_id = b.node_id
         AND item.calibrated_at >= b.period_start::timestamp AT TIME ZONE 'UTC'
         AND item.calibrated_at < b.period_end::timestamp AT TIME ZONE 'UTC'
       ORDER BY item.calibrated_at DESC, item.id DESC
       LIMIT 1
  ) AS calibration ON TRUE
  LEFT JOIN LATERAL (
      SELECT gap.occurred_at, gap.reason
        FROM node_traffic_gaps AS gap
       WHERE gap.node_id = b.node_id
         AND gap.occurred_at >= GREATEST(
             b.period_start::timestamp AT TIME ZONE 'UTC',
             COALESCE(calibration.calibrated_at,
                      b.period_start::timestamp AT TIME ZONE 'UTC'))
       ORDER BY gap.occurred_at DESC, gap.id DESC
       LIMIT 1
  ) AS latest_gap ON TRUE
 ORDER BY b.name, b.node_id
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_shape_requires_only_yearly_month() {
        assert!(valid_policy(&UpdateNodeTrafficRequest {
            cycle_kind: NodeTrafficCycleKind::Monthly,
            reset_month: None,
            reset_day: 31,
            calibrated_total_bytes: None,
        })
        .is_ok());
        assert!(valid_policy(&UpdateNodeTrafficRequest {
            cycle_kind: NodeTrafficCycleKind::Yearly,
            reset_month: Some(2),
            reset_day: 29,
            calibrated_total_bytes: Some("1099511627776".to_owned()),
        })
        .is_ok());
        assert!(valid_policy(&UpdateNodeTrafficRequest {
            cycle_kind: NodeTrafficCycleKind::Monthly,
            reset_month: Some(1),
            reset_day: 1,
            calibrated_total_bytes: None,
        })
        .is_err());
    }

    #[test]
    fn calibration_is_an_exact_unsigned_decimal() {
        assert_eq!(
            parse_decimal_u64("18446744073709551615", "value").unwrap(),
            u64::MAX
        );
        for bad in ["", "-1", "1.5", "1 0", "18446744073709551616"] {
            assert!(parse_decimal_u64(bad, "value").is_err(), "{bad}");
        }
    }

    #[test]
    fn only_a_long_delivery_gap_across_utc_midnight_needs_period_calibration() {
        let midnight = 20_000 * 86_400;
        assert!(!crosses_unobserved_utc_day(midnight - 30, midnight + 30));
        assert!(!crosses_unobserved_utc_day(midnight + 1, midnight + 10_000));
        assert!(crosses_unobserved_utc_day(midnight - 300, midnight + 300));
    }
}
