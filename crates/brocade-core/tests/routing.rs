use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    compile::compile,
    ir::routing::{compile_app, DestMatch as IrMatch},
    model::{
        Accept, Action, AppView, Chain, DestMatch as ModelMatch, Dns, DomainStrategy, Front,
        FrontStrategy, Grant, HopDial, HopIn, HopPool, HopWire, Ingress, IngressWires,
        ModelSnapshot, Node, ProjectionEndpoint, Rule, Step, Transport, User, WireGuardKeys,
    },
    Level,
};
use ipnet::Ipv4Net;

#[test]
fn compile_app_completes_empty_rule_tables_with_terminal_default() {
    // A chain's order is written into its rules: hk → sg → us rests entirely on explicit
    // `any → Forward`, and the compiler no longer fills in intermediate forwarding from a
    // declared trunk. Only a node whose rule table (or trailing fallback) is empty gets
    // "exit here" appended.
    let app = AppView {
        id: "lin".to_owned(),
        label: "线性".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i-hk", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![any_forward("sg")], None),
            step("c", "sg", vec![any_forward("us")], None),
        ],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], false),
        node("sg", Some("sg.example.net"), [10, 66, 0, 2], false),
        node("us", Some("us.example.net"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert!(diagnostics.is_empty());
    assert_eq!(rule_summary(&ir, "c", "hk"), ["any -> forward:sg"]);
    assert_eq!(rule_summary(&ir, "c", "sg"), ["any -> forward:us"]);
    // us writes no rules: the compiler appends "exit here", and egress_allowed is true so
    // it is permitted
    assert_eq!(rule_summary(&ir, "c", "us"), ["any -> egress"]);
}

#[test]
fn compile_app_does_not_invent_forward_edges() {
    // An empty rule table means exiting, not forwarding somewhere automatically — the order
    // is written, not guessed. On this chain hk writes no rules, traffic reaching hk is
    // handled there, and sg never enters the member set at all.
    let app = AppView {
        id: "lin".to_owned(),
        label: "线性".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i-hk", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], false),
        node("sg", Some("sg.example.net"), [10, 66, 0, 2], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert_eq!(rule_summary(&ir, "c", "hk"), ["any -> block"]);
    assert!(!ir.steps.iter().any(|step| step.node == "sg"));
}

#[test]
fn front_downstream_expands_to_sorted_domain_and_ip_rules() {
    let app = front_app(vec![step(
        "c-front",
        "hk",
        vec![Rule {
            dest_match: ModelMatch::FrontDownstream,
            action: Action::Egress { send_through: None },
        }],
        None,
    )]);
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        node("us", Some("us1.example.net"), [10, 66, 0, 2], true),
        node("au", Some("203.0.113.7"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "front.default-block"));
    assert_eq!(
        rule_summary(&ir, "c-front", "hk"),
        [
            "domain_suffix:us1.example.net -> egress",
            "ip_cidr:203.0.113.7 -> egress",
            "any -> block",
        ]
    );
    assert!(
        !ir.ingresses
            .iter()
            .find(|ingress| ingress.id == "i-front")
            .unwrap()
            .sniff
    );
    assert!(
        ir.ingresses
            .iter()
            .find(|ingress| ingress.id == "i-us")
            .unwrap()
            .sniff
    );
}

#[test]
fn front_downstream_deliberately_ignores_subscription_projection() {
    let mut app = front_app(vec![step(
        "c-front",
        "hk",
        vec![Rule {
            dest_match: ModelMatch::FrontDownstream,
            action: Action::Egress { send_through: None },
        }],
        None,
    )]);
    app.ingresses
        .iter_mut()
        .find(|ingress| ingress.id == "i-us")
        .unwrap()
        .projection
        .v4 = Some(ProjectionEndpoint {
        host: "custom-relay.example.net".to_owned(),
        port: 443,
        download: None,
    });
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        node("us", Some("us.example.net"), [10, 66, 0, 2], true),
        node("au", Some("203.0.113.7"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert_eq!(
        rule_summary(&ir, "c-front", "hk"),
        [
            "domain_suffix:us.example.net -> egress",
            "ip_cidr:203.0.113.7 -> egress",
            "any -> block",
        ]
    );
    assert!(
        rule_summary(&ir, "c-front", "hk")
            .iter()
            .all(|rule| !rule.contains("custom-relay.example.net")),
        "Projection 只写订阅，不应进入 FrontDownstream 展开"
    );
}

#[test]
fn empty_front_downstream_reports_error_and_keeps_safe_error_match() {
    let mut app = front_app(vec![step(
        "c-front",
        "hk",
        vec![Rule {
            dest_match: ModelMatch::FrontDownstream,
            action: Action::Block,
        }],
        None,
    )]);
    // Expansion targets are the exits with a non-empty front: remove them all and the group
    // name has no reachable host under it
    for ingress in &mut app.ingresses {
        ingress.front = None;
    }
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        node("us", Some("us1.example.net"), [10, 66, 0, 2], true),
        node("au", Some("203.0.113.7"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error && diagnostic.code == "rule.front-scope"
    }));
    assert_eq!(
        rule_summary(&ir, "c-front", "hk"),
        ["front_downstream -> block", "any -> block"]
    );
}

#[test]
fn nested_front_downstream_blocks_publish_instead_of_becoming_a_dead_rule() {
    let app = front_app(vec![step(
        "c-front",
        "hk",
        vec![Rule {
            dest_match: ModelMatch::All(vec![
                ModelMatch::FrontDownstream,
                ModelMatch::Port(vec!["443".to_owned()]),
            ]),
            action: Action::Egress { send_through: None },
        }],
        None,
    )]);
    let mut snapshot = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        node("us", Some("us.example.net"), [10, 66, 0, 2], true),
        node("au", Some("203.0.113.7"), [10, 66, 0, 3], true),
    ]);
    snapshot.apps = vec![app];

    let output = compile(&snapshot);

    assert!(!output.can_publish(), "嵌套规则不能进入发布：{output:#?}");
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error
            && diagnostic.code == "rule.front-scope"
            && diagnostic.message.contains("不能嵌套在 All 中")
    }));
    assert!(output.project_node("hk").is_err());
}

