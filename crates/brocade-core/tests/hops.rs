use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    ir::{
        hops::{compile_hops, HopDialWire, HopPath},
        routing::compile_app,
        system::compile_system,
    },
    model::{
        Accept, Action, AppView, Chain, DestMatch, DisabledWireGuardLink, Dns, DomainStrategy,
        HopDial, HopEncryption, HopIn, HopPool, HopWire, Ingress, IngressWires, IpFamily,
        ModelSnapshot, Node, Reality, Rule, Step, Transport, User, WireGuardKeys,
    },
    Level,
};
use ipnet::Ipv4Net;

#[test]
fn compile_hops_resolves_forward_to_system_ir_endpoint_and_target_credential() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step("c", "hk", vec![forward("sg")], None),
        step(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty());
    assert_eq!(rir.hops.len(), 1);
    let hop = &rir.hops[0];
    assert_eq!(hop.chain, "c");
    assert_eq!(hop.from, "hk");
    assert_eq!(hop.to, "sg");
    assert_eq!(hop.link, "hk|sg");
    assert_eq!(hop.address, "10.66.0.2");
    assert_eq!(hop.port, 20000);
    assert_eq!(hop.credential.uuid, "uuid-sg");
    assert_eq!(hop.credential.label, "c@sg");
}

/// One relay, two chains, each dialing its own address and entering its own inbound.
///
/// This is the entire reason relay ports hang off the chain. A neighbor inside the
/// datacenter dials the internal address and enters the unencrypted port (same datacenter,
/// saving CPU); a relay outside dials the public address and enters the encrypted one. With
/// relay ports on the node the second half is simply inexpressible — one `hop_security` per
/// machine, and two chains must share it.
#[test]
fn compile_hops_lets_each_chain_have_its_own_inbound_on_the_same_relay() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("jp", [10, 66, 0, 3], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app_two_chains(vec![
        // Same datacenter: dial the internal address, enter the unencrypted port
        step("lan", "hk", vec![forward_addr("sg", "10.0.0.9:8443")], None),
        step_hop(
            "lan",
            "sg",
            vec![any_egress()],
            Some(accept("u1", "lan@sg")),
            20000,
            HopWire::None,
        ),
        // Cross-border: dial the public address, enter the encrypted port
        step(
            "wan",
            "jp",
            vec![forward_addr("sg", "sg.example.net:20001")],
            None,
        ),
        step_hop(
            "wan",
            "sg",
            vec![any_egress()],
            Some(accept("u2", "wan@sg")),
            20001,
            encryption(),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let lan = rir.hops.iter().find(|hop| hop.chain == "lan").unwrap();
    assert_eq!(lan.address, "10.0.0.9");
    assert_eq!(lan.path, HopPath::Direct);
    // It dials the port written on the chain rather than the peer inbound's 20000 — with
    // forwarding in between the two differ by nature.
    assert_eq!(lan.port, 8443);
    assert_eq!(lan.security, HopDialWire::None);

    let wan = rir.hops.iter().find(|hop| hop.chain == "wan").unwrap();
    assert_eq!(wan.address, "sg.example.net");
    assert_eq!(wan.path, HopPath::Direct);
    assert_eq!(wan.port, 20001);
    assert!(matches!(wan.security, HopDialWire::Encryption { .. }));

    // The material differs — precisely what relay ports on the node cannot do.
    assert_ne!(lan.security, wan.security);
}

#[test]
fn compile_hops_derives_public_dial_port_from_the_target_hop_in() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "sg.example.net:20000")],
            None,
        ),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
            20002,
            HopWire::None,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(rir.hops.len(), 1);
    let hop = &rir.hops[0];
    assert_eq!(hop.address, "sg.example.net");
    assert_eq!(hop.port, 20002);
    assert_eq!(hop.path, HopPath::Direct);
}

#[test]
fn compile_hops_rejects_direct_dial_to_a_nat_public_ip() {
    let mut sg = node("sg", [10, 66, 0, 2], true);
    sg.public_ipv4_nat = true;
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true), sg]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "sg.example.net:20000")],
            None,
        ),
        step(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(
        diagnostics.iter().any(|d| d.code == "hop.nat-public"),
        "{diagnostics:?}"
    );
}

