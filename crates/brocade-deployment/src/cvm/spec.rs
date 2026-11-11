use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub id: String,
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub sha256: String,
    pub boot_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub vcpus: u32,
    pub memory_bytes: u64,
    pub root_disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub min_vcpus: u32,
    pub max_vcpus: u32,
    pub min_memory_bytes: u64,
    pub max_memory_bytes: u64,
    pub memory_step_bytes: u64,
    pub min_root_disk_bytes: u64,
    pub max_root_disk_bytes: u64,
    pub root_disk_step_bytes: u64,
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_vcpus == 0
            || self.max_vcpus < self.min_vcpus
            || self.max_vcpus > 1024
            || self.min_memory_bytes < 32 * 1024 * 1024
            || self.max_memory_bytes < self.min_memory_bytes
            || self.max_memory_bytes > i64::MAX as u64
            || self.memory_step_bytes == 0
            || !self.memory_step_bytes.is_multiple_of(1024 * 1024)
            || !self.min_memory_bytes.is_multiple_of(self.memory_step_bytes)
            || self.min_root_disk_bytes == 0
            || self.max_root_disk_bytes < self.min_root_disk_bytes
            || self.max_root_disk_bytes > i64::MAX as u64
            || self.root_disk_step_bytes == 0
            || !self
                .min_root_disk_bytes
                .is_multiple_of(self.root_disk_step_bytes)
        {
            return Err("CVM independent resource limits are invalid".into());
        }
        Ok(())
    }

    pub fn validate_resources(&self, resources: &Resources) -> Result<(), String> {
        self.validate()?;
        if !(self.min_vcpus..=self.max_vcpus).contains(&resources.vcpus)
            || !(self.min_memory_bytes..=self.max_memory_bytes).contains(&resources.memory_bytes)
            || !resources
                .memory_bytes
                .is_multiple_of(self.memory_step_bytes)
            || !(self.min_root_disk_bytes..=self.max_root_disk_bytes)
                .contains(&resources.root_disk_bytes)
            || !resources
                .root_disk_bytes
                .is_multiple_of(self.root_disk_step_bytes)
        {
            return Err("CPU、内存或系统盘超出机器支持范围".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkKind {
    Public,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSource {
    pub id: String,
    pub name: String,
    pub kind: NetworkKind,
    pub description: String,
    /// Per-instance limit for a source such as Cube's built-in contact device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attachments: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_ipv6: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfacePurpose {
    Contact,
    Business,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceRequest {
    pub source_id: String,
    pub purpose: InterfacePurpose,
    #[serde(default)]
    pub default_ipv4: bool,
    #[serde(default)]
    pub default_ipv6: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated: Option<AllocatedNic>,
    #[serde(flatten)]
    pub connection: InterfaceRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSpec {
    #[serde(default)]
    pub routing: ContainerRoutes,
    pub name: String,
    pub image: Image,
    pub resources: Resources,
    pub ssh_public_key: String,
    pub interfaces: Vec<InterfaceSpec>,
}
