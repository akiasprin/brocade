//! Per-node physical interface accounting and reset policy.
//!
//! Agent readings are absolute logical counters. This layer accepts only monotonic readings from
//! the same meter epoch, converts them to idempotent deltas, and keeps those deltas by UTC day.
//! Reset dates and calibrations are presentation/accounting anchors; neither asks an Agent to zero
//! its durable meter, so policy edits and control-plane outages cannot break continuity.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::{SystemTime, UNIX_EPOCH},
};

use brocade_deployment::protocol::NodeTrafficReading;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{AdminContext, Result, StoreError};

pub use brocade_deployment::traffic_limit::{NodeTrafficAccountingMode, NodeTrafficCycleKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficItem {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    pub cycle_kind: NodeTrafficCycleKind,
    pub accounting_mode: NodeTrafficAccountingMode,
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    /// Provider allowance for this reset cycle. NULL means the allowance has not been configured.
    pub quota_bytes: Option<String>,
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
    pub limit: crate::traffic_limit::NodeTrafficLimitView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficView {
    pub nodes: Vec<NodeTrafficItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNodeTrafficRequest {
    pub cycle_kind: NodeTrafficCycleKind,
    /// Optional for a rolling Console upgrade: an already-open older UI keeps the current mode.
    #[serde(default)]
    pub accounting_mode: Option<NodeTrafficAccountingMode>,
    /// Required for yearly and forbidden for monthly.
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    /// Exact positive byte count, or NULL to remove the provider allowance.
    #[serde(default)]
    pub quota_bytes: Option<String>,
    /// When present, append a total-only calibration at the server's current UTC time.
    #[serde(default)]
    pub calibrated_total_bytes: Option<String>,
}

/// Where the error quantile that widens P50 into P90 came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineTrafficBandBasis {
    /// This machine's own backtests over the remaining horizon.
    Node,
    /// Backtests pooled over every machine in scope.
    Fleet,
    /// No machine in scope has enough history yet; a fixed upper error stands in.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficBand {
    pub basis: MachineTrafficBandBasis,
    /// Days ahead that the backtests forecast. A fleet basis may use a shorter horizon than the
    /// remaining days when history is short, but never less than half of them.
    pub horizon_days: u32,
    /// Backtests behind the quantile; zero for the default.
    pub samples: u32,
    /// 90th percentile of `actual / forecast - 1` for the remaining traffic, floored at zero.
    pub upper_error_basis_points: u32,
}

/// The daily forecasts recorded during the latest closed period, compared with its final total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficAccuracy {
    pub period_start_unix_secs: i64,
    pub period_end_unix_secs: i64,
    pub actual_bytes: String,
    pub forecasts: u32,
    /// Mean of `|P50 - actual| / actual` over those forecasts.
    pub p50_error_basis_points: u32,
    /// Forecasts whose P90 the final total did not exceed.
    pub p90_covered: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineTrafficConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficUser {
    pub tenant_id: String,
    pub user_id: String,
    /// Estimated machine footprint for this user's logical payload over the current topology.
    /// A forwarded byte crosses the machine NIC twice, so the basis is
    /// `(uplink + downlink) * 2`, bounded by the observed physical total. This is not a user
    /// billing counter.
    pub attributed_bytes: String,
    pub projected_bytes: String,
    pub share_basis_points: u16,
    pub apps: Vec<String>,
}

/// Physical NIC bytes the ledger accounted to one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficDay {
    pub day_start_unix_secs: i64,
    pub bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficModelNode {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    pub cycle_kind: NodeTrafficCycleKind,
    pub accounting_mode: NodeTrafficAccountingMode,
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    pub period_start_unix_secs: i64,
    pub period_end_unix_secs: i64,
    /// Start of the equal-length window that ends at `period_start_unix_secs`. It supplies
    /// `previous_period_bytes` and `previous_days`.
    pub previous_period_start_unix_secs: i64,
    pub quota_bytes: Option<String>,
    pub used_bytes: String,
    pub previous_period_bytes: String,
    /// The rest of the period is expected at this many bytes per day: complete days of the last
    /// 28, weighted so that a day counts half as much as one four days newer. Before the first
    /// complete day it is the average since tracking began.
    pub daily_rate_bytes: String,
    /// Complete days behind `daily_rate_bytes`; zero means the average since tracking began.
    pub rate_days: u32,
    /// `used_bytes + daily_rate_bytes * remaining days`.
    pub projected_p50_bytes: String,
    /// P50 with its remaining part raised by `band.upper_error_basis_points`.
    pub projected_p90_bytes: String,
    pub band: MachineTrafficBand,
    pub confidence: MachineTrafficConfidence,
    /// `used_bytes` is missing traffic only a calibration can restore: tracking began after the
    /// later of the period start and the latest calibration, or the gap below may have lost bytes
    /// or carried the previous period's bytes into this one.
    pub needs_calibration: bool,
    /// Ledger days inside the current period, oldest first; a day without accounted bytes is
    /// absent. Without a calibration they sum to `used_bytes`; a calibration replaces the total
    /// from `calibrated_at_unix_secs` on, so `used_bytes` stays authoritative.
    pub days: Vec<MachineTrafficDay>,
    /// Ledger days inside the previous window, oldest first.
    pub previous_days: Vec<MachineTrafficDay>,
    pub tracking_started_at_unix_secs: Option<i64>,
    /// Latest calibration inside the current period.
    pub calibrated_at_unix_secs: Option<i64>,
    /// Latest gap since the later of the period start and that calibration that may have changed
    /// the period total. A gap whose previous UTC day still has reports is left out: the Agent
    /// persists its meter every ten seconds, so it loses at most the samples after that report.
    pub gap_at_unix_secs: Option<i64>,
    pub gap_reason: Option<String>,
    /// Absent until a period with recorded daily forecasts has closed.
    pub last_period_accuracy: Option<MachineTrafficAccuracy>,
    pub users: Vec<MachineTrafficUser>,
    pub unattributed_bytes: String,
    pub projected_unattributed_bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficModelView {
    pub generated_at_unix_secs: i64,
    /// Physical accounting is authoritative. User attribution is deliberately identified as an
    /// estimate because relay counters do not contain a user identity.
    pub attribution: String,
    pub nodes: Vec<MachineTrafficModelNode>,
}

fn valid_policy(request: &UpdateNodeTrafficRequest) -> Result<()> {
    if !(1..=31).contains(&request.reset_day) {
        return Err(StoreError::InvalidData(
            "流量重置日期需在 1–31 日之间".to_owned(),
        ));
    }
    if let Some(quota) = request.quota_bytes.as_deref() {
        if parse_decimal_u64(quota, "机器流量配额")? == 0 {
            return Err(StoreError::InvalidData("机器流量配额必须大于零".to_owned()));
        }
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
    brocade_deployment::traffic_limit::validate_day_boundaries(
        &reading.day_boundaries,
        reading.sampled_at_unix_secs,
        reading.rx_bytes,
        reading.tx_bytes,
    )
    .map_err(StoreError::InvalidData)?;
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

/// Why a reading could not simply extend the ledger; stored in `node_traffic_gaps.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapReason {
    MeterReplaced,
    MachineReboot,
    InterfaceChanged,
    CounterRegressed,
    AgentDiscontinuity,
    ReportGap,
}

impl GapReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::MeterReplaced => "meter-replaced",
            Self::MachineReboot => "machine-reboot",
            Self::InterfaceChanged => "interface-changed",
            Self::CounterRegressed => "counter-regressed",
            Self::AgentDiscontinuity => "agent-discontinuity",
            Self::ReportGap => "report-gap",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "meter-replaced" => Self::MeterReplaced,
            "machine-reboot" => Self::MachineReboot,
            "interface-changed" => Self::InterfaceChanged,
            "counter-regressed" => Self::CounterRegressed,
            "agent-discontinuity" => Self::AgentDiscontinuity,
            "report-gap" => Self::ReportGap,
            other => {
                return Err(StoreError::InvalidData(format!(
                    "invalid node traffic gap reason {other}"
                )))
            }
        })
    }
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
    let sampled_at = reading
        .sampled_at_unix_secs
        .unwrap_or(observed_at_unix_secs);
    if sampled_at <= 0 || sampled_at > observed_at_unix_secs.saturating_add(300) {
        return Err(StoreError::InvalidData(
            "invalid traffic sample timestamp".into(),
        ));
    }
    let observed_at_unix_secs = sampled_at;
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
            (reading.discontinuities > 0).then_some(GapReason::AgentDiscontinuity),
        ),
        Some(old) if old.meter_id != reading.meter_id => (0, 0, Some(GapReason::MeterReplaced)),
        Some(old) => {
            let rx = reading.rx_bytes.checked_sub(old.rx_bytes);
            let tx = reading.tx_bytes.checked_sub(old.tx_bytes);
            let regressed = rx.is_none() || tx.is_none();
            let discontinuity_changed = reading.discontinuities != old.discontinuities;
            (
                rx.unwrap_or(0),
                tx.unwrap_or(0),
                if regressed {
                    Some(GapReason::CounterRegressed)
                } else if old.boot_id != reading.boot_id {
                    Some(GapReason::MachineReboot)
                } else if old.interface != reading.interface {
                    Some(GapReason::InterfaceChanged)
                } else if discontinuity_changed {
                    Some(GapReason::AgentDiscontinuity)
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
    let (daily_deltas, boundaries_complete) = split_daily_deltas(
        previous.as_ref(),
        reading,
        observed_at_unix_secs,
        rx_delta,
        tx_delta,
    );
    if gap_reason.is_none()
        && !boundaries_complete
        && previous.as_ref().is_some_and(|old| {
            crosses_unobserved_utc_day(old.last_reported_at_unix_secs, observed_at_unix_secs)
        })
    {
        gap_reason = Some(GapReason::ReportGap);
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

    for (day, rx_delta, tx_delta) in daily_deltas {
        if rx_delta == 0 && tx_delta == 0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO node_traffic_daily (node_id, day, rx_bytes, tx_bytes)
             VALUES ($1, (to_timestamp($2) AT TIME ZONE 'UTC')::date, $3::numeric, $4::numeric)
             ON CONFLICT (node_id, day) DO UPDATE SET
                rx_bytes = node_traffic_daily.rx_bytes + EXCLUDED.rx_bytes,
                tx_bytes = node_traffic_daily.tx_bytes + EXCLUDED.tx_bytes",
        )
        .bind(node_id)
        .bind(day as f64)
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

/// Split only newly accepted bytes at durable Agent boundaries; retransmission stays a no-op.
fn split_daily_deltas(
    previous: Option<&PreviousReading>,
    reading: &NodeTrafficReading,
    at: i64,
    rx_delta: u64,
    tx_delta: u64,
) -> (Vec<(i64, u64, u64)>, bool) {
    let day = at.div_euclid(86_400) * 86_400;
    let Some(old) = previous.filter(|old| old.meter_id == reading.meter_id) else {
        return (vec![(day, rx_delta, tx_delta)], false);
    };
    let old_day = old.last_reported_at_unix_secs.div_euclid(86_400) * 86_400;
    let boundaries: Vec<_> = reading
        .day_boundaries
        .iter()
        .filter(|b| {
            b.day_start_unix_secs > old_day
                && b.day_start_unix_secs <= day
                && b.rx_bytes >= old.rx_bytes
                && b.tx_bytes >= old.tx_bytes
        })
        .collect();
    let complete = i64::try_from(boundaries.len()).ok() == Some((day - old_day) / 86_400);
    if !complete {
        return (vec![(day, rx_delta, tx_delta)], false);
    }
    let (mut rx, mut tx, mut cursor) = (old.rx_bytes, old.tx_bytes, old_day);
    let mut result = Vec::new();
    for boundary in boundaries {
        result.push((cursor, boundary.rx_bytes - rx, boundary.tx_bytes - tx));
        (rx, tx, cursor) = (
            boundary.rx_bytes,
            boundary.tx_bytes,
            boundary.day_start_unix_secs,
        );
    }
    result.push((
        day,
        reading.rx_bytes.saturating_sub(rx),
        reading.tx_bytes.saturating_sub(tx),
    ));
    (result, true)
}

async fn insert_gap(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    observed_at_unix_secs: i64,
    reason: GapReason,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO node_traffic_gaps (node_id, occurred_at, reason)
         VALUES ($1, to_timestamp($2), $3)",
    )
    .bind(node_id)
    .bind(observed_at_unix_secs as f64)
    .bind(reason.as_str())
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
    let quota = request
        .quota_bytes
        .as_deref()
        .map(|value| parse_decimal_u64(value, "机器流量配额"))
        .transpose()?;
    let mut tx = pool.begin().await?;
    if sqlx::query_scalar::<_, String>("SELECT id FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
    {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }
    let limit_policy = crate::traffic_limit::policy_in_transaction(&mut tx, node_id).await?;
    limit_policy
        .validate(quota)
        .map_err(StoreError::InvalidData)?;
    let accounting_mode = match request.accounting_mode {
        Some(mode) => mode,
        None => sqlx::query_scalar::<_, String>(
            "SELECT accounting_mode FROM node_traffic_policy WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?
        .as_deref()
        .map(|mode| NodeTrafficAccountingMode::parse(mode).map_err(StoreError::InvalidData))
        .transpose()?
        .unwrap_or(NodeTrafficAccountingMode::BidirectionalSum),
    };
    sqlx::query(
        "INSERT INTO node_traffic_policy (
            node_id, cycle_kind, accounting_mode, reset_month, reset_day, quota_bytes,
            updated_at, updated_by
         ) VALUES ($1, $2, $3, $4, $5, $6::numeric, now(), $7)
         ON CONFLICT (node_id) DO UPDATE SET
            cycle_kind = EXCLUDED.cycle_kind,
            accounting_mode = EXCLUDED.accounting_mode,
            reset_month = EXCLUDED.reset_month,
            reset_day = EXCLUDED.reset_day,
            quota_bytes = EXCLUDED.quota_bytes,
            updated_at = EXCLUDED.updated_at,
            updated_by = EXCLUDED.updated_by",
    )
    .bind(node_id)
    .bind(request.cycle_kind.as_str())
    .bind(accounting_mode.as_str())
    .bind(request.reset_month.map(i32::from))
    .bind(i32::from(request.reset_day))
    .bind(quota.map(|value| value.to_string()))
    .bind(actor.operator_id())
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE node_traffic_limit SET revision = revision + 1 WHERE node_id = $1")
        .bind(node_id)
        .execute(&mut *tx)
        .await?;
    if let Some(target) = calibration {
        sqlx::query(
            "INSERT INTO node_traffic_calibrations (
                node_id, target_total_bytes, accounted_total_bytes, accounting_mode,
                accounted_rx_bytes, accounted_tx_bytes, calibrated_by
             )
             SELECT $1, $2::numeric,
                    COALESCE(accounted_rx_bytes + accounted_tx_bytes, 0), $3,
                    COALESCE(accounted_rx_bytes, 0), COALESCE(accounted_tx_bytes, 0), $4
               FROM (SELECT 1) AS singleton
               LEFT JOIN node_traffic_state ON node_id = $1",
        )
        .bind(node_id)
        .bind(target.to_string())
        .bind(accounting_mode.as_str())
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
        .bind(None::<&str>)
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
            let accounting_mode =
                NodeTrafficAccountingMode::parse(&row.try_get::<String, _>("accounting_mode")?)
                    .map_err(StoreError::InvalidData)?;
            Ok(NodeTrafficItem {
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                name: row.try_get("name")?,
                cycle_kind: cycle,
                accounting_mode,
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
                quota_bytes: row.try_get("quota_bytes")?,
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
                limit: crate::traffic_limit::view_from_row(&row)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(NodeTrafficView { nodes })
}

fn mul_div(value: u64, numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        return 0;
    }
    let scaled = u128::from(value)
        .saturating_mul(u128::from(numerator))
        .saturating_add(u128::from(denominator) / 2)
        / u128::from(denominator);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectionalTrafficDay {
    day: i64,
    rx_bytes: u64,
    tx_bytes: u64,
}

fn direction_ledger(days: &[DirectionalTrafficDay], receive: bool) -> Vec<(i64, u64)> {
    days.iter()
        .map(|day| (day.day, if receive { day.rx_bytes } else { day.tx_bytes }))
        .collect()
}

/// Scalar series used only to learn an error band. Current-period totals and projections apply
/// the mode to cumulative directional values below; in particular, daily maxima are never summed
/// into the provider total.
fn accounting_rate_ledger(
    days: &[DirectionalTrafficDay],
    mode: NodeTrafficAccountingMode,
) -> Vec<(i64, u64)> {
    days.iter()
        .map(|day| (day.day, mode.apply(day.rx_bytes, day.tx_bytes)))
        .collect()
}

fn directional_window_totals(days: &[DirectionalTrafficDay], from: i64, to: i64) -> (u64, u64) {
    days.iter()
        .filter(|day| day.day >= from && day.day < to)
        .fold((0_u64, 0_u64), |(rx, tx), day| {
            (
                rx.saturating_add(day.rx_bytes),
                tx.saturating_add(day.tx_bytes),
            )
        })
}

/// Ledger days in `[from, to)`, keeping the ascending query order. Each point is the increase of
/// the selected cumulative accounting counter on that day, so the series sums to
/// `mode.apply(sum(rx), sum(tx))` even for bidirectional max.
fn ledger_window(
    days: &[DirectionalTrafficDay],
    mode: NodeTrafficAccountingMode,
    from: i64,
    to: i64,
) -> Vec<MachineTrafficDay> {
    let mut rx = 0_u64;
    let mut tx = 0_u64;
    let mut accounted = 0_u64;
    days.iter()
        .filter(|day| day.day >= from && day.day < to)
        .map(|day| {
            rx = rx.saturating_add(day.rx_bytes);
            tx = tx.saturating_add(day.tx_bytes);
            let next = mode.apply(rx, tx);
            let bytes = next.saturating_sub(accounted);
            accounted = next;
            MachineTrafficDay {
                day_start_unix_secs: day.day,
                bytes: bytes.to_string(),
            }
        })
        .collect()
}

fn attributable_physical_bytes(physical: u64, logical: u64, has_user_demand: bool) -> u64 {
    if !has_user_demand {
        return 0;
    }
    // Proxy payload enters and leaves the same machine. The NIC ledger therefore sees each
    // logical byte twice: once on the client-facing leg and once on the next-hop-facing leg.
    physical.min(logical.saturating_mul(2))
}

const DAY: i64 = 86_400;
/// A complete day's weight in the daily rate halves every this many days. Backtests on synthetic
/// fleets with level shifts, weekly cycles and daily noise had their lowest error between three
/// and five days; shorter chases noise, longer lags a new heavy user.
const RATE_HALF_LIFE_DAYS: f64 = 4.0;
/// Older days no longer enter the daily rate; their weight would be below one percent.
const RATE_WINDOW_DAYS: usize = 28;
/// Fewer observed days than this leave the rate to chance: confidence is low.
const RATE_DAYS_FEW: usize = 3;
/// Observed days a rate needs before a backtest uses it or confidence can be high.
const RATE_DAYS_ENOUGH: usize = 7;
/// An error quantile needs this many backtests...
const BAND_MIN_SAMPLES: usize = 20;
/// ...and as many as this many non-overlapping horizons would give. Origins one day apart share
/// most of a long horizon, so a count alone overstates what a short history knows.
const BAND_MIN_WINDOWS: usize = 8;
/// Fleet history may stand in for a horizon at most this many times its own length.
const BAND_MAX_STRETCH: usize = 2;
/// Upper error before any machine in scope has enough history.
const BAND_DEFAULT_UPPER_ERROR: f64 = 0.5;
const FORECAST_ACTOR: &str = "system:traffic-forecast";
/// Daily forecasts stay long enough to review a yearly period after it closes.
pub const MACHINE_TRAFFIC_FORECAST_RETENTION_DAYS: u32 = 800;

#[derive(Debug, Clone, Copy)]
struct TrafficGap {
    occurred_at: i64,
    reason: GapReason,
}

/// One complete UTC day as the daily rate sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RateDay {
    Observed(f64),
    /// Bytes are missing, or cannot be told apart from a neighbouring day's.
    Unobserved,
}

/// The period, tracking and calibration instants a machine's forecast is judged against.
#[derive(Debug, Clone, Copy)]
struct PeriodAnchors {
    start: i64,
    end: i64,
    tracking_started_at: Option<i64>,
    calibrated_at: Option<i64>,
}

fn day_start(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(DAY) * DAY
}

/// Ledger byte counts are far below 2^53, where `f64` stops being exact.
fn bytes_f64(bytes: u64) -> f64 {
    bytes as f64
}

/// Bytes from a non-negative estimate. `as` saturates at `u64::MAX`; a negative or NaN estimate
/// is no traffic.
fn estimated_bytes(value: f64) -> u64 {
    if value > 0.0 {
        value.round() as u64
    } else {
        0
    }
}

fn basis_points(ratio: f64) -> u32 {
    u32::try_from(estimated_bytes(ratio * 10_000.0)).unwrap_or(u32::MAX)
}

fn ledger_bytes(ledger: &[(i64, u64)], day: i64) -> u64 {
    ledger
        .binary_search_by_key(&day, |(ledger_day, _)| *ledger_day)
        .map_or(0, |index| ledger[index].1)
}

/// The last UTC day before `day` with accounted bytes, or the day before tracking began.
///
/// Every reporting day carries at least the Agent's own control traffic, so the days in between
/// are days on which the Console heard nothing from this machine.
fn last_reported_day(ledger: &[(i64, u64)], day: i64, tracking_started_at: i64) -> i64 {
    let floor = day_start(tracking_started_at) - DAY;
    ledger
        .partition_point(|(ledger_day, _)| *ledger_day < day)
        .checked_sub(1)
        .map_or(floor, |index| ledger[index].0.max(floor))
}

/// Whether a gap may have changed the total of the period starting at `period_start`.
fn gap_changes_total(
    gap: TrafficGap,
    ledger: &[(i64, u64)],
    tracking_started_at: i64,
    period_start: i64,
) -> bool {
    let day = day_start(gap.occurred_at);
    let reported = last_reported_day(ledger, day, tracking_started_at);
    match gap.reason {
        // Absolute counters keep every byte across an outage. Only an outage that may span the
        // reset moves the previous period's bytes into this one.
        GapReason::ReportGap => reported < period_start && day >= period_start,
        // The bytes after the last durable sample are lost. While the Agent still reported the
        // day before, that is at most its final samples; after a silent day it may be days.
        _ => reported < day - DAY,
    }
}

fn day_index(first: i64, today: i64, day: i64) -> Option<usize> {
    if day < first || day >= today {
        return None;
    }
    usize::try_from((day - first) / DAY).ok()
}

/// Complete UTC days from the first fully tracked day through yesterday.
///
/// An outage that crossed midnight books its deferred bytes on the day reporting resumed; they
/// are spread evenly over the outage instead. Days around a gap that may have lost bytes are left
/// out, so neither a lump nor a hole reads as a change in traffic.
fn daily_series(
    ledger: &[(i64, u64)],
    gaps: &[TrafficGap],
    tracking_started_at: i64,
    today: i64,
) -> Vec<RateDay> {
    let first = if tracking_started_at.rem_euclid(DAY) == 0 {
        tracking_started_at
    } else {
        day_start(tracking_started_at) + DAY
    };
    let count = usize::try_from((today - first).div_euclid(DAY)).unwrap_or(0);
    let mut days = vec![RateDay::Observed(0.0); count];
    for &(day, bytes) in ledger {
        if let Some(index) = day_index(first, today, day) {
            days[index] = RateDay::Observed(bytes_f64(bytes));
        }
    }
    let mut set = |day: i64, value: RateDay| {
        if let Some(index) = day_index(first, today, day) {
            if days[index] != RateDay::Unobserved {
                days[index] = value;
            }
        }
    };
    for gap in gaps {
        let day = day_start(gap.occurred_at);
        let reported = last_reported_day(ledger, day, tracking_started_at);
        let silent = (day - reported) / DAY - 1;
        if gap.reason == GapReason::ReportGap {
            if day < today {
                let share = bytes_f64(ledger_bytes(ledger, day)) / (silent + 1) as f64;
                for offset in 1..=silent + 1 {
                    set(reported + offset * DAY, RateDay::Observed(share));
                }
            } else {
                // Reporting resumed today; the outage's bytes sit in today's partial day.
                for offset in 1..=silent {
                    set(reported + offset * DAY, RateDay::Unobserved);
                }
            }
        } else if silent > 0 {
            for offset in 0..=silent + 1 {
                set(reported + offset * DAY, RateDay::Unobserved);
            }
        }
    }
    days
}

/// Weights of the daily rate by age in days, newest first.
fn rate_weights() -> [f64; RATE_WINDOW_DAYS] {
    std::array::from_fn(|age| 0.5_f64.powf(age as f64 / RATE_HALF_LIFE_DAYS))
}

/// Weighted daily bytes over the observed days of the window that ends before `end`, and how many
/// days were observed.
fn recent_rate(
    days: &[RateDay],
    end: usize,
    weights: &[f64; RATE_WINDOW_DAYS],
) -> Option<(f64, usize)> {
    let start = end.saturating_sub(RATE_WINDOW_DAYS);
    let (mut weighted, mut total, mut observed) = (0.0, 0.0, 0);
    for (day, weight) in days[start..end].iter().rev().zip(weights) {
        if let RateDay::Observed(bytes) = day {
            weighted += weight * bytes;
            total += weight;
            observed += 1;
        }
    }
    (observed > 0).then(|| (weighted / total, observed))
}

/// Average bytes per day since tracking began, for a machine without an observed complete day in
/// the window. The first hour is too volatile to scale up, so it counts as at least an hour.
fn rate_since(ledger: &[(i64, u64)], tracking_started_at: i64, now: i64) -> f64 {
    let from = day_start(tracking_started_at);
    let bytes = ledger
        .iter()
        .filter(|(day, _)| *day >= from)
        .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes));
    let elapsed = now.saturating_sub(tracking_started_at).max(3_600);
    bytes_f64(bytes) / elapsed as f64 * DAY as f64
}

/// What a backtest at any horizon needs from one machine's complete days.
struct Backtest {
    /// The rate a forecast made at the start of each day would have used, when its window held
    /// enough observed days.
    rates: Vec<Option<f64>>,
    /// Prefix sums of observed bytes and of unobserved days.
    bytes: Vec<f64>,
    unobserved: Vec<usize>,
}

impl Backtest {
    fn new(days: &[RateDay], weights: &[f64; RATE_WINDOW_DAYS]) -> Self {
        let rates = (0..days.len())
            .map(|origin| {
                recent_rate(days, origin, weights)
                    .filter(|(rate, observed)| *observed >= RATE_DAYS_ENOUGH && *rate > 0.0)
                    .map(|(rate, _)| rate)
            })
            .collect();
        let (mut sum, mut missing) = (0.0, 0);
        let mut bytes = Vec::with_capacity(days.len() + 1);
        let mut unobserved = Vec::with_capacity(days.len() + 1);
        bytes.push(sum);
        unobserved.push(missing);
        for day in days {
            match day {
                RateDay::Observed(value) => sum += value,
                RateDay::Unobserved => missing += 1,
            }
            bytes.push(sum);
            unobserved.push(missing);
        }
        Self {
            rates,
            bytes,
            unobserved,
        }
    }

    fn len(&self) -> usize {
        self.rates.len()
    }

    /// `actual / forecast - 1` for every origin whose next `horizon` days were all observed.
    fn errors(&self, horizon: usize) -> impl Iterator<Item = f64> + '_ {
        let origins = (self.len() + 1).saturating_sub(horizon);
        (0..origins).filter_map(move |origin| {
            let rate = self.rates[origin]?;
            let end = origin + horizon;
            if self.unobserved[end] != self.unobserved[origin] {
                return None;
            }
            Some((self.bytes[end] - self.bytes[origin]) / (rate * horizon as f64) - 1.0)
        })
    }
}

/// Backtests pooled over every machine in scope, computed once per horizon.
struct FleetBacktests<'a> {
    machines: Vec<&'a Backtest>,
    longest: usize,
    pooled: HashMap<usize, Vec<f64>>,
}

impl<'a> FleetBacktests<'a> {
    fn new(machines: Vec<&'a Backtest>) -> Self {
        let longest = machines
            .iter()
            .map(|machine| machine.len())
            .max()
            .unwrap_or(0);
        Self {
            machines,
            longest,
            pooled: HashMap::new(),
        }
    }

    fn errors(&mut self, horizon: usize) -> &[f64] {
        // An origin needs a full rate window before it and the whole horizon after it.
        if horizon + RATE_DAYS_ENOUGH > self.longest {
            return &[];
        }
        let machines = &self.machines;
        self.pooled.entry(horizon).or_insert_with(|| {
            machines
                .iter()
                .flat_map(|machine| machine.errors(horizon))
                .collect()
        })
    }
}

fn enough_backtests(samples: usize, horizon: usize) -> bool {
    samples >= BAND_MIN_SAMPLES && samples >= BAND_MIN_WINDOWS * horizon
}

/// Conformal 90% upper error: the ⌈0.9·(n + 1)⌉-th smallest of n backtests, floored at zero.
/// The extra rank keeps the bound from claiming more than a finite history shows.
fn upper_error(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() + 1) * 9).div_ceil(10).min(sorted.len());
    sorted
        .get(rank.saturating_sub(1))
        .map_or(0.0, |error| error.max(0.0))
}

