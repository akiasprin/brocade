//! Pure write-time and compiler checks for routing selectors. External geodata stays in Console.
use std::net::IpAddr;

use ipnet::IpNet;
use serde::Serialize;

use crate::{
    model::{DestMatch, ModelSnapshot, Rule},
    Diagnostic,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleIssue {
    pub location: String,
    pub rule: Option<usize>,
    pub field: String,
    pub value: String,
    pub message: String,
}

pub fn issue(
    location: &str,
    rule: Option<usize>,
    value: &str,
    message: impl Into<String>,
) -> RuleIssue {
    RuleIssue {
        location: location.into(),
        rule,
        field: "m".into(),
        value: value.into(),
        message: message.into(),
    }
}

pub fn valid_domain(value: &str) -> bool {
    let value = value.trim_end_matches('.');
    !value.is_empty()
        && value.len() <= 253
        && value.parse::<IpAddr>().is_err()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

pub fn valid_port(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    let port = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u16>().is_ok()
    };
    match parts.as_slice() {
        [one] => port(one),
        [first, last] => {
            port(first) && port(last) && first.parse::<u16>().ok() <= last.parse::<u16>().ok()
        }
        _ => false,
    }
}

pub fn valid_geo_selector(value: &str, ip: bool) -> bool {
    let mut parts = value.split('@');
    let code = parts.next().unwrap_or_default();
    let name = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.!".contains(&b))
    };
    let attrs: Vec<_> = parts.collect();
    name(code)
        && (!ip || attrs.len() <= 1)
        && attrs.into_iter().all(|attr| {
            if ip {
                matches!(attr.to_ascii_lowercase().as_str(), "ipv4" | "ipv6")
            } else {
                name(attr)
            }
        })
}

pub fn atoms(selector: &DestMatch) -> Vec<(&'static str, &str)> {
    let (kind, values) = match selector {
        DestMatch::DomainSuffix(v) => ("domain_suffix", v),
        DestMatch::DomainKeyword(v) => ("domain_keyword", v),
        DestMatch::Geosite(v) => ("geosite", v),
        DestMatch::Geoip(v) => ("geoip", v),
        DestMatch::IpCidr(v) => ("ip_cidr", v),
        DestMatch::Port(v) => ("port", v),
        DestMatch::Protocol(v) => ("protocol", v),
        DestMatch::DomainRegex(v) => return vec![("domain_regex", v)],
        DestMatch::All(v) => return v.iter().flat_map(atoms).collect(),
        _ => return vec![],
    };
    values.iter().map(|v| (kind, v.as_str())).collect()
}

pub fn validate_match(selector: &DestMatch, location: &str, rule: Option<usize>) -> Vec<RuleIssue> {
    let mut issues = vec![];
    if matches!(selector, DestMatch::DomainSuffix(v) | DestMatch::DomainKeyword(v) | DestMatch::Geosite(v) | DestMatch::Geoip(v) | DestMatch::IpCidr(v) | DestMatch::Port(v) | DestMatch::Protocol(v) if v.is_empty())
        || matches!(selector, DestMatch::All(v) if v.is_empty())
    {
        issues.push(issue(location, rule, "", "请填写匹配内容"));
    }
    for (kind, value) in atoms(selector) {
        let message = if value.trim().is_empty() {
            Some("请填写匹配内容")
        } else {
            match kind {
                "domain_suffix" if !valid_domain(value) => Some("不是域名"),
                "domain_keyword"
                    if value.chars().any(char::is_control)
                        || [
                            "full:", "domain:", "regexp:", "geosite:", "ext:", "keyword:",
                        ]
                        .iter()
                        .any(|p| value.to_ascii_lowercase().starts_with(p)) =>
                {
                    Some("不能带匹配前缀")
                }
                "ip_cidr"
                    if value.parse::<IpNet>().is_err() && value.parse::<IpAddr>().is_err() =>
                {
                    Some("IP 或网段无效")
                }
                "port" if !valid_port(value) => Some("端口或范围无效"),
                "geosite" if !valid_geo_selector(value, false) => Some("分类或属性无效"),
                "geoip" if !valid_geo_selector(value, true) => Some("分类或地址族无效"),
                "protocol"
                    if !matches!(
                        value,
                        "http"
                            | "http1"
                            | "http2"
                            | "tls"
                            | "quic"
                            | "bittorrent"
                            | "fakedns"
                            | "fakedns+others"
                    ) =>
                {
                    Some("协议无效")
                }
                _ => None,
            }
        };
        if let Some(message) = message {
            issues.push(issue(location, rule, value, message));
        }
    }
    if let DestMatch::All(values) = selector {
        // Validate empty nested lists as well as their atoms.
        for value in values {
            issues.extend(
                validate_match(value, location, rule)
                    .into_iter()
                    .filter(|i| i.value.is_empty()),
            );
        }
    }
    issues
}