#[test]
fn compile_hops_accepts_bracketed_ipv6_direct_address() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "[2001:db8::9]:8443")],
            None,
        ),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
            20000,
            HopWire::None,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(rir.hops[0].address, "2001:db8::9");
    assert_eq!(rir.hops[0].port, 8443);
    assert_eq!(rir.hops[0].path, HopPath::Direct);
}

/// The peer opened no relay port on this chain: error out rather than compile a hop dialing
/// thin air.
#[test]
fn compile_hops_requires_the_target_to_open_an_inbound_for_this_chain() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step("c", "hk", vec![forward("sg")], None),
        // A credential exists, but this chain opened no port on this machine
        step_no_hop("c", "sg", vec![any_egress()], Some(accept("u", "c@sg"))),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty(), "不该编出一条拨不到的 hop");
    let diagnostic = diagnostics
        .iter()
        .find(|d| d.code == "relay.no-hop-in")
        .expect("要报 relay.no-hop-in");
    assert_eq!(diagnostic.level, Level::Error);
}

/// A malformed address errors out rather than being guessed at.
///
/// Handing an unparseable string downstream puts an undialable address in the artifacts
/// while the compile stays green.
#[test]
fn compile_hops_rejects_a_malformed_dial_address() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        // No port
        step("c", "hk", vec![forward_addr("sg", "sg.example.net")], None),
        step("c", "sg", vec![any_egress()], Some(accept("u", "c@sg"))),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty(), "不该编出一条地址不成立的 hop");
    let diagnostic = diagnostics
        .iter()
        .find(|d| d.code == "hop.dial-malformed")
        .expect("要报 hop.dial-malformed");
    assert_eq!(diagnostic.level, Level::Error);
}

/// Writing no `dial` takes the overlay, with the port coming from that peer's own
/// `hop_in.port`.
///
/// The overlay has no intermediate forwarding, so one must dial whichever port the far side
/// listens on — unlike dialing a concrete address, the chain cannot write a different
/// port.
#[test]
fn compile_hops_defaults_to_overlay_and_takes_the_targets_own_port() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step("c", "hk", vec![forward("sg")], None),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("u", "c@sg")),
            20007,
            HopWire::None,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(rir.hops[0].address, "10.66.0.2");
    assert_eq!(rir.hops[0].port, 20007);
    assert_eq!(rir.hops[0].path, HopPath::Overlay);
}

#[test]
fn compile_hops_deduplicates_same_chain_from_to_hop() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                    action: Action::Forward {
                        to: "sg".to_owned(),
                        dial: HopDial::Overlay,
                        pool: HopPool::None,
                    },
                },
                forward("sg"),
            ],
            None,
        ),
        step(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty());
    assert_eq!(rir.hops.len(), 1);
}

#[test]
fn compile_hops_reports_forward_target_without_accept() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step("c", "hk", vec![forward("sg")], None),
        step("c", "sg", vec![any_egress()], None),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error
            && diagnostic.code == "relay.no-accept"
            && diagnostic.location == "c/sg"
    }));
}

#[test]
fn compile_hops_reports_forward_when_the_target_is_unreachable() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("edge", [10, 66, 0, 9], false),
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![Chain {
            id: "c".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "c".to_owned(),
            subscription_country: None,
        }],
        ingresses: vec![ingress("i", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![forward("edge")], None),
            step(
                "c",
                "edge",
                vec![any_egress()],
                Some(accept("uuid-edge", "c@edge")),
            ),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error
            && diagnostic.code == "hop.unreachable"
            && diagnostic.location == "c/hk->edge"
    }));
}

#[test]
fn compile_hops_dials_the_address_written_on_the_chain() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "sg-relay.example.net:8443")],
            None,
        ),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
            20000,
            encryption(),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let hop = &rir.hops[0];
    assert_eq!(hop.address, "sg-relay.example.net");
    assert_eq!(hop.port, 8443, "端口取链上写的，不是对端 inbound 的 20000");
    assert_eq!(hop.path, HopPath::Direct);
    assert_eq!(
        hop.security,
        HopDialWire::Encryption {
            public_key: "pub-hop".to_owned(),
        },
        "拨号方只该拿到公钥"
    );
}