fn measured_band(
    basis: MachineTrafficBandBasis,
    horizon: usize,
    samples: &[f64],
) -> (MachineTrafficBand, f64) {
    let upper = upper_error(samples);
    (
        MachineTrafficBand {
            basis,
            horizon_days: u32::try_from(horizon).unwrap_or(u32::MAX),
            samples: u32::try_from(samples.len()).unwrap_or(u32::MAX),
            upper_error_basis_points: basis_points(upper),
        },
        upper,
    )
}

/// The upper error for the rest of the period: this machine's backtests when they cover enough
/// independent horizons, else the fleet's at this horizon or the longest one with enough history
/// down to half of it, else the default.
fn forecast_band(
    own: &Backtest,
    fleet: &mut FleetBacktests<'_>,
    remaining_days: f64,
) -> (MachineTrafficBand, f64) {
    // A partial day still needs a forecast of one.
    let horizon = (remaining_days.ceil() as usize).max(1);
    let own_errors = own.errors(horizon).collect::<Vec<_>>();
    if enough_backtests(own_errors.len(), horizon) {
        return measured_band(MachineTrafficBandBasis::Node, horizon, &own_errors);
    }
    for candidate in (horizon.div_ceil(BAND_MAX_STRETCH)..=horizon).rev() {
        let pooled = fleet.errors(candidate);
        if enough_backtests(pooled.len(), candidate) {
            return measured_band(MachineTrafficBandBasis::Fleet, candidate, pooled);
        }
    }
    (
        MachineTrafficBand {
            basis: MachineTrafficBandBasis::Default,
            horizon_days: u32::try_from(horizon).unwrap_or(u32::MAX),
            samples: 0,
            upper_error_basis_points: basis_points(BAND_DEFAULT_UPPER_ERROR),
        },
        BAND_DEFAULT_UPPER_ERROR,
    )
}

