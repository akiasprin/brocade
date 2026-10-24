//! Per-app access quotas. Speed applies only after the billing quota is exhausted.
use serde::{Deserialize, Serialize};

pub const USER_TRAFFIC_PROTOCOL_VERSION: u32 = 44;
pub const MAX_BUDGET_BYTES: u64 = 16 * 1024 * 1024;
pub const BUDGET_TTL_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserTrafficKind {
    Access,
    /// Retired policy discriminator kept for compatibility; new forwarding policies are rejected.
    Forwarding,
}
impl UserTrafficKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Forwarding => "forwarding",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExhaustedAction {
    Suspend,
    Throttle { rate_bps: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserTrafficPolicy {
    /// None removes this scope's quota.
    pub quota_bytes: Option<String>,
    pub exhausted_action: ExhaustedAction,
}
impl UserTrafficPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(value) = &self.quota_bytes {
            let bytes = crate::traffic_limit::decimal_bytes(value)?;
            if bytes == 0 || bytes > i64::MAX as u64 {
                return Err("用户额度需为大于零的字节数，或留空表示不限额".into());
            }
        }
        if let ExhaustedAction::Throttle { rate_bps } = self.exhausted_action {
            if !(1_000..=crate::traffic_limit::MAX_TRAFFIC_RATE_BPS).contains(&rate_bps) {
                return Err("超额速度需在 1 Kbps–1 Tbps 之间".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTrafficPolicyView {
    pub kind: UserTrafficKind,
    /// Access policies are scoped to this app; forwarding has no app.
    pub app_id: Option<String>,
    pub policy: Option<UserTrafficPolicy>,
    pub used_bytes: String,
    pub exhausted: bool,
    pub service_available: bool,
    pub unsupported_nodes: Vec<String>,
    pub pending_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBudgetRequest {
    /// An authenticated ingress label, or an exact dynamic forwarding tag. Never a client-supplied user id.
    pub source: String,
    pub request_id: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UserBudgetGrant {
    Unlimited {
        valid_ms: u64,
        managed: bool,
    },
    Suspended {
        valid_ms: u64,
    },
    Limited {
        bytes: u64,
        rate_bps: u64,
        valid_ms: u64,
    },
    Retry {
        after_ms: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exhausted_speed_can_be_configured_and_values_are_bounded() {
        let mut policy = UserTrafficPolicy {
            quota_bytes: Some("1".into()),
            exhausted_action: ExhaustedAction::Throttle { rate_bps: 1_000 },
        };
        assert!(policy.validate().is_ok());
        policy.quota_bytes = Some("0".into());
        assert!(policy.validate().is_err());
        policy.quota_bytes = None;
        policy.exhausted_action = ExhaustedAction::Throttle { rate_bps: 0 };
        assert!(policy.validate().is_err());
        assert!(serde_json::from_str::<UserTrafficKind>("\"other\"").is_err());
    }
}

/// User-list summary; policy editing and detailed node readiness use UserTrafficPolicyView.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserTrafficStatus {
    pub tenant_id: String,
    pub user_id: String,
    pub kind: UserTrafficKind,
    pub app_id: Option<String>,
    pub quota_bytes: Option<String>,
    pub used_bytes: String,
    pub exhausted: bool,
    pub suspended: bool,
    pub rate_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UserTrafficEffect {
    Unlimited { managed: bool },
    Limited { rate_bps: u64 },
    Suspended,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserTrafficAssignment {
    pub source: String,
    pub effect: UserTrafficEffect,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTrafficSnapshot {
    pub sources: Vec<UserTrafficAssignment>,
    pub sha256: String,
}
