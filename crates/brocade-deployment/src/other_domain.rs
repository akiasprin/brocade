//! Optional administrator-only DNS observations, independent of billing and site categories.
use serde::{Deserialize, Serialize};

pub const OTHER_DOMAIN_POLICY_HEADER: &str = "x-brocade-other-domain-policy";
pub const OTHER_DOMAIN_VERSION: u32 = 1;
pub const DETAIL_CHECKPOINT_VERSION: u32 = 2;
pub const OTHER_DOMAIN_MAX_ROWS: usize = 8192; // Two windows of at most 4096 pairs.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtherDomainPolicy {
    pub enabled: bool,
    pub top_n: u32,
    pub max_domains_per_user: u32,
    pub node_capacity: u32,
    pub report_interval_secs: u32,
}
impl Default for OtherDomainPolicy {
    fn default() -> Self {
        // Fail closed for missing/legacy wire policies. Fresh Console defaults live in the
        // database and are always sent explicitly, so an old Console cannot enable collection.
        Self {
            enabled: false,
            top_n: 128,
            max_domains_per_user: 1024,
            node_capacity: 4096,
            report_interval_secs: 30,
        }
    }
}
impl OtherDomainPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=512).contains(&self.top_n)
            || self.max_domains_per_user < self.top_n
            || self.max_domains_per_user > 4096
            || self.node_capacity < self.max_domains_per_user
            || self.node_capacity > 4096
            || !(15..=300).contains(&self.report_interval_secs)
        {
            return Err("invalid other-domain collection budget");
        }
        Ok(())
    }
}

/// Canonical ASCII DNS names only. Never accept IP literals, URLs or request paths.
pub fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain.parse::<std::net::IpAddr>().is_err()
        && domain.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherDomainCounter {
    /// A new ID on every admission; zero is reserved for legacy cumulative counters.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub counter_id: u64,
    pub bucket: i64,
    pub label: String,
    pub domain: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}

pub(crate) fn is_zero(value: &u64) -> bool {
    *value == 0
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailUsageReport<Counter> {
    pub version: u32,
    pub epoch: String,
    pub activation_unix_secs: i64,
    pub read_at_unix_secs: i64,
    pub usage_generation_id: i64,
    pub limited: bool,
    pub counters: Vec<Counter>,
}
pub type OtherDomainReport = DetailUsageReport<OtherDomainCounter>;
impl<Counter> DetailUsageReport<Counter> {
    pub fn map_counters<Other>(
        self,
        map: impl FnMut(Counter) -> Other,
    ) -> DetailUsageReport<Other> {
        DetailUsageReport {
            version: self.version,
            epoch: self.epoch,
            activation_unix_secs: self.activation_unix_secs,
            read_at_unix_secs: self.read_at_unix_secs,
            usage_generation_id: self.usage_generation_id,
            limited: self.limited,
            counters: self.counters.into_iter().map(map).collect(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOtherDomainRow {
    pub domain: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetailUsage<Row> {
    pub collection_enabled: bool,
    pub top_n: u32,
    pub range_start_unix_secs: i64,
    pub range_end_unix_secs: i64,
    pub reporting_nodes: u64,
    pub expected_nodes: u64,
    pub limited: bool,
    pub observed_since_unix_secs: Option<i64>,
    /// Earliest stored hour bucket in this scope and range, before the Top-N limit.
    #[serde(default)]
    pub first_record_unix_secs: Option<i64>,
    pub latest_report_unix_secs: Option<i64>,
    pub rows: Vec<Row>,
}
pub type UserOtherDomains = UserDetailUsage<UserOtherDomainRow>;
impl<Row> UserDetailUsage<Row> {
    pub fn map_rows<Other>(self, map: impl FnMut(Row) -> Other) -> UserDetailUsage<Other> {
        UserDetailUsage {
            collection_enabled: self.collection_enabled,
            top_n: self.top_n,
            range_start_unix_secs: self.range_start_unix_secs,
            range_end_unix_secs: self.range_end_unix_secs,
            reporting_nodes: self.reporting_nodes,
            expected_nodes: self.expected_nodes,
            limited: self.limited,
            observed_since_unix_secs: self.observed_since_unix_secs,
            first_record_unix_secs: self.first_record_unix_secs,
            latest_report_unix_secs: self.latest_report_unix_secs,
            rows: self.rows.into_iter().map(map).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_and_dns_boundary() {
        let default = OtherDomainPolicy::default();
        assert!(!default.enabled);
        assert_eq!(default.top_n, 128);
        assert_eq!(default.max_domains_per_user, 1024);
        assert!(default.validate().is_ok());
        assert!(OtherDomainPolicy {
            top_n: 1025,
            ..default
        }
        .validate()
        .is_err());
        assert!(OtherDomainPolicy {
            node_capacity: 512,
            ..default
        }
        .validate()
        .is_err());
        for name in ["video.example", "xn--fsqu00a.example", "localhost"] {
            assert!(valid_domain(name));
        }
        for name in [
            "",
            "EXAMPLE.test",
            "1.2.3.4",
            "::1",
            "https://a.test/x",
            "a.test:443",
            "a..test",
            "a.test.",
            "a/b",
            "_a.test",
            "例子.test",
        ] {
            assert!(!valid_domain(name));
        }
    }
}
