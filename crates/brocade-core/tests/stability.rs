use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    artifacts::{grants, subscription, xray},
    compile::compile,
    format::{json, uri, yaml},
    model::{
        Accept, Action, AppView, Chain, DestMatch, Dns, DomainStrategy, Grant, HopDial, HopIn,
        HopPool, HopWire, Ingress, IngressWires, ModelSnapshot, Node, Rule, Step, Transport, User,
        WireGuardKeys,
    },
};
use ipnet::Ipv4Net;

#[test]
fn formatted_artifacts_are_stable_across_unordered_model_collections() {
    let stable = artifact_texts(snapshot());
    let mut reordered = snapshot();

    reordered.nodes.reverse();
    reordered.users.reverse();
    reordered.apps.reverse();
    for app in &mut reordered.apps {
        app.chains.reverse();
        app.ingresses.reverse();
        app.fronts.reverse();
        app.steps.reverse();
        app.grants.reverse();
    }

    assert_eq!(artifact_texts(reordered), stable);
}

fn artifact_texts(snapshot: ModelSnapshot) -> (String, String, String, String) {
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    assert_eq!(output.summary.warnings, 0, "{:#?}", output.diagnostics);

    let node_plan = output.project_node("hk").unwrap();
    let xray_text = json::xray(&xray::build(&node_plan));
    let grants_text = json::grant_sync_batch(&grants::build(&node_plan));

    let user_plan = output.project_user("platform.acme", "alice").unwrap();
    let subscription = subscription::build(&user_plan);
    let uri_text = uri::subscription(&subscription);
    let clash_text = yaml::clash_subscription(&subscription);

    (xray_text, grants_text, uri_text, clash_text)
}

fn snapshot() -> ModelSnapshot {
    ModelSnapshot {
        revision: 71,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes: vec![
            node("hk", "hk.example.net", [10, 66, 0, 1]),
            node("sg", "sg.example.net", [10, 66, 0, 2]),
        ],
        users: vec![
            User {
                tenant: "platform.beta".to_owned(),
                id: "bob".to_owned(),
                uuid: "uuid-bob".to_owned(),
            },
            User {
                tenant: "platform.acme".to_owned(),
                id: "alice".to_owned(),
                uuid: "uuid-alice".to_owned(),
            },
        ],
        external_outbounds: Vec::new(),
        apps: vec![direct_app(), relay_app()],
    }
}

fn direct_app() -> AppView {
    AppView {
        id: "direct".to_owned(),
        label: "直出".to_owned(),
        chains: vec![chain("c-direct", "香港直出")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i-direct", "c-direct", "hk", None)
        }],
        fronts: Vec::new(),
        steps: vec![step(
            "c-direct",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::DomainSuffix(vec!["ads.example".to_owned()]),
                    action: Action::Block,
                },
                any_egress(),
            ],
            None,
        )],
        grants: vec![grant("platform.acme", "alice", "i-direct")],
    }
}

fn relay_app() -> AppView {
    AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay", "新加坡中转")],
        ingresses: vec![ingress("i-relay", "c-relay", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c-relay",
                "sg",
                vec![any_egress()],
                Some(Accept {
                    uuid: "uuid-relay-sg".to_owned(),
                    label: "c-relay@sg".to_owned(),
                }),
            ),
            step(
                "c-relay",
                "hk",
                vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Forward {
                        to: "sg".to_owned(),
                        dial: HopDial::Overlay,
                        pool: HopPool::None,
                    },
                }],
                None,
            ),
        ],
        grants: vec![grant("platform.acme", "alice", "i-relay")],
    }
}

fn node(id: &str, public_ipv4: &str, overlay: [u8; 4]) -> Node {
    Node {
        mtu: None,
        connection: Default::default(),
        retired: false,
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        public_ipv4: Some(public_ipv4.to_owned()),
        public_ipv6: None,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        overlay_addr: Ipv4Addr::from(overlay),
        certificate_name: None,
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: true,
        egress_allowed: true,
        dns: Dns::System,
        domain_strategy: DomainStrategy::default(),
    }
}

fn chain(id: &str, name: &str) -> Chain {
    Chain {
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: name.to_owned(),
    }
}

fn ingress(id: &str, chain: &str, node: &str, flow: Option<&str>) -> Ingress {
    Ingress {
        id: id.to_owned(),
        chain: chain.to_owned(),
        node: node.to_owned(),
        bind: IpAddr::from(Ipv4Addr::UNSPECIFIED),
        port: 443,
        front: None,
        projection: Default::default(),
        guard: brocade_core::model::IngressGuard::OPEN,
        identity: brocade_core::model::IngressIdentity {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            short_ids: vec!["0123abcd".to_owned()],
        },
        wires: IngressWires::Vless(Transport::VlessReality(
            brocade_core::model::RealitySettings {
                dest: "www.example.com:443".to_owned(),
                server_names: vec!["www.example.com".to_owned()],
                fingerprint: "chrome".to_owned(),
                flow: flow.map(str::to_owned),
                fallback_mode: Default::default(),
                fallback_guard: true,
                fallback_limits: Default::default(),
            },
        )),
    }
}

// A credential means a relay port is configured. The same test as the old model's
// "an inbound is produced only when clients is non-empty" — the port used to come from
// the machine-wide hop_port, whereas each chain now carries its own, fixed at 20000
// throughout these tests.
fn step(chain: &str, node: &str, rules: Vec<Rule>, accept: Option<Accept>) -> Step {
    let hop_in = accept.as_ref().map(|_| HopIn {
        port: 20000,
        security: HopWire::None,
    });
    Step {
        chain: chain.to_owned(),
        node: node.to_owned(),
        accept,
        hop_in,
        rules,
    }
}

fn any_egress() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

fn grant(tenant: &str, user: &str, ingress: &str) -> Grant {
    Grant {
        tenant: tenant.to_owned(),
        user: user.to_owned(),
        ingress: ingress.to_owned(),
    }
}

/// `HopDial`'s wire format. The store layer hand-writes its parser and relies on serde to
/// recognize this field (materialize.rs), so a format change leaves it unable to read the
/// whole chain back while the artifacts look entirely correct (the default overlay is a
/// legitimate path).
#[test]
fn hop_dial_wire_format_is_stable() {
    use brocade_core::model::{HopDial, IpFamily};
    let cases = [
        (HopDial::Overlay, r#"{"t":"overlay"}"#),
        (HopDial::Addr("h:1".to_owned()), r#"{"t":"addr","v":"h:1"}"#),
        (
            HopDial::Reverse(IpFamily::V4),
            r#"{"t":"reverse","v":"v4"}"#,
        ),
        (
            HopDial::Reverse(IpFamily::V6),
            r#"{"t":"reverse","v":"v6"}"#,
        ),
    ];
    for (value, wire) in cases {
        assert_eq!(serde_json::to_string(&value).unwrap(), wire);
        assert_eq!(
            serde_json::from_str::<HopDial>(wire).unwrap(),
            value,
            "{wire}"
        );
    }
}
