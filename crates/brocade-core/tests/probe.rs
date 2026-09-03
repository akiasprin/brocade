//! The end-to-end probe work list.
//!
//! These assertions watch one thing: the path a probe takes must be identical to a real
//! user's. Hence the repeated comparison of `project_user`'s result against
//! `project_probe`'s below — any parameter that diverges between the two measures the
//! health of a different path, and presents it as "the chain is fine", which is worse than
//! not probing at all.

use brocade_core::{
    compile::compile,
    model::{is_probe_label, parse_grant_label, probe_uuid},
    physical::{probe::ProbeSecurity, user::UserSecurityPlan},
};

mod fixture;
use fixture::demo_snapshot;

/// The work list corresponds strictly to the ingresses on this machine — no more, no less.
///
/// More means having a machine dial someone else's port (which it cannot reach, producing a
/// steady false fault); fewer means a chain nobody probes, and that may be exactly the
/// broken one.
#[test]
fn probe_targets_match_this_nodes_own_ingresses() {
    let output = compile(&demo_snapshot());
    let ir = output.unpublishable_view();

    let mut total = 0;
    for node_id in ["au-01", "hk-01", "jp-01", "sg-01", "us-01"] {
        let mine = ir
            .apps
            .iter()
            .flat_map(|app| app.ingresses.iter())
            .filter(|ingress| ingress.node == node_id)
            .map(|ingress| ingress.id.clone())
            .collect::<std::collections::BTreeSet<_>>();

        let targets = output
            .project_probe(node_id)
            .unwrap()
            .targets
            .into_iter()
            .map(|target| target.ingress_id)
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(targets, mine, "{node_id}");
        total += targets.len();
    }
    assert!(total >= 4, "演示里该有好几个入口，实际 {total}");
}

