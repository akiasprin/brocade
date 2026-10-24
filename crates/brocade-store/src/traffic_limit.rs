use crate::{AdminContext, Result, StoreError};
use brocade_deployment::traffic_limit::{
    decimal_bytes, NodeTrafficAccountingMode, NodeTrafficCycleKind, TrafficLimitAnchor,
    TrafficLimitDesired, TrafficLimitObservation, TrafficLimitPolicy, MAX_TRAFFIC_RATE_BPS,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTrafficLimitView {
    pub revision: u64,
    pub policy: TrafficLimitPolicy,
    pub supported: bool,
    pub observation: Option<TrafficLimitObservation>,
    pub reported_at_unix_secs: Option<i64>,
}

pub(crate) fn view_from_row(row: &sqlx::postgres::PgRow) -> Result<NodeTrafficLimitView> {
    Ok(NodeTrafficLimitView {
        revision: u64::try_from(row.try_get::<i64, _>("limit_revision")?)
            .map_err(|_| StoreError::InvalidData("invalid traffic limit revision".into()))?,
        policy: serde_json::from_value(row.try_get("limit_policy")?)?,
        supported: row.try_get("limit_supported")?,
        observation: row
            .try_get::<Option<serde_json::Value>, _>("limit_observation")?
            .map(serde_json::from_value)
            .transpose()?,
        reported_at_unix_secs: row.try_get("limit_reported_at")?,
    })
}

pub(crate) async fn policy_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<TrafficLimitPolicy> {
    sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT policy FROM node_traffic_limit WHERE node_id = $1",
    )
    .bind(node)
    .fetch_optional(&mut **tx)
    .await?
    .map(serde_json::from_value)
    .transpose()
    .map(|p| p.unwrap_or_default())
    .map_err(Into::into)
}

