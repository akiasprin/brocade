//! A chain reference is public routing intent; subscriber credentials stay in node-only products.
use super::*;

pub const CHAIN_EGRESS_CONTRACT: &str = "cvm-chain-egress-v1";
/// Interface-local source identity for Linux's IPv4 reverse-path validation. It has no prefix
/// route and is shared only by owned TUNs; it is never a Guest/business address allocation.
/// Host conntrack reassembles forwarded UDP fragments before policy routing. The local
/// userspace link must accept a full IP datagram; physical transport MTUs remain independent.
pub const CHAIN_TUN_MTU: u32 = 65535;
/// The userspace writer fragments replies before they enter the physical primary link.
pub const CHAIN_STACK_MTU: u32 = 1280;
pub const CHAIN_TUN_IPV4: &str = "169.254.254.254";
/// Transport sockets must not re-enter a native instance's own OUTPUT selector.
/// Outside both the native NIC mark range and the per-chain routing mark range.
pub const CHAIN_CLIENT_SOCKET_MARK: u32 = 0x00b9_0000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEgressBinding {
    pub chain_id: String,
    pub mark: u32,
    pub device: String,
}

/// Credential-bearing product. Only an authenticated assigned Agent or a system administrator
/// may read its source. The instance's public four-file bundle contains references only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainClientArtifact {
    #[serde(default)]
    pub blocked: bool,
    /// Apply host OUTPUT protections only for the adopted physical operating system.
    #[serde(default)]
    pub host_system: bool,
    pub instance_id: String,
    pub chain_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub app_id: String,
    pub ingress_id: String,
    pub ingress_node: String,
    pub binding: ChainEgressBinding,
    pub sha256: String,
    pub content: String,
    /// Frozen native guard; only the owned table's atomic replacement is a local binding.
    #[serde(default)]
    pub guard: String,
    /// A fixed display projection, never a regex-redacted guess at arbitrary credentials.
    pub redacted: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainClientReceipt {
    pub chain_id: String,
    pub sha256: String,
    #[serde(default)]
    pub guard_sha256: String,
    pub configured: bool,
    pub error: Option<String>,
}
