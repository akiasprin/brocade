use super::{
    IpFamily, UnlockCode, UnlockConnectionObservation, UnlockPart, UnlockResult, UnlockStatus,
};

/// Categories carried in the existing bounded connection-error field. Unknown/old
/// categories retain generic network handling throughout a rolling upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockDialFailure {
    NoIpv6Egress,
    NetworkUnreachable,
    HostUnreachable,
    ConnectionRefused,
    SourceUnavailable,
    SourceFamilyMismatch,
    PermissionDenied,
    Timeout,
    ConnectionReset,
}

impl UnlockDialFailure {
    pub fn from_trace_code(code: &str) -> Option<Self> {
        Some(match code {
            "ipv6_no_address" | "ipv6_not_supported" => Self::NoIpv6Egress,
            "network_unreachable" => Self::NetworkUnreachable,
            "host_unreachable" => Self::HostUnreachable,
            "connection_refused" => Self::ConnectionRefused,
            "source_unavailable" => Self::SourceUnavailable,
            "source_family_mismatch" => Self::SourceFamilyMismatch,
            "permission_denied" => Self::PermissionDenied,
            "connect_timeout" => Self::Timeout,
            "connection_reset" => Self::ConnectionReset,
            _ => return None,
        })
    }

    /// Total attempts, not retries. Transient or unknown errors keep the caller's
    /// existing request/deadline budget instead of acquiring a new retry loop.
    pub fn attempt_limit(self) -> Option<usize> {
        match self {
            Self::NoIpv6Egress
            | Self::SourceUnavailable
            | Self::SourceFamilyMismatch
            | Self::PermissionDenied => Some(1),
            Self::NetworkUnreachable | Self::HostUnreachable | Self::ConnectionRefused => Some(2),
            Self::Timeout | Self::ConnectionReset => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockDialFailureSummary {
    pub failure: Option<UnlockDialFailure>,
    pub retry_allowed: bool,
}

/// A conclusion needs complete request coverage, and is scoped to a subitem and
/// optionally one domain. A failed hostname must not suppress another routed exit.
pub fn observed_dial_failure(
    records: &[UnlockConnectionObservation],
    part: UnlockPart,
    expected: u32,
    domain: Option<&str>,
) -> Option<UnlockDialFailureSummary> {
    let records = records
        .iter()
        .filter(|record| record.part == part)
        .collect::<Vec<_>>();
    if expected == 0 || expected as usize != records.len() {
        return None;
    }
    if records
        .iter()
        .map(|record| &record.connection.id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != records.len()
    {
        return None;
    }
    let selected = records
        .into_iter()
        .filter(|record| domain.is_none_or(|domain| record.connection.domain == domain))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return None;
    }
    let mut common = None;
    let mut mixed = false;
    let mut retry_allowed = false;
    for record in &selected {
        let connection = &record.connection;
        if !connection.valid() || !connection.complete || connection.connected {
            return None;
        }
        let failure = UnlockDialFailure::from_trace_code(connection.error.as_deref()?)?;
        if failure == UnlockDialFailure::NoIpv6Egress && connection.family != Some(IpFamily::V6) {
            return None;
        }
        mixed |= common.is_some_and(|previous| previous != failure);
        common = Some(failure);
        retry_allowed |= failure
            .attempt_limit()
            .is_none_or(|limit| selected.len() < limit);
    }
    Some(UnlockDialFailureSummary {
        failure: if mixed { None } else { common },
        retry_allowed,
    })
}

impl UnlockResult {
    /// Finalize absence from the completed stream as well as the immediate HTTP
    /// error path. A late evidence write must not leave a confirmed absence as a
    /// generic failure merely because the first reader saw an incomplete file.
    pub fn apply_absent_family_evidence(&mut self, family: IpFamily) {
        let absence = |part| {
            let expected = self.connection_requests.get(&part).copied().unwrap_or(0);
            let records = self
                .connection_observations
                .iter()
                .filter(|record| record.part == part)
                .collect::<Vec<_>>();
            if expected == 0
                || records.len() != expected as usize
                || records
                    .iter()
                    .map(|record| &record.connection.id)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != records.len()
                || records.iter().any(|record| {
                    !record.connection.valid()
                        || !record.connection.complete
                        || record.connection.connected
                })
            {
                return None;
            }
            if records
                .iter()
                .all(|record| record.connection.error.as_deref() == Some("dns_empty"))
            {
                return Some(if family == IpFamily::V6 {
                    UnlockCode::NoIpv6Dns
                } else {
                    UnlockCode::NoIpv4Dns
                });
            }
            (family == IpFamily::V6
                && observed_dial_failure(&self.connection_observations, part, expected, None)
                    .is_some_and(|summary| {
                        summary.failure == Some(UnlockDialFailure::NoIpv6Egress)
                    }))
            .then_some(UnlockCode::NoIpv6Egress)
        };
        let codes = [
            UnlockPart::Service,
            UnlockPart::Webpage,
            UnlockPart::Premium,
            UnlockPart::Cdn,
        ]
        .map(absence);
        if let Some(detail) = &mut self.youtube {
            for (page, code) in [
                (&mut detail.webpage, codes[1]),
                (&mut detail.premium, codes[2]),
            ] {
                if page.status == UnlockStatus::Failed {
                    if let Some(code) = code {
                        page.status = UnlockStatus::Skipped;
                        page.code = code;
                        page.http_status = None;
                        page.page_region = None;
                        page.topbar_region = None;
                        page.privacy_region = None;
                    }
                }
            }
            if detail.cdn.error.is_some() {
                if let Some(code) = codes[3] {
                    detail.cdn.error = Some(code);
                }
            }
            self.status = detail.premium.status;
            self.code = detail.premium.code;
            self.region = detail.premium.region().map(str::to_owned);
        } else if self.status == UnlockStatus::Failed {
            if let Some(code) = codes[0] {
                self.status = UnlockStatus::Skipped;
                self.code = code;
                self.region = None;
                self.client_dns = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: u32, code: &str, domain: &str) -> UnlockConnectionObservation {
        serde_json::from_value(serde_json::json!({
            "part":"service","version":2,"id":format!("{id:032x}"),"domain":domain,
            "exit":"exit-a","outbound":"out:egress:probe-v6","profile":"a".repeat(64),
            "normal_strategy":"UseIP","family":"v6","connected":false,"complete":true,
            "error":code,"lookups":[]
        }))
        .unwrap()
    }
    #[test]
    fn dial_failure_requires_complete_unique_coverage_and_the_attempted_family() {
        let mut records = vec![record(1, "ipv6_no_address", "gemini.google.com")];
        let summary = observed_dial_failure(&records, UnlockPart::Service, 1, None).unwrap();
        assert_eq!(summary.failure, Some(UnlockDialFailure::NoIpv6Egress));
        assert!(!summary.retry_allowed);
        assert!(observed_dial_failure(&records, UnlockPart::Service, 2, None).is_none());
        records[0].connection.family = Some(IpFamily::V4);
        assert!(observed_dial_failure(&records, UnlockPart::Service, 1, None).is_none());
        records[0].connection.family = Some(IpFamily::V6);
        records[0].connection.complete = false;
        assert!(observed_dial_failure(&records, UnlockPart::Service, 1, None).is_none());
        records[0].connection.complete = true;
        records.push(records[0].clone());
        assert!(observed_dial_failure(&records, UnlockPart::Service, 2, None).is_none());
    }
    #[test]
    fn explicit_unreachable_is_confirmed_once_and_transient_or_legacy_errors_keep_the_budget() {
        for code in [
            "network_unreachable",
            "host_unreachable",
            "connection_refused",
        ] {
            let one = record(1, code, "gemini.google.com");
            assert!(
                observed_dial_failure(std::slice::from_ref(&one), UnlockPart::Service, 1, None)
                    .unwrap()
                    .retry_allowed
            );
            assert!(
                !observed_dial_failure(
                    &[one, record(2, code, "gemini.google.com")],
                    UnlockPart::Service,
                    2,
                    None
                )
                .unwrap()
                .retry_allowed
            );
        }
        for code in ["connect_timeout", "connection_reset", "connect_error"] {
            let records = [
                record(1, code, "gemini.google.com"),
                record(2, code, "gemini.google.com"),
            ];
            assert!(
                observed_dial_failure(&records, UnlockPart::Service, 2, None)
                    .is_none_or(|failure| failure.retry_allowed)
            );
        }
    }
    #[test]
    fn a_domain_exit_failure_cannot_suppress_another_domain_or_a_success() {
        let mut records = vec![
            record(1, "ipv6_no_address", "api.openai.com"),
            record(2, "connect_timeout", "ios.chat.openai.com"),
        ];
        assert!(
            !observed_dial_failure(&records, UnlockPart::Service, 2, Some("api.openai.com"))
                .unwrap()
                .retry_allowed
        );
        assert!(
            observed_dial_failure(
                &records,
                UnlockPart::Service,
                2,
                Some("ios.chat.openai.com")
            )
            .unwrap()
            .retry_allowed
        );
        assert!(
            observed_dial_failure(&records, UnlockPart::Service, 2, Some("chatgpt.com")).is_none()
        );
        let summary = observed_dial_failure(&records, UnlockPart::Service, 2, None).unwrap();
        assert_eq!(summary.failure, None);
        assert!(summary.retry_allowed);
        records[1].connection.connected = true;
        records[1].connection.error = None;
        assert!(observed_dial_failure(&records, UnlockPart::Service, 2, None).is_none());
    }

    #[test]
    fn completed_absence_finalizes_the_result_even_if_the_immediate_http_read_was_late() {
        let mut outcome: UnlockResult=serde_json::from_value(serde_json::json!({"service":"gemini","status":"failed","code":"network_error","region":null,"connection_requests":{"service":1}})).unwrap();
        outcome.connection_observations = vec![record(1, "ipv6_no_address", "gemini.google.com")];
        let evidence = outcome.connection_observations.clone();
        outcome.apply_absent_family_evidence(IpFamily::V6);
        assert_eq!(
            (outcome.status, outcome.code),
            (UnlockStatus::Skipped, UnlockCode::NoIpv6Egress)
        );
        assert_eq!(outcome.connection_observations, evidence);
        assert!(outcome.valid());
        outcome.status = UnlockStatus::Failed;
        outcome.code = UnlockCode::NetworkError;
        outcome.apply_absent_family_evidence(IpFamily::V4);
        assert_eq!(outcome.status, UnlockStatus::Failed);
    }

    #[test]
    fn youtube_absence_does_not_erase_premium_or_cdn_failures_in_another_exit() {
        let mut outcome:UnlockResult=serde_json::from_value(serde_json::json!({
            "service":"youtube_premium","status":"failed","code":"network_error","region":null,
            "connection_requests":{"webpage":1,"premium":2,"cdn":1},
            "youtube":{
                "webpage":{"status":"failed","code":"network_error","http_status":null,"page_region":null,"topbar_region":null,"privacy_region":null},
                "premium":{"status":"failed","code":"network_error","http_status":null,"page_region":null,"topbar_region":null,"privacy_region":null},
                "cdn":{"http_status":null,"node":null,"airport":null,"country":null,"error":"timeout","elapsed_ms":3000}
            }
        })).unwrap();
        for (id, part, code, domain) in [
            (1, UnlockPart::Webpage, "ipv6_no_address", "www.youtube.com"),
            (
                2,
                UnlockPart::Premium,
                "network_unreachable",
                "www.youtube.com",
            ),
            (
                3,
                UnlockPart::Premium,
                "network_unreachable",
                "www.youtube.com",
            ),
            (
                4,
                UnlockPart::Cdn,
                "connect_timeout",
                "redirector.googlevideo.com",
            ),
        ] {
            let mut item = record(id, code, domain);
            item.part = part;
            outcome.connection_observations.push(item);
        }
        outcome.apply_absent_family_evidence(IpFamily::V6);
        let detail = outcome.youtube.as_ref().unwrap();
        assert_eq!(
            (detail.webpage.status, detail.webpage.code),
            (UnlockStatus::Skipped, UnlockCode::NoIpv6Egress)
        );
        assert_eq!(detail.premium.status, UnlockStatus::Failed);
        assert_eq!(detail.cdn.error, Some(UnlockCode::Timeout));
        assert_eq!(outcome.status, UnlockStatus::Failed);
        assert!(outcome.valid());
    }
}
