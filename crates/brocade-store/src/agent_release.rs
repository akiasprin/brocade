//! Read-only baseline from the pre-ledger Agent approval.
//! It is never consulted to dispatch updates. New work is owned by binary_release.
//! Preserve the original scope and timestamps, including deleted machine identities.
use crate::Result;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentReleaseScope {
    Off,
    Nodes,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRelease {
    pub release_id: Option<String>,
    pub scope: AgentReleaseScope,
    pub nodes: Vec<String>,
    pub note: Option<String>,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub released_at: Option<String>,
    pub released_by: Option<String>,
}

pub async fn load_agent_release(pool: &PgPool) -> Result<AgentRelease> {
    let row = sqlx::query(
        "SELECT agent_release_id, agent_release_scope, agent_release_nodes,
                agent_release_version, agent_release_commit, agent_release_note,
                agent_released_at::text AS agent_released_at, agent_released_by
         FROM control_state WHERE id = TRUE",
    )
    .fetch_one(pool)
    .await?;
    Ok(AgentRelease {
        release_id: row.try_get("agent_release_id")?,
        scope: serde_json::from_value(serde_json::Value::String(
            row.try_get("agent_release_scope")?,
        ))?,
        nodes: serde_json::from_value(row.try_get("agent_release_nodes")?)?,
        note: row.try_get("agent_release_note")?,
        version: row.try_get("agent_release_version")?,
        commit: row.try_get("agent_release_commit")?,
        released_at: row.try_get("agent_released_at")?,
        released_by: row.try_get("agent_released_by")?,
    })
}
