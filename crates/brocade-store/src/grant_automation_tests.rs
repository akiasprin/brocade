use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
};

use brocade_core::model::{
    Action, AppView, Chain, DestMatch, Dns, DomainStrategy, Grant, Ingress, IngressWires,
    ModelSnapshot, Node, Rule, Step, Transport, User, WireGuardKeys,
};
use brocade_deployment::plan::{plan_deployment, DesiredGrants};
use ipnet::Ipv4Net;

use super::compiled_grants;

fn any_egress() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

fn model() -> ModelSnapshot {
    let mut model = snapshot(vec![
        node("hk", [10, 66, 0, 1], Dns::System),
        node("spare", [10, 66, 0, 2], Dns::System),
    ]);
    model.apps = vec![app("app", "chain", "ingress", 443, any_egress())];
    model
}

fn legacy(snapshot: &ModelSnapshot) -> Option<BTreeMap<String, DesiredGrants>> {
    plan_deployment(snapshot, &[]).ok().map(|plan| {
        plan.targets
            .into_iter()
            .map(|target| (target.node_id, target.desired.grants))
            .collect()
    })
}

#[tokio::test]
async fn permission_projection_keeps_running_accounts_when_an_ingress_changes_group() {
    let base = model();
    let expected = compiled_grants(&base).await.unwrap();
    assert!(
        expected.is_some(),
        "the running topology must be publishable"
    );
    let mut moved = base.clone();
    moved.apps[0].id = "new-group".to_owned();
    let projected = crate::serving::permission_projection(base.clone(), &moved);
    assert_eq!(
        compiled_grants(&projected).await.unwrap(),
        expected,
        "moving a stable ingress must not revoke accounts on its still-running topology"
    );

    moved.apps[0].grants.clear();
    let revoked = crate::serving::permission_projection(base, &moved);
    assert_ne!(
        compiled_grants(&revoked).await.unwrap(),
        expected,
        "an explicit revocation after moving must still reach the running ingress"
    );
}

#[tokio::test]
async fn grants_short_path_matches_full_plans_for_permission_changes() {
    let base = model();
    let mut cases = Vec::new();
    let mut revoked = base.clone();
    revoked.apps[0].grants.clear();
    cases.push(("revoke", revoked));
    let mut moved = base.clone();
    moved.apps[0].ingresses[0].node = "spare".to_owned();
    moved.apps[0].steps[0].node = "spare".to_owned();
    cases.push(("move-ingress", moved));
    let mut rotated = base.clone();
    rotated.users[0].uuid = "rotated-uuid".to_owned();
    cases.push(("rotate-user", rotated));
    let mut flow = base.clone();
    let IngressWires::Vless(Transport::VlessReality(reality)) =
        &mut flow.apps[0].ingresses[0].wires
    else {
        panic!("fixture transport")
    };
    reality.flow = Some("xtls-rprx-vision".to_owned());
    cases.push(("flow", flow));
    let mut retired = base.clone();
    retired.nodes[0].retired = true;
    cases.push(("retire", retired));
    let mut deleted = base.clone();
    deleted.apps.clear();
    cases.push(("delete-chain", deleted));
    let mut mtproto = base.clone();
    let previous = mtproto.apps[0].ingresses[0].wires.clone();
    mtproto.apps[0].ingresses[0].wires = IngressWires::WithMtProto {
        other: Some(Box::new(previous)),
        mtproto: Default::default(),
    };
    cases.push(("additional-protocol", mtproto));
    let expected = legacy(&base).expect("publishable fixture");
    assert_eq!(
        compiled_grants(&base).await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        expected["spare"],
        DesiredGrants::Present {
            inbounds: Vec::new()
        }
    );
    for (label, mut candidate) in cases {
        candidate.revision += 1;
        let old = legacy(&candidate).unwrap_or_else(|| panic!("invalid {label} fixture"));
        assert_ne!(old, expected, "{label} must change accounts");
        assert_eq!(
            compiled_grants(&candidate).await.unwrap(),
            Some(old),
            "{label}"
        );
    }
}

#[tokio::test]
async fn grants_short_path_ignores_configuration_only_changes_and_keeps_validation() {
    let base = model();
    let expected = compiled_grants(&base).await.unwrap().unwrap();
    let mut config = base.clone();
    config.revision += 1;
    config.nodes[0].public_ipv4 = Some("new.example.net".to_owned());
    config.nodes[0].name = "renamed".to_owned();
    config.apps[0].ingresses[0].port = 8443;
    config.apps[0].steps[0].rules.insert(
        0,
        Rule {
            dest_match: DestMatch::DomainSuffix(vec!["example.org".to_owned()]),
            action: Action::Block,
        },
    );
    assert_eq!(legacy(&config), Some(expected.clone()));
    assert_eq!(compiled_grants(&config).await.unwrap(), Some(expected));
    let mut invalid = config;
    invalid.users.push(User {
        id: "duplicate".to_owned(),
        ..invalid.users[0].clone()
    });
    assert_eq!(legacy(&invalid), None);
    assert_eq!(
        compiled_grants(&invalid).await.unwrap(),
        None,
        "invalid models must still queue conservative reconciliation"
    );
}

