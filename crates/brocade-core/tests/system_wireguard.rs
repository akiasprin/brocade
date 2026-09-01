use std::net::Ipv4Addr;

use brocade_core::{
    artifacts::wireguard,
    diagnostic::summarize_diagnostics,
    format::ini,
    ir::system::{compile_system, Dial, LinkWrap},
    model::{Dns, DomainStrategy, ModelSnapshot, Node, WgTransport, WireGuardKeys},
    physical::node::{build_node_plan, project_node},
    Level,
};
use ipnet::Ipv4Net;

#[test]
fn compile_system_derives_mesh_dial_from_reachability() {
    let doc = doc(vec![
        node("b", None, [10, 66, 0, 2], true),
        node("a", Some("a.example.net"), [10, 66, 0, 1], true),
        node("c", Some("c.example.net"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();

    let sys = compile_system(&doc, &mut diagnostics);

    assert!(diagnostics.is_empty());
    assert_eq!(
        sys.nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(sys.links.len(), 3);
    assert_eq!(link_dial(&sys, "a", "b"), Some(Dial::BtoA));
    assert_eq!(link_dial(&sys, "a", "c"), Some(Dial::Both));
    assert_eq!(link_dial(&sys, "b", "c"), Some(Dial::AtoB));
}

#[test]
fn nat_ipv4_is_skipped_when_ipv6_is_reachable() {
    let mut b = node("b", Some("b.example.net"), [10, 66, 0, 2], true);
    b.public_ipv4_nat = true;
    b.public_ipv6 = Some("2001:db8::2".to_owned());
    let doc = doc(vec![
        node("a", Some("a.example.net"), [10, 66, 0, 1], true),
        b,
    ]);
    let mut diagnostics = Vec::new();

    let sys = compile_system(&doc, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let b = sys.nodes.iter().find(|node| node.id == "b").unwrap();
    assert_eq!(
        b.wireguard.as_ref().unwrap().endpoint.as_deref(),
        Some("[2001:db8::2]:51820")
    );
    assert_eq!(link_dial(&sys, "a", "b"), Some(Dial::Both));
}

// Neither side can reach the other: no link is generated, and the release is not blocked.
// Two machines behind NAT failing to handshake is a physical fact, the combination is
// inevitable at scale under a full mesh, and reporting an error would surrender the whole
// model's shippability for a link nobody will take. Where a chain really does need this
// hop, `hop.unreachable` blocks it — see hops.rs.
#[test]
fn compile_system_skips_links_with_no_reachable_endpoint_without_blocking() {
    let doc = doc(vec![
        node("a", None, [10, 66, 0, 1], true),
        node("b", None, [10, 66, 0, 2], true),
    ]);
    let mut diagnostics = Vec::new();

    let sys = compile_system(&doc, &mut diagnostics);

    assert!(sys.links.is_empty());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].level, Level::Info);
    assert_eq!(diagnostics[0].code, "link.no-endpoint");
    assert_eq!(diagnostics[0].location, "a|b");
    assert!(summarize_diagnostics(&diagnostics).can_publish);
}

#[test]
fn wireguard_plan_and_format_keep_endpoint_rules_in_one_place() {
    let doc = doc(vec![
        node("b", None, [10, 66, 0, 2], true),
        node("a", Some("a.example.net"), [10, 66, 0, 1], true),
        node("c", Some("c.example.net"), [10, 66, 0, 3], true),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let plan = build_node_plan(&sys, "b");
    let artifact = wireguard::build(&plan);
    let text = ini::wireguard(&artifact);

    assert!(!text.contains("ListenPort ="));
    assert!(text.contains("Address    = 10.66.0.2/32"));
    assert!(text.contains("# a"));
    assert!(text.contains("Endpoint   = a.example.net:51820"));
    assert!(text.contains("# c"));
    assert!(text.contains("Endpoint   = c.example.net:51820"));
    assert_eq!(text.matches("PersistentKeepalive = 25").count(), 2);
}

#[test]
fn wireguard_endpoint_formats_ipv6_literal() {
    let mut a = node("a", None, [10, 66, 0, 1], true);
    a.public_ipv6 = Some("2001:db8::1".to_owned());
    let doc = doc(vec![a, node("b", None, [10, 66, 0, 2], true)]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let plan = build_node_plan(&sys, "b");
    let artifact = wireguard::build(&plan);
    let text = ini::wireguard(&artifact);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(text.contains("Endpoint   = [2001:db8::1]:51820"));
}

#[test]
fn non_backbone_node_materializes_as_disabled_wireguard_artifact() {
    let doc = doc(vec![node(
        "edge",
        Some("edge.example.net"),
        [10, 66, 0, 9],
        false,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let plan = build_node_plan(&sys, "edge");
    let artifact = wireguard::build(&plan);
    let text = ini::wireguard(&artifact);

    assert!(sys.nodes.is_empty());
    assert_eq!(text, "# edge 不在 overlay 里，没有 WireGuard 配置。\n");
}

/// Two properties that should hold for any model, not expectations of one scenario.
///
/// 1. Every link has an Endpoint written on at least one side. A link with neither never
///    handshakes, while looking entirely normal in the artifacts — two tidy `[Peer]`
///    sections that nobody will ever dial.
/// 2. A `Dial::Both` link carries no keepalive. Where both sides can reach each other there
///    is no question of who holds the NAT mapping open, and writing one sends a wasted
///    packet every 25 seconds — from each side of every link.
#[test]
fn wireguard_endpoints_and_keepalives_hold_their_invariants() {
    // All three reachability combinations are present: two public machines and two behind
    // NAT, yielding Both / AtoB / BtoA
    let mut behind_nat = node("b-nat", Some("b.example.net"), [10, 66, 0, 2], true);
    behind_nat.public_ipv4_nat = true;
    let mut also_nat = node("d-nat", Some("d.example.net"), [10, 66, 0, 4], true);
    also_nat.public_ipv4_nat = true;
    let doc = doc(vec![
        node("a-pub", Some("a.example.net"), [10, 66, 0, 1], true),
        behind_nat,
        node("c-pub", Some("c.example.net"), [10, 66, 0, 3], true),
        also_nat,
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    // Precondition: all three dial variants actually occur, or this test spins over a pile
    // of identical links
    let dials = sys.links.iter().map(|link| link.dial).collect::<Vec<_>>();
    for want in [Dial::Both, Dial::AtoB, Dial::BtoA] {
        assert!(dials.contains(&want), "没造出 {want:?}：{dials:?}");
    }

    for link in &sys.links {
        let endpoint_of = |id: &str| {
            sys.nodes
                .iter()
                .find(|node| node.id == id)
                .and_then(|node| node.wireguard.as_ref())
                .and_then(|wireguard| wireguard.endpoint.clone())
        };
        assert!(
            endpoint_of(&link.a).is_some() || endpoint_of(&link.b).is_some(),
            "链路 {} 两侧都没有 Endpoint，永远握不上手",
            link.id
        );
    }

    for node in &sys.nodes {
        let text = ini::wireguard(&wireguard::build(&build_node_plan(&sys, &node.id)));
        for block in text.split("[Peer]").skip(1) {
            if !block.contains("PersistentKeepalive") {
                continue;
            }
            let peer = block
                .lines()
                .find_map(|line| line.trim().strip_prefix("# "))
                .unwrap_or_default()
                .to_owned();
            let dial = sys
                .links
                .iter()
                .find(|link| {
                    (link.a == node.id && link.b == peer) || (link.b == node.id && link.a == peer)
                })
                .map(|link| link.dial);
            assert_ne!(
                dial,
                Some(Dial::Both),
                "{} 对 {peer} 写了 keepalive，可这条链路两边都拨得动",
                node.id
            );
        }
    }
}

fn link_dial(sys: &brocade_core::ir::system::SystemIr, a: &str, b: &str) -> Option<Dial> {
    let id = format!("l-{a}-{b}");
    sys.links
        .iter()
        .find(|link| link.id == id)
        .map(|link| link.dial)
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 7,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: Vec::new(),
        external_outbounds: Vec::new(),
        apps: Vec::new(),
    }
}

/// Fake TCP's two ends must be derived from one IR, so the direction and the port cannot be
/// configured wrongly — which is the whole point of replacing hand configuration with the
/// IR.
#[test]
fn fake_tcp_rewrites_the_endpoint_and_pairs_both_ends() {
    let hk = node("hk", Some("hk.example.net"), [10, 66, 0, 1], true);
    let mut sg = node("sg", Some("sg.example.net"), [10, 66, 0, 2], true);
    sg.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let doc = doc(vec![hk, sg]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );

    let hk_plan = project_node(&sys, &[], "hk");
    let sg_plan = project_node(&sys, &[], "sg");

    // The dialing end: wg dials local loopback and shows no trace of phantun
    let peer = hk_plan
        .wireguard
        .as_ref()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.node_id == "sg")
        .unwrap();
    let endpoint = peer.endpoint.as_deref().unwrap();
    assert!(endpoint.starts_with("127.0.0.1:"), "{endpoint}");

    // The port the client connects to, matching the peer's declared fake-TCP port
    let client = &hk_plan.phantun.as_ref().unwrap().clients[0];
    assert_eq!(client.peer_node_id, "sg");
    assert_eq!(client.remote_tcp_endpoint, "sg.example.net:39743");
    assert_eq!(endpoint, format!("127.0.0.1:{}", client.listen_udp_port));
    assert!(hk_plan.phantun.as_ref().unwrap().servers.is_empty());

    // The dialed end: the server accepts TCP and forwards to the local wg UDP port
    let server = &sg_plan.phantun.as_ref().unwrap().servers[0];
    assert_eq!(server.tcp_port, 39743);
    assert_eq!(server.forward_to_udp_port, 51820);

    // The reverse direction should have no client: hk is plain UDP and sg dials it
    // directly
    assert!(sg_plan.phantun.as_ref().unwrap().clients.is_empty());
    let back = sg_plan
        .wireguard
        .as_ref()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.node_id == "hk")
        .unwrap();
    assert_eq!(back.endpoint, None);
}

/// The fake-TCP side must never dial out.
///
/// On receiving any decryptable packet, WireGuard updates that peer's Endpoint to the
/// packet's source address (roaming, which cannot be turned off). A fake-TCP machine's
/// outbound UDP usually works, so the moment it dials, the peer moves the Endpoint off
/// phantun's local loopback onto its public UDP address and replies to a port whose inbound
/// is sealed — voiding the whole phantun path on the spot, with the handshake stuck at 0
/// forever.
#[test]
fn a_fake_tcp_node_never_dials_out() {
    let mut sg = node("sg", Some("sg.example.net"), [10, 66, 0, 2], true);
    sg.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        sg,
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    assert_eq!(sys.links.len(), 1);
    let link = &sys.links[0];
    // Links sort by id, so a=hk and b=sg; only hk may dial sg
    assert_eq!(link.a, "hk");
    assert_eq!(link.b, "sg");
    assert_eq!(link.dial, Dial::AtoB, "伪 TCP 那侧不该拨");

    // In the artifacts: sg's wg0.conf must carry no Endpoint for hk
    let sg_plan = project_node(&sys, &[], "sg");
    let peer = sg_plan
        .wireguard
        .as_ref()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.node_id == "hk")
        .unwrap();
    assert_eq!(peer.endpoint, None, "伪 TCP 那侧写了 Endpoint 就会去拨");
}

/// With fake TCP on both sides this restriction does not apply: dialing out also goes
/// through each side's phantun, the peer learns its own tun's address, and the return path
/// holds.
#[test]
fn two_fake_tcp_nodes_still_dial_each_other() {
    let mut a = node("a", Some("a.example.net"), [10, 66, 0, 1], true);
    a.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let mut b = node("b", Some("b.example.net"), [10, 66, 0, 2], true);
    b.wireguard.transport = WgTransport::FakeTcp { port: 39744 };
    let doc = doc(vec![a, b]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    assert_eq!(sys.links[0].dial, Dial::Both);
}

/// Declaring fake TCP leaves no usable UDP endpoint. Keeping an undialable address has
/// `Dial` derive a link that never handshakes, while the artifacts look entirely
/// correct.
#[test]
fn fake_tcp_clears_the_udp_endpoint() {
    let mut sg = node("sg", Some("sg.example.net"), [10, 66, 0, 2], true);
    sg.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let doc = doc(vec![
        node("hk", Some("hk.example.net"), [10, 66, 0, 1], true),
        sg,
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let sg = sys.nodes.iter().find(|n| n.id == "sg").unwrap();
    // Only a machine on the backbone has this half; a relay off it has wireguard as
    // None
    let sg_wg = sg.wireguard.as_ref().expect("sg 在 overlay 里");
    assert_eq!(sg_wg.endpoint, None);
    // The endpoint moved onto the link: the server is hosted on sg (which is reachable) and
    // hk dials it
    match &sys.links[0].wrap {
        LinkWrap::FakeTcp { servers } => {
            assert_eq!(
                servers.get("sg").map(String::as_str),
                Some("sg.example.net:39743")
            );
            assert_eq!(servers.get("hk"), None);
        }
        other => panic!("该穿外衣: {other:?}"),
    }
    // It can still be dialed, so the link holds — but only the far side dials it, never the
    // other way
    assert_eq!(sys.links.len(), 1);
    assert_eq!(sys.links[0].dial, Dial::AtoB);
}

fn node(id: &str, public_ipv4: Option<&str>, overlay_addr: [u8; 4], on_overlay: bool) -> Node {
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
        overlay_addr: Ipv4Addr::from(overlay_addr),
        certificate_name: None,
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

/// A machine behind NAT declaring fake TCP: the server moves to the peer and it dials out
/// as the client.
///
/// Were the node to declare who hosts the server, a machine behind NAT would contradict
/// itself the moment it declared one: it says "come dial me" while having no dialable
/// address. Under that model the compiler can only drop the declaration silently — neither
/// machine starts a phantun, the link falls back to bare UDP, and the diagnostics are empty
/// while the console is all green.
#[test]
fn a_nat_node_declaring_fake_tcp_gets_a_client_and_the_peer_hosts() {
    let mut tw = node("tw", Some("192.0.2.92"), [10, 66, 0, 2], true);
    tw.public_ipv4_nat = true;
    tw.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let jp = node("jp", Some("203.0.113.49"), [10, 66, 0, 1], true);
    let doc = doc(vec![tw, jp]);
    let mut diagnostics = Vec::new();

    let sys = compile_system(&doc, &mut diagnostics);
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );

    // The server is hosted on jp, using the port tw declared
    assert_eq!(sys.links.len(), 1);
    match &sys.links[0].wrap {
        LinkWrap::FakeTcp { servers } => {
            assert_eq!(
                servers.get("jp").map(String::as_str),
                Some("203.0.113.49:39743")
            );
            assert_eq!(servers.get("tw"), None, "NAT 那侧架不了服务端");
        }
        other => panic!("该穿外衣: {other:?}"),
    }
    // Links sort by id: a=jp and b=tw, and only tw can dial jp
    assert_eq!(link_dial(&sys, "jp", "tw"), Some(Dial::BtoA));

    let jp_plan = project_node(&sys, &[], "jp");
    let tw_plan = project_node(&sys, &[], "tw");

    let server = &jp_plan.phantun.as_ref().expect("jp 该架服务端").servers[0];
    assert_eq!(server.tcp_port, 39743);
    assert_eq!(server.forward_to_udp_port, 51820);
    assert!(jp_plan.phantun.as_ref().unwrap().clients.is_empty());

    let client = &tw_plan.phantun.as_ref().expect("tw 该起客户端").clients[0];
    assert_eq!(client.peer_node_id, "jp");
    assert_eq!(client.remote_tcp_endpoint, "203.0.113.49:39743");
    assert!(tw_plan.phantun.as_ref().unwrap().servers.is_empty());

    // tw's wg dials local loopback and the phantun client picks the packets up
    let peer = tw_plan
        .wireguard
        .as_ref()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.node_id == "jp")
        .unwrap();
    assert_eq!(
        peer.endpoint.as_deref(),
        Some(format!("127.0.0.1:{}", client.listen_udp_port).as_str())
    );

    // jp does not dial tw, so its wg0.conf carries no Endpoint — leaving the watchdog
    // nothing to push back, so those 96 rounds of tug of war vanish structurally rather than
    // being suppressed by a guard
    let back = jp_plan
        .wireguard
        .as_ref()
        .unwrap()
        .peers
        .iter()
        .find(|p| p.node_id == "tw")
        .unwrap();
    assert_eq!(back.endpoint, None);
    // Hosting a server requires a fixed wg port for the server to forward packets to
    assert_eq!(
        sys.nodes
            .iter()
            .find(|n| n.id == "jp")
            .unwrap()
            .wireguard
            .as_ref()
            .unwrap()
            .listen_port,
        Some(51820)
    );
}

/// With both ends behind NAT neither can host a server. This used to pass without a
/// word.
#[test]
fn fake_tcp_between_two_nat_nodes_is_reported() {
    let mut a = node("a", Some("1.1.1.1"), [10, 66, 0, 1], true);
    a.public_ipv4_nat = true;
    a.wireguard.transport = WgTransport::FakeTcp { port: 39743 };
    let mut b = node("b", Some("2.2.2.2"), [10, 66, 0, 2], true);
    b.public_ipv4_nat = true;
    let doc = doc(vec![a, b]);
    let mut diagnostics = Vec::new();

    let sys = compile_system(&doc, &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .any(|d| d.level == Level::Info && d.code == "link.fake-tcp-unhostable"),
        "{diagnostics:#?}"
    );
    // The same reasoning as `link.no-endpoint`: this pair was never meant to be required to
    // interconnect, generating no link suffices, and the whole model's shippability need not
    // be traded for it. The accompanying no-endpoint states the other side of the same
    // fact.
    assert!(sys.links.is_empty(), "{:#?}", sys.links);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.level == Level::Info && d.code == "link.no-endpoint"),
        "{diagnostics:#?}"
    );
    assert!(summarize_diagnostics(&diagnostics).can_publish);
}
