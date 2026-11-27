//! CVM contracts shared by Console and Agent; backend execution stays node-local.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

mod diagnostics;
pub use diagnostics::*;
mod artifacts;
pub use artifacts::*;
mod capabilities;
mod chain_egress;
pub use chain_egress::*;
mod entry;
mod entry_policy;
pub use entry_policy::*;
mod host;
mod metrics;
mod network_quota;
mod nic;
mod observation;
mod route_target;
mod spec;
pub use route_target::*;
pub mod stack;
mod state;
pub mod terminal;
mod validation;
pub use capabilities::*;
pub use entry::*;
pub use host::*;
pub use metrics::{parse_system_metrics, SYSTEM_METRICS_SCRIPT};
pub use network_quota::*;
pub use nic::*;
pub use observation::*;
pub use spec::*;
pub use state::*;
pub use validation::{valid_digest, valid_id};
use validation::{valid_interface_address, valid_mac};

pub const MAX_INSTANCES_PER_MACHINE: usize = 256;
pub const MAX_INTERFACES: usize = 16;
pub const MAX_CATALOG_ITEMS: usize = 128;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardware_profile_and_system_image_are_independent_contracts() {
        let image = Image {
            id: "debian".into(),
            name: "Debian".into(),
            version: "13".into(),
            architecture: "x86_64".into(),
            sha256: "a".repeat(64),
            boot_mode: "cube-oci".into(),
        };
        let value = serde_json::to_value(image).unwrap();
        assert!(value.get("memory_bytes").is_none() && value.get("vcpus").is_none());
    }
    #[test]
    fn configured_sources_only_create_the_contact_device() {
        let caps = Capabilities {
            max_interfaces: 4,
            network_sources: vec![NetworkSource {
                id: "public".into(),
                name: "Public".into(),
                kind: NetworkKind::Public,
                description: String::new(),
                max_attachments: Some(1),
                supports_ipv6: Some(false),
            }],
            ..Default::default()
        };
        let contact = InterfaceRequest {
            source_id: "public".into(),
            purpose: InterfacePurpose::Contact,
            default_ipv4: false,
            default_ipv6: false,
        };
        let business = InterfaceRequest {
            source_id: "own".into(),
            purpose: InterfacePurpose::Business,
            default_ipv4: true,
            default_ipv6: false,
        };
        assert!(caps
            .validate_interfaces(std::slice::from_ref(&contact))
            .is_ok());
        assert!(caps
            .validate_interfaces(&[contact.clone(), business.clone()])
            .is_err());
        assert!(caps
            .validate_interfaces(&[contact.clone(), contact.clone()])
            .is_err());
        assert!(caps
            .validate_interfaces(&[contact, business.clone(), business.clone()])
            .is_err());
        assert!(caps.validate_interfaces(&[business]).is_err());
    }
}

#[test]
fn independent_resources_enforce_each_limit_without_catalogue_coupling() {
    let limits = ResourceLimits {
        min_vcpus: 1,
        max_vcpus: 8,
        min_memory_bytes: 128 * 1024 * 1024,
        max_memory_bytes: 4 * 1024 * 1024 * 1024,
        memory_step_bytes: 64 * 1024 * 1024,
        min_root_disk_bytes: 3 * 1024 * 1024 * 1024,
        max_root_disk_bytes: 32 * 1024 * 1024 * 1024,
        root_disk_step_bytes: 1024 * 1024 * 1024,
    };
    let mut resources = Resources {
        vcpus: 3,
        memory_bytes: 192 * 1024 * 1024,
        root_disk_bytes: 4 * 1024 * 1024 * 1024,
    };
    limits.validate_resources(&resources).unwrap();
    resources.memory_bytes += 1;
    assert!(limits.validate_resources(&resources).is_err());
    resources.memory_bytes -= 1;
    resources.vcpus = 9;
    assert!(limits.validate_resources(&resources).is_err());
    let mut invalid = limits;
    invalid.memory_step_bytes = 0;
    assert!(invalid.validate_resources(&resources).is_err());
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GuestCredential {
    pub instance_id: String,
    pub revision: u64,
    pub password: String,
}

impl GuestCredential {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.instance_id)
            || self.revision == 0
            || self.password.len() != 32
            || !self
                .password
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid Guest credential".into());
        }
        Ok(())
    }
}
