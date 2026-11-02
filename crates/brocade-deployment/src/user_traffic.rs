//! Per-app access quotas and independent, aggregate user speed caps.
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
    /// Independent of the quota; None leaves the normal speed uncapped.
    #[serde(default)]
    pub base_rate_bps: Option<u64>,
    pub exhausted_action: ExhaustedAction,
}
impl UserTrafficPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.base_rate_bps.is_some_and(|rate| {
            !(1_000..=crate::traffic_limit::MAX_TRAFFIC_RATE_BPS).contains(&rate)
        }) {
            return Err("常态速度需在 1 Kbps–1 Tbps 之间".into());
        }
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

    pub fn needs_budget(&self) -> bool {
        self.base_rate_bps.is_some()
            || (self.quota_bytes.is_some()
                && matches!(self.exhausted_action, ExhaustedAction::Throttle { .. }))
    }

    /// One resolver for management status, node assignments and byte reservations.
    pub fn effect(&self, exhausted: bool, available: bool) -> UserTrafficEffect {
        if !available {
            return UserTrafficEffect::Suspended;
        }
        let mut rate = self.base_rate_bps;
        if self.quota_bytes.is_some() && exhausted {
            match self.exhausted_action {
                ExhaustedAction::Suspend => return UserTrafficEffect::Suspended,
                ExhaustedAction::Throttle { rate_bps } => {
                    rate = Some(rate.map_or(rate_bps, |base| base.min(rate_bps)));
                }
            }
        }
        rate.map_or(
            UserTrafficEffect::Unlimited {
                managed: self.needs_budget(),
            },
            |rate_bps| UserTrafficEffect::Limited { rate_bps },
        )
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
    /// Explicit capability marker so an older Console cannot silently ignore a base cap.
    #[serde(default)]
    pub base_rate_supported: bool,
    pub effective: UserTrafficEffect,
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
    fn independent_and_exhausted_speeds_are_bounded() {
        let mut policy = UserTrafficPolicy {
            quota_bytes: Some("1".into()),
            base_rate_bps: None,
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

#[cfg(test)]
mod base_rate_tests {
    use super::*;
    #[test]
    fn constant_speed_quota_and_service_priority() {
        let mut policy = UserTrafficPolicy {
            quota_bytes: None,
            base_rate_bps: Some(50_000_000),
            exhausted_action: ExhaustedAction::Throttle {
                rate_bps: 5_000_000,
            },
        };
        assert_eq!(
            policy.effect(false, true),
            UserTrafficEffect::Limited {
                rate_bps: 50_000_000
            }
        );
        assert_eq!(
            policy.effect(true, true),
            UserTrafficEffect::Limited {
                rate_bps: 50_000_000
            },
            "no quota cannot exhaust"
        );
        policy.quota_bytes = Some("100".into());
        assert_eq!(
            policy.effect(true, true),
            UserTrafficEffect::Limited {
                rate_bps: 5_000_000
            }
        );
        policy.base_rate_bps = Some(1000);
        assert_eq!(
            policy.effect(true, true),
            UserTrafficEffect::Limited { rate_bps: 1000 }
        );
        policy.exhausted_action = ExhaustedAction::Suspend;
        assert_eq!(policy.effect(true, true), UserTrafficEffect::Suspended);
        assert_eq!(policy.effect(false, false), UserTrafficEffect::Suspended);
        policy.base_rate_bps = None;
        assert_eq!(
            policy.effect(false, true),
            UserTrafficEffect::Unlimited { managed: false }
        );
        for base in [0, 999, 1_000_000_000_001] {
            policy.base_rate_bps = Some(base);
            assert!(policy.validate().is_err());
        }
        for base in [1000, 1_000_000_000_000] {
            policy.base_rate_bps = Some(base);
            assert!(policy.validate().is_ok());
        }
        let old: UserTrafficPolicy =
            serde_json::from_str(r#"{"quota_bytes":null,"exhausted_action":{"kind":"suspend"}}"#)
                .unwrap();
        assert_eq!(old.base_rate_bps, None);
    }
}
