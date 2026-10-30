//! Reconcile connection-bound evidence with the immutable job profile and reachable exits.
use brocade_core::model::{EgressDnsFallback, ModelSnapshot};
use brocade_deployment::unlock_probe::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn annotate(
    snapshot: &ModelSnapshot,
    exits: &[String],
    family: IpFamily,
    results: &mut [UnlockResult],
) {
    let mut ids = BTreeMap::new();
    for record in results
        .iter()
        .flat_map(|result| &result.connection_observations)
    {
        *ids.entry(record.connection.id.clone()).or_insert(0usize) += 1;
    }
    let output = crate::compile_cache::compile_incremental(snapshot);
    let mut profiles = BTreeMap::new();
    for exit in results
        .iter()
        .flat_map(|r| {
            r.connection_observations
                .iter()
                .map(|o| o.connection.exit.as_str())
        })
        .collect::<BTreeSet<_>>()
    {
        if !exits.iter().any(|allowed| allowed == exit) {
            continue;
        }
        let Ok(plan) = crate::compile_cache::project_node(&output, exit) else {
            continue;
        };
        let config = brocade_core::format::json::xray(&brocade_core::artifacts::xray::build(&plan));
        let Ok(config) = serde_json::from_str(&config) else {
            continue;
        };
        if let Some(profile) = UnlockProbeProfile::from_config(&config) {
            profiles.insert(exit.to_owned(), profile);
        }
    }
    for result in results {
        // Keep raw observations byte-equivalent for report replay and audit. A
        // duplicate across services invalidates attribution, not the stored input.
        let mut checked = result.clone();
        for record in &mut checked.connection_observations {
            if ids[&record.connection.id] != 1 {
                record.connection.complete = false;
            }
        }
        annotate_result(snapshot, &profiles, family, &mut checked);
        result.dns_path = checked.dns_path;
        result.dns_parts = checked.dns_parts;
    }
}