/// One machine's rate and completeness, before the fleet's backtests are known.
struct NodeForecast {
    backtest: Backtest,
    daily_rate: f64,
    rate_days: usize,
    remaining_days: f64,
    needs_calibration: bool,
    gap: Option<TrafficGap>,
}

struct DirectionalNodeForecast {
    accounting: NodeForecast,
    rx_daily_rate: f64,
    tx_daily_rate: f64,
    rate_days: usize,
}

fn prepare_forecast(
    anchors: PeriodAnchors,
    ledger: &[(i64, u64)],
    gaps: &[TrafficGap],
    now: i64,
    weights: &[f64; RATE_WINDOW_DAYS],
) -> NodeForecast {
    let remaining_days = (anchors.end - now.clamp(anchors.start, anchors.end)) as f64 / DAY as f64;
    let Some(tracking_started_at) = anchors.tracking_started_at else {
        // Nothing reported yet: there is no rate, and no tracked total a calibration could repair.
        return NodeForecast {
            backtest: Backtest::new(&[], weights),
            daily_rate: 0.0,
            rate_days: 0,
            remaining_days,
            needs_calibration: false,
            gap: None,
        };
    };
    let days = daily_series(ledger, gaps, tracking_started_at, day_start(now));
    let (daily_rate, rate_days) = recent_rate(&days, days.len(), weights)
        .unwrap_or_else(|| (rate_since(ledger, tracking_started_at, now), 0));
    // A calibration fixes the total at its own instant; only what follows can unsettle it again.
    let anchor = anchors
        .calibrated_at
        .map_or(anchors.start, |at| at.max(anchors.start));
    let gap = gaps
        .iter()
        .rev()
        .filter(|gap| gap.occurred_at >= anchor && gap.occurred_at < anchors.end)
        .find(|gap| gap_changes_total(**gap, ledger, tracking_started_at, anchors.start))
        .copied();
    NodeForecast {
        backtest: Backtest::new(&days, weights),
        daily_rate,
        rate_days,
        remaining_days,
        needs_calibration: tracking_started_at > anchor || gap.is_some(),
        gap,
    }
}

