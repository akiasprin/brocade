use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    compile::compile,
    model::{
        Accept, Action, AppView, Chain, DestMatch, Dns, DomainStrategy, EgressDnsAddressStrategy,
        EgressDnsFallback, EgressDnsResolution, EgressDnsTransport, Front, FrontStrategy, Grant,
        HopDial, HopIn, HopPool, Ingress, IngressGuard, IngressWires, ModelSnapshot, Node,
        NodeEgressDnsPolicy, Rule, Step, Transport, User, WireGuardKeys,
    },
    Level,
};
use ipnet::Ipv4Net;

#[test]
fn compile_output_projects_only_when_publishable() {
    let mut snapshot = snapshot(vec![node("hk", [10, 66, 0, 1], Dns::System)]);
    snapshot.apps = vec![
        app("z", "c-z", "i-z", 8443, any_egress()),
        app("a", "c-a", "i-a", 443, any_egress()),
    ];

    let output = compile(&snapshot);

    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    assert_eq!(output.summary.warnings, 0, "{:#?}", output.diagnostics);
    assert!(output.can_publish());
    // The snapshot is materialized in `apps.position` order. Compilation must preserve that
    // semantic order instead of silently falling back to the stable IDs.
    assert_eq!(
        output
            .unpublishable_view()
            .apps
            .iter()
            .map(|app| app.app_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("z"), Some("a")]
    );

    let node_plan = output.project_node("hk").unwrap();
    assert!(node_plan.xray.is_some());

    let user_plan = output.project_user("platform.acme", "alice").unwrap();
    assert_eq!(user_plan.entries.len(), 2);
}

#[test]
fn compile_output_blocks_publish_on_errors() {
    let mut snapshot = snapshot(vec![node("hk", [10, 66, 0, 1], Dns::System)]);
    snapshot.users.push(User {
        tenant: "platform.beta".to_owned(),
        id: "bob".to_owned(),
        uuid: "uuid-alice".to_owned(),
    });

    let output = compile(&snapshot);

    assert!(!output.can_publish());
    assert_eq!(output.summary.errors, 1, "{:#?}", output.diagnostics);
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error && diagnostic.code == "user.uuid-dup"
    }));

    let blocked = output.project_node("hk").unwrap_err();
    assert_eq!(blocked.summary, output.summary);
    assert_eq!(blocked.diagnostics, output.diagnostics);
}

#[test]
fn compile_output_allows_publish_with_warnings() {
    let mut snapshot = snapshot(vec![
        node(
            "hk",
            [10, 66, 0, 1],
            Dns::Servers(vec!["8.8.8.8".to_owned()]),
        ),
        node("sg", [10, 66, 0, 2], Dns::System),
    ]);
    // The chain must be alive: hk forwards to sg and sg exits. hk's own rule table has no
    // Egress — its dns-unused warning is what this verifies. any_block cannot carry it: a
    // chain that explicitly blocks everything has no exit path, which is a
    // chain.no-egress-path error and blocks the release.
    let mut a = app("a", "c-a", "i-a", 443, any_block());
    a.steps = vec![
        Step {
            chain: "c-a".to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Forward {
                    to: "sg".to_owned(),
                    dial: HopDial::Overlay,
                    pool: HopPool::None,
                },
            }],
        },
        Step {
            chain: "c-a".to_owned(),
            node: "sg".to_owned(),
            accept: Some(Accept {
                uuid: "uuid-sg".to_owned(),
                label: "c-a@sg".to_owned(),
            }),
            hop_in: Some(HopIn {
                port: 20000,
                security: Default::default(),
            }),
            rules: vec![any_egress()],
        },
    ];
    snapshot.apps = vec![a];

    let output = compile(&snapshot);

    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    assert_eq!(output.summary.warnings, 1, "{:#?}", output.diagnostics);
    assert!(output.can_publish());
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Warn && diagnostic.code == "node.dns-unused"
    }));
    assert!(output.project_node("hk").is_ok());
}

