//! Claude region support, independent of the login page and browser verification.
use super::{get_without_redirects, result, Page, ProbeContext};
use brocade_deployment::unlock_probe::{UnlockCode, UnlockResult, UnlockService, UnlockStatus};
use std::{net::IpAddr, time::Instant};

pub(super) const TARGET: &str = "https://claude.ai/cdn-cgi/trace";

// Anthropic Supported Regions Policy, effective 2026-11-12, checked 2026-11-17:
// https://www.anthropic.com/legal/supported-regions
// Includes the listed countries' territories and dependencies. Country-level evidence
// cannot distinguish the policy's exclusions within Ukraine or account eligibility.
const SUPPORTED_REGIONS: &[&str] = &[
    "AD", "AE", "AG", "AI", "AL", "AM", "AO", "AR", "AS", "AT", "AU", "AW", "AX", "AZ", "BA", "BB",
    "BD", "BE", "BF", "BG", "BH", "BI", "BJ", "BL", "BM", "BN", "BO", "BQ", "BR", "BS", "BT", "BV",
    "BW", "BZ", "CA", "CC", "CD", "CF", "CG", "CH", "CI", "CK", "CL", "CM", "CO", "CR", "CV", "CW",
    "CX", "CY", "CZ", "DE", "DJ", "DK", "DM", "DO", "DZ", "EC", "EE", "EG", "ER", "ES", "ET", "FI",
    "FJ", "FK", "FM", "FO", "FR", "GA", "GB", "GD", "GE", "GF", "GG", "GH", "GI", "GL", "GM", "GN",
    "GP", "GQ", "GR", "GS", "GT", "GU", "GW", "GY", "HM", "HN", "HR", "HT", "HU", "ID", "IE", "IL",
    "IM", "IN", "IO", "IQ", "IS", "IT", "JE", "JM", "JO", "JP", "KE", "KG", "KH", "KI", "KM", "KN",
    "KR", "KW", "KY", "KZ", "LA", "LB", "LC", "LI", "LK", "LR", "LS", "LT", "LU", "LV", "LY", "MA",
    "MC", "MD", "ME", "MF", "MG", "MH", "MK", "ML", "MN", "MP", "MQ", "MR", "MS", "MT", "MU", "MV",
    "MW", "MX", "MY", "MZ", "NA", "NC", "NE", "NF", "NG", "NI", "NL", "NO", "NP", "NR", "NU", "NZ",
    "OM", "PA", "PE", "PF", "PG", "PH", "PK", "PL", "PM", "PN", "PR", "PS", "PT", "PW", "PY", "QA",
    "RE", "RO", "RS", "RW", "SA", "SB", "SC", "SD", "SE", "SG", "SH", "SI", "SJ", "SK", "SL", "SM",
    "SN", "SO", "SR", "SS", "ST", "SV", "SX", "SZ", "TC", "TD", "TF", "TG", "TH", "TJ", "TK", "TL",
    "TM", "TN", "TO", "TR", "TT", "TV", "TW", "TZ", "UA", "UG", "UM", "US", "UY", "UZ", "VA", "VC",
    "VG", "VI", "VN", "VU", "WF", "WS", "YT", "ZA", "ZM", "ZW",
];
const UNSUPPORTED_REGIONS: &[&str] = &[
    "AF", "AQ", "BY", "CN", "CU", "EH", "HK", "IR", "KP", "MM", "MO", "RU", "SY", "VE", "YE",
];

pub(super) fn detect(
    deadline: Instant,
    proxy: Option<&ProbeContext>,
) -> Result<UnlockResult, UnlockCode> {
    detect_with(|url| get_without_redirects(url, deadline, proxy))
}

fn detect_with(
    fetch: impl FnOnce(&str) -> Result<Page, UnlockCode>,
) -> Result<UnlockResult, UnlockCode> {
    let page = fetch(TARGET)?;
    Ok(parse(page.response.status, &page.response.body))
}

pub(super) fn parse(status: u16, body: &str) -> UnlockResult {
    let region = if status == 200 {
        trace_country(body)
    } else {
        None
    };
    let (status, code) = match region {
        Some(region) if SUPPORTED_REGIONS.binary_search(&region).is_ok() => {
            (UnlockStatus::Available, UnlockCode::WebAvailable)
        }
        Some(region) if UNSUPPORTED_REGIONS.binary_search(&region).is_ok() => (
            UnlockStatus::UnsupportedRegion,
            UnlockCode::RegionUnsupported,
        ),
        _ => (UnlockStatus::Failed, UnlockCode::UnrecognizedResponse),
    };
    result(
        UnlockService::Claude,
        status,
        code,
        if status == UnlockStatus::Failed {
            None
        } else {
            region.map(str::to_owned)
        },
    )
}

