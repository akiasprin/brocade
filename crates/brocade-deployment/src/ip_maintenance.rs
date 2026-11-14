//! Persistent machine maintenance intent and bounded execution receipts.
//!
//! This is operational state, independent of immutable proxy configuration releases.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
pub mod data;

pub const IP_MAINTENANCE_PROTOCOL_VERSION: u32 = 68;
pub const IP_MAINTENANCE_RESET_PROTOCOL_VERSION: u32 = 69;
pub const IP_MAINTENANCE_DUAL_PROTOCOL_VERSION: u32 = 70;
pub const MAX_ACTIONS: u8 = 20;
pub const MAX_REPORT_ACTIONS: usize = 32;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ROUND_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceFamily {
    V4,
    V6,
    Dual,
}
impl MaintenanceFamily {
    pub fn accepts(self, ip: IpAddr) -> bool {
        matches!(
            (self, ip.to_canonical()),
            (Self::V4, IpAddr::V4(_)) | (Self::V6, IpAddr::V6(_)) | (Self::Dual, _)
        )
    }
    pub fn includes(self, family: Self) -> bool {
        family != Self::Dual && (self == Self::Dual || self == family)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberRange {
    pub min: u32,
    pub max: u32,
}
impl NumberRange {
    fn valid(&self, min: u32, max: u32) -> bool {
        self.min >= min && self.min <= self.max && self.max <= max
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwellBand {
    pub weight: u8,
    pub seconds: NumberRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceBehavior {
    pub google_actions: NumberRange,
    pub google_dwell_secs: NumberRange,
    pub trust_actions: NumberRange,
    pub trust_dwell: Vec<DwellBand>,
    /// Search, news, maps, platform-specific ecosystem/network check.
    pub android_actions: [u8; 4],
    pub apple_actions: [u8; 4],
    pub desktop_actions: [u8; 4],
    pub referer_percent: u8,
    pub request_timeout_secs: u32,
    pub timer_jitter_secs: u32,
    pub runner_jitter_secs: u32,
    pub cookie_idle_days: u32,
}
impl Default for MaintenanceBehavior {
    fn default() -> Self {
        Self {
            google_actions: NumberRange { min: 5, max: 8 },
            google_dwell_secs: NumberRange { min: 45, max: 75 },
            trust_actions: NumberRange { min: 3, max: 6 },
            trust_dwell: vec![
                DwellBand {
                    weight: 45,
                    seconds: NumberRange { min: 8, max: 20 },
                },
                DwellBand {
                    weight: 35,
                    seconds: NumberRange { min: 20, max: 60 },
                },
                DwellBand {
                    weight: 15,
                    seconds: NumberRange { min: 60, max: 180 },
                },
                DwellBand {
                    weight: 5,
                    seconds: NumberRange { min: 180, max: 479 },
                },
            ],
            android_actions: [25, 30, 30, 15],
            apple_actions: [30, 35, 25, 10],
            desktop_actions: [20, 40, 20, 20],
            referer_percent: 70,
            request_timeout_secs: 15,
            timer_jitter_secs: 180,
            runner_jitter_secs: 179,
            cookie_idle_days: 14,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpMaintenancePolicy {
    pub enabled: bool,
    pub region: String,
    pub family: MaintenanceFamily,
    pub source_ip: Option<IpAddr>,
    pub expected_public_ip: Option<IpAddr>,
    #[serde(default)]
    pub source_ipv6: Option<IpAddr>,
    #[serde(default)]
    pub expected_public_ipv6: Option<IpAddr>,
    pub google: bool,
    pub trust: bool,
    pub google_percent: u8,
    pub interval_minutes: u32,
    pub daily_requests: u32,
    pub daily_download_mib: u32,
    pub cooldown_minutes: u32,
    pub continue_offline: bool,
    pub behavior: MaintenanceBehavior,
}
impl Default for IpMaintenancePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            region: "JP/Default/Tokyo".into(),
            family: MaintenanceFamily::V4,
            source_ip: None,
            expected_public_ip: None,
            source_ipv6: None,
            expected_public_ipv6: None,
            google: true,
            trust: true,
            google_percent: 70,
            interval_minutes: 45,
            daily_requests: 1500,
            daily_download_mib: 128,
            cooldown_minutes: 60,
            continue_offline: false,
            behavior: MaintenanceBehavior::default(),
        }
    }
}
impl IpMaintenancePolicy {
    pub fn protocol_version(&self) -> u32 {
        if self.family == MaintenanceFamily::Dual {
            IP_MAINTENANCE_DUAL_PROTOCOL_VERSION
        } else {
            IP_MAINTENANCE_PROTOCOL_VERSION
        }
    }
    pub fn for_family(&self, family: MaintenanceFamily) -> Result<Self, String> {
        if !self.family.includes(family) {
            return Err("address_family_unselected".into());
        }
        let mut policy = self.clone();
        if self.family == MaintenanceFamily::Dual && family == MaintenanceFamily::V6 {
            policy.source_ip = self.source_ipv6;
            policy.expected_public_ip = self.expected_public_ipv6;
        }
        policy.family = family;
        policy.source_ipv6 = None;
        policy.expected_public_ipv6 = None;
        Ok(policy)
    }
    pub fn validate(&self) -> Result<(), String> {
        let b = &self.behavior;
        if self.region.is_empty()
            || self.region.len() > 160
            || self.region.split('/').count() != 3
            || self
                .region
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !self
                .region
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/_-.".contains(&c))
        {
            return Err("养护地区标识无效".into());
        }
        let primary_family = if self.family == MaintenanceFamily::Dual {
            MaintenanceFamily::V4
        } else {
            self.family
        };
        if self
            .source_ip
            .into_iter()
            .chain(self.expected_public_ip)
            .any(|ip| !primary_family.accepts(ip))
            || self
                .source_ipv6
                .into_iter()
                .chain(self.expected_public_ipv6)
                .any(|ip| {
                    self.family != MaintenanceFamily::Dual || !MaintenanceFamily::V6.accepts(ip)
                })
        {
            return Err("来源地址和预期出口必须与所选地址族一致".into());
        }
        if self.google_percent > 100 {
            return Err("Google 比例需在 0–100 之间".into());
        }
        if !(5..=1440).contains(&self.interval_minutes)
            || !(10..=10_000).contains(&self.daily_requests)
            || !(1..=1024).contains(&self.daily_download_mib)
            || !(1..=1440).contains(&self.cooldown_minutes)
        {
            return Err("养护周期、每日预算或冷却时间超出允许范围".into());
        }
        if !b.google_actions.valid(1, u32::from(MAX_ACTIONS))
            || !b.trust_actions.valid(1, u32::from(MAX_ACTIONS))
            || !b.google_dwell_secs.valid(5, 600)
            || b.trust_dwell.len() != 4
            || b.trust_dwell
                .iter()
                .map(|band| u32::from(band.weight))
                .sum::<u32>()
                != 100
            || b.trust_dwell.iter().any(|band| !band.seconds.valid(0, 600))
            || [&b.android_actions, &b.apple_actions, &b.desktop_actions]
                .into_iter()
                .any(|weights| weights.iter().map(|v| u32::from(*v)).sum::<u32>() != 100)
            || b.referer_percent > 100
            || !(3..=60).contains(&b.request_timeout_secs)
            || b.timer_jitter_secs > 600
            || b.runner_jitter_secs > 600
            || !(1..=30).contains(&b.cookie_idle_days)
        {
            return Err("动作数量、停留分布、平台比例或会话参数无效".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpMaintenanceDesired {
    pub generation: u64,
    pub policy: IpMaintenancePolicy,
    pub control_sequence: u64,
    pub manual_request_id: Option<String>,
    pub stop_sequence: u64,
    pub target_round_id: Option<String>,
    pub manual_sequence: u64,
    pub manual_expires_at: Option<i64>,
    pub session_generation: u64,
    #[serde(default)]
    pub report_sequence_floor: u64,
    #[serde(default)]
    pub data_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateIpMaintenance {
    pub expected_generation: u64,
    pub policy: IpMaintenancePolicy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpMaintenanceCommand {
    pub expected_generation: u64,
    pub request_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpMaintenanceStop {
    pub expected_generation: u64,
    pub request_id: String,
    pub round_id: String,
}
impl IpMaintenanceCommand {
    pub fn validate(&self) -> Result<(), String> {
        if self.request_id.is_empty()
            || self.request_id.len() > 80
            || !self
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err("控制请求标识无效".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceModule {
    Google,
    Trust,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenancePhase {
    Disabled,
    Waiting,
    Jitter,
    Preflight,
    Requesting,
    Dwelling,
    Postcheck,
    Complete,
    Interrupted,
    Cooldown,
    BudgetWait,
    BlockedEgress,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceAction {
    pub index: u32,
    /// A fixed action category; never a complete URL or keyword.
    pub kind: String,
    pub host: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub response_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceEvidence {
    pub source: String,
    pub country: Option<String>,
    pub status: Option<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpMaintenanceReport {
    #[serde(default)]
    pub family: Option<MaintenanceFamily>,
    pub generation: u64,
    pub run_generation: u64,
    pub agent_instance_id: String,
    pub applied_control_sequence: u64,
    pub applied_stop_sequence: u64,
    pub applied_session_generation: u64,
    pub worker_observed_at_unix_secs: i64,
    pub last_started_at_unix_secs: Option<i64>,
    pub cooldown_until_unix_secs: Option<i64>,
    /// Strictly increasing, durable Agent receipt sequence.
    pub sequence: u64,
    pub phase: MaintenancePhase,
    pub round_id: Option<String>,
    pub module: Option<MaintenanceModule>,
    pub actual_exit_ip: Option<IpAddr>,
    #[serde(default)]
    pub data_digest: Option<String>,
    pub actions_total: u32,
    pub action_index: u32,
    pub phase_deadline_unix_secs: Option<i64>,
    pub next_run_unix_secs: Option<i64>,
    pub daily_requests: u32,
    pub daily_response_bytes: u64,
    pub actions: Vec<MaintenanceAction>,
    pub evidence: Vec<MaintenanceEvidence>,
    #[serde(default)]
    pub evidence_observed_at_unix_secs: Option<i64>,
    pub reason: Option<String>,
    pub observed_at_unix_secs: i64,
}
impl IpMaintenanceReport {
    pub fn validate(&self) -> Result<(), String> {
        let safe = |s: &str, limit: usize| s.len() <= limit && !s.chars().any(char::is_control);
        if self.family == Some(MaintenanceFamily::Dual)
            || self
                .family
                .zip(self.actual_exit_ip)
                .is_some_and(|(family, ip)| !family.accepts(ip))
            || self.sequence == 0
            || self.agent_instance_id.is_empty()
            || !safe(&self.agent_instance_id, 80)
            || self.worker_observed_at_unix_secs <= 0
            || self.run_generation > self.generation
            || self.data_digest.as_ref().is_some_and(|digest| {
                digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
            || self.observed_at_unix_secs <= 0
            || self.actions_total > u32::from(MAX_ACTIONS)
            || self.action_index > self.actions_total
            || self.actions.len() > MAX_REPORT_ACTIONS
            || self.evidence.len() > 3
            || self.reason.as_ref().is_some_and(|s| !safe(s, 200))
            || self
                .round_id
                .as_ref()
                .is_some_and(|s| !safe(s, 80) || s.is_empty())
            || self.actions.iter().any(|a| {
                a.index > u32::from(MAX_ACTIONS)
                    || !safe(&a.kind, 40)
                    || !safe(&a.host, 253)
                    || a.host.contains(['/', '?', '@'])
                    || a.error.as_ref().is_some_and(|s| !safe(s, 80))
            })
            || self.evidence.iter().any(|e| {
                !matches!(
                    e.source.as_str(),
                    "google_jump" | "youtube_premium" | "youtube_music"
                ) || e
                    .country
                    .as_ref()
                    .is_some_and(|c| c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()))
            })
        {
            return Err("养护回执无效或超出大小限制".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpMaintenanceView {
    #[serde(default)]
    pub default_policy: IpMaintenancePolicy,
    pub desired: IpMaintenanceDesired,
    pub supported: bool,
    #[serde(default)]
    pub session_reset_supported: bool,
    #[serde(default)]
    pub dual_stack_supported: bool,
    pub lifecycle_active: bool,
    pub report: Option<IpMaintenanceReport>,
    #[serde(default)]
    pub family_reports: Vec<IpMaintenanceReport>,
    pub received_at_unix_secs: Option<i64>,
    pub data_updated_at_unix_secs: Option<i64>,
    pub agents_updated_at_unix_secs: Option<i64>,
    pub data_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpMaintenanceRuns {
    pub runs: Vec<IpMaintenanceReport>,
    pub next_cursor: Option<u64>,
}
impl MaintenancePhase {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Jitter | Self::Preflight | Self::Requesting | Self::Dwelling | Self::Postcheck
        )
    }
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Complete
                | Self::Interrupted
                | Self::Failed
                | Self::Cooldown
                | Self::BlockedEgress
                | Self::BudgetWait
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dual_stack_policy_projects_addresses_without_cross_family_fallback() {
        let mut policy = IpMaintenancePolicy {
            family: MaintenanceFamily::Dual,
            source_ip: Some("1.1.1.1".parse().unwrap()),
            source_ipv6: Some("2606:4700::1111".parse().unwrap()),
            ..Default::default()
        };
        policy.validate().unwrap();
        assert_eq!(policy.protocol_version(), 70);
        for family in [MaintenanceFamily::V4, MaintenanceFamily::V6] {
            let selected = policy.for_family(family).unwrap();
            selected.validate().unwrap();
            assert_eq!(selected.family, family);
            assert!(family.accepts(selected.source_ip.unwrap()));
            assert!(selected.source_ipv6.is_none());
        }
        assert!(policy.for_family(MaintenanceFamily::Dual).is_err());
        policy.source_ipv6 = Some("1.1.1.1".parse().unwrap());
        assert!(policy.validate().is_err());
        policy.source_ipv6 = None;
        policy.source_ip = Some("2606:4700::1111".parse().unwrap());
        assert!(policy.validate().is_err());
    }
    #[test]
    fn defaults_preserve_behavior_without_enabling_network_activity() {
        let p = IpMaintenancePolicy::default();
        p.validate().unwrap();
        assert!(!p.enabled);
        assert_eq!(p.google_percent, 70);
        assert_eq!(p.interval_minutes, 45);
        assert_eq!(p.behavior.android_actions, [25, 30, 30, 15]);
        assert_eq!(p.behavior.trust_dwell[3].seconds.max, 479);
    }
    #[test]
    fn rejects_mismatched_source_and_malformed_distributions() {
        let mut p = IpMaintenancePolicy {
            source_ip: Some("::1".parse().unwrap()),
            ..Default::default()
        };
        assert!(p.validate().is_err());
        p.source_ip = None;
        p.behavior.trust_dwell[0].weight = 44;
        assert!(p.validate().is_err());
        p.behavior = MaintenanceBehavior::default();
        p.region = "JP/../Tokyo".into();
        assert!(p.validate().is_err());
    }
    #[test]
    fn additive_fields_allow_rolling_upgrade() {
        let mut value = serde_json::to_value(IpMaintenanceDesired::default()).unwrap();
        value["future"] = serde_json::json!(true);
        assert!(serde_json::from_value::<IpMaintenanceDesired>(value).is_ok());
    }
}
