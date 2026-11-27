//! Signed native KVM stack installation and compatibility manifests and the
//! per-host maintenance lease. Installed Shim is not a running VM observation.
use super::valid_digest;
use serde::{Deserialize, Serialize};

pub const MAX_STACK_BUNDLE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_STACK_WINDOW_SECS: u64 = 24 * 60 * 60;
pub const STACK_OBSERVATION_MAX_AGE_SECS: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackStorage {
    Xfs,
    Plain,
}

impl StackStorage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xfs => "xfs",
            Self::Plain => "plain",
        }
    }

    /// The cold proof of the supported storage backend with no retained roots.
    pub fn empty_roots_sha256(self) -> String {
        let evidence = format!(r#"["brocade-cvm-cold-roots-v1","{}",[]]"#, self.as_str());
        brocade_core::hash::sha256_hex(evidence.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackPair {
    pub contract: String,
    pub architecture: String,
    pub storage_backend: StackStorage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    pub target_sha256: String,
    pub target_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_cubelet_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_shim_sha256: Option<String>,
    pub target_cubelet_sha256: String,
    pub target_shim_sha256: String,
    pub expires_at: u64,
}

impl StackPair {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_source = match self.contract.as_str() {
            "brocade-cvm-stack-cold-pair-v1" => self
                .source()
                .is_some_and(|source| source.valid() && source.bundle_sha256 != self.target_sha256),
            "brocade-cvm-stack-fresh-install-v1" => {
                self.source_sha256.is_none()
                    && self.source_cubelet_sha256.is_none()
                    && self.source_shim_sha256.is_none()
            }
            _ => false,
        };
        if !valid_source
            || self.architecture != "x86_64"
            || !(1..=MAX_STACK_BUNDLE_BYTES).contains(&self.target_bytes)
            || self.expires_at == 0
            || !self.target().valid()
        {
            return Err("invalid Cube stack compatibility manifest");
        }
        Ok(())
    }

    pub fn is_install(&self) -> bool {
        self.contract == "brocade-cvm-stack-fresh-install-v1"
    }

    pub fn source(&self) -> Option<StackInstalled> {
        Some(StackInstalled {
            bundle_sha256: self.source_sha256.clone()?,
            cubelet_sha256: self.source_cubelet_sha256.clone()?,
            shim_sha256: self.source_shim_sha256.clone()?,
            storage_backend: self.storage_backend,
        })
    }

    pub fn target(&self) -> StackInstalled {
        StackInstalled {
            bundle_sha256: self.target_sha256.clone(),
            cubelet_sha256: self.target_cubelet_sha256.clone(),
            shim_sha256: self.target_shim_sha256.clone(),
            storage_backend: self.storage_backend,
        }
    }
}

/// Ed25519 over the exact UTF-8 payload bytes, not a reserialized object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedStackPair {
    pub payload: String,
    pub signature: String,
}

pub fn stack_hex<const N: usize>(input: &str) -> Option<[u8; N]> {
    fn digit(value: u8) -> Option<u8> {
        match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            _ => None,
        }
    }
    if input.len() != N * 2 {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(input.as_bytes().chunks_exact(2)) {
        *byte = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(bytes)
}

impl SignedStackPair {
    pub fn checked(
        &self,
        verify: impl FnOnce(&[u8], &[u8]) -> bool,
    ) -> Result<StackPair, &'static str> {
        let sig =
            stack_hex::<64>(&self.signature).ok_or("invalid Cube stack signature encoding")?;
        if self.payload.len() > 4096 || !verify(self.payload.as_bytes(), &sig) {
            return Err("Cube stack manifest signature verification failed");
        }
        let pair: StackPair =
            serde_json::from_str(&self.payload).map_err(|_| "invalid signed Cube stack payload")?;
        pair.validate()?;
        Ok(pair)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackInstalled {
    pub bundle_sha256: String,
    pub cubelet_sha256: String,
    pub shim_sha256: String,
    pub storage_backend: StackStorage,
}

impl StackInstalled {
    pub fn valid(&self) -> bool {
        [&self.bundle_sha256, &self.cubelet_sha256, &self.shim_sha256]
            .into_iter()
            .all(|value| valid_digest(value))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackReleaseStatus {
    Preparing,
    Ready,
    Approved,
    Applying,
    Succeeded,
    Failed,
    Recovered,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackRelease {
    pub id: i64,
    pub node_id: String,
    pub source_sha256: Option<String>,
    pub target_sha256: String,
    pub status: StackReleaseStatus,
    pub expires_at: u64,
    pub error: Option<String>,
    pub roots_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackOffer {
    pub id: i64,
    pub expires_at: u64,
    pub status: StackReleaseStatus,
    pub roots_sha256: Option<String>,
    pub artifact: SignedStackPair,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackReport {
    pub id: i64,
    pub status: StackReleaseStatus,
    pub roots_sha256: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackPoll {
    pub installed: Option<StackInstalled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installable: Option<StackStorage>,
    pub report: Option<StackReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackClaim {
    pub roots_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_install_cannot_use_upgrade_source_fields() {
        let target = StackPair {
            contract: "brocade-cvm-stack-fresh-install-v1".into(),
            architecture: "x86_64".into(),
            storage_backend: StackStorage::Plain,
            source_sha256: None,
            target_sha256: "a".repeat(64),
            target_bytes: 1,
            source_cubelet_sha256: None,
            source_shim_sha256: None,
            target_cubelet_sha256: "b".repeat(64),
            target_shim_sha256: "c".repeat(64),
            expires_at: 1,
        };
        assert!(target.is_install());
        assert!(target.validate().is_ok());
        assert!(target.source().is_none());
        let mut inconsistent = target;
        inconsistent.source_shim_sha256 = Some("d".repeat(64));
        assert!(inconsistent.validate().is_err());
        inconsistent.contract = "brocade-cvm-stack-cold-pair-v1".into();
        assert!(inconsistent.validate().is_err());
        inconsistent.source_sha256 = Some("e".repeat(64));
        inconsistent.source_cubelet_sha256 = Some("f".repeat(64));
        assert!(inconsistent.validate().is_ok());
    }

    #[test]
    fn reject_noncanonical_hex_and_unverified_payloads() {
        assert_eq!(stack_hex::<2>("ab03"), Some([0xab, 3]));
        assert_eq!(stack_hex::<1>("AB"), None);
        assert_eq!(stack_hex::<1>("aa00"), None);
        let document = SignedStackPair {
            payload: "{}".into(),
            signature: "11".repeat(64),
        };
        assert!(document.checked(|_, _| false).is_err());
        assert!(document.checked(|_, _| true).is_err());
    }
}