/// `AsIs` means xray's own DNS is never asked: the domain reaches the dialer untouched and
/// the machine's resolver settles it. Servers configured on such a node are therefore dead
/// weight, and saying so is the whole point of the warning — the combination is legal, and
/// which half the operator meant to change is theirs to decide.
///
/// It must stay a warning rather than becoming `dns.route-ambiguous`. That error guards the
/// internal DNS's own queries, which under `AsIs` do not exist; this machine has two distinct
/// egress outbounds and would trip it, so the test would catch the day the ordering in
/// `validate_app_set_dns` is reshuffled and the unreachable check runs anyway.
#[test]
fn compile_output_warns_when_as_is_bypasses_configured_dns() {
    let mut snapshot = snapshot(vec![Node {
        domain_strategy: DomainStrategy::AsIs,
        ..node(
            "hk",
            [10, 66, 0, 1],
            Dns::Servers(vec!["8.8.8.8".to_owned()]),
        )
    }]);
    let mut a = app("a", "c-a", "i-a", 443, any_egress());
    a.steps = vec![Step {
        chain: "c-a".to_owned(),
        node: "hk".to_owned(),
        accept: Some(Accept {
            uuid: "uuid-hk".to_owned(),
            label: "c-a@hk".to_owned(),
        }),
        hop_in: None,
        rules: vec![
            Rule {
                dest_match: DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                action: Action::Egress {
                    send_through: Some("10.66.0.1".parse().unwrap()),
                },
            },
            any_egress(),
        ],
    }];
    snapshot.apps = vec![a];

    let output = compile(&snapshot);

    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    assert!(output.can_publish());
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Warn && diagnostic.code == "node.dns-bypassed"
    }));
    // The two Egress rules differ in send_through, so this machine really does have two
    // egress outbounds — the state the ambiguity error is about.
    assert!(!output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "dns.route-ambiguous"));
}

#[test]
fn compile_output_warns_when_as_is_bypasses_machine_dns_policies() {
    let mut snapshot = snapshot(vec![Node {
        domain_strategy: DomainStrategy::AsIs,
        ..node("hk", [10, 66, 0, 1], Dns::System)
    }]);
    snapshot.node_egress_dns = vec![NodeEgressDnsPolicy {
        node: "hk".to_owned(),
        position: 0,
        selector: DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
        resolution: EgressDnsResolution {
            address: "192.0.2.53".to_owned(),
            port: 53,
            transport: EgressDnsTransport::Tcp,
            address_strategy: EgressDnsAddressStrategy::UseIp,
            fallback: EgressDnsFallback::Stop,
        },
    }];
    snapshot.apps = vec![app("a", "c-a", "i-a", 443, any_egress())];

    let output = compile(&snapshot);

    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Warn && diagnostic.code == "node.dns-bypassed"
    }));
}

/// A decommissioned node's world shuts down automatically: the ingresses on it are not
/// rendered, chains whose trunk includes it are disabled entirely, and the release does not
/// error. After decommissioning all three artifacts are Disabled, the configuration is
/// absent from users' subscriptions, and the other nodes' artifacts carry no trace of the
/// chain.
#[test]
fn retired_node_ingress_and_chains_are_not_rendered() {
    let mut hk = node("hk", [10, 66, 0, 1], Dns::System);
    hk.retired = true;
    let sg = node("sg", [10, 66, 0, 2], Dns::System);
    let mut snapshot = snapshot(vec![hk, sg]);
    // app() hangs the chain, the ingress, and the rules all on hk — exactly how it looks
    // when nothing was moved before decommissioning
    snapshot.apps = vec![app("a", "c-a", "i-a", 443, any_egress())];

    let output = compile(&snapshot);
    assert!(
        output.can_publish(),
        "退役不该被链拦下：{:#?}",
        output.diagnostics
    );

    let ir = output.unpublishable_view();
    let app_ir = &ir.apps[0];
    assert!(
        app_ir.ingresses.is_empty(),
        "IR 里不应有退役节点的入口：{:#?}",
        app_ir.ingresses
    );
    assert!(
        app_ir.chains.is_empty() && app_ir.steps.is_empty(),
        "主干含退役节点的链应整链停用：{:#?}",
        (&app_ir.chains, &app_ir.steps)
    );
    assert!(
        app_ir.grants.is_empty(),
        "入口已经消失，IR 不应留下悬空授权：{:#?}",
        app_ir.grants
    );

    // The model grant still points at i-a, but it leaves the IR together with the ingress,
    // so the user's subscription has no such entry.
    let user_plan = output.project_user("platform.acme", "alice").unwrap();
    assert!(user_plan.entries.is_empty(), "{:#?}", user_plan.entries);

    // The decommissioned node's artifacts: xray takes Disabled, and grant sync must carry
    // no ingress update either
    let node_plan = output.project_node("hk").unwrap();
    assert!(node_plan.xray.is_none());
    assert!(
        node_plan.grant_sync.updates.is_empty(),
        "{:#?}",
        node_plan.grant_sync.updates
    );

    // The chain's non-decommissioned nodes render as usual, but the chain is disabled: it
    // carries neither an ingress nor a relay port
    let sg_plan = output.project_node("sg").unwrap();
    let sg_xray = sg_plan.xray.expect("sg 在模型里，应当有 xray 产物");
    assert!(
        sg_xray.inbounds.is_empty() && sg_xray.hop_inbounds.is_empty(),
        "停用的链不该在 sg 身上留下痕迹：{:#?}",
        sg_xray
    );
}