/// `used` is known; only the remaining days are forecast, so both values meet `used` at the end.
#[cfg(test)]
fn project(used: u64, daily_rate: f64, remaining_days: f64, upper_error: f64) -> (u64, u64) {
    let remaining = daily_rate * remaining_days;
    let p50 = used.saturating_add(estimated_bytes(remaining));
    let p90 = used.saturating_add(estimated_bytes(remaining * (1.0 + upper_error)));
    (p50, p90.max(p50))
}

/// Project each immutable direction, then apply the provider rule to the cumulative period. This
/// is the critical distinction for `bidirectional_max`: `max(sum(rx), sum(tx))`, never
/// `sum(max(daily_rx, daily_tx))`.
fn project_directional(
    mode: NodeTrafficAccountingMode,
    used: u64,
    current: (u64, u64),
    daily_rate: (f64, f64),
    remaining_days: f64,
    upper_error: f64,
) -> (u64, u64) {
    let (current_rx, current_tx) = current;
    let (rx_daily_rate, tx_daily_rate) = daily_rate;
    let current_accounted = mode.apply(current_rx, current_tx);
    let projected = |multiplier: f64| {
        let rx =
            current_rx.saturating_add(estimated_bytes(rx_daily_rate * remaining_days * multiplier));
        let tx =
            current_tx.saturating_add(estimated_bytes(tx_daily_rate * remaining_days * multiplier));
        used.saturating_add(mode.apply(rx, tx).saturating_sub(current_accounted))
    };
    let p50 = projected(1.0);
    let p90 = projected(1.0 + upper_error);
    (p50, p90.max(p50))
}

fn forecast_confidence(
    needs_calibration: bool,
    rate_days: usize,
    basis: MachineTrafficBandBasis,
) -> MachineTrafficConfidence {
    if needs_calibration || rate_days < RATE_DAYS_FEW || basis == MachineTrafficBandBasis::Default {
        MachineTrafficConfidence::Low
    } else if rate_days < RATE_DAYS_ENOUGH || basis == MachineTrafficBandBasis::Fleet {
        MachineTrafficConfidence::Medium
    } else {
        MachineTrafficConfidence::High
    }
}

#[derive(Default)]
struct UserDemand {
    bytes: u64,
    apps: BTreeSet<String>,
}

struct EligibleUserUsage {
    period_start: i64,
    tenant_id: String,
    user_id: String,
    app_label: String,
    bytes: u64,
}

