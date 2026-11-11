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
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmInstance {
    pub tenant_id: String,
    pub user_id: String,
    pub node_id: String,
    pub desired: InstanceDesired,
    pub status: ApplyStatus,
    pub report: Option<InstanceReport>,
    pub updated_at: String,
    pub created_at: String,
    pub reserved: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CvmQuota {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_network_quotas: Vec<CvmHostNetworkQuotaChange>,
    pub tenant_id: String,
    pub user_id: String,
    pub enabled: bool,
    pub allowed_nodes: Vec<String>,
    pub max_instances: u32,
    pub max_vcpus: u64,
    pub max_memory_bytes: u64,
    pub max_disk_bytes: u64,
    pub max_interfaces: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmOwner {
    pub tenant_id: String,
    pub user_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmQuotaUsage {
    pub tenant_id: String,
    pub user_id: String,
    pub instances: u64,
    pub vcpus: u64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    pub interfaces: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmWorkspace {
    /// Removed identities offered only to administrators for explicit re-enrollment.
    pub available_hosts: Vec<CvmHost>,
    pub network_quotas: Vec<CvmHostNetworkQuota>,
    #[serde(default)]
    pub usage: Vec<CvmQuotaUsage>,
    #[serde(default)]
    pub can_manage_network: bool,
    pub hosts: Vec<CvmHost>,
    pub instances: Vec<CvmInstance>,
    pub quotas: Vec<CvmQuota>,
    pub owners: Vec<CvmOwner>,
    pub can_manage_quotas: bool,
    pub can_write: bool,
    pub redacted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmCreate {
    pub request_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub node_id: String,
    pub name: String,
    pub image_id: String,
    pub resources: Resources,
    pub ssh_public_key: String,
    pub interfaces: Vec<InterfaceRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CvmChange {
    Start,
    Stop,
    Restart,
    Reinstall {
        image_id: String,
        ssh_public_key: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmCommand {
    pub request_id: String,
    pub expected_generation: u64,
    #[serde(flatten)]
    pub change: CvmChange,
}