/// What was decommissioned is downstream of the chain (a relay or an exit) while the head
/// machine is perfectly fine.
///
/// With the chain disabled and the ingress still rendered, the head holds an xray with no
/// forwarding rules and an entirely empty `outbounds` — a config xray will not even start —
/// while 443 still listens and subscriptions still hand it to users. The validation layer
/// used to block that broken artifact with `ingress.no-chain` ("the ingress points at a
/// nonexistent chain"), at the cost of the machine being undecommissionable: someone had to
/// move the ingress by hand before it would ship. With the test completed, the ingress
/// vanishes along with the chain, compilation is clean, and the decommissioning ships as
/// usual.
#[test]
fn retiring_a_downstream_node_takes_the_whole_chain_and_its_ingress_with_it() {
    let hk = node("hk", [10, 66, 0, 1], Dns::System);
    let mut sg = node("sg", [10, 66, 0, 2], Dns::System);
    sg.retired = true;
    let mut snapshot = snapshot(vec![hk, sg]);
    let mut a = app("a", "c-a", "i-a", 443, any_egress());
    // hk is the head and forwards to sg; sg exits. The Forward deliberately lives in hk's
    // second Step fragment: membership discovery must merge all fragments rather than taking
    // the first one, or it misses the retired downstream and leaves the chain running.
    a.steps = vec![
        Step {
            chain: "c-a".to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::DomainSuffix(vec!["internal.example".to_owned()]),
                action: Action::Block,
            }],
        },
        Step {
            chain: "c-a".to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Forward {
                    to: "sg".to_owned(),
                    dial: HopDial::default(),
                    pool: HopPool::None,
                },
            }],
        },
        Step {
            chain: "c-a".to_owned(),
            node: "sg".to_owned(),
            accept: Some(Accept {
                uuid: "uuid-hop".to_owned(),
                label: "hop".to_owned(),
            }),
            hop_in: Some(HopIn {
                port: 20001,
                security: Default::default(),
            }),
            rules: vec![any_egress()],
        },
    ];
    snapshot.apps = vec![a];

    let output = compile(&snapshot);
    assert!(
        output.can_publish(),
        "下游退役不该挡发布：{:#?}",
        output.diagnostics
    );
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    let ir = output.unpublishable_view();
    let app_ir = &ir.apps[0];
    assert!(
        app_ir.chains.is_empty() && app_ir.steps.is_empty(),
        "主干含退役节点的链应整链停用：{:#?}",
        (&app_ir.chains, &app_ir.steps)
    );
    assert!(
        app_ir.ingresses.is_empty(),
        "链停用了，挂在它上面的入口也不该渲染：{:#?}",
        app_ir.ingresses
    );
    assert!(
        app_ir.grants.is_empty(),
        "整链停用后，IR 不应留下指向其入口的授权：{:#?}",
        app_ir.grants
    );

    // The head machine was not decommissioned and its artifacts are produced as usual, but
    // the chain leaves no trace on it: the ingress port is closed and there is not one
    // forwarding rule — anything else would be an open door with no road behind it.
    let hk_plan = output.project_node("hk").unwrap();
    let hk_xray = hk_plan.xray.expect("hk 没退役，应当有 xray 产物");
    assert!(
        hk_xray.inbounds.is_empty() && hk_xray.routing_rules.is_empty(),
        "停用链不该在链头留下入口或规则：{:#?}",
        hk_xray
    );
    assert!(
        hk_plan.grant_sync.updates.is_empty(),
        "入口都没了，不该再往它上面同步用户：{:#?}",
        hk_plan.grant_sync.updates
    );

    // The model grant still points at i-a, but it leaves the IR together with the ingress,
    // so the user's subscription has no such entry.
    let user_plan = output.project_user("platform.acme", "alice").unwrap();
    assert!(user_plan.entries.is_empty(), "{:#?}", user_plan.entries);
}

/// When a front group's `via` points at an ingress shut down by a decommissioning, the
/// `via` is dropped with it. Otherwise it merely becomes `front.unknown-via` reporting "the
/// front group points at a nonexistent ingress" — a different code, and the decommissioning
/// still will not ship.
#[test]
fn a_front_drops_via_entries_whose_ingress_was_retired_away() {
    let hk = node("hk", [10, 66, 0, 1], Dns::System);
    let mut sg = node("sg", [10, 66, 0, 2], Dns::System);
    sg.retired = true;
    let mut snapshot = snapshot(vec![hk, sg]);

    // Front group "f" exits through i-a, whose chain is already disabled because sg was
    // decommissioned
    let mut a = app("a", "c-a", "i-a", 443, any_egress());
    a.steps = vec![Step {
        chain: "c-a".to_owned(),
        node: "hk".to_owned(),
        accept: None,
        hop_in: None,
        rules: vec![Rule {
            dest_match: DestMatch::Any,
            action: Action::Forward {
                to: "sg".to_owned(),
                dial: HopDial::default(),
                pool: HopPool::None,
            },
        }],
    }];
    a.fronts = vec![Front {
        id: "f".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "前置组".to_owned(),
        via: vec!["i-a".to_owned()],
        external_via: Vec::new(),
        strategy: FrontStrategy::UrlTest,
    }];
    snapshot.apps = vec![a];

    let output = compile(&snapshot);
    assert!(
        output.can_publish(),
        "前置组指向的入口被退役带走，不该挡发布：{:#?}",
        output.diagnostics
    );
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|d| d.code == "front.unknown-via"),
        "{:#?}",
        output.diagnostics
    );
    let ir = output.unpublishable_view();
    assert!(
        ir.apps[0].fronts[0].via.is_empty(),
        "指向已消失入口的 via 应当摘掉：{:#?}",
        ir.apps[0].fronts
    );
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

