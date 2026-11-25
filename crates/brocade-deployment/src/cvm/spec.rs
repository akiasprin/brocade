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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "InstanceSpecFields")]
pub struct InstanceSpec {
    pub execution: ExecutionTarget,
    pub routing: ContainerRoutes,
    pub name: String,
    pub image: Option<Image>,
    pub resources: Resources,
    /// Reference to a separately delivered secret; Cube requires a nonzero revision.
    pub login_revision: u64,
    /// Retained only when reading the published key-based format. Never create new key-based specs.
    pub legacy_ssh_public_key: Option<String>,
    pub interfaces: Vec<InterfaceSpec>,
}

#[derive(Deserialize)]
struct InstanceSpecFields {
    execution: Option<ExecutionTarget>,
    routing: Option<ContainerRoutes>,
    name: String,
    image: Option<Image>,
    resources: Resources,
    #[serde(default, deserialize_with = "present_login_revision")]
    login_revision: Option<u64>,
    ssh_public_key: Option<String>,
    interfaces: Vec<InterfaceSpec>,
}

fn present_login_revision<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

impl TryFrom<InstanceSpecFields> for InstanceSpec {
    type Error = String;

    fn try_from(fields: InstanceSpecFields) -> Result<Self, Self::Error> {
        let legacy = fields.login_revision.is_none() && fields.ssh_public_key.is_some();
        let execution = fields
            .execution
            .or_else(|| legacy.then_some(ExecutionTarget::Cube))
            .ok_or("missing field `execution`")?;
        let routing = fields
            .routing
            .or_else(|| legacy.then(ContainerRoutes::default))
            .ok_or("missing field `routing`")?;
        let login_revision = fields
            .login_revision
            .or_else(|| legacy.then_some(0))
            .ok_or("missing field `login_revision`")?;
        Ok(Self {
            execution,
            routing,
            name: fields.name,
            image: fields.image,
            resources: fields.resources,
            login_revision,
            legacy_ssh_public_key: if legacy { fields.ssh_public_key } else { None },
            interfaces: fields.interfaces,
        })
    }
}

impl Serialize for InstanceSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let legacy_key = self
            .legacy_ssh_public_key
            .as_ref()
            .filter(|_| self.login_revision == 0);
        let execution = legacy_key.is_none() || !self.execution.is_cube();
        let mut fields = serializer.serialize_struct(
            "InstanceSpec",
            5 + usize::from(execution) + usize::from(self.image.is_some()),
        )?;
        // Published specification hashes cover these exact field bytes and their order.
        if execution {
            fields.serialize_field("execution", &self.execution)?;
        }
        fields.serialize_field("routing", &self.routing)?;
        fields.serialize_field("name", &self.name)?;
        if let Some(image) = &self.image {
            fields.serialize_field("image", image)?;
        }
        fields.serialize_field("resources", &self.resources)?;
        if let Some(key) = legacy_key {
            fields.serialize_field("ssh_public_key", key)?;
        } else {
            fields.serialize_field("login_revision", &self.login_revision)?;
        }
        fields.serialize_field("interfaces", &self.interfaces)?;
        fields.end()
    }
}