/// The REALITY variant projects only its public half. A private key leaking into a `Hop`
/// would travel with the App IR into the initiating machine's artifacts — scattering a
/// relay's private key to every machine that dials it.
#[test]
fn compile_hops_never_leaks_the_targets_private_key() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "sg-relay.example.net:443")],
            None,
        ),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
            443,
            HopWire::Reality(Reality {
                private_key: "priv-hop-sg".to_owned(),
                public_key: "pub-hop-sg".to_owned(),
                short_ids: vec!["sid-a".to_owned(), "sid-b".to_owned()],
                dest: "apps.apple.com:443".to_owned(),
                server_names: vec!["apps.apple.com".to_owned(), "apple.example".to_owned()],
                fingerprint: "chrome".to_owned(),
                flow: Some("xtls-rprx-vision".to_owned()),
            }),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    let hop = &rir.hops[0];
    assert_eq!(
        hop.security,
        HopDialWire::Reality {
            public_key: "pub-hop-sg".to_owned(),
            server_name: "apps.apple.com".to_owned(),
            short_id: "sid-a".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        "server_names / short_ids 取第一项，跟用户投影一致"
    );
    let rendered = serde_json::to_string(&rir.hops).unwrap();
    assert!(
        !rendered.contains("priv-hop-sg"),
        "私钥不该出现在任何一跳里：{rendered}"
    );
}

/// A dialed machine can relay without joining the overlay, and produces no wg artifact.
///
/// This is the path where an all-public deployment needs not one machine on the backbone:
/// the ability to relay is decided by someone on a chain dialing it plus its having opened a
/// port for that chain, no longer by backbone membership. The backbone retains one purpose —
/// a fallback for machines behind NAT.
#[test]
fn a_public_relay_does_not_need_the_overlay() {
    // Neither end is on the backbone: in an all-public deployment nobody needs wg
    let mut doc = doc(vec![
        node("hk", [10, 66, 0, 1], false),
        node("sg", [10, 66, 0, 2], false),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_addr("sg", "sg-relay.example.net:8443")],
            None,
        ),
        step_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
            20000,
            encryption(),
        ),
    ]);
    // app must go into doc: `compile_system` decides who can relay by whether a chain opened
    // a relay port on them, and a machine off the backbone depends entirely on that to enter
    // the system layer. In the real flow (`compile`) the two are one snapshot anyway; passing
    // them separately is a testing convenience.
    doc.apps = vec![app.clone()];
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "链上写了地址，这一跳该成立：{diagnostics:#?}"
    );
    let hop = &rir.hops[0];
    assert_eq!(hop.address, "sg-relay.example.net");
    assert_eq!(hop.path, HopPath::Direct);

    // There should be no link at all: nobody is on the backbone
    assert!(
        sys.links.is_empty(),
        "不在 overlay 里的机器之间不该生成 wg 链路"
    );
    assert_eq!(sys.node_count, 0, "overlay 成员数是 0");

    // On the artifact side: wg is off while the relay inbound remains
    let plan = brocade_core::physical::node::project_node(&sys, &[rir], "sg");
    assert!(plan.wireguard.is_none(), "不在 overlay 里就不该出 wg 配置");
    let inbounds = plan.xray.unwrap().hop_inbounds;
    assert_eq!(inbounds.len(), 1);
    assert_eq!(inbounds[0].chain, "c");
    assert_eq!(
        inbounds[0].listen.to_string(),
        "0.0.0.0",
        "有人直接拨地址，就得绑 0.0.0.0"
    );
}

/// The converse: the chain says to take the overlay while the target is not on it at all —
/// this hop has no path.
#[test]
fn a_hop_over_the_overlay_to_a_node_outside_it_is_rejected() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], false),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &two_hop_app(), &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "hop.unreachable" && d.level == Level::Error),
        "{diagnostics:#?}"
    );
}