#[test]
fn grants_resolve_users_by_tenant_and_id() {
    let mut app = AppView {
        id: "lin".to_owned(),
        label: "线性".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i-hk", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![
            Grant {
                tenant: "platform.acme".to_owned(),
                user: "alice".to_owned(),
                ingress: "i-hk".to_owned(),
            },
            Grant {
                tenant: "platform.beta".to_owned(),
                user: "alice".to_owned(),
                ingress: "i-hk".to_owned(),
            },
            Grant {
                tenant: "platform.acme".to_owned(),
                user: "alice".to_owned(),
                ingress: "i-missing".to_owned(),
            },
        ],
    };
    let mut doc = doc(vec![node(
        "hk",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    doc.users.push(User {
        tenant: "platform.acme".to_owned(),
        id: "alice".to_owned(),
        uuid: "uuid-acme".to_owned(),
    });
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert_eq!(ir.grants.len(), 1);
    assert_eq!(ir.grants[0].label, "alice@platform.acme#i-hk");
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "grant.no-user"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "grant.no-ingress"));

    app.grants.reverse();
    let mut diagnostics = Vec::new();
    let ir_reversed = compile_app(&doc, &app, &mut diagnostics);
    assert_eq!(ir.grants, ir_reversed.grants);
}

#[test]
fn unordered_entity_order_does_not_change_compiled_app_ir() {
    let app = AppView {
        id: "stable".to_owned(),
        label: "稳定".to_owned(),
        chains: vec![chain("c-b"), chain("c-a")],
        ingresses: vec![
            ingress("i-b", "c-b", "b", None),
            ingress("i-a", "c-a", "a", None),
        ],
        fronts: Vec::new(),
        steps: vec![
            step("c-b", "b", vec![any_egress()], None),
            step("c-a", "a", vec![any_egress()], None),
        ],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("b", Some("b.example.net"), [10, 66, 0, 2], true),
        node("a", Some("a.example.net"), [10, 66, 0, 1], true),
    ]);
    let mut diagnostics = Vec::new();
    let baseline = compile_app(&doc, &app, &mut diagnostics);

    let mut shuffled_doc = doc;
    shuffled_doc.nodes.reverse();
    let mut shuffled_app = app;
    // Chain array order is the explicit app-local position and is therefore semantic. The other
    // model collections remain unordered inputs and must compile canonically.
    shuffled_app.ingresses.reverse();
    shuffled_app.steps.reverse();

    let mut diagnostics = Vec::new();
    let shuffled = compile_app(&shuffled_doc, &shuffled_app, &mut diagnostics);

    assert_eq!(baseline, shuffled);
}