/// Capacity planning joins two deliberately different ledgers:
///
/// * physical NIC bytes decide whether the provider allowance is at risk;
/// * `(user uplink + user downlink) * 2` estimates the machine footprint because forwarded
///   payload crosses the NIC on both legs; user counters then distribute that work across users
///   eligible to traverse the node in the current topology.
///
/// Relay counters contain no user identity, so attribution is a bounded proportional estimate and
/// any physical remainder stays visible instead of being silently charged to a user.
///
/// The rest of the period is forecast from the recent daily rate. The P90 error comes from
/// backtests of the same method on the ledger, so the forecast and its error stay consistent.
pub async fn load_machine_traffic_model(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<MachineTrafficModelView> {
    let traffic = load_node_traffic(pool, actor).await?;
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StoreError::InvalidData("system clock is before unix epoch".to_owned()))?
            .as_secs(),
    )
    .map_err(|_| StoreError::InvalidData("system clock exceeds i64 unix time".to_owned()))?;
    let tenant_scope = actor.tenant_scope();
    let tenant_pattern = actor.tenant_scope_like_pattern();

    let physical_rows = sqlx::query(
        "SELECT d.node_id,
                extract(epoch FROM d.day::timestamp AT TIME ZONE 'UTC')::bigint AS day_start,
                d.rx_bytes::text AS rx_bytes,
                d.tx_bytes::text AS tx_bytes
           FROM node_traffic_daily AS d
           JOIN nodes AS n ON n.id = d.node_id
          WHERE d.day >= (now() AT TIME ZONE 'UTC')::date - 740
            AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY d.node_id, d.day",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;
    let mut physical_days: HashMap<String, Vec<DirectionalTrafficDay>> = HashMap::new();
    for row in physical_rows {
        let node_id: String = row.try_get("node_id")?;
        physical_days
            .entry(node_id)
            .or_default()
            .push(DirectionalTrafficDay {
                day: row.try_get("day_start")?,
                rx_bytes: parse_u64_column(&row, "rx_bytes")?,
                tx_bytes: parse_u64_column(&row, "tx_bytes")?,
            });
    }

    let gap_rows = sqlx::query(
        "SELECT g.node_id,
                extract(epoch FROM g.occurred_at)::bigint AS occurred_at,
                g.reason
           FROM node_traffic_gaps AS g
           JOIN nodes AS n ON n.id = g.node_id
          WHERE g.occurred_at >= now() - interval '740 days'
            AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY g.node_id, g.occurred_at, g.id",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;
    let mut gaps_by_node: HashMap<String, Vec<TrafficGap>> = HashMap::new();
    for row in gap_rows {
        let node_id: String = row.try_get("node_id")?;
        gaps_by_node.entry(node_id).or_default().push(TrafficGap {
            occurred_at: row.try_get("occurred_at")?,
            reason: GapReason::parse(&row.try_get::<String, _>("reason")?)?,
        });
    }

    // A user's app usage is copied only onto nodes their current grant can traverse. UNION removes
    // the ingress from the step branch where it is also represented as a chain member.
    let demand_rows = sqlx::query(
        "WITH eligible AS (
             SELECT g.tenant_id, g.user_id, g.app_id, i.node_id
               FROM grants AS g
               JOIN ingresses AS i ON i.id = g.ingress_id
             UNION
             SELECT g.tenant_id, g.user_id, g.app_id, s.node_id
               FROM grants AS g
               JOIN ingresses AS i ON i.id = g.ingress_id
               JOIN steps AS s ON s.chain_id = i.chain_id
         )
         SELECT e.node_id, e.tenant_id, e.user_id, e.app_id,
                COALESCE(a.label, e.app_id) AS app_label,
                extract(epoch FROM u.period_start)::bigint AS period_start,
                (u.uplink_bytes + u.downlink_bytes)::text AS bytes
           FROM eligible AS e
           JOIN usage_rollups AS u
             ON u.tenant_id = e.tenant_id
            AND u.user_id = e.user_id
            AND u.app_id = e.app_id
           LEFT JOIN apps AS a ON a.id = e.app_id
          WHERE u.period_start >= now() - interval '370 days'
            AND ($1::text IS NULL OR e.tenant_id = $1 OR e.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY e.node_id, e.tenant_id, e.user_id, u.period_start",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;
    let mut demand_by_node: HashMap<String, Vec<EligibleUserUsage>> = HashMap::new();
    for row in demand_rows {
        let node_id: String = row.try_get("node_id")?;
        demand_by_node
            .entry(node_id)
            .or_default()
            .push(EligibleUserUsage {
                period_start: row.try_get("period_start")?,
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                app_label: row.try_get("app_label")?,
                bytes: parse_u64_column(&row, "bytes")?,
            });
    }

    let logical_rows = sqlx::query(
        "SELECT r.node_id,
                extract(epoch FROM r.period_start)::bigint AS period_start,
                (r.user_uplink_bytes + r.user_downlink_bytes
                 + r.relay_uplink_bytes + r.relay_downlink_bytes)::text AS bytes
           FROM usage_node_rollups AS r
           JOIN nodes AS n ON n.id = r.node_id
          WHERE r.period_start >= now() - interval '370 days'
            AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY r.node_id, r.period_start",
    )
    .bind(tenant_scope)
    .bind(&tenant_pattern)
    .fetch_all(pool)
    .await?;
    let mut logical_by_node: HashMap<String, Vec<(i64, u64)>> = HashMap::new();
    for row in logical_rows {
        let node_id: String = row.try_get("node_id")?;
        logical_by_node.entry(node_id).or_default().push((
            row.try_get("period_start")?,
            parse_u64_column(&row, "bytes")?,
        ));
    }
    // Forwarding is attributed directly on its entry machine; never project it through grants
    // or add it to the already complete physical NIC ledger.
    let forwarding_rows = sqlx::query(
        "SELECT r.node_id,r.tenant_id,r.user_id,extract(epoch FROM r.period_start)::bigint AS period_start,
                sum(r.uplink_bytes+r.downlink_bytes)::text AS bytes
         FROM forwarding_usage_rollups r JOIN nodes n ON n.id=r.node_id
         WHERE r.period_start >= now()-interval '370 days'
           AND ($1::text IS NULL OR r.tenant_id=$1 OR r.tenant_id LIKE $2 ESCAPE '\\')
         GROUP BY r.node_id,r.tenant_id,r.user_id,r.period_start ORDER BY r.node_id,r.period_start",
    ).bind(tenant_scope).bind(&tenant_pattern).fetch_all(pool).await?;
    for row in forwarding_rows {
        let node_id: String = row.try_get("node_id")?;
        let period_start = row.try_get("period_start")?;
        let bytes = parse_u64_column(&row, "bytes")?;
        demand_by_node
            .entry(node_id.clone())
            .or_default()
            .push(EligibleUserUsage {
                period_start,
                tenant_id: row.try_get("tenant_id")?,
                user_id: row.try_get("user_id")?,
                app_label: "转发".to_owned(),
                bytes,
            });
        logical_by_node
            .entry(node_id)
            .or_default()
            .push((period_start, bytes));
    }

    let mut accuracy =
        load_forecast_accuracy(pool, tenant_scope, tenant_pattern.as_deref()).await?;

    let weights = rate_weights();
    let forecasts = traffic
        .nodes
        .iter()
        .map(|item| {
            let days = physical_days
                .get(&item.node_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let gaps = gaps_by_node
                .get(&item.node_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let rx_ledger = direction_ledger(days, true);
            let tx_ledger = direction_ledger(days, false);
            let accounting_ledger = accounting_rate_ledger(days, item.accounting_mode);
            let anchors = PeriodAnchors {
                start: item.period_start_unix_secs,
                end: item.period_end_unix_secs,
                tracking_started_at: item.tracking_started_at_unix_secs,
                calibrated_at: item.calibrated_at_unix_secs,
            };
            let rx = prepare_forecast(anchors, &rx_ledger, gaps, now, &weights);
            let tx = prepare_forecast(anchors, &tx_ledger, gaps, now, &weights);
            DirectionalNodeForecast {
                accounting: prepare_forecast(anchors, &accounting_ledger, gaps, now, &weights),
                rx_daily_rate: rx.daily_rate,
                tx_daily_rate: tx.daily_rate,
                rate_days: rx.rate_days.min(tx.rate_days),
            }
        })
        .collect::<Vec<_>>();
    // Machines with too little history of their own borrow the error of the whole scope.
    let bands = {
        let mut fleet = FleetBacktests::new(
            forecasts
                .iter()
                .map(|forecast| &forecast.accounting.backtest)
                .collect(),
        );
        forecasts
            .iter()
            .map(|forecast| {
                forecast_band(
                    &forecast.accounting.backtest,
                    &mut fleet,
                    forecast.accounting.remaining_days,
                )
            })
            .collect::<Vec<_>>()
    };

    let mut nodes = Vec::with_capacity(traffic.nodes.len());
    for ((item, forecast), (band, upper)) in traffic.nodes.into_iter().zip(forecasts).zip(bands) {
        let start = item.period_start_unix_secs;
        let end = item.period_end_unix_secs;
        let duration = end.saturating_sub(start).max(DAY);
        let previous_start = start.saturating_sub(duration);
        let days = physical_days
            .get(&item.node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let current_days = ledger_window(days, item.accounting_mode, start, end);
        let previous_days = ledger_window(days, item.accounting_mode, previous_start, start);
        let (current_rx, current_tx) = directional_window_totals(days, start, end);
        let (previous_rx, previous_tx) = directional_window_totals(days, previous_start, start);
        let previous_total = item.accounting_mode.apply(previous_rx, previous_tx);
        let used = parse_decimal_u64(&item.total_bytes, "机器物理流量")?;
        let (p50, p90) = project_directional(
            item.accounting_mode,
            used,
            (current_rx, current_tx),
            (forecast.rx_daily_rate, forecast.tx_daily_rate),
            forecast.accounting.remaining_days,
            upper,
        );
        let effective_daily_rate = if forecast.accounting.remaining_days > 0.0 {
            bytes_f64(p50.saturating_sub(used)) / forecast.accounting.remaining_days
        } else {
            0.0
        };

        let mut user_demand: BTreeMap<(String, String), UserDemand> = BTreeMap::new();
        for usage in demand_by_node
            .get(&item.node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if usage.period_start < start || usage.period_start >= end {
                continue;
            }
            let demand = user_demand
                .entry((usage.tenant_id.clone(), usage.user_id.clone()))
                .or_default();
            demand.bytes = demand.bytes.saturating_add(usage.bytes);
            demand.apps.insert(usage.app_label.clone());
        }
        let total_demand = user_demand
            .values()
            .fold(0_u64, |sum, demand| sum.saturating_add(demand.bytes));
        let logical = logical_by_node
            .get(&item.node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter(|(period, _)| *period >= start && *period < end)
            .fold(0_u64, |sum, (_, value)| sum.saturating_add(*value));
        let attributable = attributable_physical_bytes(used, logical, total_demand > 0);
        let mut attributed_sum = 0_u64;
        let mut users = user_demand
            .into_iter()
            .filter(|(_, demand)| demand.bytes > 0)
            .map(|((tenant_id, user_id), demand)| {
                let attributed = mul_div(attributable, demand.bytes, total_demand);
                attributed_sum = attributed_sum.saturating_add(attributed);
                let projected = mul_div(p50, attributed, used);
                MachineTrafficUser {
                    tenant_id,
                    user_id,
                    attributed_bytes: attributed.to_string(),
                    projected_bytes: projected.to_string(),
                    share_basis_points: u16::try_from(mul_div(attributed, 10_000, used))
                        .unwrap_or(10_000),
                    apps: demand.apps.into_iter().collect(),
                }
            })
            .collect::<Vec<_>>();
        users.sort_by(|left, right| {
            parse_decimal_u64(&right.attributed_bytes, "用户归因流量")
                .unwrap_or(0)
                .cmp(&parse_decimal_u64(&left.attributed_bytes, "用户归因流量").unwrap_or(0))
                .then_with(|| left.tenant_id.cmp(&right.tenant_id))
                .then_with(|| left.user_id.cmp(&right.user_id))
        });
        let unattributed = used.saturating_sub(attributed_sum.min(used));
        let projected_unattributed = p50.saturating_sub(users.iter().fold(0_u64, |sum, user| {
            sum.saturating_add(
                parse_decimal_u64(&user.projected_bytes, "用户预测流量").unwrap_or(0),
            )
        }));
        let confidence = forecast_confidence(
            forecast.accounting.needs_calibration,
            forecast.rate_days,
            band.basis,
        );
        let last_period_accuracy = accuracy.remove(&item.node_id);
        nodes.push(MachineTrafficModelNode {
            node_id: item.node_id,
            tenant_id: item.tenant_id,
            name: item.name,
            cycle_kind: item.cycle_kind,
            accounting_mode: item.accounting_mode,
            reset_month: item.reset_month,
            reset_day: item.reset_day,
            period_start_unix_secs: start,
            period_end_unix_secs: end,
            previous_period_start_unix_secs: previous_start,
            quota_bytes: item.quota_bytes,
            used_bytes: used.to_string(),
            previous_period_bytes: previous_total.to_string(),
            daily_rate_bytes: estimated_bytes(effective_daily_rate).to_string(),
            rate_days: u32::try_from(forecast.rate_days).unwrap_or(u32::MAX),
            projected_p50_bytes: p50.to_string(),
            projected_p90_bytes: p90.to_string(),
            band,
            confidence,
            needs_calibration: forecast.accounting.needs_calibration,
            days: current_days,
            previous_days,
            tracking_started_at_unix_secs: item.tracking_started_at_unix_secs,
            calibrated_at_unix_secs: item.calibrated_at_unix_secs,
            gap_at_unix_secs: forecast.accounting.gap.map(|gap| gap.occurred_at),
            gap_reason: forecast
                .accounting
                .gap
                .map(|gap| gap.reason.as_str().to_owned()),
            last_period_accuracy,
            users,
            unattributed_bytes: unattributed.to_string(),
            projected_unattributed_bytes: projected_unattributed.to_string(),
        });
    }
    Ok(MachineTrafficModelView {
        generated_at_unix_secs: now,
        attribution: "current_topology_proportional".to_owned(),
        nodes,
    })
}

/// Accuracy of each machine's latest closed period that has recorded forecasts. The final total
/// follows the same rule as the live view: the period's ledger, or the latest calibration in the
/// period plus what the meter accounted from it to the period end.
async fn load_forecast_accuracy(
    pool: &PgPool,
    tenant_scope: Option<&str>,
    tenant_pattern: Option<&str>,
) -> Result<HashMap<String, MachineTrafficAccuracy>> {
    let rows = sqlx::query(
        "WITH latest AS (
             SELECT DISTINCT ON (f.node_id) f.node_id, f.period_start, f.period_end,
                    f.accounting_mode
               FROM node_traffic_forecasts AS f
               JOIN nodes AS n ON n.id = f.node_id
               LEFT JOIN node_traffic_policy AS p ON p.node_id = f.node_id
              WHERE f.period_end <= (now() AT TIME ZONE 'UTC')::date
                AND f.accounting_mode = COALESCE(p.accounting_mode, 'bidirectional_sum')
                AND ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\\')
              ORDER BY f.node_id, f.period_end DESC, f.period_start DESC
         ), closing AS (
             SELECT latest.*,
                    CASE WHEN calibration.target_total_bytes IS NOT NULL
                         THEN calibration.target_total_bytes +
                              CASE latest.accounting_mode
                                  WHEN 'bidirectional_sum' THEN
                                      CASE WHEN calibration.accounted_rx_bytes IS NULL THEN
                                          GREATEST(
                                              COALESCE(state.accounted_rx_bytes + state.accounted_tx_bytes, 0)
                                              - COALESCE(later.rx_bytes + later.tx_bytes, 0)
                                              - calibration.accounted_total_bytes, 0)
                                      ELSE
                                          GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                                   - COALESCE(later.rx_bytes, 0)
                                                   - calibration.accounted_rx_bytes, 0)
                                          + GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                                     - COALESCE(later.tx_bytes, 0)
                                                     - calibration.accounted_tx_bytes, 0)
                                      END
                                  WHEN 'bidirectional_max' THEN GREATEST(
                                      GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                               - COALESCE(later.rx_bytes, 0)
                                               - calibration.accounted_rx_bytes, 0),
                                      GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                               - COALESCE(later.tx_bytes, 0)
                                               - calibration.accounted_tx_bytes, 0)
                                  )
                                  WHEN 'outbound_only' THEN
                                      GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                               - COALESCE(later.tx_bytes, 0)
                                               - calibration.accounted_tx_bytes, 0)
                                  WHEN 'inbound_only' THEN
                                      GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                               - COALESCE(later.rx_bytes, 0)
                                               - calibration.accounted_rx_bytes, 0)
                              END
                         ELSE CASE latest.accounting_mode
                                  WHEN 'bidirectional_sum' THEN
                                      COALESCE(within.rx_bytes + within.tx_bytes, 0)
                                  WHEN 'bidirectional_max' THEN GREATEST(
                                      COALESCE(within.rx_bytes, 0), COALESCE(within.tx_bytes, 0))
                                  WHEN 'outbound_only' THEN COALESCE(within.tx_bytes, 0)
                                  WHEN 'inbound_only' THEN COALESCE(within.rx_bytes, 0)
                              END
                    END AS actual_bytes
               FROM latest
               LEFT JOIN node_traffic_state AS state ON state.node_id = latest.node_id
               LEFT JOIN LATERAL (
                   SELECT sum(day.rx_bytes) AS rx_bytes, sum(day.tx_bytes) AS tx_bytes
                     FROM node_traffic_daily AS day
                    WHERE day.node_id = latest.node_id
                      AND day.day >= latest.period_start
                      AND day.day < latest.period_end
               ) AS within ON TRUE
               LEFT JOIN LATERAL (
                   SELECT sum(day.rx_bytes) AS rx_bytes, sum(day.tx_bytes) AS tx_bytes
                     FROM node_traffic_daily AS day
                    WHERE day.node_id = latest.node_id
                      AND day.day >= latest.period_end
               ) AS later ON TRUE
               LEFT JOIN LATERAL (
                   SELECT item.target_total_bytes, item.accounted_total_bytes,
                          item.accounted_rx_bytes, item.accounted_tx_bytes
                     FROM node_traffic_calibrations AS item
                    WHERE item.node_id = latest.node_id
                      AND item.calibrated_at >= latest.period_start::timestamp AT TIME ZONE 'UTC'
                      AND item.calibrated_at < latest.period_end::timestamp AT TIME ZONE 'UTC'
                      AND item.accounting_mode = latest.accounting_mode
                    ORDER BY item.calibrated_at DESC, item.id DESC
                    LIMIT 1
               ) AS calibration ON TRUE
         )
         SELECT closing.node_id,
                extract(epoch FROM closing.period_start::timestamp AT TIME ZONE 'UTC')::bigint
                    AS period_start_unix_secs,
                extract(epoch FROM closing.period_end::timestamp AT TIME ZONE 'UTC')::bigint
                    AS period_end_unix_secs,
                closing.actual_bytes::text AS actual_bytes,
                count(*) AS forecasts,
                round(avg(abs(f.p50_bytes - closing.actual_bytes) / closing.actual_bytes)
                      * 10000)::bigint AS p50_error_basis_points,
                count(*) FILTER (WHERE closing.actual_bytes <= f.p90_bytes) AS p90_covered
           FROM closing
           JOIN node_traffic_forecasts AS f
             ON f.node_id = closing.node_id
            AND f.period_start = closing.period_start
            AND f.period_end = closing.period_end
            AND f.accounting_mode = closing.accounting_mode
          WHERE closing.actual_bytes > 0
          GROUP BY closing.node_id, closing.period_start, closing.period_end,
                   closing.actual_bytes",
    )
    .bind(tenant_scope)
    .bind(tenant_pattern)
    .fetch_all(pool)
    .await?;
    let count = |row: &sqlx::postgres::PgRow, name: &str| -> Result<u32> {
        u32::try_from(row.try_get::<i64, _>(name)?)
            .map_err(|_| StoreError::InvalidData(format!("forecast accuracy {name} out of range")))
    };
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("node_id")?,
                MachineTrafficAccuracy {
                    period_start_unix_secs: row.try_get("period_start_unix_secs")?,
                    period_end_unix_secs: row.try_get("period_end_unix_secs")?,
                    actual_bytes: row.try_get("actual_bytes")?,
                    forecasts: count(row, "forecasts")?,
                    p50_error_basis_points: count(row, "p50_error_basis_points")?,
                    p90_covered: count(row, "p90_covered")?,
                },
            ))
        })
        .collect()
}

