//! Operational machine limits, shared accounting semantics, and offline reset boundaries.
use serde::{Deserialize, Serialize};

pub const TRAFFIC_LIMIT_PROTOCOL_VERSION: u32 = 42;
pub const MAX_TRAFFIC_LIMIT_RULES: usize = 8;
pub const MAX_TRAFFIC_DAY_BOUNDARIES: usize = 400;
pub const MAX_TRAFFIC_RATE_BPS: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeTrafficCycleKind {
    Monthly,
    Yearly,
}

impl NodeTrafficCycleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeTrafficAccountingMode {
    BidirectionalSum,
    BidirectionalMax,
    OutboundOnly,
    InboundOnly,
}

impl NodeTrafficAccountingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BidirectionalSum => "bidirectional_sum",
            Self::BidirectionalMax => "bidirectional_max",
            Self::OutboundOnly => "outbound_only",
            Self::InboundOnly => "inbound_only",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "bidirectional_sum" => Ok(Self::BidirectionalSum),
            "bidirectional_max" => Ok(Self::BidirectionalMax),
            "outbound_only" => Ok(Self::OutboundOnly),
            "inbound_only" => Ok(Self::InboundOnly),
            _ => Err("invalid machine traffic accounting mode".into()),
        }
    }
    pub fn apply(self, rx: u64, tx: u64) -> u64 {
        match self {
            Self::BidirectionalSum => rx.saturating_add(tx),
            Self::BidirectionalMax => rx.max(tx),
            Self::OutboundOnly => tx,
            Self::InboundOnly => rx,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrafficLimitThreshold {
    QuotaPercent { basis_points: u32 },
    Absolute { bytes: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLimitRule {
    pub threshold: TrafficLimitThreshold,
    pub rate_bps: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficLimitLayer {
    System,
    #[default]
    Application,
    Both,
}
impl TrafficLimitLayer {
    pub fn system(self) -> bool {
        matches!(self, Self::System | Self::Both)
    }
    pub fn application(self) -> bool {
        matches!(self, Self::Application | Self::Both)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLimitPolicy {
    #[serde(default)]
    pub layer: TrafficLimitLayer,
    pub enabled: bool,
    pub rules: Vec<TrafficLimitRule>,
}

pub fn decimal_bytes(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("用量需为非负整数字节数".into());
    }
    value.parse().map_err(|_| "用量超出 u64 范围".into())
}

impl TrafficLimitPolicy {
    pub fn validate(&self, quota: Option<u64>) -> Result<(), String> {
        if self.rules.len() > MAX_TRAFFIC_LIMIT_RULES || (self.enabled && self.rules.is_empty()) {
            return Err("自动限速需设置 1–8 条规则".into());
        }
        for rule in &self.rules {
            if !(1_000..=MAX_TRAFFIC_RATE_BPS).contains(&rule.rate_bps) {
                return Err("限速需在 1 Kbps–1 Tbps 之间".into());
            }
            match &rule.threshold {
                TrafficLimitThreshold::QuotaPercent { basis_points } => {
                    if !(1..=10_000).contains(basis_points) {
                        return Err("百分比阈值需大于 0 且不超过 100%".into());
                    }
                    if self.enabled && quota.is_none_or(|q| q == 0) {
                        return Err("启用百分比限速前需设置机器流量额度".into());
                    }
                }
                TrafficLimitThreshold::Absolute { bytes } => {
                    if decimal_bytes(bytes)? == 0 {
                        return Err("绝对用量阈值必须大于零".into());
                    }
                }
            }
        }
        Ok(())
    }

    /// All matching connections share the strictest matching machine limit in each direction.
    pub fn rate(&self, used: u64, quota: Option<u64>) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        self.rules
            .iter()
            .filter(|rule| match &rule.threshold {
                TrafficLimitThreshold::QuotaPercent { basis_points } => quota.is_some_and(|q| {
                    u128::from(used) * 10_000 >= u128::from(q) * u128::from(*basis_points)
                }),
                TrafficLimitThreshold::Absolute { bytes } => {
                    decimal_bytes(bytes).is_ok_and(|b| used >= b)
                }
            })
            .map(|rule| rule.rate_bps)
            .min()
    }
}

/// Absolute logical counter at a UTC day boundary. The sampling window straddling midnight
/// belongs to the new day, matching normal daily accounting (at most one sample of uncertainty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficDayBoundary {
    pub day_start_unix_secs: i64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Validate the same durable boundaries before local persistence and remote ledger ingestion.
pub fn validate_day_boundaries(
    boundaries: &[TrafficDayBoundary],
    sampled_at: Option<i64>,
    rx: u64,
    tx: u64,
) -> Result<(), String> {
    if boundaries.len() > MAX_TRAFFIC_DAY_BOUNDARIES {
        return Err("too many traffic day boundaries".into());
    }
    let mut previous: Option<&TrafficDayBoundary> = None;
    for boundary in boundaries {
        if boundary.day_start_unix_secs <= 0
            || boundary.day_start_unix_secs % 86_400 != 0
            || sampled_at.is_none_or(|at| boundary.day_start_unix_secs > at)
            || boundary.rx_bytes > rx
            || boundary.tx_bytes > tx
            || previous.is_some_and(|old| {
                old.day_start_unix_secs >= boundary.day_start_unix_secs
                    || old.rx_bytes > boundary.rx_bytes
                    || old.tx_bytes > boundary.tx_bytes
            })
        {
            return Err("invalid traffic day boundary".into());
        }
        previous = Some(boundary);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLimitAnchor {
    pub meter_id: String,
    pub sequence: u64,
    pub sampled_at_unix_secs: i64,
    pub period_start_unix_secs: i64,
    pub meter_rx_bytes: u64,
    pub meter_tx_bytes: u64,
    /// Directional amounts since the period start, or since the active calibration.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub calibrated_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLimitDesired {
    pub revision: u64,
    pub policy: TrafficLimitPolicy,
    pub quota_bytes: Option<u64>,
    pub cycle_kind: NodeTrafficCycleKind,
    pub accounting_mode: NodeTrafficAccountingMode,
    pub reset_month: Option<u8>,
    pub reset_day: u8,
    pub anchor: Option<TrafficLimitAnchor>,
}

impl TrafficLimitDesired {
    pub fn validate(&self) -> Result<(), String> {
        self.policy.validate(self.quota_bytes)?;
        if !(1..=31).contains(&self.reset_day)
            || match self.cycle_kind {
                NodeTrafficCycleKind::Monthly => self.reset_month.is_some(),
                NodeTrafficCycleKind::Yearly => {
                    !self.reset_month.is_some_and(|m| (1..=12).contains(&m))
                }
            }
        {
            return Err("无效的流量重置日期".into());
        }
        Ok(())
    }

    pub fn period(&self, now: i64) -> (i64, i64) {
        let (year, month, _) = civil_from_days(now.div_euclid(86_400));
        let anchor_month = self.reset_month.map(i64::from).unwrap_or(month);
        let this = reset_at(year, anchor_month, self.reset_day);
        let (start_year, start_month) = if now >= this {
            (year, anchor_month)
        } else {
            previous_month(year, anchor_month, self.cycle_kind)
        };
        let (end_year, end_month) = match self.cycle_kind {
            NodeTrafficCycleKind::Yearly => (start_year + 1, start_month),
            NodeTrafficCycleKind::Monthly if start_month == 12 => (start_year + 1, 1),
            NodeTrafficCycleKind::Monthly => (start_year, start_month + 1),
        };
        (
            reset_at(start_year, start_month, self.reset_day),
            reset_at(end_year, end_month, self.reset_day),
        )
    }
}

fn previous_month(year: i64, month: i64, cycle: NodeTrafficCycleKind) -> (i64, i64) {
    match cycle {
        NodeTrafficCycleKind::Yearly => (year - 1, month),
        NodeTrafficCycleKind::Monthly if month == 1 => (year - 1, 12),
        NodeTrafficCycleKind::Monthly => (year, month - 1),
    }
}

fn reset_at(year: i64, month: i64, reset_day: u8) -> i64 {
    let days = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    days_from_civil(year, month, i64::from(reset_day).min(days)) * 86_400
}

// Proleptic Gregorian conversion; keeps the small protocol crate independent of a clock or I/O.
fn days_from_civil(mut year: i64, month: i64, day: i64) -> i64 {
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficLimitPhase {
    Disabled,
    Waiting,
    Ready,
    Limited,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLayerObservation {
    pub phase: TrafficLimitPhase,
    pub rate_bps: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficLimitObservation {
    pub system: Option<TrafficLayerObservation>,
    pub application: Option<TrafficLayerObservation>,
    pub revision: u64,
    pub phase: TrafficLimitPhase,
    pub interface: Option<String>,
    pub rate_bps: Option<u64>,
    pub used_bytes: Option<String>,
    pub period_start_unix_secs: Option<i64>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn desired() -> TrafficLimitDesired {
        TrafficLimitDesired {
            revision: 1,
            policy: TrafficLimitPolicy::default(),
            quota_bytes: None,
            cycle_kind: NodeTrafficCycleKind::Monthly,
            accounting_mode: NodeTrafficAccountingMode::BidirectionalSum,
            reset_month: None,
            reset_day: 31,
            anchor: None,
        }
    }
    #[test]
    fn traffic_limit_rules_are_inclusive_order_independent_and_exact_at_u64_max() {
        let p = TrafficLimitPolicy {
            layer: TrafficLimitLayer::Application,
            enabled: true,
            rules: vec![
                TrafficLimitRule {
                    threshold: TrafficLimitThreshold::QuotaPercent {
                        basis_points: 9_000,
                    },
                    rate_bps: 100_000_000,
                },
                TrafficLimitRule {
                    threshold: TrafficLimitThreshold::Absolute { bytes: "98".into() },
                    rate_bps: 512_000,
                },
            ],
        };
        assert_eq!(p.rate(89, Some(100)), None);
        assert_eq!(p.rate(90, Some(100)), Some(100_000_000));
        assert_eq!(p.rate(98, Some(100)), Some(512_000));
        assert_eq!(p.rate(u64::MAX, Some(u64::MAX)), Some(512_000));
        assert!(p.validate(None).is_err());
        assert_eq!(
            NodeTrafficAccountingMode::BidirectionalMax.apply(20, 30),
            30
        );
    }
    #[test]
    fn traffic_limit_period_clamps_month_ends_and_leap_years_in_utc() {
        let mut d = desired();
        for (year, feb_end) in [(2024, 29), (2026, 28), (2100, 28), (2000, 29)] {
            let start = days_from_civil(year, 2, feb_end) * 86_400;
            assert_eq!(
                d.period(start),
                (start, days_from_civil(year, 3, 31) * 86_400)
            );
            assert_eq!(d.period(start - 1).1, start);
        }
        d.cycle_kind = NodeTrafficCycleKind::Yearly;
        d.reset_month = Some(2);
        let at = days_from_civil(2024, 2, 29) * 86_400;
        assert_eq!(d.period(at).1, days_from_civil(2025, 2, 28) * 86_400);
        for day in -10_000..30_000 {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
    }
}