fn trace_country(body: &str) -> Option<&str> {
    let (mut host, mut ip, mut region) = (None, None, None);
    for line in body.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once('=')?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return None;
        }
        match key {
            "h" => {
                if host.replace(value).is_some() {
                    return None;
                }
            }
            "ip" => {
                if ip.replace(value.parse::<IpAddr>().ok()?).is_some() {
                    return None;
                }
            }
            "loc" => {
                if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
                    return None;
                }
                if region.replace(value).is_some() {
                    return None;
                }
            }
            _ => {}
        }
    }
    if host == Some("claude.ai") && ip.is_some() {
        region
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;

    fn trace(region: &str, ip: &str) -> String {
        format!("fl=fixture\nh=claude.ai\nip={ip}\nloc={region}\nhttp=http/1.1\ntls=TLSv1.3\n")
    }

    #[test]
    fn both_address_families_use_the_same_region_policy() {
        for ip in ["203.0.113.1", "2001:db8::1"] {
            for region in ["TW", "US", "JP", "GB", "SG", "PR", "GU", "GG", "AX"] {
                let outcome = parse(200, &trace(region, ip));
                assert_eq!(outcome.status, UnlockStatus::Available, "{ip} {region}");
                assert_eq!(outcome.region.as_deref(), Some(region));
                assert!(outcome.valid());
            }
            for region in UNSUPPORTED_REGIONS {
                let outcome = parse(200, &trace(region, ip));
                assert_eq!(
                    outcome.status,
                    UnlockStatus::UnsupportedRegion,
                    "{ip} {region}"
                );
                assert_eq!(outcome.region.as_deref(), Some(*region));
                assert!(outcome.valid());
            }
        }
    }

    #[test]
    fn region_policy_is_sorted_disjoint_and_contains_only_two_letter_codes() {
        for regions in [SUPPORTED_REGIONS, UNSUPPORTED_REGIONS] {
            assert!(regions.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(regions
                .iter()
                .all(|code| code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_uppercase())));
        }
        assert_eq!(SUPPORTED_REGIONS.len() + UNSUPPORTED_REGIONS.len(), 249);
        assert!(UNSUPPORTED_REGIONS
            .iter()
            .all(|code| SUPPORTED_REGIONS.binary_search(code).is_err()));
    }

    #[test]
    fn only_complete_plaintext_service_traces_are_region_evidence() {
        for body in [
            String::new(),
            "loc=TW".into(),
            "h=claude.ai\nloc=TW\n".into(),
            trace("ZZ", "203.0.113.1"),
            trace("TWN", "203.0.113.1"),
            trace("tw", "203.0.113.1"),
            trace("TW", "invalid"),
            trace("TW", "203.0.113.1").replace("h=claude.ai", "h=www.cloudflare.com"),
            trace("TW", "203.0.113.1") + "loc=CN\n",
            trace("TW", "203.0.113.1") + "ip=2001:db8::1\n",
            trace("TW", "203.0.113.1") + "h=claude.ai\n",
            "<title>Claude</title>Log in to Claude. Continue with email.".into(),
            "<title>Just a moment...</title><script>window._cf_chl_opt={};</script>".into(),
            "<html>\nh=claude.ai\nip=203.0.113.1\nloc=TW\n</html>".into(),
        ] {
            let outcome = parse(200, &body);
            assert_eq!(outcome.status, UnlockStatus::Failed, "{body}");
            assert_eq!(outcome.code, UnlockCode::UnrecognizedResponse);
            assert!(outcome.region.is_none());
            assert!(outcome.valid());
        }
    }

    #[test]
    fn trace_values_do_not_trigger_webpage_or_cloudflare_classification() {
        let body = trace("TW", "2001:db8::1")
            + "uag=access denied; window._cf_chl_opt; not available in your country\n";
        assert_eq!(parse(200, &body).status, UnlockStatus::Available);
        for status in [204, 302, 403, 451, 500] {
            let outcome = parse(status, &trace("TW", "2001:db8::1"));
            assert_eq!(outcome.status, UnlockStatus::Failed, "{status}");
            assert_eq!(outcome.code, UnlockCode::UnrecognizedResponse);
            assert!(outcome.region.is_none());
        }
    }

    #[test]
    fn query_errors_and_redirects_never_fall_back_to_a_login_page() {
        for status in [200, 302, 403] {
            let mut calls = Vec::new();
            let outcome = detect_with(|url| {
                calls.push(url.to_owned());
                Ok(Page {
                    response: HttpResponse {
                        status,
                        body: trace("TW", "203.0.113.1"),
                        headers: [("location".into(), vec!["https://claude.ai/login".into()])]
                            .into(),
                    },
                    url: reqwest::Url::parse(TARGET).unwrap(),
                })
            })
            .unwrap();
            assert_eq!(calls, [TARGET]);
            assert_eq!(
                outcome.status,
                if status == 200 {
                    UnlockStatus::Available
                } else {
                    UnlockStatus::Failed
                }
            );
        }
        for code in [
            UnlockCode::Timeout,
            UnlockCode::NetworkError,
            UnlockCode::NoIpv6Dns,
        ] {
            assert_eq!(
                detect_with(|url| {
                    assert_eq!(url, TARGET);
                    Err(code)
                }),
                Err(code)
            );
        }
    }

    #[test]
    #[ignore = "queries Claude's region endpoint through the production family-specific HTTP transport"]
    fn live_region_query_on_both_families() {
        for (family, address_family) in [
            ("v4", crate::http::AddressFamily::V4),
            ("v6", crate::http::AddressFamily::V6),
        ] {
            let context = ProbeContext {
                client: crate::http::diagnostic_family_client(address_family).unwrap(),
                request_timeout: super::super::REQUEST_TIMEOUT,
                ipv6: family == "v6",
                trace: None,
                trace_expected: false,
                service: Some(UnlockService::Claude),
                part: brocade_deployment::unlock_probe::UnlockPart::Service,
                requests: Default::default(),
            };
            let started = Instant::now();
            let outcome = super::super::check(
                UnlockService::Claude,
                started + super::super::SERVICE_TIMEOUT,
                Some(&context),
            )
            .unwrap_or_else(|code| result(UnlockService::Claude, UnlockStatus::Failed, code, None));
            assert!(outcome.valid());
            println!(
                "{}",
                serde_json::json!({"family":family,"result":outcome,"elapsed_ms":started.elapsed().as_millis(),"requests":*context.requests.lock().unwrap()})
            );
        }
    }
}