/// Record today's forecast of every measured machine, once per UTC day. The first record of a
/// day stands, so the history shows what operators saw rather than a later revision.
pub async fn record_machine_traffic_forecasts(pool: &PgPool) -> Result<u64> {
    let view =
        load_machine_traffic_model(pool, &AdminContext::system_admin(FORECAST_ACTOR)).await?;
    let measured = view
        .nodes
        .iter()
        .filter(|node| node.tracking_started_at_unix_secs.is_some())
        .collect::<Vec<_>>();
    if measured.is_empty() {
        return Ok(0);
    }
    let column = |value: fn(&MachineTrafficModelNode) -> String| {
        measured.iter().map(|&node| value(node)).collect::<Vec<_>>()
    };
    let inserted = sqlx::query(
        "INSERT INTO node_traffic_forecasts (
            node_id, day, taken_at, period_start, period_end,
            quota_bytes, accounting_mode, used_bytes, p50_bytes, p90_bytes
         )
         SELECT item.node_id,
                (to_timestamp($1) AT TIME ZONE 'UTC')::date,
                to_timestamp($1),
                (to_timestamp(item.period_start) AT TIME ZONE 'UTC')::date,
                (to_timestamp(item.period_end) AT TIME ZONE 'UTC')::date,
                item.quota::numeric, item.accounting_mode,
                item.used::numeric, item.p50::numeric, item.p90::numeric
           FROM UNNEST($2::text[], $3::bigint[], $4::bigint[], $5::text[], $6::text[],
                       $7::text[], $8::text[], $9::text[])
                AS item(node_id, period_start, period_end, quota, accounting_mode, used, p50, p90)
         ON CONFLICT (node_id, day) DO UPDATE SET
            taken_at = EXCLUDED.taken_at,
            period_start = EXCLUDED.period_start,
            period_end = EXCLUDED.period_end,
            quota_bytes = EXCLUDED.quota_bytes,
            accounting_mode = EXCLUDED.accounting_mode,
            used_bytes = EXCLUDED.used_bytes,
            p50_bytes = EXCLUDED.p50_bytes,
            p90_bytes = EXCLUDED.p90_bytes
         WHERE node_traffic_forecasts.accounting_mode <> EXCLUDED.accounting_mode",
    )
    .bind(view.generated_at_unix_secs as f64)
    .bind(column(|node| node.node_id.clone()))
    .bind(
        measured
            .iter()
            .map(|node| node.period_start_unix_secs)
            .collect::<Vec<_>>(),
    )
    .bind(
        measured
            .iter()
            .map(|node| node.period_end_unix_secs)
            .collect::<Vec<_>>(),
    )
    .bind(
        measured
            .iter()
            .map(|node| node.quota_bytes.clone())
            .collect::<Vec<_>>(),
    )
    .bind(
        measured
            .iter()
            .map(|node| node.accounting_mode.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(column(|node| node.used_bytes.clone()))
    .bind(column(|node| node.projected_p50_bytes.clone()))
    .bind(column(|node| node.projected_p90_bytes.clone()))
    .execute(pool)
    .await?;
    Ok(inserted.rows_affected())
}

pub async fn prune_machine_traffic_forecasts(pool: &PgPool, retain_days: u32) -> Result<u64> {
    prune_machine_traffic_forecasts_hours(pool, retain_days.saturating_mul(24)).await
}

pub(crate) async fn prune_machine_traffic_forecasts_hours(
    pool: &PgPool,
    retain_hours: u32,
) -> Result<u64> {
    crate::retention::cleanup::prune_batch(pool, "node_traffic_forecasts", retain_hours).await
}

pub(crate) const NODE_TRAFFIC_VIEW_SQL: &str = r#"
WITH policies AS (
    SELECT n.id AS node_id, n.tenant_id, n.name,
           COALESCE(p.cycle_kind, 'monthly') AS cycle_kind,
           COALESCE(p.accounting_mode, 'bidirectional_sum') AS accounting_mode,
           p.reset_month,
           COALESCE(p.reset_day, 1) AS reset_day,
           p.quota_bytes,
           (now() AT TIME ZONE 'UTC')::date AS today
      FROM nodes AS n
      LEFT JOIN node_traffic_policy AS p ON p.node_id = n.id
     WHERE ($1::text IS NULL OR n.tenant_id = $1 OR n.tenant_id LIKE $2 ESCAPE '\')
       AND ($3::text IS NULL OR n.id = $3)
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
SELECT b.node_id, b.tenant_id, b.name, b.cycle_kind, b.accounting_mode,
       b.reset_month, b.reset_day,
       b.quota_bytes::text AS quota_bytes,
       extract(epoch FROM b.period_start::timestamp AT TIME ZONE 'UTC')::bigint
           AS period_start_unix_secs,
       extract(epoch FROM b.period_end::timestamp AT TIME ZONE 'UTC')::bigint
           AS period_end_unix_secs,
       daily.rx_bytes::text AS rx_bytes,
       daily.tx_bytes::text AS tx_bytes,
       (CASE WHEN calibration.target_total_bytes IS NOT NULL
             THEN calibration.target_total_bytes
                  + CASE b.accounting_mode
                        WHEN 'bidirectional_sum' THEN
                            CASE WHEN calibration.accounted_rx_bytes IS NULL THEN
                                GREATEST(
                                    COALESCE(state.accounted_rx_bytes + state.accounted_tx_bytes, 0)
                                    - calibration.accounted_total_bytes,
                                    0
                                )
                            ELSE
                                GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                         - calibration.accounted_rx_bytes, 0)
                                + GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                           - calibration.accounted_tx_bytes, 0)
                            END
                        WHEN 'bidirectional_max' THEN GREATEST(
                            GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                     - calibration.accounted_rx_bytes, 0),
                            GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                     - calibration.accounted_tx_bytes, 0)
                        )
                        WHEN 'outbound_only' THEN
                            GREATEST(COALESCE(state.accounted_tx_bytes, 0)
                                     - calibration.accounted_tx_bytes, 0)
                        WHEN 'inbound_only' THEN
                            GREATEST(COALESCE(state.accounted_rx_bytes, 0)
                                     - calibration.accounted_rx_bytes, 0)
                    END
             ELSE CASE b.accounting_mode
                      WHEN 'bidirectional_sum' THEN daily.rx_bytes + daily.tx_bytes
                      WHEN 'bidirectional_max' THEN GREATEST(daily.rx_bytes, daily.tx_bytes)
                      WHEN 'outbound_only' THEN daily.tx_bytes
                      WHEN 'inbound_only' THEN daily.rx_bytes
                  END
        END)::text AS total_bytes,
       state.meter_id, state.sequence::text AS meter_sequence,
       state.agent_rx_bytes::text AS meter_rx_bytes, state.agent_tx_bytes::text AS meter_tx_bytes,
       COALESCE(calibration.target_total_bytes, 0)::text AS limit_calibrated_total,
       (calibration.target_total_bytes IS NOT NULL AND calibration.accounted_rx_bytes IS NULL) AS limit_legacy_calibration,
       (CASE WHEN calibration.target_total_bytes IS NOT NULL THEN
           GREATEST(COALESCE(state.accounted_rx_bytes, 0) - COALESCE(calibration.accounted_rx_bytes, 0), 0)
           ELSE daily.rx_bytes END)::text AS limit_rx_bytes,
       (CASE WHEN calibration.target_total_bytes IS NOT NULL THEN
           GREATEST(COALESCE(state.accounted_tx_bytes, 0) - COALESCE(calibration.accounted_tx_bytes, 0), 0)
           ELSE daily.tx_bytes END)::text AS limit_tx_bytes,
       COALESCE(limiter.policy, '{"enabled":false,"rules":[]}'::jsonb) AS limit_policy,
       COALESCE(limiter.revision, 0) AS limit_revision,
       limiter.observation AS limit_observation,
       extract(epoch FROM limiter.reported_at)::bigint AS limit_reported_at,
       COALESCE(agent.agent_protocol_version >= 42, false) AS limit_supported,
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
  LEFT JOIN node_traffic_limit AS limiter ON limiter.node_id = b.node_id
  LEFT JOIN node_agent_state AS agent ON agent.node_id = b.node_id
  LEFT JOIN LATERAL (
      SELECT COALESCE(sum(day.rx_bytes), 0) AS rx_bytes,
             COALESCE(sum(day.tx_bytes), 0) AS tx_bytes
        FROM node_traffic_daily AS day
       WHERE day.node_id = b.node_id
         AND day.day >= b.period_start
         AND day.day < b.period_end
  ) AS daily ON TRUE
  LEFT JOIN LATERAL (
      SELECT item.calibrated_at, item.target_total_bytes, item.accounted_total_bytes,
             item.accounted_rx_bytes, item.accounted_tx_bytes
        FROM node_traffic_calibrations AS item
       WHERE item.node_id = b.node_id
         AND item.accounting_mode = b.accounting_mode
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
            accounting_mode: Some(NodeTrafficAccountingMode::BidirectionalSum),
            reset_month: None,
            reset_day: 31,
            quota_bytes: Some("10995116277760".to_owned()),
            calibrated_total_bytes: None,
        })
        .is_ok());
        assert!(valid_policy(&UpdateNodeTrafficRequest {
            cycle_kind: NodeTrafficCycleKind::Yearly,
            accounting_mode: Some(NodeTrafficAccountingMode::BidirectionalMax),
            reset_month: Some(2),
            reset_day: 29,
            quota_bytes: None,
            calibrated_total_bytes: Some("1099511627776".to_owned()),
        })
        .is_ok());
        assert!(valid_policy(&UpdateNodeTrafficRequest {
            cycle_kind: NodeTrafficCycleKind::Monthly,
            accounting_mode: Some(NodeTrafficAccountingMode::OutboundOnly),
            reset_month: Some(1),
            reset_day: 1,
            quota_bytes: None,
            calibrated_total_bytes: None,
        })
        .is_err());
    }

    const D: u64 = 100 << 30;

    fn steady(from: i64, to: i64, bytes: u64) -> Vec<(i64, u64)> {
        (from..to).map(|day| (day * DAY, bytes)).collect()
    }

    fn gap(day: f64, reason: GapReason) -> TrafficGap {
        TrafficGap {
            occurred_at: (day * DAY as f64) as i64,
            reason,
        }
    }

    fn observed(days: &[RateDay]) -> Vec<Option<u64>> {
        days.iter()
            .map(|day| match day {
                RateDay::Observed(bytes) => Some(estimated_bytes(*bytes)),
                RateDay::Unobserved => None,
            })
            .collect()
    }

    fn anchors(start: i64, tracking_started_at: f64) -> PeriodAnchors {
        PeriodAnchors {
            start: start * DAY,
            end: (start + 30) * DAY,
            tracking_started_at: Some((tracking_started_at * DAY as f64) as i64),
            calibrated_at: None,
        }
    }

    #[test]
    fn the_daily_series_covers_complete_tracked_days_before_today() {
        let ledger = steady(10, 16, D);
        let midday = 10 * DAY + 3_600;
        assert_eq!(
            observed(&daily_series(&ledger, &[], midday, 15 * DAY)),
            vec![Some(D); 4],
            "the partial tracking day and today stay out"
        );
        assert_eq!(daily_series(&ledger, &[], 10 * DAY, 15 * DAY).len(), 5);
        assert!(daily_series(&ledger, &[], midday, 11 * DAY).is_empty());
    }

    #[test]
    fn an_outage_spreads_its_deferred_bytes_over_the_silent_days() {
        let mut ledger = steady(0, 5, D);
        ledger.extend([(7 * DAY, 3 * D), (8 * DAY, D)]);
        let outage = [gap(7.01, GapReason::ReportGap)];
        assert_eq!(
            observed(&daily_series(&ledger, &outage, 0, 9 * DAY)),
            vec![Some(D); 9]
        );
        // Resumed today: the bytes sit in today's partial day, so the silent days are unknown.
        let resumed = [gap(7.01, GapReason::ReportGap)];
        let mut days = vec![Some(D); 5];
        days.extend([None, None]);
        assert_eq!(
            observed(&daily_series(&steady(0, 5, D), &resumed, 0, 7 * DAY)),
            days
        );
    }

    #[test]
    fn only_a_gap_after_a_silent_day_hides_days_from_the_rate() {
        let mut ledger = steady(0, 5, D);
        ledger.extend([(6 * DAY, D / 2), (7 * DAY, D)]);
        let reboot = [gap(6.1, GapReason::MachineReboot)];
        assert_eq!(
            observed(&daily_series(&ledger, &reboot, 0, 8 * DAY)),
            vec![
                Some(D),
                Some(D),
                Some(D),
                Some(D),
                None,
                None,
                None,
                Some(D)
            ]
        );
        let reported_daily = steady(0, 8, D);
        assert_eq!(
            observed(&daily_series(&reported_daily, &reboot, 0, 8 * DAY)),
            vec![Some(D); 8]
        );
    }

    #[test]
    fn a_gap_changes_the_total_only_when_bytes_may_be_lost_or_misplaced() {
        let period_start = 30 * DAY;
        let changes = |ledger: &[(i64, u64)], gap: TrafficGap| {
            gap_changes_total(gap, ledger, 0, period_start)
        };
        let daily = steady(0, 40, D);
        assert!(!changes(&daily, gap(35.1, GapReason::MachineReboot)));
        assert!(!changes(&daily, gap(35.1, GapReason::MeterReplaced)));
        let silent = steady(0, 34, D);
        assert!(changes(&silent, gap(35.1, GapReason::MachineReboot)));
        assert!(changes(&silent, gap(35.1, GapReason::MeterReplaced)));
        // Absolute counters keep an outage's bytes; only crossing the reset misplaces them.
        assert!(!changes(&steady(0, 33, D), gap(33.1, GapReason::ReportGap)));
        assert!(changes(&steady(0, 30, D), gap(30.01, GapReason::ReportGap)));
        assert!(changes(&steady(0, 29, D), gap(32.5, GapReason::ReportGap)));
    }

    #[test]
    fn the_daily_rate_favours_recent_complete_days_inside_the_window() {
        let weights = rate_weights();
        let rate = |days: &[RateDay]| recent_rate(days, days.len(), &weights).unwrap();
        let steady_days = vec![RateDay::Observed(1_000.0); 40];
        assert!((rate(&steady_days).0 - 1_000.0).abs() < 1e-6);
        assert_eq!(rate(&steady_days).1, RATE_WINDOW_DAYS);

        let mut newest = steady_days.clone();
        newest[39] = RateDay::Observed(2_000.0);
        let mut older = steady_days.clone();
        older[19] = RateDay::Observed(2_000.0);
        assert!(rate(&newest).0 > rate(&older).0);
        assert!(rate(&older).0 > 1_000.0);

        let mut beyond = steady_days.clone();
        beyond[..12].fill(RateDay::Observed(9_000.0));
        assert!((rate(&beyond).0 - 1_000.0).abs() < 1e-6);

        let mut holes = steady_days;
        holes[35] = RateDay::Unobserved;
        holes[30] = RateDay::Unobserved;
        assert!((rate(&holes).0 - 1_000.0).abs() < 1e-6);
        assert_eq!(rate(&holes).1, RATE_WINDOW_DAYS - 2);
        assert_eq!(recent_rate(&[RateDay::Unobserved; 3], 3, &weights), None);
    }

    #[test]
    fn the_projection_meets_used_at_the_period_end() {
        assert_eq!(project(1_000, 100.0, 0.0, 0.3), (1_000, 1_000));
        assert_eq!(project(1_000, 100.0, 2.5, 0.2), (1_250, 1_300));
        assert_eq!(project(1_000, 100.0, 2.5, 0.0), (1_250, 1_250));
        assert_eq!(project(u64::MAX, 1e30, 30.0, 0.5), (u64::MAX, u64::MAX));
    }

    #[test]
    fn the_upper_error_takes_the_conformal_rank() {
        let samples = (0..20).map(|n| f64::from(n) / 100.0).collect::<Vec<_>>();
        // ⌈0.9 × 21⌉ = 19: the 19th smallest of 20.
        assert!((upper_error(&samples) - 0.18).abs() < 1e-12);
        assert_eq!(upper_error(&[-0.3, -0.2]), 0.0);
        assert_eq!(upper_error(&[]), 0.0);
    }

    #[test]
    fn the_band_prefers_own_history_then_the_fleet_then_the_default() {
        let weights = rate_weights();
        let machine =
            |days: usize| Backtest::new(&vec![RateDay::Observed(1_000.0); days], &weights);
        let long = machine(120);
        let short = machine(10);
        let empty = machine(0);

        let mut fleet = FleetBacktests::new(vec![&long, &short]);
        let (band, upper) = forecast_band(&long, &mut fleet, 2.4);
        assert_eq!(
            (band.basis, band.horizon_days, band.upper_error_basis_points),
            (MachineTrafficBandBasis::Node, 3, 0)
        );
        assert_eq!(upper, 0.0);
        let (band, _) = forecast_band(&short, &mut fleet, 2.4);
        assert_eq!(
            (band.basis, band.horizon_days),
            (MachineTrafficBandBasis::Fleet, 3)
        );

        // Thirty days of history cannot back a 20-day horizon, but 17 days is within reach.
        let month = (0..20).map(|_| machine(30)).collect::<Vec<_>>();
        let mut fleet = FleetBacktests::new(month.iter().collect());
        let (band, _) = forecast_band(&empty, &mut fleet, 20.0);
        assert_eq!(
            (band.basis, band.horizon_days),
            (MachineTrafficBandBasis::Fleet, 17)
        );

        let fortnight = (0..20).map(|_| machine(15)).collect::<Vec<_>>();
        let mut fleet = FleetBacktests::new(fortnight.iter().collect());
        let (band, upper) = forecast_band(&empty, &mut fleet, 20.0);
        assert_eq!(
            (band.basis, band.samples, band.upper_error_basis_points),
            (MachineTrafficBandBasis::Default, 0, 5_000)
        );
        assert_eq!(upper, BAND_DEFAULT_UPPER_ERROR);
    }

    #[test]
    fn confidence_needs_a_complete_total_recent_days_and_own_history() {
        use MachineTrafficBandBasis::{Default, Fleet, Node};
        use MachineTrafficConfidence::{High, Low, Medium};
        assert_eq!(forecast_confidence(false, 7, Node), High);
        assert_eq!(forecast_confidence(false, 28, Fleet), Medium);
        assert_eq!(forecast_confidence(false, 3, Node), Medium);
        assert_eq!(forecast_confidence(false, 2, Node), Low);
        assert_eq!(forecast_confidence(false, 28, Default), Low);
        assert_eq!(forecast_confidence(true, 28, Node), Low);
    }

    #[test]
    fn a_partly_tracked_previous_period_does_not_inflate_the_forecast() {
        // Tracking began halfway through the previous period. The old previous-shape estimate put
        // P50 at five times the real total here.
        let ledger = steady(20, 51, D);
        let now = 50 * DAY + DAY / 5;
        let forecast = prepare_forecast(anchors(30, 20.5), &ledger, &[], now, &rate_weights());
        assert_eq!(forecast.rate_days, RATE_WINDOW_DAYS);
        assert!(!forecast.needs_calibration);
        let used = 20 * D + D / 5;
        let (p50, _) = project(used, forecast.daily_rate, forecast.remaining_days, 0.0);
        assert_eq!(p50, 30 * D);
    }

    #[test]
    fn tracking_inside_the_period_keeps_its_rate_and_asks_for_calibration() {
        // The period average would spread two tracked days over twelve: P50 = 5 days' traffic.
        let ledger = vec![(10 * DAY, D / 2), (11 * DAY, D)];
        let forecast = prepare_forecast(anchors(0, 10.5), &ledger, &[], 12 * DAY, &rate_weights());
        assert_eq!((forecast.rate_days, forecast.daily_rate), (1, D as f64));
        assert!(forecast.needs_calibration);
        let (p50, _) = project(D / 2 + D, forecast.daily_rate, forecast.remaining_days, 0.0);
        assert_eq!(p50, D / 2 + 19 * D);

        // A calibration after tracking began makes the total whole again.
        let mut calibrated = anchors(0, 10.5);
        calibrated.calibrated_at = Some(11 * DAY);
        let forecast = prepare_forecast(calibrated, &ledger, &[], 12 * DAY, &rate_weights());
        assert!(!forecast.needs_calibration);
    }

    #[test]
    fn a_step_up_in_traffic_reaches_the_forecast_within_days() {
        // From day 15 a new heavy user doubles the traffic; the allowance is 40 days' worth.
        let mut ledger = steady(-30, 15, D);
        ledger.extend(steady(15, 20, 2 * D));
        let forecast = prepare_forecast(anchors(0, -30.0), &ledger, &[], 20 * DAY, &rate_weights());
        let used = 15 * D + 10 * D;
        let (p50, _) = project(used, forecast.daily_rate, forecast.remaining_days, 0.0);
        // The period average says 37.5 days' worth and would not warn until day 23.
        assert!(p50 > 40 * D, "{p50}");
    }

    #[test]
    fn the_first_day_of_a_period_uses_the_complete_days_before_it() {
        let ledger = steady(-30, 1, D);
        let now = DAY / 2;
        let forecast = prepare_forecast(anchors(0, -30.0), &ledger, &[], now, &rate_weights());
        let (p50, _) = project(D / 2, forecast.daily_rate, forecast.remaining_days, 0.0);
        assert_eq!(p50, 30 * D);
    }

    #[test]
    fn only_gaps_that_may_change_the_total_ask_for_calibration() {
        let weights = rate_weights();
        let daily = steady(-30, 10, D);
        let reboot = [gap(5.2, GapReason::MachineReboot)];
        let forecast = prepare_forecast(anchors(0, -30.0), &daily, &reboot, 10 * DAY, &weights);
        assert!(!forecast.needs_calibration);
        assert!(forecast.gap.is_none());

        let mut silent = steady(-30, 4, D);
        silent.extend(steady(5, 10, D));
        let forecast = prepare_forecast(anchors(0, -30.0), &silent, &reboot, 10 * DAY, &weights);
        assert!(forecast.needs_calibration);
        assert_eq!(
            forecast.gap.map(|gap| gap.reason),
            Some(GapReason::MachineReboot)
        );

        let mut calibrated = anchors(0, -30.0);
        calibrated.calibrated_at = Some(6 * DAY);
        let forecast = prepare_forecast(calibrated, &silent, &reboot, 10 * DAY, &weights);
        assert!(!forecast.needs_calibration);
    }

    #[test]
    fn ledger_window_is_half_open_and_keeps_exact_bytes() {
        let day = 86_400;
        let ledger = [
            DirectionalTrafficDay {
                day: 9 * day,
                rx_bytes: 1,
                tx_bytes: 0,
            },
            DirectionalTrafficDay {
                day: 10 * day,
                rx_bytes: 2,
                tx_bytes: 3,
            },
            DirectionalTrafficDay {
                day: 11 * day,
                rx_bytes: 5,
                tx_bytes: 7,
            },
            DirectionalTrafficDay {
                day: 12 * day,
                rx_bytes: 4,
                tx_bytes: 0,
            },
        ];
        assert_eq!(
            ledger_window(
                &ledger,
                NodeTrafficAccountingMode::BidirectionalSum,
                10 * day,
                12 * day,
            ),
            vec![
                MachineTrafficDay {
                    day_start_unix_secs: 10 * day,
                    bytes: "5".to_owned(),
                },
                MachineTrafficDay {
                    day_start_unix_secs: 11 * day,
                    bytes: "12".to_owned(),
                },
            ]
        );
        assert!(ledger_window(
            &ledger,
            NodeTrafficAccountingMode::BidirectionalSum,
            13 * day,
            14 * day,
        )
        .is_empty());
    }

    #[test]
    fn bidirectional_max_is_taken_after_period_directions_are_accumulated() {
        let ledger = [
            DirectionalTrafficDay {
                day: 0,
                rx_bytes: 100,
                tx_bytes: 0,
            },
            DirectionalTrafficDay {
                day: DAY,
                rx_bytes: 0,
                tx_bytes: 100,
            },
        ];
        let days = ledger_window(
            &ledger,
            NodeTrafficAccountingMode::BidirectionalMax,
            0,
            2 * DAY,
        );
        assert_eq!(
            days.iter()
                .map(|day| day.bytes.as_str())
                .collect::<Vec<_>>(),
            ["100", "0"]
        );
        let (rx, tx) = directional_window_totals(&ledger, 0, 2 * DAY);
        assert_eq!(
            NodeTrafficAccountingMode::BidirectionalMax.apply(rx, tx),
            100
        );
    }

    #[test]
    fn every_machine_accounting_mode_derives_from_the_same_raw_directions() {
        use NodeTrafficAccountingMode::{
            BidirectionalMax, BidirectionalSum, InboundOnly, OutboundOnly,
        };
        assert_eq!(BidirectionalSum.apply(120, 80), 200);
        assert_eq!(BidirectionalMax.apply(120, 80), 120);
        assert_eq!(InboundOnly.apply(120, 80), 120);
        assert_eq!(OutboundOnly.apply(120, 80), 80);
    }

    #[test]
    fn user_machine_footprint_doubles_combined_uplink_and_downlink() {
        // `logical` is already uplink + downlink. Forwarding crosses the NIC on both legs.
        assert_eq!(attributable_physical_bytes(1_000, 300, true), 600);
        assert_eq!(attributable_physical_bytes(500, 300, true), 500);
        assert_eq!(attributable_physical_bytes(1_000, 300, false), 0);
        assert_eq!(
            attributable_physical_bytes(u64::MAX, u64::MAX, true),
            u64::MAX
        );
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
