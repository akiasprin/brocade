//! Live, per-machine log retention policy.
//!
//! This is deliberately not part of `ModelSettings`: no generated artifact contains it and a
//! rollback must never restore an old disk-safety ceiling. The three workload classes have
//! separate bounds: Agent's journal namespace, Xray's rotated pair, and every Phantun instance's
//! rotated pair. The agent receives the resolved values on every desired-state poll, including an
//! otherwise idle 204 response.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

pub use brocade_deployment::protocol::{
    DEFAULT_AGENT_LOG_MAX_MIB, MAX_AGENT_LOG_MAX_MIB, MIN_AGENT_LOG_MAX_MIB,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogPolicyView {
    pub global: AgentLogLimits,
    pub nodes: Vec<NodeLogPolicyItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogLimits {
    pub agent_journal_mib: u32,
    pub xray_mib: u32,
    pub phantun_mib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogLimitOverrides {
    /// `None` means this class follows the corresponding fleet default continuously.
    pub agent_journal_mib: Option<u32>,
    pub xray_mib: Option<u32>,
    pub phantun_mib: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLogPolicyItem {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    pub overrides: AgentLogLimitOverrides,
    pub effective: AgentLogLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAgentLogDefaultRequest {
    pub agent_journal_mib: u32,
    pub xray_mib: u32,
    pub phantun_mib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNodeLogPolicyRequest {
    /// `None` clears that class's override and resumes inheritance from the fleet default.
    pub agent_journal_mib: Option<u32>,
    pub xray_mib: Option<u32>,
    pub phantun_mib: Option<u32>,
}

fn valid(value: u32) -> Result<u32> {
    if !(MIN_AGENT_LOG_MAX_MIB..=MAX_AGENT_LOG_MAX_MIB).contains(&value) {
        return Err(StoreError::InvalidData(format!(
            "日志上限需在 {MIN_AGENT_LOG_MAX_MIB}–{MAX_AGENT_LOG_MAX_MIB} MiB 之间"
        )));
    }
    Ok(value)
}

fn u32_column(owner: &str, value: i32) -> Result<u32> {
    let value =
        u32::try_from(value).map_err(|_| StoreError::InvalidData(format!("{owner} 是负数")))?;
    valid(value)
}

pub async fn load_agent_log_policy(
    pool: &PgPool,
    actor: &AdminContext,
) -> Result<AgentLogPolicyView> {
    let global_row = sqlx::query(
        "SELECT agent_log_max_mib, xray_log_max_mib, phantun_log_max_mib
           FROM control_state
          WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    let global = AgentLogLimits {
        agent_journal_mib: u32_column(
            "control_state.agent_log_max_mib",
            global_row.try_get("agent_log_max_mib")?,
        )?,
        xray_mib: u32_column(
            "control_state.xray_log_max_mib",
            global_row.try_get("xray_log_max_mib")?,
        )?,
        phantun_mib: u32_column(
            "control_state.phantun_log_max_mib",
            global_row.try_get("phantun_log_max_mib")?,
        )?,
    };
    let rows = sqlx::query(
        "SELECT id AS node_id, tenant_id, name,
                agent_log_max_mib, xray_log_max_mib, phantun_log_max_mib
           FROM nodes
          WHERE ($1::text IS NULL OR tenant_id = $1 OR tenant_id LIKE $2 ESCAPE '\\')
          ORDER BY name, id",
    )
    .bind(actor.tenant_scope())
    .bind(actor.tenant_scope_like_pattern())
    .fetch_all(pool)
    .await?;

    let nodes = rows
        .into_iter()
        .map(|row| {
            let overrides = AgentLogLimitOverrides {
                agent_journal_mib: row
                    .try_get::<Option<i32>, _>("agent_log_max_mib")?
                    .map(|value| u32_column("nodes.agent_log_max_mib", value))
                    .transpose()?,
                xray_mib: row
                    .try_get::<Option<i32>, _>("xray_log_max_mib")?
                    .map(|value| u32_column("nodes.xray_log_max_mib", value))
                    .transpose()?,
                phantun_mib: row
                    .try_get::<Option<i32>, _>("phantun_log_max_mib")?
                    .map(|value| u32_column("nodes.phantun_log_max_mib", value))
                    .transpose()?,
            };
            Ok(NodeLogPolicyItem {
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                name: row.try_get("name")?,
                effective: AgentLogLimits {
                    agent_journal_mib: overrides
                        .agent_journal_mib
                        .unwrap_or(global.agent_journal_mib),
                    xray_mib: overrides.xray_mib.unwrap_or(global.xray_mib),
                    phantun_mib: overrides.phantun_mib.unwrap_or(global.phantun_mib),
                },
                overrides,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(AgentLogPolicyView { global, nodes })
}

pub async fn update_agent_log_default(
    pool: &PgPool,
    actor: &AdminContext,
    request: UpdateAgentLogDefaultRequest,
) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update agent log policy".to_owned(),
        ));
    }
    let limits = AgentLogLimits {
        agent_journal_mib: valid(request.agent_journal_mib)?,
        xray_mib: valid(request.xray_mib)?,
        phantun_mib: valid(request.phantun_mib)?,
    };
    sqlx::query(
        "UPDATE control_state
            SET agent_log_max_mib = $1,
                xray_log_max_mib = $2,
                phantun_log_max_mib = $3
          WHERE id = TRUE",
    )
    .bind(i32::try_from(limits.agent_journal_mib).expect("validated log MiB fits i32"))
    .bind(i32::try_from(limits.xray_mib).expect("validated log MiB fits i32"))
    .bind(i32::try_from(limits.phantun_mib).expect("validated log MiB fits i32"))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_node_log_policy(
    pool: &PgPool,
    actor: &AdminContext,
    node_id: &str,
    request: UpdateNodeLogPolicyRequest,
) -> Result<()> {
    if !actor.is_system_admin() {
        return Err(StoreError::Forbidden(
            "only system-admin can update node log policy".to_owned(),
        ));
    }
    let overrides = AgentLogLimitOverrides {
        agent_journal_mib: request.agent_journal_mib.map(valid).transpose()?,
        xray_mib: request.xray_mib.map(valid).transpose()?,
        phantun_mib: request.phantun_mib.map(valid).transpose()?,
    };
    let changed = sqlx::query(
        "UPDATE nodes
            SET agent_log_max_mib = $2,
                xray_log_max_mib = $3,
                phantun_log_max_mib = $4
          WHERE id = $1",
    )
    .bind(node_id)
    .bind(
        overrides
            .agent_journal_mib
            .map(|value| i32::try_from(value).expect("validated log MiB fits i32")),
    )
    .bind(
        overrides
            .xray_mib
            .map(|value| i32::try_from(value).expect("validated log MiB fits i32")),
    )
    .bind(
        overrides
            .phantun_mib
            .map(|value| i32::try_from(value).expect("validated log MiB fits i32")),
    )
    .execute(pool)
    .await?;
    if changed.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }
    Ok(())
}

/// Resolve inheritance for one authenticated node. Read fresh on every poll so neither a global
/// edit nor clearing a machine override needs a release or a control-plane restart.
pub async fn effective_node_log_limits(pool: &PgPool, node_id: &str) -> Result<AgentLogLimits> {
    let row = sqlx::query(
        "SELECT COALESCE(node.agent_log_max_mib, control.agent_log_max_mib) AS agent_journal_mib,
                COALESCE(node.xray_log_max_mib, control.xray_log_max_mib) AS xray_mib,
                COALESCE(node.phantun_log_max_mib, control.phantun_log_max_mib) AS phantun_mib
           FROM nodes AS node
          CROSS JOIN control_state AS control
          WHERE node.id = $1 AND control.id = TRUE",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    Ok(AgentLogLimits {
        agent_journal_mib: u32_column(
            "effective agent journal max",
            row.try_get("agent_journal_mib")?,
        )?,
        xray_mib: u32_column("effective xray log max", row.try_get("xray_mib")?)?,
        phantun_mib: u32_column("effective phantun log max", row.try_get("phantun_mib")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_inclusive() {
        assert_eq!(valid(MIN_AGENT_LOG_MAX_MIB).unwrap(), 16);
        assert_eq!(valid(MAX_AGENT_LOG_MAX_MIB).unwrap(), 4096);
        assert!(valid(MIN_AGENT_LOG_MAX_MIB - 1).is_err());
        assert!(valid(MAX_AGENT_LOG_MAX_MIB + 1).is_err());
    }
}
