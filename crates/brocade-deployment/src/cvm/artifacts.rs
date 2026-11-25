//! Immutable CVM products. Kinds own both apply semantics and the local binding whitelist.
use super::*;
use serde_json::Value;

pub const CVM_ARTIFACT_COMPILER: &str = "cube-artifacts-v1";
pub const MAX_IMAGE_MANIFEST_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CvmArtifactKind {
    #[serde(rename = "cube/sandbox.json")]
    Sandbox,
    #[serde(rename = "cube/egress.json")]
    Egress,
    #[serde(rename = "cube/attachments.json")]
    Attachments,
    #[serde(rename = "guest/init.json")]
    GuestInit,
}
impl CvmArtifactKind {
    pub fn apply(&self) -> &'static str {
        match self {
            Self::Sandbox => "stopped",
            Self::Egress | Self::Attachments => "hot",
            Self::GuestInit => "boot_identity",
        }
    }
    pub fn bindings(&self) -> &'static [&'static str] {
        match self {
            Self::Sandbox => &[
                "/annotations/brocade.cvm.expected_root",
                "/annotations/cube.master.components.cube-shim.version",
            ],
            Self::Attachments => &["/runtime_id"],
            Self::Egress | Self::GuestInit => &[],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmImageManifest {
    pub sha256: String,
    /// Exact verified bytes as UTF-8, not a parsed/re-serialized approximation.
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmArtifact {
    pub kind: CvmArtifactKind,
    pub sha256: String,
    /// Canonical JSON bytes; sha256 always covers these exact bytes.
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmArtifactInput {
    pub instance_id: String,
    pub spec: InstanceSpec,
    pub image_manifest: CvmImageManifest,
    /// Frozen executor facts, including storage backend. Not live capacity/utilization.
    pub capabilities: Capabilities,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmArtifactBundle {
    pub compiler: String,
    pub input_sha256: String,
    pub spec_sha256: String,
    pub input: CvmArtifactInput,
    pub files: Vec<CvmArtifact>,
}
impl CvmArtifactBundle {
    pub fn file(&self, kind: CvmArtifactKind) -> Result<&CvmArtifact, String> {
        self.files
            .iter()
            .find(|f| f.kind == kind)
            .ok_or_else(|| "CVM artifact kind missing".into())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmArtifactReceipt {
    pub compiler: String,
    pub input_sha256: String,
    pub mismatches: Vec<CvmArtifactKind>,
    pub files: Vec<CvmArtifactApplied>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvmArtifactApplied {
    pub kind: CvmArtifactKind,
    pub compiled_sha256: String,
    /// Present only after the corresponding native executor acknowledged the bound content.
    pub effective_sha256: Option<String>,
    pub bindings: Value,
}
