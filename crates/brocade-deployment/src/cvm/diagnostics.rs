use super::*;

/// Recent control-plane execution and transport process output. Packet payloads and
/// credentials are never log data. One node shares a bounded window across its tunnels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkLogSource {
    Agent,
    Xray,
    Phantun,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkLogLevel {
    Info,
    Warning,
    Error,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkLogEvent {
    pub at: u64,
    pub resource_id: Option<String>,
    pub source: NetworkLogSource,
    pub level: NetworkLogLevel,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkResourcePhase {
    Preparing,
    Waiting,
    Connected,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkResourceProgress {
    pub id: String,
    pub phase: NetworkResourcePhase,
    pub since: u64,
    pub waiting_seconds: u64,
    pub message: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDiagnostics {
    pub collected_at: u64,
    pub dropped_events: u64,
    pub events: Vec<NetworkLogEvent>,
    pub resources: Vec<NetworkResourceProgress>,
}
impl NetworkDiagnostics {
    pub fn validate(&self) -> Result<(), String> {
        if self.events.len() > 256
            || self.resources.len() > 1024
            || self.events.iter().any(|e| {
                e.message.len() > 1024 || e.resource_id.as_ref().is_some_and(|id| !valid_id(id))
            })
            || self
                .resources
                .iter()
                .any(|t| !valid_id(&t.id) || t.message.len() > 500)
        {
            return Err("invalid CVM network diagnostics".into());
        }
        Ok(())
    }
}