#[test]
fn multiple_ingresses_cannot_make_the_compiled_root_depend_on_array_order() {
    let app = AppView {
        id: "stable".to_owned(),
        label: "稳定".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![
            ingress("i-sg", "c", "sg", None),
            ingress("i-hk", "c", "hk", None),
        ],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![any_egress()], None),
            step("c", "sg", vec![any_egress()], None),
        ],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        node("sg", Some("sg.example.net"), [10, 66, 0, 2], true),
    ]);
    let mut diagnostics = Vec::new();
    let baseline = compile_app(&doc, &app, &mut diagnostics);

    let mut reordered = app;
    reordered.ingresses.reverse();
    let mut reordered_diagnostics = Vec::new();
    let reordered = compile_app(&doc, &reordered, &mut reordered_diagnostics);

    assert_eq!(baseline, reordered);
    assert_eq!(diagnostics, reordered_diagnostics);
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "chain.multi-ingress"));
    assert_eq!(baseline.chains[0].root.as_deref(), Some("hk"));
    assert_eq!(
        baseline
            .steps
            .iter()
            .map(|step| step.node.as_str())
            .collect::<Vec<_>>(),
        ["hk"]
    );
}

/// Several doors on one machine is the ordinary case, not the broken one: a Hysteria 2 ingress
/// beside a VLESS ingress, or two ports differing only in flow. What `chain.multi-ingress` is
/// about is the head having two *machines* to be, so ingresses that name the same machine must
/// pass — otherwise the check reads as "one door per chain", which is not the rule and would
/// block a configuration the fleet is expected to run.
#[test]
fn several_ingresses_on_one_machine_are_not_a_multi_ingress_error() {
    let app = AppView {
        id: "stable".to_owned(),
        label: "稳定".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![
            ingress_at_port("i-vless", "c", "hk", 443),
            ingress_at_port("i-hy2", "c", "hk", 8443),
        ],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "chain.multi-ingress"),
        "{diagnostics:#?}"
    );
    assert_eq!(ir.chains[0].root.as_deref(), Some("hk"));
    assert_eq!(ir.ingresses.len(), 2, "{ir:#?}");
}

fn ingress_at_port(id: &str, chain: &str, node: &str, port: u16) -> Ingress {
    Ingress {
        port,
        ..ingress(id, chain, node, None)
    }
}

/// Multiple Step fragments for one `(chain, node)` contribute to one ordered rule table. The
/// target being the same does not make either rule redundant: their matches select different
/// traffic.
#[test]
fn step_fragments_to_the_same_target_merge_their_rule_tables() {
    let domain_forward = Rule {
        dest_match: ModelMatch::DomainSuffix(vec!["example.com".to_owned()]),
        action: Action::Forward {
            to: "sg".to_owned(),
            dial: HopDial::Overlay,
            pool: HopPool::None,
        },
    };
    let cidr_forward = Rule {
        dest_match: ModelMatch::IpCidr(vec!["203.0.113.0/24".to_owned()]),
        action: Action::Forward {
            to: "sg".to_owned(),
            dial: HopDial::Overlay,
            pool: HopPool::None,
        },
    };
    let app = AppView {
        id: "duplicate-step".to_owned(),
        label: "重复 Step".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i-hk", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![domain_forward], None),
            step("c", "hk", vec![cidr_forward], None),
            step("c", "sg", vec![any_egress()], None),
        ],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], false),
        node("sg", Some("sg.example.net"), [10, 66, 0, 2], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert_eq!(
        rule_summary(&ir, "c", "hk"),
        [
            "domain_suffix:example.com -> forward:sg",
            "ip_cidr:203.0.113.0/24 -> forward:sg",
            "any -> block",
        ]
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "合法的规则片段不应产生错误：{diagnostics:#?}"
    );
}

