use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmHost {
    pub host_revision: u64,
    pub can_manage_host: bool,
    pub node_id: String,
    pub name: String,
    pub fresh: bool,
    pub observed_at: String,
    pub capabilities: Capabilities,
    pub terminal_enabled: bool,
    pub terminal_revision: u64,
    pub can_manage_terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_sessions: Option<CvmTerminalSessions>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmTerminalSessions {
    pub used: u64,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmInstance {
    pub revision_id: Option<u64>,
    pub published_revision_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_deployment_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<String>,
    pub tenant_id: String,
    pub user_id: Option<String>,
    pub node_id: String,
    pub desired: InstanceDesired,
    pub status: ApplyStatus,
    pub report: Option<InstanceReport>,
    pub updated_at: String,
    pub created_at: String,
    pub reserved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmOwner {
    pub tenant_id: String,
    pub user_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmWorkspace {
    pub usage: Vec<CvmResourceUsage>,
    pub tenants: Vec<CvmTenant>,
    #[serde(default)]
    pub can_publish: bool,
    pub pending_revision_id: Option<u64>,
    /// Removed identities offered only to administrators for explicit re-enrollment.
    pub available_hosts: Vec<CvmHost>,
    #[serde(default)]
    pub can_manage_network: bool,
    pub hosts: Vec<CvmHost>,
    pub instances: Vec<CvmInstance>,
    pub owners: Vec<CvmOwner>,
    pub can_use: bool,
    pub can_write: bool,
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmCreate {
    pub request_id: String,
    pub tenant_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    pub node_id: String,
    pub name: String,
    pub image_id: String,
    pub resources: Resources,
    pub interfaces: Vec<InterfaceRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CvmChange {
    ReleaseHost,
    Assign {
        user_id: Option<String>,
    },
    ResetPassword,
    Start,
    Stop,
    Restart,
    Reinstall {
        image_id: String,
    },
    Configure {
        name: String,
        vcpus: u32,
        memory_bytes: u64,
    },
    Delete {
        #[serde(default)]
        delete_data: bool,
    },
    Detach {
        interface_id: String,
    },
    Retry,
    UndoPending {
        expected_revision: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmCommand {
    pub request_id: String,
    pub expected_generation: u64,
    #[serde(flatten)]
    pub change: CvmChange,
}

impl CvmInstance {
    pub(super) fn user(&self) -> &str {
        self.user_id.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmTenant {
    pub tenant_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmResourceUsage {
    pub tenant_id: String,
    pub user_id: String,
    pub instances: u64,
    pub vcpus: u64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    pub interfaces: u64,
}
