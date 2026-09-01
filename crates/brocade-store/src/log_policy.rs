//! Live, per-machine log retention policy.
//!
//! This is deliberately not part of `ModelSettings`: no generated artifact contains it and a
//! rollback must never restore an old disk-safety ceiling. The agent receives the resolved value
//! on every desired-state poll, including an otherwise idle 204 response.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::{AdminContext, Result, StoreError};

pub use brocade_deployment::protocol::{
    DEFAULT_AGENT_LOG_MAX_MIB, MAX_AGENT_LOG_MAX_MIB, MIN_AGENT_LOG_MAX_MIB,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogPolicyView {
    pub global_max_mib: u32,
    pub nodes: Vec<NodeLogPolicyItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLogPolicyItem {
    pub node_id: String,
    pub tenant_id: String,
    pub name: String,
    /// `None` means this machine follows the fleet default continuously.
    pub override_max_mib: Option<u32>,
    pub effective_max_mib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAgentLogDefaultRequest {
    pub max_mib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNodeLogPolicyRequest {
    /// `None` clears the override and resumes inheritance from the fleet default.
    pub max_mib: Option<u32>,
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
    let global =
        sqlx::query_scalar::<_, i32>("SELECT agent_log_max_mib FROM control_state WHERE id = TRUE")
            .fetch_one(pool)
            .await?;
    let global_max_mib = u32_column("control_state.agent_log_max_mib", global)?;
    let rows = sqlx::query(
        "SELECT id AS node_id, tenant_id, name, agent_log_max_mib
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
            let raw = row.try_get::<Option<i32>, _>("agent_log_max_mib")?;
            let override_max_mib = raw
                .map(|value| u32_column("nodes.agent_log_max_mib", value))
                .transpose()?;
            Ok(NodeLogPolicyItem {
                node_id: row.try_get("node_id")?,
                tenant_id: row.try_get("tenant_id")?,
                name: row.try_get("name")?,
                effective_max_mib: override_max_mib.unwrap_or(global_max_mib),
                override_max_mib,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(AgentLogPolicyView {
        global_max_mib,
        nodes,
    })
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
    let max_mib = valid(request.max_mib)?;
    sqlx::query("UPDATE control_state SET agent_log_max_mib = $1 WHERE id = TRUE")
        .bind(i32::try_from(max_mib).expect("validated log MiB fits i32"))
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
    let max_mib = request.max_mib.map(valid).transpose()?;
    let changed = sqlx::query("UPDATE nodes SET agent_log_max_mib = $2 WHERE id = $1")
        .bind(node_id)
        .bind(max_mib.map(|value| i32::try_from(value).expect("validated log MiB fits i32")))
        .execute(pool)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("node {node_id}")));
    }
    Ok(())
}

/// Resolve inheritance for one authenticated node. Read fresh on every poll so neither a global
/// edit nor clearing a machine override needs a release or a control-plane restart.
pub async fn effective_node_log_max_mib(pool: &PgPool, node_id: &str) -> Result<u32> {
    let value = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(node.agent_log_max_mib, control.agent_log_max_mib)
           FROM nodes AS node
          CROSS JOIN control_state AS control
          WHERE node.id = $1 AND control.id = TRUE",
    )
    .bind(node_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| StoreError::NotFound(format!("node {node_id}")))?;
    u32_column("effective agent log max", value)
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