#[test]
fn step_fragments_reject_conflicting_accept_and_hop_in_metadata() {
    let first_accept = Accept {
        uuid: "uuid-first".to_owned(),
        label: "first".to_owned(),
    };
    let second_accept = Accept {
        uuid: "uuid-second".to_owned(),
        label: "second".to_owned(),
    };
    let mut second_fragment = step("c", "sg", vec![any_egress()], Some(second_accept));
    second_fragment.hop_in.as_mut().unwrap().port = 20001;
    let app = AppView {
        id: "conflicting-step-metadata".to_owned(),
        label: "冲突元数据".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i-hk", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![any_forward("sg")], None),
            step("c", "sg", Vec::new(), Some(first_accept)),
            second_fragment,
        ],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], false),
        node("sg", Some("sg.example.net"), [10, 66, 0, 2], true),
    ]);
    let mut diagnostics = Vec::new();

    let ir = compile_app(&doc, &app, &mut diagnostics);

    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "step.accept-conflict"));
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "step.hop-in-conflict"));
    let sg = ir.steps.iter().find(|step| step.node == "sg").unwrap();
    assert!(
        sg.accept.is_none() && sg.hop_in.is_none(),
        "冲突的元数据应 fail closed：{sg:#?}"
    );
}

fn rule_summary(ir: &brocade_core::ir::routing::AppIr, chain: &str, node: &str) -> Vec<String> {
    ir.steps
        .iter()
        .find(|step| step.chain == chain && step.node == node)
        .unwrap()
        .rules
        .iter()
        .map(|rule| {
            format!(
                "{} -> {}",
                match_summary(&rule.dest_match),
                action_summary(&rule.action)
            )
        })
        .collect()
}

fn match_summary(dest_match: &IrMatch) -> String {
    match dest_match {
        IrMatch::Any => "any".to_owned(),
        IrMatch::DomainSuffix(values) => format!("domain_suffix:{}", values.join(",")),
        IrMatch::IpCidr(values) => format!("ip_cidr:{}", values.join(",")),
        IrMatch::FrontDownstream => "front_downstream".to_owned(),
        other => format!("{other:?}"),
    }
}

fn action_summary(action: &Action) -> String {
    match action {
        Action::Forward { to, .. } => format!("forward:{to}"),
        Action::Egress { .. } => "egress".to_owned(),
        Action::Proxy { outbound } => format!("proxy:{outbound}"),
        Action::Block => "block".to_owned(),
    }
}

fn front_app(steps: Vec<Step>) -> AppView {
    AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front"), chain("c-us"), chain("c-au")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
            ingress("i-au", "c-au", "au", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            external_via: Vec::new(),
            strategy: FrontStrategy::UrlTest,
        }],
        steps,
        grants: Vec::new(),
    }
}

fn chain(id: &str) -> Chain {
    Chain {
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        subscription_country: None,
    }
}

fn ingress(id: &str, chain: &str, node: &str, front: Option<&str>) -> Ingress {
    Ingress {
        id: id.to_owned(),
        chain: chain.to_owned(),
        node: node.to_owned(),
        bind: IpAddr::from(Ipv4Addr::UNSPECIFIED),
        port: 443,
        front: front.map(str::to_owned),
        projection: Default::default(),
        guard: brocade_core::model::IngressGuard::OPEN,
        identity: brocade_core::model::IngressIdentity {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            short_ids: vec![format!("sid-{id}")],
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
        dest_match: ModelMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

fn any_forward(to: &str) -> Rule {
    Rule {
        dest_match: ModelMatch::Any,
        action: Action::Forward {
            to: to.to_owned(),
            dial: HopDial::Overlay,
            pool: HopPool::None,
        },
    }
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 11,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: Vec::new(),
        external_outbounds: Vec::new(),
        apps: Vec::new(),
    }
}

fn node(id: &str, public_ipv4: Option<&str>, overlay: [u8; 4], egress_allowed: bool) -> Node {
    Node {
        mtu: None,
        connection: Default::default(),
        retired: false,
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        public_ipv4: public_ipv4.map(str::to_owned),
        public_ipv6: None,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        overlay_addr: Ipv4Addr::from(overlay),
        certificate_name: None,
        certificate_track: None,
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: true,
        egress_allowed,
        dns: Dns::System,
        domain_strategy: DomainStrategy::default(),
    }
}