/// The probe parameters align field by field with what subscriptions hand users.
#[test]
fn probe_dials_exactly_what_a_real_user_dials() {
    let output = compile(&demo_snapshot());
    let alice = output.project_user("platform.acme", "alice").unwrap();

    let mut checked = 0;
    for node_id in ["hk-01", "jp-01", "sg-01", "us-01", "au-01"] {
        for target in output.project_probe(node_id).unwrap().targets {
            let Some(entry) = alice
                .entries
                .iter()
                .find(|entry| entry.ingress_id == target.ingress_id)
            else {
                continue;
            };

            assert_eq!(target.port, entry.port, "{}", target.ingress_id);
            // Every parameter, compared as one value rather than field by field: a probe is only
            // a measurement of the path a subscriber takes while the two are handed identical
            // parameters, and a field added to one side and not the other would slip past a
            // list of named comparisons. flow is in here for the sharpest version of that — it
            // decides whether Vision is used, and those are two different data planes.
            assert_eq!(
                probe_params(&target.security),
                user_params(&entry.security),
                "{}",
                target.ingress_id
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "至少要比到两个入口，实际 {checked}");
}

/// It dials the local machine rather than its own public IP. Going over the public internet
/// also measures inbound routing, which is not a property of this chain — and when inbound
/// routing is down, the chain itself may be fine.
#[test]
fn probe_dials_the_local_address_not_the_public_one() {
    let output = compile(&demo_snapshot());
    for node_id in ["hk-01", "jp-01", "sg-01", "us-01"] {
        for target in output.project_probe(node_id).unwrap().targets {
            assert!(
                matches!(target.dial_host.as_str(), "127.0.0.1" | "::1"),
                "{} 拨的是 {}",
                target.ingress_id,
                target.dial_host
            );
        }
    }
}

/// The probe identity is a credential: it must match the one in the ingress, and must not
/// be any real user.
#[test]
fn probe_identity_rides_the_grant_channel() {
    let output = compile(&demo_snapshot());

    for node_id in ["hk-01", "jp-01", "sg-01", "us-01"] {
        let plan = output.project_node(node_id).unwrap();
        let probe = output.project_probe(node_id).unwrap();

        for target in &probe.targets {
            let update = plan
                .grant_sync
                .updates
                .iter()
                .find(|update| update.inbound_tag.ends_with(&target.ingress_id))
                .unwrap_or_else(|| panic!("{} 没有对应的授权下发", target.ingress_id));

            let client = update
                .clients
                .iter()
                .find(|client| is_probe_label(&client.label))
                .unwrap_or_else(|| panic!("{} 的下发里没有探测凭据", target.ingress_id));

            // The UUID in the work list and the one shipped to xray must be the same. Were
            // they not, the probe would knock with a credential xray does not know, and the
            // symptom — a failed handshake — looks exactly like a genuinely broken
            // chain.
            assert_eq!(client.uuid, target.uuid, "{}", target.ingress_id);
        }
    }
}

/// The probe credential must not be taken for a user: billing and subscriptions both
/// identify people by `{user}@{tenant}#{ingress}`.
#[test]
fn probe_identity_is_invisible_to_billing_and_subscriptions() {
    let output = compile(&demo_snapshot());

    for node_id in ["hk-01", "jp-01", "sg-01", "us-01", "au-01"] {
        for update in output.project_node(node_id).unwrap().grant_sync.updates {
            for client in update.clients {
                if is_probe_label(&client.label) {
                    assert_eq!(
                        parse_grant_label(&client.label),
                        None,
                        "探测凭据被当成了用户：{}",
                        client.label
                    );
                }
            }
        }
    }

    // The subscription side pins it down from the other direction: neither user's entries
    // may contain a single probe.
    for user_id in ["alice", "bob"] {
        let plan = output.project_user("platform.acme", user_id).unwrap();
        assert!(!is_probe_label(&plan.user), "{user_id} 自己就是探测身份？");
        for entry in plan.entries {
            assert!(
                !is_probe_label(&entry.grant_id),
                "{user_id} 的订阅里混进了探测条目：{}",
                entry.grant_id
            );
        }
    }
}

/// Expected exits are a set and the test is membership. A forking chain has several egress
/// nodes, and given a single value it would be reliably misreported as exiting in the wrong
/// place.
#[test]
fn expected_exits_cover_every_egress_node_of_the_chain() {
    let output = compile(&demo_snapshot());
    let ir = output.unpublishable_view();

    let mut saw_multi = false;
    for node_id in ["hk-01", "jp-01", "sg-01", "us-01"] {
        for target in output.project_probe(node_id).unwrap().targets {
            assert!(
                !target.exit_nodes.is_empty(),
                "{} 一个出口都没有",
                target.chain_id
            );
            saw_multi |= target.exit_nodes.len() > 1;

            // Membership is asserted in both directions. Present-when-it-should-be: one address
            // missing and the probe reports the wrong exit whenever traffic happens to leave
            // through that machine — a false alarm appearing in only some rounds. Absent-when-it
            // -should-be: NAT on either family withdraws that machine's addresses entirely, and a
            // one-way assertion would not notice a family creeping back in, which is the false
            // accusation this rule exists to prevent (`physical/probe.rs::public_addresses`).
            for exit in &target.exit_nodes {
                let node = ir
                    .apps
                    .iter()
                    .flat_map(|app| app.nodes.iter())
                    .find(|node| node.id == *exit)
                    .unwrap();
                let behind_nat = (node.public_ipv4.is_some() && node.public_ipv4_nat)
                    || (node.public_ipv6.is_some() && node.public_ipv6_nat);
                for address in [node.public_ipv4.as_ref(), node.public_ipv6.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    assert_eq!(
                        target.expected_exit_ips.contains(address),
                        !behind_nat,
                        "{} 的期望出口对 {exit} 的 {address} 判断错了（behind_nat={behind_nat}）",
                        target.chain_id
                    );
                }
            }
        }
    }
    assert!(saw_multi, "演示里该有分叉链，否则这个测试没测到要害");
}

/// The UUID is derived from the private key, so knowing the ingress id alone does not yield
/// it.
#[test]
fn probe_uuid_cannot_be_derived_from_public_information() {
    let output = compile(&demo_snapshot());
    for target in output.project_probe("hk-01").unwrap().targets {
        assert_ne!(
            target.uuid,
            probe_uuid("", &target.ingress_id),
            "空私钥就能算出 {} 的探测凭据",
            target.ingress_id
        );
    }
}

/// The probe's parameters as a comparable tuple: security layer, then the four values a client
/// needs, then flow.
fn probe_params(
    security: &ProbeSecurity,
) -> (&'static str, String, String, String, String, Option<String>) {
    match security {
        ProbeSecurity::Reality(reality) => (
            "reality",
            reality.public_key.clone(),
            reality.short_id.clone(),
            reality.server_name.clone(),
            reality.fingerprint.clone(),
            reality.flow.clone(),
        ),
        ProbeSecurity::Tls(tls) => (
            "tls",
            String::new(),
            String::new(),
            tls.server_name.clone(),
            String::new(),
            tls.flow.clone(),
        ),
        ProbeSecurity::Hysteria2(hysteria) => (
            "hysteria2",
            serde_json::to_string(&hysteria.settings).expect("serialize Hysteria 2 settings"),
            String::new(),
            hysteria.server_name.clone(),
            String::new(),
            None,
        ),
    }
}

fn user_params(
    security: &UserSecurityPlan,
) -> (&'static str, String, String, String, String, Option<String>) {
    match security {
        UserSecurityPlan::Reality(reality) => (
            "reality",
            reality.public_key.clone(),
            reality.short_id.clone(),
            reality.server_name.clone(),
            reality.fingerprint.clone(),
            reality.flow.clone(),
        ),
        UserSecurityPlan::Tls(tls) => (
            "tls",
            String::new(),
            String::new(),
            tls.server_name.clone(),
            String::new(),
            tls.flow.clone(),
        ),
        UserSecurityPlan::Hysteria2(hysteria) => (
            "hysteria2",
            serde_json::to_string(&hysteria.settings).expect("serialize Hysteria 2 settings"),
            String::new(),
            hysteria.server_name.clone(),
            String::new(),
            None,
        ),
    }
}