pub(crate) async fn update_policy(
    pool: &PgPool,
    actor: &AdminContext,
    node: &str,
    policy: TrafficLimitPolicy,
) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update machine limits".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    if sqlx::query_scalar::<_, String>("SELECT id FROM nodes WHERE id = $1 FOR UPDATE")
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
    {
        return Err(StoreError::NotFound(format!("node {node}")));
    }
    let quota = sqlx::query_scalar::<_, Option<String>>(
        "SELECT quota_bytes::text FROM node_traffic_policy WHERE node_id = $1",
    )
    .bind(node)
    .fetch_optional(&mut *tx)
    .await?
    .flatten()
    .map(|v| decimal_bytes(&v).map_err(StoreError::InvalidData))
    .transpose()?;
    policy.validate(quota).map_err(StoreError::InvalidData)?;
    if policy_in_transaction(&mut tx, node).await? != policy {
        let json = serde_json::to_value(&policy)?;
        sqlx::query(
            "INSERT INTO node_traffic_limit (node_id, revision, policy, updated_by)
            VALUES ($1, 1, $2, $3) ON CONFLICT (node_id) DO UPDATE SET
            revision = node_traffic_limit.revision + 1, policy = EXCLUDED.policy,
            updated_by = EXCLUDED.updated_by, updated_at = now()",
        )
        .bind(node)
        .bind(&json)
        .bind(actor.operator_id())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO node_traffic_limit_events (node_id, policy, actor) VALUES ($1, $2, $3)",
        )
        .bind(node)
        .bind(json)
        .bind(actor.operator_id())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn desired(pool: &PgPool, node: &str) -> Result<TrafficLimitDesired> {
    let mut tx = pool.begin().await?;
    // A sample and its directional ledger amounts must describe the same accepted sequence.
    sqlx::query("SELECT node_id FROM node_traffic_state WHERE node_id = $1 FOR UPDATE")
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
    let row = sqlx::query(crate::traffic::NODE_TRAFFIC_VIEW_SQL)
        .bind(None::<&str>)
        .bind(None::<&str>)
        .bind(Some(node))
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("node {node}")))?;
    let bytes = |name: &str| -> Result<u64> {
        decimal_bytes(&row.try_get::<String, _>(name)?).map_err(StoreError::InvalidData)
    };
    let anchor = if let Some(meter_id) = row.try_get::<Option<String>, _>("meter_id")? {
        let legacy: bool = row.try_get("limit_legacy_calibration")?;
        Some(TrafficLimitAnchor {
            meter_id,
            sequence: bytes("meter_sequence")?,
            sampled_at_unix_secs: row.try_get("last_reported_at_unix_secs")?,
            period_start_unix_secs: row.try_get("period_start_unix_secs")?,
            meter_rx_bytes: bytes("meter_rx_bytes")?,
            meter_tx_bytes: bytes("meter_tx_bytes")?,
            rx_bytes: if legacy { 0 } else { bytes("limit_rx_bytes")? },
            tx_bytes: if legacy { 0 } else { bytes("limit_tx_bytes")? },
            calibrated_total_bytes: bytes(if legacy {
                "total_bytes"
            } else {
                "limit_calibrated_total"
            })?,
        })
    } else {
        None
    };
    let view = view_from_row(&row)?;
    let cycle: String = row.try_get("cycle_kind")?;
    let desired = TrafficLimitDesired {
        revision: view.revision,
        policy: view.policy,
        quota_bytes: row
            .try_get::<Option<String>, _>("quota_bytes")?
            .map(|v| decimal_bytes(&v).map_err(StoreError::InvalidData))
            .transpose()?,
        cycle_kind: match cycle.as_str() {
            "monthly" => NodeTrafficCycleKind::Monthly,
            "yearly" => NodeTrafficCycleKind::Yearly,
            _ => return Err(StoreError::InvalidData("invalid traffic cycle".into())),
        },
        accounting_mode: NodeTrafficAccountingMode::parse(
            &row.try_get::<String, _>("accounting_mode")?,
        )
        .map_err(StoreError::InvalidData)?,
        reset_month: row
            .try_get::<Option<i32>, _>("reset_month")?
            .map(u8::try_from)
            .transpose()
            .map_err(|_| StoreError::InvalidData("invalid reset month".into()))?,
        reset_day: u8::try_from(row.try_get::<i32, _>("reset_day")?)
            .map_err(|_| StoreError::InvalidData("invalid reset day".into()))?,
        anchor,
    };
    desired.validate().map_err(StoreError::InvalidData)?;
    tx.commit().await?;
    Ok(desired)
}

pub(crate) async fn report(
    pool: &PgPool,
    node: &str,
    observation: TrafficLimitObservation,
) -> Result<()> {
    let invalid_layer = [&observation.system, &observation.application]
        .into_iter()
        .flatten()
        .any(|layer| {
            layer.error.as_ref().is_some_and(|s| s.len() > 2048)
                || layer
                    .rate_bps
                    .is_some_and(|r| !(1_000..=MAX_TRAFFIC_RATE_BPS).contains(&r))
        });
    if invalid_layer
        || observation.error.as_ref().is_some_and(|s| s.len() > 2048)
        || observation
            .interface
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 64 || s.contains('/'))
        || observation
            .rate_bps
            .is_some_and(|r| !(1_000..=MAX_TRAFFIC_RATE_BPS).contains(&r))
        || observation
            .used_bytes
            .as_ref()
            .is_some_and(|s| decimal_bytes(s).is_err())
    {
        return Err(StoreError::InvalidData(
            "invalid traffic limit observation".into(),
        ));
    }
    let revision = i64::try_from(observation.revision)
        .map_err(|_| StoreError::InvalidData("invalid limit revision".into()))?;
    sqlx::query("INSERT INTO node_traffic_limit (node_id, observation, reported_at)
        SELECT $1, $2, now() WHERE $3 = 0 OR EXISTS (
            SELECT 1 FROM node_traffic_limit WHERE node_id = $1 AND revision = $3
        ) ON CONFLICT (node_id) DO UPDATE SET observation = EXCLUDED.observation, reported_at = now()
        WHERE node_traffic_limit.revision = $3")
        .bind(node).bind(serde_json::to_value(&observation)?).bind(revision).execute(pool).await?;
    Ok(())
}