fn annotate_result(
    snapshot: &ModelSnapshot,
    profiles: &BTreeMap<String, UnlockProbeProfile>,
    family: IpFamily,
    result: &mut UnlockResult,
) {
    result.dns_path = None;
    result.dns_parts.clear();
    let parts: &[UnlockPart] = if result.service == UnlockService::YoutubePremium {
        &[UnlockPart::Webpage, UnlockPart::Premium, UnlockPart::Cdn]
    } else {
        &[UnlockPart::Service]
    };
    for part in parts {
        let records = result
            .connection_observations
            .iter()
            .filter(|o| o.part == *part)
            .collect::<Vec<_>>();
        let expected = result.connection_requests.get(part).copied().unwrap_or(0);
        let mut unknown = expected == 0 || records.len() != (expected as usize);
        let mut conditional = 0;
        let mut paths = vec![];
        let mut exits = BTreeSet::new();
        for observation in &records {
            let record = &observation.connection;
            if !result
                .service
                .probe_hosts()
                .contains(&record.domain.as_str())
            {
                unknown = true;
                continue;
            }
            let Some(profile) = profiles.get(&record.exit) else {
                unknown = true;
                continue;
            };
            let Some(normal) = brocade_core::model::probe_family_base(&record.outbound, family)
            else {
                unknown = true;
                continue;
            };
            if !record.valid()
                || !record.complete
                || record.profile != profile.fingerprint
                || profile.strategies.get(normal) != Some(&record.normal_strategy)
                || record.connected && record.family != Some(family)
                || record.lookups.len() != 1
                || record
                    .lookups
                    .iter()
                    .any(|lookup| !lookup.complete || lookup.domain != record.domain)
            {
                unknown = true;
                continue;
            }
            exits.insert(record.exit.clone());
            match measured_path(
                snapshot,
                record,
                profile.bindings.get(normal).copied(),
                family,
            ) {
                None => unknown = true,
                Some(None) => {}
                Some(Some(path)) => {
                    if path.normal_family.is_some_and(|normal| normal != family) {
                        conditional += 1;
                    }
                    if path.normal_family.is_none() {
                        unknown = true;
                    }
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
            }
        }
        paths.sort_by(|a, b| a.exit.cmp(&b.exit));
        let kind = if conditional > 0 && conditional == records.len() && !unknown {
            UnlockDnsKind::Fallback
        } else if conditional > 0 {
            UnlockDnsKind::Partial
        } else if unknown {
            UnlockDnsKind::Unknown
        } else {
            UnlockDnsKind::Direct
        };
        result.dns_parts.push(UnlockDnsPartPath {
            part: *part,
            kind,
            paths,
            exits: exits.into_iter().collect(),
        });
    }
    // This aggregate remains useful to non-split consumers, but is never used to
    // overwrite the independently derived YouTube webpage/Premium/CDN facts.
    let mut aggregate = result.dns_parts.iter().flat_map(|part| part.paths.iter());
    if result
        .dns_parts
        .iter()
        .all(|part| part.kind == UnlockDnsKind::Fallback)
    {
        if let Some(first) = aggregate.next() {
            if aggregate.all(|path| path == first) {
                result.dns_path = Some(first.clone());
            }
        }
    }
}

// Outer None means insufficient evidence; Some(None) is a measured primary answer.
fn measured_path(
    snapshot: &ModelSnapshot,
    record: &UnlockConnectionTrace,
    binding: Option<IpFamily>,
    family: IpFamily,
) -> Option<Option<UnlockDnsPath>> {
    let lookup = record.lookups.first()?;
    let first = lookup.steps.first()?;
    let policies = snapshot
        .node_egress_dns
        .iter()
        .filter(|p| p.node == record.exit)
        .filter_map(|p| brocade_core::physical::node::egress_dns_policy_tag(p).map(|tag| (tag, p)))
        .collect::<BTreeMap<_, _>>();
    for step in &lookup.steps {
        if step.scoped != policies.contains_key(&step.tag) {
            return None;
        }
        if let Some(policy) = policies.get(&step.tag) {
            let count = policy.resolution.address.as_slice().len();
            if count > 1 && step.members.is_empty()
                || step
                    .members
                    .iter()
                    .any(|member| member.index == 0 || member.index as usize > count)
                || step
                    .winner
                    .is_some_and(|winner| winner == 0 || winner as usize > count)
            {
                return None;
            }
            if !step.members.is_empty() && step.winner.is_none() {
                let all_empty = step
                    .members
                    .iter()
                    .all(|member| member.outcome == UnlockDnsOutcome::Empty);
                let any_other = step.members.iter().any(|member| member.other);
                if step.members.len() != count
                    || step.outcome
                        != if all_empty {
                            UnlockDnsOutcome::Empty
                        } else {
                            UnlockDnsOutcome::Error
                        }
                    || step.other != (all_empty && any_other)
                {
                    return None;
                }
            }
        }
    }
    if first.outcome == UnlockDnsOutcome::Answer && lookup.steps.len() == 1 {
        return Some(None);
    }
    if !first.scoped
        || !matches!(
            first.outcome,
            UnlockDnsOutcome::Empty | UnlockDnsOutcome::Error
        )
    {
        return None;
    }
    let mut used = BTreeMap::new();
    let mut terminal = None;
    let mut source = None;
    for (index, step) in lookup.steps.iter().enumerate() {
        let last = index + 1 == lookup.steps.len();
        if step.scoped {
            let policy = policies.get(&step.tag)?;
            used.insert(policy.position, (*policy, step));
            match step.outcome {
                UnlockDnsOutcome::Answer => {
                    if !last {
                        return None;
                    };
                    terminal = Some(EgressDnsFallback::Machine);
                    source = Some(UnlockDnsSource::Policy);
                }
                UnlockDnsOutcome::Empty | UnlockDnsOutcome::Error => {
                    if policy.resolution.fallback == EgressDnsFallback::Stop {
                        if !last
                            || record.connected
                            || !matches!(record.error.as_deref(), Some("dns_empty" | "dns_error"))
                        {
                            return None;
                        };
                        terminal = Some(EgressDnsFallback::Stop);
                    }
                }
            }
        } else {
            if !last || step.outcome != UnlockDnsOutcome::Answer || policies.contains_key(&step.tag)
            {
                return None;
            }
            terminal = Some(EgressDnsFallback::Machine);
            source = Some(UnlockDnsSource::Machine);
        }
    }
    let normal_family = normal_family(record, binding, family, first, source.is_some());
    Some(Some(UnlockDnsPath {
        exit: record.exit.clone(),
        normal_family,
        fallback: terminal?,
        rules: used
            .values()
            .map(|(p, step)| UnlockDnsRule {
                selector: p.selector.clone(),
                resolution: p.resolution.clone(),
                selected_member: step.winner,
                members: step.members.clone(),
            })
            .collect(),
        answer_from: source,
        reason: Some(if first.outcome == UnlockDnsOutcome::Error {
            UnlockDnsReason::ResolverError
        } else {
            UnlockDnsReason::NoAddress
        }),
    }))
}

fn normal_family(
    record: &UnlockConnectionTrace,
    binding: Option<IpFamily>,
    family: IpFamily,
    first: &UnlockDnsStep,
    answered: bool,
) -> Option<IpFamily> {
    let other = match family {
        IpFamily::V4 => IpFamily::V6,
        IpFamily::V6 => IpFamily::V4,
    };
    let positive_other = first.outcome == UnlockDnsOutcome::Empty && first.other;
    let (preferred, ordered) = match record.normal_strategy.as_str() {
        "UseIP" => (None, false),
        "UseIPv4" | "ForceIPv4" => (Some(IpFamily::V4), false),
        "UseIPv6" | "ForceIPv6" => (Some(IpFamily::V6), false),
        "UseIPv4v6" => (Some(IpFamily::V4), true),
        "UseIPv6v4" => (Some(IpFamily::V6), true),
        _ => return None,
    };
    if let Some(bound) = binding {
        if preferred.is_some_and(|preferred| preferred != bound) && !ordered {
            return None;
        }
        return if bound == family && answered {
            Some(family)
        } else if bound == other && positive_other {
            Some(other)
        } else {
            None
        };
    }
    if preferred == Some(family) && answered {
        return Some(family);
    }
    if positive_other && (preferred.is_none() || preferred == Some(other) || ordered) {
        return Some(other);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use brocade_core::model::*;

    #[test]
    fn racing_group_records_the_adopted_member_of_a_later_policy() {
        let (mut snapshot, profiles, mut result) = fixture();
        let mut group = snapshot.node_egress_dns[0].clone();
        group.position += 1;
        group.selector = DestMatch::DomainSuffix(vec!["google.com".into()]);
        group.resolution.address = vec!["192.0.2.53".into(), "198.51.100.53".into()].into();
        let tag = brocade_core::physical::node::egress_dns_policy_tag(&group).unwrap();
        snapshot.node_egress_dns.push(group);
        let lookup = &mut result.connection_observations[0].connection.lookups[0];
        lookup.steps.truncate(1);
        lookup.steps.push(UnlockDnsStep {
            tag,
            scoped: true,
            outcome: UnlockDnsOutcome::Answer,
            other: false,
            winner: Some(2),
            members: vec![UnlockDnsMember {
                index: 2,
                outcome: UnlockDnsOutcome::Answer,
                other: false,
                attempts: 1,
                elapsed_ms: 12,
                rcode: 0,
                cache: "miss".into(),
            }],
        });
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Fallback);
        let path = &result.dns_parts[0].paths[0];
        assert_eq!(path.answer_from, Some(UnlockDnsSource::Policy));
        assert_eq!(path.rules[1].selected_member, Some(2));
        assert_eq!(path.rules[1].members[0].attempts, 1);
        result.connection_observations[0].connection.lookups[0].steps[1].winner = Some(3);
        result.connection_observations[0].connection.lookups[0].steps[1].members[0].index = 3;
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Unknown);
        assert!(result.dns_parts[0].paths.is_empty());
    }

    #[test]
    fn incomplete_group_failure_cannot_claim_a_missing_family() {
        let (mut snapshot, profiles, mut result) = fixture();
        snapshot.node_egress_dns[0].resolution.address =
            vec!["192.0.2.53".into(), "198.51.100.53".into()].into();
        let tag = brocade_core::physical::node::egress_dns_policy_tag(&snapshot.node_egress_dns[0])
            .unwrap();
        let first = &mut result.connection_observations[0].connection.lookups[0].steps[0];
        first.tag = tag;
        first.members = vec![UnlockDnsMember {
            index: 1,
            outcome: UnlockDnsOutcome::Empty,
            other: true,
            attempts: 1,
            elapsed_ms: 10,
            rcode: 0,
            cache: "miss".into(),
        }];
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Unknown);
        assert!(result.dns_parts[0].paths.is_empty());
    }

    fn fixture() -> (
        ModelSnapshot,
        BTreeMap<String, UnlockProbeProfile>,
        UnlockResult,
    ) {
        let policy = NodeEgressDnsPolicy {
            node: "mo".into(),
            position: 4,
            selector: DestMatch::Geosite(vec!["google".into()]),
            resolution: EgressDnsResolution {
                address: "1.1.1.1".into(),
                port: 53,
                transport: EgressDnsTransport::Udp,
                address_strategy: EgressDnsAddressStrategy::UseIpv6,
                fallback: EgressDnsFallback::Machine,
            },
        };
        let tag = brocade_core::physical::node::egress_dns_policy_tag(&policy).unwrap();
        let mut duplicate = policy.clone();
        duplicate.node = "other".into();
        let snapshot = ModelSnapshot {
            revision: 1,
            overlay_cidr: "10.66.0.0/16".parse().unwrap(),
            settings: Default::default(),
            nodes: vec![],
            node_egress_dns: vec![policy, duplicate],
            users: vec![],
            external_outbounds: vec![],
            apps: vec![],
        };
        let profile = UnlockProbeProfile {
            fingerprint: "a".repeat(64),
            strategies: BTreeMap::from([("out:forward".into(), "UseIP".into())]),
            bindings: Default::default(),
        };
        let profiles = BTreeMap::from([("mo".into(), profile.clone()), ("other".into(), profile)]);
        let result:UnlockResult=serde_json::from_value(serde_json::json!({"service":"gemini","status":"available","code":"web_available","region":"TR","connection_requests":{"service":1},"connection_observations":[{"part":"service","version":2,"id":"00000000000000000000000000000001","domain":"gemini.google.com","exit":"mo","outbound":"out:forward:probe-v4","profile":"a".repeat(64),"normal_strategy":"UseIP","family":"v4","connected":true,"complete":true,"lookups":[{"version":1,"domain":"gemini.google.com","strategy":"FORCE_IP4","complete":true,"steps":[{"tag":tag,"scoped":true,"outcome":"empty","other":true},{"tag":"xray.system.opaque","scoped":false,"outcome":"answer","other":false}]}]}]})).unwrap();
        (snapshot, profiles, result)
    }
    #[test]
    fn actual_origin_disambiguates_identical_policies_across_multiple_exits() {
        let (snapshot, profiles, mut result) = fixture();
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_path.as_ref().unwrap().exit, "mo");
        assert_eq!(
            result.dns_path.as_ref().unwrap().normal_family,
            Some(IpFamily::V6)
        );
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Fallback);
    }
    #[test]
    fn stale_unreachable_wrong_family_and_missing_records_are_unknown() {
        for case in 0..6 {
            let (snapshot, profiles, mut result) = fixture();
            let record = &mut result.connection_observations[0].connection;
            match case {
                0 => record.exit = "forged".into(),
                1 => record.profile = "b".repeat(64),
                2 => record.family = Some(IpFamily::V6),
                3 => record.lookups[0].complete = false,
                4 => record.lookups[0].steps[0].tag = "dns:stale".into(),
                _ => {
                    result.connection_requests.insert(UnlockPart::Service, 2);
                }
            }
            annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
            assert!(result.dns_path.is_none(), "case {case}");
            assert!(matches!(
                result.dns_parts[0].kind,
                UnlockDnsKind::Unknown | UnlockDnsKind::Partial
            ));
        }
    }
    #[test]
    fn mixed_connections_are_partial_and_normal_fallback_is_not_excluded() {
        let (snapshot, mut profiles, mut result) = fixture();
        let mut primary = result.connection_observations[0].clone();
        primary.connection.id = "00000000000000000000000000000002".into();
        primary.connection.lookups[0].steps.truncate(1);
        primary.connection.lookups[0].steps[0].outcome = UnlockDnsOutcome::Answer;
        result.connection_observations.push(primary);
        result.connection_requests.insert(UnlockPart::Service, 2);
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Partial);
        assert!(result.dns_path.is_none());
        profiles
            .get_mut("mo")
            .unwrap()
            .strategies
            .insert("out:forward".into(), "UseIPv4".into());
        for observation in &mut result.connection_observations {
            observation.connection.normal_strategy = "UseIPv4".into();
        }
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Direct);
        assert!(result.dns_path.is_none());
    }
    #[test]
    fn youtube_parts_retain_distinct_exits_and_missing_parts_stay_unknown() {
        let (snapshot, profiles, mut result) = fixture();
        result.service = UnlockService::YoutubePremium;
        result.connection_requests = BTreeMap::from([
            (UnlockPart::Webpage, 1),
            (UnlockPart::Premium, 1),
            (UnlockPart::Cdn, 1),
        ]);
        result.connection_observations[0].part = UnlockPart::Webpage;
        result.connection_observations[0].connection.domain = "www.youtube.com".into();
        result.connection_observations[0].connection.lookups[0].domain = "www.youtube.com".into();
        let mut premium = result.connection_observations[0].clone();
        premium.part = UnlockPart::Premium;
        premium.connection.exit = "other".into();
        premium.connection.id = "00000000000000000000000000000002".into();
        result.connection_observations.push(premium);
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].exits, vec!["mo"]);
        assert_eq!(result.dns_parts[1].exits, vec!["other"]);
        assert_eq!(result.dns_parts[2].kind, UnlockDnsKind::Unknown);
        assert!(result.dns_path.is_none());
    }
    #[test]
    fn normal_strategy_and_source_binding_control_conditional_exclusion() {
        let (_, _, result) = fixture();
        let mut record = result.connection_observations[0].connection.clone();
        for (strategy, binding, expected) in [
            ("UseIP", None, Some(IpFamily::V6)),
            ("UseIPv4", None, Some(IpFamily::V4)),
            ("ForceIPv4", None, Some(IpFamily::V4)),
            ("UseIPv6", None, Some(IpFamily::V6)),
            ("UseIPv4v6", None, Some(IpFamily::V4)),
            ("UseIPv6v4", None, Some(IpFamily::V6)),
            ("UseIP", Some(IpFamily::V4), Some(IpFamily::V4)),
            ("UseIP", Some(IpFamily::V6), Some(IpFamily::V6)),
            ("UseIPv6", Some(IpFamily::V4), None),
            ("AsIs", None, None),
        ] {
            record.normal_strategy = strategy.into();
            assert_eq!(
                normal_family(
                    &record,
                    binding,
                    IpFamily::V4,
                    &record.lookups[0].steps[0],
                    true
                ),
                expected,
                "{strategy} {binding:?}"
            );
        }
    }
    #[test]
    fn resolver_errors_keep_real_fallback_without_claiming_an_absent_family() {
        let (snapshot, profiles, mut result) = fixture();
        result.connection_observations[0].connection.lookups[0].steps[0].outcome =
            UnlockDnsOutcome::Error;
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        let part = &result.dns_parts[0];
        assert_eq!(part.kind, UnlockDnsKind::Unknown);
        assert_eq!(part.paths[0].normal_family, None);
        assert_eq!(part.paths[0].answer_from, Some(UnlockDnsSource::Machine));
        assert_eq!(part.paths[0].reason, Some(UnlockDnsReason::ResolverError));
        assert!(result.dns_path.is_none());
    }
    #[test]
    fn stopped_resolution_requires_a_failed_connection_and_matching_stop_policy() {
        let (mut snapshot, profiles, mut result) = fixture();
        snapshot.node_egress_dns[0].resolution.fallback = EgressDnsFallback::Stop;
        let tag = brocade_core::physical::node::egress_dns_policy_tag(&snapshot.node_egress_dns[0])
            .unwrap();
        let record = &mut result.connection_observations[0].connection;
        record.lookups[0].steps.truncate(1);
        record.lookups[0].steps[0].tag = tag;
        record.connected = false;
        record.family = None;
        record.error = Some("dns_empty".into());
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(
            result.dns_parts[0].paths[0].fallback,
            EgressDnsFallback::Stop
        );
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Fallback);
        result.connection_observations[0].connection.connected = true;
        result.connection_observations[0].connection.family = Some(IpFamily::V4);
        annotate_result(&snapshot, &profiles, IpFamily::V4, &mut result);
        assert_eq!(result.dns_parts[0].kind, UnlockDnsKind::Unknown);
    }
}