#[tokio::test]
#[ignore = "local release-profile timing comparison; no production data"]
async fn grants_short_path_timing() {
    use std::{hint::black_box, time::Instant};
    let mut model = snapshot(
        (1..=24)
            .map(|i| node(&format!("n{i}"), [10, 66, 0, i], Dns::System))
            .collect(),
    );
    for i in 0..96 {
        let mut project = app(
            &format!("app{i}"),
            &format!("chain{i}"),
            &format!("ingress{i}"),
            20_000 + i,
            any_egress(),
        );
        let node_id = format!("n{}", i % 24 + 1);
        project.ingresses[0].node = node_id.clone();
        project.steps[0].node = node_id;
        for rule in 0..20 {
            project.steps[0].rules.insert(
                0,
                Rule {
                    dest_match: DestMatch::DomainSuffix(vec![format!("rule{rule}.example.org")]),
                    action: Action::Block,
                },
            );
        }
        model.apps.push(project);
    }
    let start = Instant::now();
    let expected = legacy(&model).expect("publishable benchmark fixture");
    let legacy_first_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    assert_eq!(
        compiled_grants(&model).await.unwrap(),
        Some(expected.clone())
    );
    let short_first_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut old_times = Vec::new();
    let mut new_times = Vec::new();
    let mut cached_times = Vec::new();
    let mut previous = model.clone();
    for revision in 0..10 {
        let mut edited = model.clone();
        edited.revision += revision + 1;
        edited.apps[0].ingresses[0].port += revision as u16 + 1;
        let old_first = revision % 2 == 0;
        for old in [old_first, !old_first] {
            let start = Instant::now();
            let result = if old {
                (legacy(black_box(&previous)), legacy(black_box(&edited)))
            } else {
                (
                    compiled_grants(black_box(&previous)).await.unwrap(),
                    compiled_grants(black_box(&edited)).await.unwrap(),
                )
            };
            let millis = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(result, (Some(expected.clone()), Some(expected.clone())));
            if old {
                old_times.push(millis);
            } else {
                new_times.push(millis);
            }
        }
        let start = Instant::now();
        let cached = compiled_grants(black_box(&edited)).await.unwrap();
        cached_times.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(cached, Some(expected.clone()));
        previous = edited;
    }
    old_times.sort_by(f64::total_cmp);
    new_times.sort_by(f64::total_cmp);
    cached_times.sort_by(f64::total_cmp);
    eprintln!("grants benchmark: nodes=24 chains=96 rules=2016 first_full_ms={legacy_first_ms:.3} first_short_ms={short_first_ms:.3} revision_pair_full_median_ms={:.3} revision_pair_short_median_ms={:.3} cached_short_median_ms={:.3}", old_times[old_times.len()/2], new_times[new_times.len()/2], cached_times[cached_times.len()/2]);
}

fn snapshot(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 51,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: vec![User {
            tenant: "platform.acme".to_owned(),
            id: "alice".to_owned(),
            uuid: "uuid-alice".to_owned(),
        }],
        external_outbounds: Vec::new(),
        apps: Vec::new(),
    }
}

fn node(id: &str, overlay: [u8; 4], dns: Dns) -> Node {
    Node {
        mtu: None,
        connection: Default::default(),
        retired: false,
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        public_ipv4: Some(format!("{id}.example.net")),
        public_ipv6: None,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        overlay_addr: Ipv4Addr::from(overlay),
        certificate_name: None,
        certificate_names: Vec::new(),
        certificate_group_id: None,
        certificate_track: None,
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: true,
        egress_allowed: true,
        dns,
        domain_strategy: DomainStrategy::default(),
    }
}

fn app(id: &str, chain_id: &str, ingress_id: &str, port: u16, default_rule: Rule) -> AppView {
    AppView {
        id: id.to_owned(),
        label: id.to_owned(),
        chains: vec![Chain {
            id: chain_id.to_owned(),
            tenant: "platform.acme".to_owned(),
            name: chain_id.to_owned(),
            subscription_country: None,
        }],
        ingresses: vec![Ingress {
            id: ingress_id.to_owned(),
            chain: chain_id.to_owned(),
            node: "hk".to_owned(),
            bind: IpAddr::from(Ipv4Addr::UNSPECIFIED),
            port,
            front: None,
            projection: Default::default(),
            guard: brocade_core::model::IngressGuard::OPEN,
            identity: brocade_core::model::IngressIdentity {
                private_key: format!("priv-{ingress_id}"),
                public_key: format!("pub-{ingress_id}"),
                short_ids: vec!["0123abcd".to_owned()],
            },
            anytls_identity: None,
            wires: IngressWires::Vless(Transport::VlessReality(
                brocade_core::model::RealitySettings {
                    dest: "www.example.com:443".to_owned(),
                    server_names: vec!["www.example.com".to_owned()],
                    fingerprint: "chrome".to_owned(),
                    flow: None,
                    fallback_mode: Default::default(),
                    fallback_guard: true,
                    fallback_limits: Default::default(),
                },
            )),
        }],
        fronts: Vec::new(),
        steps: vec![Step {
            chain: chain_id.to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![default_rule],
        }],
        grants: vec![Grant {
            tenant: "platform.acme".to_owned(),
            user: "alice".to_owned(),
            ingress: ingress_id.to_owned(),
        }],
    }
}