fn canonical(kind: &str, value: &str) -> String {
    if matches!(kind, "geosite" | "geoip" | "domain_suffix" | "protocol") {
        value.trim_end_matches('.').to_ascii_lowercase()
    } else {
        value.into()
    }
}

pub fn covers(kind: &str, wider: &str, narrower: &str) -> bool {
    let wider = canonical(kind, wider);
    let narrower = canonical(kind, narrower);
    wider == narrower
        || (kind == "domain_suffix"
            && narrower
                .strip_suffix(&wider)
                .is_some_and(|p| p.ends_with('.')))
}

/// Only compare predicates when the earlier rule's remaining constraints hold for the later rule.
/// An HTTP-only match must never shadow a later unconstrained domain, even when its domain token
/// is identical.
pub fn comparable(left: &DestMatch, right: &DestMatch, kind: &str) -> bool {
    fn remaining<'a>(selector: &'a DestMatch, kind: &str) -> Vec<&'a DestMatch> {
        match selector {
            DestMatch::All(values) => values
                .iter()
                .flat_map(|value| remaining(value, kind))
                .collect(),
            _ if atoms(selector).iter().any(|(k, _)| *k == kind) => vec![],
            _ => vec![selector],
        }
    }
    let narrower = remaining(right, kind);
    remaining(left, kind)
        .iter()
        .all(|guard| narrower.contains(guard))
}

pub fn validate_rules(rules: &[Rule], location: &str) -> Vec<RuleIssue> {
    let mut issues = vec![];
    for (index, rule) in rules.iter().enumerate() {
        issues.extend(validate_match(&rule.dest_match, location, Some(index)));
        let values = atoms(&rule.dest_match);
        for (at, (kind, value)) in values.iter().enumerate() {
            let local = values.iter().enumerate().any(|(other, (k, v))| {
                k == kind
                    && other != at
                    && covers(kind, v, value)
                    && (other < at || !covers(kind, value, v))
            });
            let earlier = rules[..index].iter().enumerate().find(|(_, r)| {
                comparable(&r.dest_match, &rule.dest_match, kind)
                    && atoms(&r.dest_match)
                        .iter()
                        .any(|(k, v)| k == kind && covers(kind, v, value))
            });
            if local || earlier.is_some() {
                issues.push(issue(
                    location,
                    Some(index),
                    value,
                    if local {
                        "本条已包含".into()
                    } else {
                        format!("规则 {} 已包含", earlier.unwrap().0 + 1)
                    },
                ));
            }
        }
    }
    issues
}

