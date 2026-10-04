//! Live fleet-wide physical-NIC tuning.
//!
//! This is operational state rather than `ModelSettings`: it changes no generated Xray artifact,
//! creates no revision, and is reconciled by every Agent from response headers on its next poll.

use brocade_deployment::protocol::{
    HostNetworkTuning, MAX_NIC_GRO_FLUSH_TIMEOUT_NS, MAX_NIC_NAPI_DEFER_HARD_IRQS,
};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

fn validate(settings: HostNetworkTuning) -> Result<HostNetworkTuning> {
    if settings.gro_flush_timeout_ns > MAX_NIC_GRO_FLUSH_TIMEOUT_NS {
        return Err(StoreError::InvalidData(format!(
            "GRO flush timeout 需在 0–{MAX_NIC_GRO_FLUSH_TIMEOUT_NS} ns 之间"
        )));
    }
    if settings.napi_defer_hard_irqs > MAX_NIC_NAPI_DEFER_HARD_IRQS {
        return Err(StoreError::InvalidData(format!(
            "NAPI defer hard IRQs 需在 0–{MAX_NIC_NAPI_DEFER_HARD_IRQS} 之间"
        )));
    }
    Ok(settings)
}

fn u32_column(name: &str, value: i32) -> Result<u32> {
    u32::try_from(value).map_err(|_| StoreError::InvalidData(format!("{name} 是负数")))
}

pub async fn load(pool: &PgPool) -> Result<HostNetworkTuning> {
    let row = sqlx::query(
        "SELECT nic_gro_flush_timeout_ns, nic_napi_defer_hard_irqs
           FROM control_state
          WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    validate(HostNetworkTuning {
        gro_flush_timeout_ns: u32_column(
            "control_state.nic_gro_flush_timeout_ns",
            row.try_get("nic_gro_flush_timeout_ns")?,
        )?,
        napi_defer_hard_irqs: u32_column(
            "control_state.nic_napi_defer_hard_irqs",
            row.try_get("nic_napi_defer_hard_irqs")?,
        )?,
    })
}

pub async fn update(
    pool: &PgPool,
    actor: &AdminContext,
    settings: HostNetworkTuning,
) -> Result<HostNetworkTuning> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update host network tuning".to_owned(),
        ));
    }
    let settings = validate(settings)?;
    sqlx::query(
        "UPDATE control_state
            SET nic_gro_flush_timeout_ns = $1,
                nic_napi_defer_hard_irqs = $2
          WHERE id = TRUE",
    )
    .bind(i32::try_from(settings.gro_flush_timeout_ns).expect("validated timeout fits i32"))
    .bind(i32::try_from(settings.napi_defer_hard_irqs).expect("validated count fits i32"))
    .execute(pool)
    .await?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_disables_each_knob_and_bounds_reject_unsafe_values() {
        assert_eq!(
            validate(HostNetworkTuning {
                gro_flush_timeout_ns: 0,
                napi_defer_hard_irqs: 0,
            })
            .unwrap(),
            HostNetworkTuning {
                gro_flush_timeout_ns: 0,
                napi_defer_hard_irqs: 0,
            }
        );
        assert!(validate(HostNetworkTuning {
            gro_flush_timeout_ns: MAX_NIC_GRO_FLUSH_TIMEOUT_NS + 1,
            napi_defer_hard_irqs: 0,
        })
        .is_err());
        assert!(validate(HostNetworkTuning {
            gro_flush_timeout_ns: 0,
            napi_defer_hard_irqs: MAX_NIC_NAPI_DEFER_HARD_IRQS + 1,
        })
        .is_err());
    }
}
