//! Fleet-wide policy for the ephemeral live-traffic channel.
//!
//! This row is durable because the operator's choice must survive a console restart. Samples are
//! deliberately not: they live in `brocade-console::realtime` and disappear when that process
//! exits. Keeping these two concerns apart prevents a faster UI refresh from changing either the
//! 30-second diagnostic series or the cumulative usage ledger.

use brocade_deployment::protocol::{
    RealtimeTelemetryPolicy, UpdateRealtimeTelemetryPolicyRequest, REALTIME_INTERVAL_OPTIONS,
};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

pub fn validate_policy(policy: RealtimeTelemetryPolicy) -> Result<RealtimeTelemetryPolicy> {
    if !REALTIME_INTERVAL_OPTIONS.contains(&policy.interval_secs) {
        return Err(StoreError::InvalidData(format!(
            "实时遥测间隔只支持 1、2 或 5 秒，收到 {} 秒",
            policy.interval_secs
        )));
    }
    Ok(policy)
}

pub async fn load_policy(pool: &PgPool) -> Result<RealtimeTelemetryPolicy> {
    let row = sqlx::query(
        "SELECT realtime_enabled, realtime_interval_secs
           FROM control_state
          WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let interval_secs = u32::try_from(row.try_get::<i32, _>("realtime_interval_secs")?)
        .map_err(|_| StoreError::InvalidData("实时遥测间隔不能是负数".to_owned()))?;
    validate_policy(RealtimeTelemetryPolicy {
        enabled: row.try_get("realtime_enabled")?,
        interval_secs,
    })
}

pub async fn update_policy(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateRealtimeTelemetryPolicyRequest,
) -> Result<RealtimeTelemetryPolicy> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update realtime telemetry policy".to_owned(),
        ));
    }
    let policy = validate_policy(RealtimeTelemetryPolicy {
        enabled: request.enabled,
        interval_secs: request.interval_secs,
    })?;
    sqlx::query(
        "UPDATE control_state
            SET realtime_enabled = $1,
                realtime_interval_secs = $2
          WHERE id = TRUE",
    )
    .bind(policy.enabled)
    .bind(i32::try_from(policy.interval_secs).expect("validated realtime interval fits i32"))
    .execute(pool)
    .await?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_operator_choices_are_valid() {
        for interval_secs in [1, 2, 5] {
            assert!(validate_policy(RealtimeTelemetryPolicy {
                enabled: true,
                interval_secs,
            })
            .is_ok());
        }
        for interval_secs in [0, 3, 6, 30] {
            assert!(validate_policy(RealtimeTelemetryPolicy {
                enabled: true,
                interval_secs,
            })
            .is_err());
        }
    }
}