pub fn validate_snapshot(snapshot: &ModelSnapshot) -> Vec<RuleIssue> {
    let mut issues = vec![];
    if !crate::cron_syntax::valid_cron(&snapshot.settings.geodata.cron) {
        let mut error = issue("settings", None, "", "cron 无效");
        error.field = "geodata.cron".into();
        issues.push(error);
    }
    for app in &snapshot.apps {
        for step in &app.steps {
            issues.extend(validate_rules(
                &step.rules,
                &format!("{}/{}/{}", app.id, step.chain, step.node),
            ));
        }
    }
    let mut policies = std::collections::BTreeMap::<String, Vec<_>>::new();
    for policy in &snapshot.node_egress_dns {
        policies
            .entry(policy.node.clone())
            .or_default()
            .push(policy);
    }
    for (node, mut entries) in policies {
        entries.sort_by_key(|p| p.position);
        let rules = entries
            .iter()
            .map(|p| Rule {
                dest_match: p.selector.clone(),
                action: crate::model::Action::Block,
            })
            .collect::<Vec<_>>();
        issues.extend(validate_rules(&rules, &format!("dns/{node}")));
    }
    issues
}

pub fn diagnostics(snapshot: &ModelSnapshot) -> Vec<Diagnostic> {
    validate_snapshot(snapshot)
        .into_iter()
        .map(|issue| {
            Diagnostic::error(
                "rule.invalid-match",
                format!(
                    "{}{}",
                    issue.location,
                    issue
                        .rule
                        .map(|i| format!("/rules/{i}/m"))
                        .unwrap_or_default()
                ),
                issue.message,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Action;
    fn rule(m: DestMatch) -> Rule {
        Rule {
            dest_match: m,
            action: Action::Block,
        }
    }
    #[test]
    fn rejects_unsafe_prefixes_and_invalid_ports() {
        for value in ["ext:/etc/passwd:cn", "geoip:cn", "192.0.2.1/33"] {
            assert!(!validate_match(&DestMatch::IpCidr(vec![value.into()]), "x", None).is_empty());
        }
        for value in ["abc", "70000", "8000-7000", "1-2-3"] {
            assert!(!valid_port(value));
        }
        for value in ["0", "443", "1-65535"] {
            assert!(valid_port(value));
        }
        assert!(!validate_match(
            &DestMatch::DomainKeyword(vec!["ext:/secret:cn".into()]),
            "x",
            None
        )
        .is_empty());
    }
    #[test]
    fn detects_ordered_shadowing_without_rejecting_intentional_exceptions() {
        let suffix = |v: &str| DestMatch::DomainSuffix(vec![v.into()]);
        assert_eq!(
            validate_rules(
                &[rule(suffix("openai.com")), rule(suffix("api.openai.com"))],
                "x"
            )
            .len(),
            1
        );
        assert!(validate_rules(
            &[rule(suffix("api.openai.com")), rule(suffix("openai.com"))],
            "x"
        )
        .is_empty());
        assert!(validate_rules(
            &[
                rule(DestMatch::All(vec![
                    suffix("openai.com"),
                    DestMatch::Port(vec!["443".into()])
                ])),
                rule(suffix("openai.com"))
            ],
            "x"
        )
        .is_empty());
        assert_eq!(
            validate_rules(
                &[rule(DestMatch::Geosite(vec![
                    "GOOGLE".into(),
                    "google".into()
                ]))],
                "x"
            )
            .len(),
            1
        );
    }

    #[test]
    fn preserves_nested_conjunction_guards_when_checking_shadowing() {
        let site = || DestMatch::Geosite(vec!["google".into()]);
        let port = || DestMatch::Port(vec!["443".into()]);
        let nested = || DestMatch::All(vec![DestMatch::All(vec![site(), port()])]);
        // The first rule only handles port 443; other ports must reach the following rule.
        assert!(validate_rules(&[rule(nested()), rule(site())], "x").is_empty());
        assert!(validate_rules(
            &[
                rule(nested()),
                rule(DestMatch::All(vec![
                    site(),
                    DestMatch::Port(vec!["80".into()])
                ]))
            ],
            "x"
        )
        .is_empty());
        // Nesting and predicate order must not hide actual duplicates or broader earlier rules.
        assert_eq!(
            validate_rules(
                &[rule(nested()), rule(DestMatch::All(vec![port(), site()]))],
                "x"
            )
            .len(),
            2
        );
        assert_eq!(
            validate_rules(&[rule(site()), rule(nested())], "x").len(),
            1
        );
    }
}