// Both ends are on the backbone but both are behind NAT — `compile_system` generates no
// link for the pair (warning only, not blocking), so this hop still has no overlay to take:
// wg does no intermediate forwarding, and without that `[Peer]` there is no dialing it.
//
// What this pins down is that membership is not reachability. While the backbone was
// unconditionally fully meshed, "both ends on the backbone" and "a link between them" were
// the same thing and checking only the former could not be wrong. Once links can be absent
// that equivalence breaks, and the symptom of omitting this check is the hardest kind:
// artifacts generated as usual, compile all green, traffic vanishing inside wg.
#[test]
fn a_hop_over_the_overlay_without_a_link_is_rejected() {
    let mut nodes = vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ];
    // Both machines' public addresses are marked NAT: neither can dial the other and no
    // link is generated.
    for n in nodes.iter_mut() {
        n.public_ipv4_nat = true;
    }
    let doc = doc(nodes);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    // Preconditions: no link was in fact generated, and the system layer did not report it
    // as an error.
    assert!(sys.links.is_empty(), "{:#?}", sys.links);
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );

    let rir = compile_app(&doc, &two_hop_app(), &mut diagnostics);
    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "hop.unreachable" && d.level == Level::Error),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_hop_over_an_explicitly_disabled_wireguard_link_is_rejected() {
    let mut doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], true),
    ]);
    doc.settings.overlay.disabled_links = vec![DisabledWireGuardLink {
        a: "hk".to_owned(),
        b: "sg".to_owned(),
    }];
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    assert!(sys.links.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "link.disabled" && d.level == Level::Info),
        "{diagnostics:#?}"
    );

    let rir = compile_app(&doc, &two_hop_app(), &mut diagnostics);
    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "hop.unreachable" && d.level == Level::Error),
        "{diagnostics:#?}"
    );
}

// Reverse access: sg is behind NAT and off the backbone, so it dials hk.
// What this pins down is that the edge did not invert — traffic still flows hk → sg with
// `from`/`to` unchanged, and all that inverts is who opens the TCP connection, which is why
// `address` resolves to the upstream's own address and the credential to the downstream's
// own identity.
#[test]
fn compile_hops_reverse_keeps_the_edge_but_dials_the_upstream() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], false),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_dial("sg", HopDial::Reverse(IpFamily::V4))],
            Some(accept("uuid-hk", "c@hk")),
        ),
        // The downstream opens no relay port: it does not listen, which is precisely why
        // this variant exists.
        step_no_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(rir.hops.len(), 1);
    let hop = &rir.hops[0];
    // The edge's direction: traffic still goes from hk to sg
    assert_eq!(hop.from, "hk");
    assert_eq!(hop.to, "sg");
    assert_eq!(hop.path, HopPath::Reverse);
    // The address is the upstream's — the initiator is sg, and what it dials is hk
    assert_eq!(hop.address, "hk.example.net");
    assert_eq!(hop.port, 20000);
    // The credential is the downstream's own identity, by which the upstream places it in
    // clients and recognizes that reverse connection
    assert_eq!(hop.credential.uuid, "uuid-sg");
    assert_eq!(hop.credential.label, "c@sg");
}

// The family is chosen explicitly with no fallback to the other. `node()` in these tests
// gives only v4, so choosing v6 must error — quietly using v4 instead means that the day the
// upstream really gains a v6 address, the hop changes family on its own with not one word of
// the model altered.
#[test]
fn compile_hops_reverse_does_not_fall_back_to_the_other_family() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], false),
    ]);
    let app = app(vec![
        step(
            "c",
            "hk",
            vec![forward_dial("sg", HopDial::Reverse(IpFamily::V6))],
            Some(accept("uuid-hk", "c@hk")),
        ),
        step_no_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty(), "没有 v6 就不该编出这一跳");
    let found = diagnostics
        .iter()
        .find(|d| d.code == "hop.unreachable" && d.level == Level::Error)
        .expect("要报 hop.unreachable");
    assert!(found.message.contains("IPv6"), "{}", found.message);
}