/// The four refusals a new entrance carries, in the table, ahead of the chain's own rule.
///
/// Order is the assertion. A chain ends in `Any → Egress`, which matches everything, and xray takes
/// the first rule that matches — so a guard rule placed after it never fires, and the only symptom
/// is that nothing is ever blocked while the console shows four switches on.
#[test]
fn an_entrance_refuses_before_its_chain_forwards() {
    let mut snapshot = snapshot(vec![node("hk", [10, 66, 0, 1], Dns::System)]);
    let mut project = app("a", "c-a", "i-a", 443, any_egress());
    project.ingresses[0].guard = IngressGuard::default();
    snapshot.apps = vec![project];

    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    let plan = output.project_node("hk").expect("hk 编译得出来");
    let xray = plan.xray.expect("hk 上有 xray");
    let rules = &xray.routing_rules;

    // Four on by default; the fifth (all UDP but 443) is off because it breaks games and voice.
    let guarded = rules
        .iter()
        .take_while(|rule| rule.outbound_tag == "out:block")
        .count();
    assert_eq!(guarded, 4, "默认开四条：{rules:#?}");

    assert_eq!(
        rules[0].dest_match,
        DestMatch::IpCidr(vec!["geoip:private".to_owned(), "10.66.0.0/16".to_owned()]),
        "第一条必须是内网与机队，且机队网段来自 system 而不是写死"
    );
    assert_eq!(
        rules[1].dest_match,
        DestMatch::Protocol(vec!["bittorrent".to_owned()])
    );
    assert_eq!(
        rules[2].dest_match,
        DestMatch::Port(vec!["25".to_owned(), "465".to_owned(), "587".to_owned()])
    );
    assert!(
        matches!(&rules[3].dest_match, DestMatch::All(values)
            if values.len() == 2 && values.contains(&DestMatch::Network(brocade_core::model::Network::Udp))),
        "放大端口那条必须只管 UDP：{:#?}",
        rules[3].dest_match
    );

    // And the chain's own rule is still there, behind them.
    assert!(
        rules[4..]
            .iter()
            .any(|rule| rule.dest_match == DestMatch::Any),
        "链路自己的规则不该被顶掉：{rules:#?}"
    );
}

/// An entrance that carries no refusals compiles exactly as it did before the feature existed.
#[test]
fn an_open_entrance_adds_no_rules() {
    let mut snapshot = snapshot(vec![node("hk", [10, 66, 0, 1], Dns::System)]);
    snapshot.apps = vec![app("a", "c-a", "i-a", 443, any_egress())];

    let plan = compile(&snapshot)
        .project_node("hk")
        .expect("hk 编译得出来");
    let xray = plan.xray.expect("hk 上有 xray");
    assert_eq!(xray.routing_rules.len(), 1, "只该有链路自己那一条");
}

/// Blocking BitTorrent behind a front is refused rather than compiled into a rule that matches
/// nothing — the failure would otherwise be invisible and in the unsafe direction.
#[test]
fn blocking_torrents_needs_an_entrance_that_sniffs() {
    let mut snapshot = snapshot(vec![node("hk", [10, 66, 0, 1], Dns::System)]);
    let mut project = app("a", "c-a", "i-a", 443, any_egress());
    project.ingresses[0].guard = IngressGuard {
        no_bittorrent: true,
        ..IngressGuard::OPEN
    };
    project.fronts = vec![Front {
        id: "f-a".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "f-a".to_owned(),
        via: vec!["i-a".to_owned()],
        external_via: Vec::new(),
        strategy: FrontStrategy::UrlTest,
    }];
    snapshot.apps = vec![project];

    let output = compile(&snapshot);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|d| d.code == "ingress.guard-needs-sniffing"),
        "{:#?}",
        output.diagnostics
    );
    assert!(!output.can_publish(), "拦不住却显示已开启，不能让它发布");
}

fn any_egress() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

fn any_block() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Block,
    }
}