// Reverse access needs its relay port on the upstream: that is the port the downstream
// connects to. Unopened, the tunnel has nowhere to attach.
#[test]
fn compile_hops_reverse_requires_the_upstream_to_open_an_inbound() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true),
        node("sg", [10, 66, 0, 2], false),
    ]);
    let app = app(vec![
        step_no_hop(
            "c",
            "hk",
            vec![forward_dial("sg", HopDial::Reverse(IpFamily::V4))],
            Some(accept("uuid-hk", "c@hk")),
        ),
        step_no_hop(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let rir = compile_app(&doc, &app, &mut diagnostics);

    let rir = compile_hops(rir, &sys, &mut diagnostics);

    assert!(rir.hops.is_empty());
    let found = diagnostics
        .iter()
        .find(|d| d.code == "relay.no-hop-in" && d.level == Level::Error)
        .expect("要报 relay.no-hop-in");
    // What is reported must be the upstream machine, not the downstream — pointing at the
    // wrong one sends the investigation the long way round
    assert!(found.message.contains("hk"), "{}", found.message);
}

fn two_hop_app() -> AppView {
    app(vec![
        step("c", "hk", vec![forward("sg")], None),
        step(
            "c",
            "sg",
            vec![any_egress()],
            Some(accept("uuid-sg", "c@sg")),
        ),
    ])
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 17,
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

fn app(steps: Vec<Step>) -> AppView {
    AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![Chain {
            id: "c".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "c".to_owned(),
            subscription_country: None,
        }],
        ingresses: vec![ingress("i", "hk")],
        fronts: Vec::new(),
        steps,
        grants: Vec::new(),
    }
}

// Two chains sharing one relay: lan enters from hk, wan from jp, and both land on sg.
fn app_two_chains(steps: Vec<Step>) -> AppView {
    let chain = |id: &str| Chain {
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: id.to_owned(),
        subscription_country: None,
    };
    AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("lan"), chain("wan")],
        ingresses: vec![
            Ingress {
                chain: "lan".to_owned(),
                ..ingress("i-lan", "hk")
            },
            Ingress {
                chain: "wan".to_owned(),
                ..ingress("i-wan", "jp")
            },
        ],
        fronts: Vec::new(),
        steps,
        grants: Vec::new(),
    }
}

fn node(id: &str, overlay_addr: [u8; 4], on_overlay: bool) -> Node {
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
        overlay_addr: Ipv4Addr::from(overlay_addr),
        certificate_name: None,
        certificate_track: None,
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: on_overlay,
        egress_allowed: true,
        dns: Dns::System,
        domain_strategy: DomainStrategy::default(),
    }
}

fn ingress(id: &str, node: &str) -> Ingress {
    Ingress {
        id: id.to_owned(),
        chain: "c".to_owned(),
        node: node.to_owned(),
        bind: IpAddr::from(Ipv4Addr::UNSPECIFIED),
        port: 443,
        front: None,
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

fn forward(to: &str) -> Rule {
    forward_dial(to, HopDial::Overlay)
}

fn forward_addr(to: &str, addr: &str) -> Rule {
    forward_dial(to, HopDial::Addr(addr.to_owned()))
}

fn forward_dial(to: &str, dial: HopDial) -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Forward {
            to: to.to_owned(),
            dial,
            pool: HopPool::None,
        },
    }
}

// The variant that specifies its own relay port. `step` supplies the default
// (20000 plus unencrypted).
fn step_hop(
    chain: &str,
    node: &str,
    rules: Vec<Rule>,
    accept: Option<Accept>,
    port: u16,
    security: HopWire,
) -> Step {
    Step {
        hop_in: Some(HopIn { port, security }),
        ..step(chain, node, rules, accept)
    }
}

// A credential but no relay port: compilation should block this rather than produce a hop
// dialing thin air.
fn step_no_hop(chain: &str, node: &str, rules: Vec<Rule>, accept: Option<Accept>) -> Step {
    Step {
        hop_in: None,
        ..step(chain, node, rules, accept)
    }
}

fn encryption() -> HopWire {
    HopWire::Encryption(HopEncryption {
        private_key: "priv-hop".to_owned(),
        public_key: "pub-hop".to_owned(),
    })
}

fn any_egress() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Egress { send_through: None },
    }
}

fn accept(uuid: &str, label: &str) -> Accept {
    Accept {
        uuid: uuid.to_owned(),
        label: label.to_owned(),
    }
}
