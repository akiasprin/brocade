use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    artifacts::subscription,
    format::{uri, yaml},
    ir::{routing::compile_app, system::compile_system, validate::validate_app},
    model::{
        AppView, Chain, Dns, DomainStrategy, ExternalOutbound, ExternalOutboundProtocol,
        ExternalOutboundSecurity, Front, FrontStrategy, Grant, Hysteria2, HysteriaBandwidth,
        HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, Ingress, IngressWires, IpFamily,
        ModelSnapshot, Node, Projection, ProjectionDownloadEndpoint, ProjectionEndpoint,
        RealityFallbackMode, RealityXhttp, Tls, TlsXhttp, Transport, User, WireGuardKeys, Xhttp,
        XhttpMode, XhttpXmux,
    },
    physical::user::{project_user, SubscriptionProtocol, UserPlan},
    Level,
};
use ipnet::Ipv4Net;

#[test]
fn uri_skips_front_entries_and_clash_renders_dialer_proxy_group() {
    let mut doc = doc(vec![
        node("hk", "hk.example.net", [10, 66, 0, 1]),
        node("us", "us.example.net", [10, 66, 0, 2]),
    ]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front", "香港入口"), chain("c-us", "美国出口")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            external_via: Vec::new(),
            strategy: FrontStrategy::UrlTest,
        }],
        steps: Vec::new(),
        grants: vec![grant("alice", "i-front"), grant("alice", "i-us")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error),
        "{diagnostics:#?}"
    );

    let plan = project_user(&[ir], "platform.acme", "alice");
    let artifact = subscription::build(&plan);
    let uri_text = uri::subscription(&artifact);
    let clash_text = yaml::clash_subscription(&artifact);
    let haitun_text = yaml::clash_haitun_subscription(&artifact);

    let mut front_only = artifact.clone();
    front_only
        .entries
        .retain(|entry| entry.front_name.is_some());
    let front_only_uri = uri::subscription(&front_only);
    assert!(
        front_only_uri
            .lines()
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with('#')),
        "只有前置代理时，URI 订阅只能包含注释：{front_only_uri:?}"
    );

    assert!(uri_text.contains("vless://uuid-alice@hk.example.net:443?"));
    assert!(uri_text.contains("#%E9%A6%99%E6%B8%AF%E5%85%A5%E5%8F%A3"));
    assert!(uri_text.contains("URI 列表表达不了"));
    assert!(uri_text.contains("换 Clash 目标即可"));
    assert!(!uri_text.contains("sing-box"));
    assert!(!uri_text.contains("us.example.net"));

    assert!(clash_text.contains("server: hk.example.net"));
    assert!(clash_text.contains("server: us.example.net"));
    assert!(clash_text.contains("  skip-domain:"), "{clash_text}");
    assert!(
        clash_text.contains("    - \"www.example.com\""),
        "{clash_text}"
    );
    assert_eq!(
        clash_text.matches("    - \"www.example.com\"").count(),
        1,
        "多个节点共用一个 servername 时只能输出一条 skip-domain：{clash_text}"
    );
    assert!(clash_text.contains("dialer-proxy: \"入口组\""));
    assert!(clash_text.contains("type: url-test"));
    assert!(clash_text.contains("proxies: [\"香港入口\"]"));
    assert!(!clash_text.contains("priv-i-front"));
    assert!(!clash_text.contains("priv-i-us"));

    assert!(
        haitun_text.starts_with("# Brocade · koipy 测速（请求时动态生成）"),
        "{haitun_text}"
    );
    assert!(haitun_text.contains("server: hk.example.net"));
    assert!(haitun_text.contains("server: us.example.net"));
    assert!(haitun_text.contains("dialer-proxy: \"入口组\""));
    assert!(haitun_text.contains("  - name: \"入口组\"\n    type: url-test"));
    assert!(haitun_text.contains(
        "  - name: \"koipy 测速\"\n    type: select\n    proxies: [\"香港入口\", \"美国出口\"]"
    ));
    assert!(haitun_text.contains("  - MATCH,koipy 测速"));
    assert!(!haitun_text.contains("rule-providers:"));
    assert!(!haitun_text.contains("dns:"));
    assert!(!haitun_text.contains("skip-domain:"));
    assert!(!haitun_text.contains("gstatic.com"));
}

#[test]
fn clash_publishes_only_explicit_manual_front_tunnels_and_keeps_warp_out() {
    let mut doc = doc(vec![
        node("hk", "hk.example.net", [10, 66, 0, 1]),
        node("us", "us.example.net", [10, 66, 0, 2]),
    ]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let socks = |id: &str, name: &str, address: &str, credential: &str| ExternalOutbound {
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: name.to_owned(),
        address: address.to_owned(),
        port: 1080,
        protocol: ExternalOutboundProtocol::Socks5 {
            username: Some("alice".to_owned()),
            credential: credential.to_owned(),
        },
        security: ExternalOutboundSecurity::None,
        bindings: Vec::new(),
    };
    doc.external_outbounds = vec![
        socks(
            "joined",
            "供应商前置",
            "joined.proxy.example",
            "joined-secret",
        ),
        socks(
            "unused",
            "未加入隧道",
            "unused.proxy.example",
            "unused-secret",
        ),
        ExternalOutbound {
            id: "warp".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "Cloudflare WARP".to_owned(),
            address: "engage.cloudflareclient.com".to_owned(),
            port: 2408,
            protocol: ExternalOutboundProtocol::Warp {
                mtu: 1280,
                keep_alive: 25,
                allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
                no_kernel_tun: true,
                domain_strategy: "ForceIP".to_owned(),
                workers: 0,
            },
            security: ExternalOutboundSecurity::None,
            bindings: Vec::new(),
        },
    ];
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front", "香港入口"), chain("c-us", "美国出口")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            // Include WARP deliberately: validation must reject it and projection must still
            // fail closed if a caller renders despite the diagnostic.
            external_via: vec!["joined".to_owned(), "warp".to_owned()],
            strategy: FrontStrategy::UrlTest,
        }],
        steps: Vec::new(),
        grants: vec![grant("alice", "i-front"), grant("alice", "i-us")],
    };
    let mut diagnostics = Vec::new();
    let system = compile_system(&doc, &mut diagnostics);
    let ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&system, &ir, &mut diagnostics);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "front.warp-machine-identity"),
        "{diagnostics:#?}"
    );

    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let clash = yaml::clash_subscription(&artifact);
    assert!(clash.contains("name: \"供应商前置\""), "{clash}");
    assert!(clash.contains("server: joined.proxy.example"), "{clash}");
    assert!(clash.contains("password: joined-secret"), "{clash}");
    assert!(
        clash.contains("proxies: [\"香港入口\", \"供应商前置\"]"),
        "{clash}"
    );
    assert!(!clash.contains("unused.proxy.example"), "{clash}");
    assert!(!clash.contains("unused-secret"), "{clash}");
    assert!(!clash.contains("engage.cloudflareclient.com"), "{clash}");
    assert!(!clash.contains("Cloudflare WARP"), "{clash}");
}

#[test]
fn clash_uses_the_fixed_subboost_standard_template_deterministically() {
    let (_, first) = render(|_| {});
    let (_, second) = render(|_| {});

    assert_eq!(first, second);
    assert!(first.starts_with("# Brocade · SubBoost 标准版"), "{first}");
    assert!(first.contains("mixed-port: 7897"), "{first}");
    assert_eq!(
        first
            .lines()
            .filter(|line| line.starts_with("  - name: \"") && !line.contains("香港"))
            .count(),
        14,
        "标准版必须有固定的 14 个分流组：{first}"
    );
    assert!(first.contains("  - RULE-SET,openai,🤖 AI 服务"));
    assert!(first.contains("  - RULE-SET,cn,🔒 国内服务"));
    assert!(first.contains("  - MATCH,🐟 漏网之鱼"));
    assert!(
        first.contains("MetaCubeX/meta-rules-dat/7a26c86cf0e7497a423ab86c37274b34e3ce4153/geo/")
    );
    assert!(!first.contains("refs/heads/"));
}

#[test]
fn clash_disambiguates_a_node_that_uses_a_standard_group_name() {
    let mut doc = doc(vec![node("hk", "hk.example.net", [10, 66, 0, 1])]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c", "🚀 节点选择")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let clash = yaml::clash_subscription(&artifact);

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(
        clash.matches("  - name: \"🚀 节点选择\"\n").count(),
        1,
        "节点名必须原样保留：{clash}"
    );
    assert!(
        clash.contains("  - name: \"🚀 节点选择 · Brocade 2\""),
        "标准组发生重名时应改自己的名字：{clash}"
    );
    assert!(clash.contains(
        "proxies: [\"🚀 节点选择 · Brocade 2\", \"⚡ 自动选择\", \"DIRECT\", \"REJECT\", \"🚀 节点选择\"]"
    ));
}

#[test]
fn uri_formats_ipv6_server_with_brackets() {
    let mut hk = node("hk", "hk.example.net", [10, 66, 0, 1]);
    hk.public_ipv4 = None;
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c", "香港入口")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let plan = project_user(&[ir], "platform.acme", "alice");
    let artifact = subscription::build(&plan);
    let uri_text = uri::subscription(&artifact);

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert!(uri_text.contains("vless://uuid-alice@[2001:db8::10]:443?"));
    assert!(artifact
        .entries
        .iter()
        .any(|entry| entry.name == "香港入口（IPv6）"));
    assert!(uri_text.contains("#%E9%A6%99%E6%B8%AF%E5%85%A5%E5%8F%A3%EF%BC%88IPv6%EF%BC%89"));
}

#[test]
fn subscription_expands_dual_stack_ingress_and_labels_ipv6() {
    let mut hk = node("hk", "hk.example.net", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c", "香港入口")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let uri_text = uri::subscription(&artifact);
    let clash_text = yaml::clash_subscription(&artifact);

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(artifact.entries.len(), 2);
    assert!(artifact
        .entries
        .iter()
        .any(|entry| entry.name == "香港入口" && entry.server == "hk.example.net"));
    assert!(artifact
        .entries
        .iter()
        .any(|entry| entry.name == "香港入口（IPv6）" && entry.server == "2001:db8::10"));
    assert!(uri_text.contains("vless://uuid-alice@hk.example.net:443?"));
    assert!(uri_text.contains("vless://uuid-alice@[2001:db8::10]:443?"));
    assert!(clash_text.contains("name: \"香港入口（IPv6）\""));
    assert!(clash_text.contains("server: 2001:db8::10"));
}

#[test]
fn explicit_subscription_country_prefixes_every_format_and_front_reference() {
    let mut hk = node("hk", "hk.example.net", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk, node("us", "us.example.net", [10, 66, 0, 2])]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let mut hk_chain = chain("c-front", "台湾入口");
    hk_chain.subscription_country = Some("TW".to_owned());
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![hk_chain, chain("c-us", "美国出口")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            external_via: Vec::new(),
            strategy: FrontStrategy::UrlTest,
        }],
        steps: Vec::new(),
        grants: vec![grant("alice", "i-front"), grant("alice", "i-us")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let uri_text = uri::subscription(&artifact);
    let clash_text = yaml::clash_subscription(&artifact);

    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error),
        "{diagnostics:#?}"
    );
    assert!(artifact
        .entries
        .iter()
        .any(|entry| entry.name == "🇹🇼 台湾入口"));
    assert!(artifact
        .entries
        .iter()
        .any(|entry| entry.name == "🇹🇼 台湾入口（IPv6）"));
    assert!(uri_text.contains("#%F0%9F%87%B9%F0%9F%87%BC%20%E5%8F%B0%E6%B9%BE%E5%85%A5%E5%8F%A3"));
    assert!(clash_text.contains("name: \"🇹🇼 台湾入口\""));
    assert!(clash_text.contains("proxies: [\"🇹🇼 台湾入口\", \"🇹🇼 台湾入口（IPv6）\"]"));
}

#[test]
fn subscription_skips_nat_public_ipv6() {
    let mut hk = node("hk", "hk.example.net", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    hk.public_ipv6_nat = true;
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c", "香港入口")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let uri_text = uri::subscription(&artifact);

    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(artifact.entries.len(), 1);
    assert_eq!(artifact.entries[0].name, "香港入口");
    assert_eq!(artifact.entries[0].server, "hk.example.net");
    assert!(!uri_text.contains("2001:db8::10"));
    assert!(!uri_text.contains("IPv6"));
}

#[test]
fn clash_front_group_keeps_dual_stack_via_members() {
    let mut hk = node("hk", "hk.example.net", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk, node("us", "us.example.net", [10, 66, 0, 2])]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front", "香港入口"), chain("c-us", "美国出口")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            external_via: Vec::new(),
            strategy: FrontStrategy::UrlTest,
        }],
        steps: Vec::new(),
        grants: vec![grant("alice", "i-front"), grant("alice", "i-us")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let clash_text = yaml::clash_subscription(&artifact);

    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error),
        "{diagnostics:#?}"
    );
    assert!(clash_text.contains("proxies: [\"香港入口\", \"香港入口（IPv6）\"]"));
}

#[test]
fn project_user_filters_by_tenant_and_user_id() {
    let mut doc = doc(vec![node("hk", "hk.example.net", [10, 66, 0, 1])]);
    doc.users.push(user("platform.acme", "alice", "uuid-acme"));
    doc.users.push(user("platform.beta", "alice", "uuid-beta"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c", "出口")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![Grant {
            tenant: "platform.beta".to_owned(),
            user: "alice".to_owned(),
            ingress: "i".to_owned(),
        }],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let acme = subscription::build(&project_user(
        std::slice::from_ref(&ir),
        "platform.acme",
        "alice",
    ));
    let beta = subscription::build(&project_user(&[ir], "platform.beta", "alice"));

    assert!(acme.entries.is_empty());
    assert_eq!(beta.entries.len(), 1);
    assert_eq!(beta.entries[0].uuid, "uuid-beta");
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 41,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: Vec::new(),
        external_outbounds: Vec::new(),
        apps: Vec::new(),
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

fn user(tenant: &str, id: &str, uuid: &str) -> User {
    User {
        tenant: tenant.to_owned(),
        id: id.to_owned(),
        uuid: uuid.to_owned(),
    }
}

fn chain(id: &str, name: &str) -> Chain {
    Chain {
        id: id.to_owned(),
        tenant: "platform.acme".to_owned(),
        name: name.to_owned(),
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
            short_ids: vec!["0123abcd".to_owned()],
        },
        wires: IngressWires::Vless(Transport::VlessReality(
            brocade_core::model::RealitySettings {
                dest: "www.example.com:443".to_owned(),
                server_names: vec!["www.example.com".to_owned()],
                fingerprint: "chrome".to_owned(),
                flow: Some("xtls-rprx-vision".to_owned()),
                fallback_mode: Default::default(),
                fallback_guard: true,
                fallback_limits: Default::default(),
            },
        )),
    }
}

fn grant(user: &str, ingress: &str) -> Grant {
    Grant {
        tenant: "platform.acme".to_owned(),
        user: user.to_owned(),
        ingress: ingress.to_owned(),
    }
}

// ── Projection ──────────────────────────────────────────────────────────
// Projection changes only the address handed to users. All four combinations are covered:
// each family projects on its own and they do not affect each other.

fn projected(host: &str, port: u16) -> Option<ProjectionEndpoint> {
    Some(ProjectionEndpoint {
        host: host.to_owned(),
        port,
        download: None,
    })
}

/// A node with public addresses in both v4 and v6, whose ingress projects per the
/// parameters. Returns (URI, Clash).
/// A subscription that does not carry the network layer produces a client configuration that
/// imports without complaint and connects to nothing: the server matches the request path
/// literally and answers `failed to validate path` to everything else.
#[test]
fn an_xhttp_ingress_reaches_both_subscription_formats() {
    let (uri_text, clash_text) = render_with_xhttp(Some(Xhttp {
        path: "/probe".to_owned(),
        host: None,
        xmux: Some(XhttpXmux::with_concurrency(16)),
        mode: XhttpMode::Auto,
    }));

    assert!(uri_text.contains("type=xhttp"), "{uri_text}");
    // Percent-encoded, because the path is a query parameter's value and a bare slash there is
    // ambiguous to some clients.
    assert!(uri_text.contains("path=%2Fprobe"), "{uri_text}");
    // In URI clients, a scalar `mux` enables the outer mux.cool protocol. It is not XHTTP's XMUX
    // concurrency and enabling it makes an otherwise valid XHTTP connection fail.
    assert!(!uri_text.contains("mux="), "{uri_text}");
    // The concurrency travels inside `extra` instead, which is the only field both client
    // families read it from — and it has to travel even with no independent download, which is
    // the case this pins: before, `extra` was written only alongside `downloadSettings` and the
    // upload concurrency reached nobody.
    for field in [
        "%22maxConcurrency%22%3A16",
        "%22hMaxRequestTimes%22%3A%22600-900%22",
        "%22hMaxReusableSecs%22%3A%221800-3000%22",
    ] {
        assert!(uri_text.contains(field), "{field} 没进入 URI：{uri_text}");
    }

    assert!(clash_text.contains("network: xhttp"), "{clash_text}");
    assert!(clash_text.contains("xhttp-opts:"), "{clash_text}");
    assert!(clash_text.contains("path: \"/probe\""), "{clash_text}");
    // mihomo's own key, `// aka XMUX` in its source. It has no `x-mux`, and it drops input keys
    // it does not know without a word, so this name is the whole setting.
    assert!(clash_text.contains("reuse-settings:"), "{clash_text}");
    assert!(!clash_text.contains("x-mux"), "{clash_text}");
    assert!(clash_text.contains("max-concurrency: 16"), "{clash_text}");
    assert!(
        clash_text.contains("h-max-request-times: \"600-900\""),
        "{clash_text}"
    );
    assert!(
        clash_text.contains("h-max-reusable-secs: \"1800-3000\""),
        "{clash_text}"
    );
}

/// One is the connection pool — a connection per stream, handed on when it goes idle — and it is
/// the only pooling a client can be given, Vision having closed the Mux.cool door. So it has to
/// pass validation and reach both formats like any other value.
#[test]
fn a_concurrency_of_one_is_a_pool_and_reaches_both_subscription_formats() {
    let (uri_text, clash_text) = render_with_xhttp(Some(Xhttp {
        path: "/probe".to_owned(),
        host: None,
        xmux: Some(XhttpXmux::with_concurrency(1)),
        mode: XhttpMode::Auto,
    }));

    for field in [
        "%22maxConcurrency%22%3A1",
        "%22hMaxRequestTimes%22%3A%22600-900%22",
        "%22hMaxReusableSecs%22%3A%221800-3000%22",
    ] {
        assert!(uri_text.contains(field), "{field} 没进入 URI：{uri_text}");
    }
    assert!(
        clash_text.contains("      reuse-settings:\n        max-concurrency: 1\n        h-max-request-times: \"600-900\"\n        h-max-reusable-secs: \"1800-3000\""),
        "{clash_text}"
    );
}

/// A server given an explicit upload shape refuses every client that names another — measured on
/// 26.4.25 across all sixteen server/client pairs. So the value has to reach the client, in both
/// formats, or the operator's own subscriptions stop working the moment it is set.
#[test]
fn an_explicit_upload_mode_reaches_both_subscription_formats() {
    let (uri_text, clash_text) = render_with_xhttp(Some(Xhttp {
        path: "/probe".to_owned(),
        host: None,
        xmux: None,
        mode: XhttpMode::PacketUp,
    }));

    assert!(uri_text.contains("mode=packet-up"), "{uri_text}");
    assert!(clash_text.contains("mode: \"packet-up\""), "{clash_text}");
}

/// The default is written nowhere, on both sides: a client left alone resolves it to the same
/// shape by itself, so spelling it out would change every artifact without changing anything a
/// machine does — and it would turn a default into a filter on subscriptions already in hand.
#[test]
fn the_default_upload_mode_is_written_nowhere() {
    let (uri_text, clash_text) = render_with_xhttp(Some(Xhttp {
        path: "/probe".to_owned(),
        host: None,
        xmux: None,
        mode: XhttpMode::Auto,
    }));

    assert!(!uri_text.contains("mode="), "{uri_text}");
    assert!(!clash_text.contains("\n      mode:"), "{clash_text}");
}

/// A subscription for a shape presenting the machine's own certificate must say so, and must not
/// carry REALITY's proof alongside. `pbk`/`sid` and `reality-opts` are what tell a client to
/// borrow a site instead of verifying a certificate: handed both, it is told two contradictory
/// things about what it is connecting to.
#[test]
fn a_tls_ingress_names_its_certificate_in_both_subscription_formats() {
    let (uri_text, clash_text) = render_with_tls(None);

    assert!(uri_text.contains("security=tls"), "{uri_text}");
    assert!(uri_text.contains("sni=hk-cert.example.net"), "{uri_text}");
    assert!(!uri_text.contains("pbk="), "{uri_text}");
    assert!(!uri_text.contains("sid="), "{uri_text}");

    assert!(
        clash_text.contains("servername: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(!clash_text.contains("reality-opts"), "{clash_text}");
    assert!(!clash_text.contains("public-key"), "{clash_text}");
}

/// A TLS entry must never tell a client to skip verification. xray removed `allowInsecure` in
/// v26.2.6 and rejects the whole config from v26.6.1 on, so the parameter does not loosen
/// verification any more — it produces a client that refuses to start. The setting now belongs to
/// the Hysteria 2 wire alone, where clients whose core is not xray still honour it.
#[test]
fn a_tls_entry_never_tells_the_client_to_skip_verification() {
    let (uri_text, clash_text) = render(|face| {
        if let Some(Transport::VlessReality(reality)) = face.wires.vless() {
            face.wires = IngressWires::Vless(Transport::VlessTls(Tls {
                flow: reality.flow.clone(),
                fingerprint: reality.fingerprint.clone(),
            }));
        }
    });

    assert!(uri_text.contains("security=tls"), "{uri_text}");
    assert!(!uri_text.contains("allowInsecure"), "{uri_text}");
    assert!(!clash_text.contains("skip-cert-verify"), "{clash_text}");
}

/// The CDN shape: its own certificate *and* the HTTP layer, both of which the client needs.
#[test]
fn a_tls_xhttp_ingress_carries_both_halves() {
    let (uri_text, clash_text) = render_with_tls(Some(Xhttp {
        path: "/probe".to_owned(),
        host: None,
        xmux: Some(XhttpXmux::with_concurrency(8)),
        mode: XhttpMode::Auto,
    }));

    assert!(uri_text.contains("security=tls"), "{uri_text}");
    assert!(uri_text.contains("type=xhttp"), "{uri_text}");
    assert!(uri_text.contains("path=%2Fprobe"), "{uri_text}");

    assert!(clash_text.contains("network: xhttp"), "{clash_text}");
    assert!(clash_text.contains("path: \"/probe\""), "{clash_text}");
    assert!(!clash_text.contains("reality-opts"), "{clash_text}");
}

#[test]
fn a_tls_xhttp_projection_can_use_an_independent_download_endpoint() {
    let (uri_text, clash_text) = render(|face| {
        face.wires = IngressWires::Vless(Transport::VlessTlsXhttp(TlsXhttp {
            tls: Tls {
                flow: Some(String::new()),
                fingerprint: "chrome".to_owned(),
            },
            xhttp: Xhttp {
                path: "/probe".to_owned(),
                host: None,
                xmux: Some(XhttpXmux::with_concurrency(8)),
                mode: XhttpMode::Auto,
            },
        }));
        face.projection.v4 = Some(ProjectionEndpoint {
            host: "104.21.35.113".to_owned(),
            port: 8443,
            download: Some(ProjectionDownloadEndpoint {
                host: "172.67.218.231".to_owned(),
                port: 8443,
                origin_port: None,
                http_host: None,
                mux: None,
            }),
        });
    });

    assert!(uri_text.contains("@104.21.35.113:8443?"), "{uri_text}");
    assert!(uri_text.contains("extra=%7B"), "{uri_text}");
    assert!(
        uri_text.contains("%22address%22%3A%22172.67.218.231%22"),
        "{uri_text}"
    );
    assert!(
        uri_text.contains("%22serverName%22%3A%22hk-cert.example.net%22"),
        "{uri_text}"
    );
    assert!(!uri_text.contains("mux="), "{uri_text}");

    assert!(clash_text.contains("download-settings:"), "{clash_text}");
    assert!(
        clash_text.contains("server: 172.67.218.231"),
        "{clash_text}"
    );
    assert!(clash_text.contains("port: 8443"), "{clash_text}");
    assert!(
        clash_text.contains("servername: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(
        clash_text.contains("host: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(clash_text.contains("path: \"/probe\""), "{clash_text}");
    // The upload half named a concurrency and the download half did not, so exactly one
    // `reuse-settings` block belongs here — the one indented under `xhttp-opts`, not under
    // `download-settings`. Counted rather than matched on the bare name: both blocks spell the
    // key identically and only the indentation tells them apart.
    assert_eq!(
        clash_text.matches("reuse-settings:").count(),
        1,
        "{clash_text}"
    );
    assert!(
        clash_text.contains("      reuse-settings:\n        max-concurrency: 8"),
        "{clash_text}"
    );
}

#[test]
fn a_reality_xhttp_projection_uses_tls_only_for_its_download() {
    let (uri_text, clash_text) = render(|face| {
        face.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
            reality: face.wires.reality().unwrap().clone(),
            xhttp: Xhttp {
                path: "/probe".to_owned(),
                host: Some("upload.route.example".to_owned()),
                xmux: Some(XhttpXmux::with_concurrency(8)),
                mode: XhttpMode::Auto,
            },
        }));
        face.wires.set_flow(None);
        face.projection.v4 = Some(ProjectionEndpoint {
            host: "198.51.100.10".to_owned(),
            port: 443,
            download: Some(ProjectionDownloadEndpoint {
                host: "cdn.example.net".to_owned(),
                port: 443,
                origin_port: Some(8443),
                http_host: Some("download.route.example".to_owned()),
                mux: Some(24),
            }),
        });
    });

    assert!(uri_text.contains("security=reality"), "{uri_text}");
    assert!(uri_text.contains("host=upload.route.example"), "{uri_text}");
    assert!(uri_text.contains("download.route.example"), "{uri_text}");
    assert!(uri_text.contains("pbk="), "{uri_text}");
    assert!(
        uri_text.contains("%22security%22%3A%22tls%22"),
        "{uri_text}"
    );
    assert!(
        uri_text.contains("%22serverName%22%3A%22hk-cert.example.net%22"),
        "{uri_text}"
    );
    assert!(clash_text.contains("reality-opts:"), "{clash_text}");
    assert!(
        clash_text.contains("servername: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(
        clash_text.contains("server: cdn.example.net"),
        "{clash_text}"
    );
    assert!(clash_text.contains("port: 443"), "{clash_text}");
    assert!(
        clash_text.contains("host: download.route.example"),
        "{clash_text}"
    );
    assert!(clash_text.contains("max-concurrency: 24"), "{clash_text}");
    assert!(!clash_text.contains("port: 8443"), "{clash_text}");
}

/// The default must keep saying `tcp` in both formats, since every client configuration already
/// in somebody's hands was written against it.
#[test]
fn a_tcp_ingress_is_unchanged_by_the_new_field() {
    let (uri_text, clash_text) = render_with_xhttp(None);

    assert!(uri_text.contains("type=tcp"), "{uri_text}");
    assert!(!uri_text.contains("path="), "{uri_text}");
    assert!(clash_text.contains("network: tcp"), "{clash_text}");
    assert!(!clash_text.contains("xhttp-opts"), "{clash_text}");
}

#[test]
fn hysteria2_subscription_carries_auth_obfs_sni_and_bandwidth() {
    let (uri_text, clash_text) = render(|face| {
        face.wires = IngressWires::Hysteria2(Hysteria2 {
            bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
            quic: brocade_core::model::HysteriaQuic::default(),
            port: 50000,
            hop: None,
            bandwidth: HysteriaBandwidth {
                up: Some("20 mbps".to_owned()),
                down: Some("100 mbps".to_owned()),
            },
            congestion: HysteriaCongestion::Brutal,
            obfs: Some(HysteriaObfs::Salamander {
                password: "camouflage secret".to_owned(),
            }),
            masquerade: HysteriaMasquerade::Proxy {
                url: "https://cover.example.net/".to_owned(),
            },
        });
    });

    assert!(
        uri_text.contains("hysteria2://uuid-alice@203.0.113.7:50000/"),
        "{uri_text}"
    );
    assert!(uri_text.contains("sni=hk-cert.example.net"), "{uri_text}");
    assert!(uri_text.contains("obfs=salamander"), "{uri_text}");
    assert!(
        uri_text.contains("obfs-password=camouflage%20secret"),
        "{uri_text}"
    );
    assert!(clash_text.contains("type: hysteria2"), "{clash_text}");
    assert!(clash_text.contains("password: uuid-alice"), "{clash_text}");
    assert!(
        clash_text.contains("sni: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(clash_text.contains("obfs: salamander"), "{clash_text}");
    assert!(clash_text.contains("up: \"20 mbps\""), "{clash_text}");
    assert!(clash_text.contains("down: \"100 mbps\""), "{clash_text}");
    // Masquerade is server-only and must not leak into a subscription.
    assert!(!uri_text.contains("cover.example.net"), "{uri_text}");
    assert!(!clash_text.contains("cover.example.net"), "{clash_text}");
}

/// Port hopping is spelled differently by each client, and both spellings are load-bearing.
///
/// The hysteria2 URI scheme has no query parameter for it — the range goes in the port component
/// (`host:50000-50009`), so a client that does not understand it sees a malformed port and
/// refuses the entry outright. mihomo takes `ports` and documents it as *ignoring* `port`, so the
/// two are alternatives: emitting both leaves a config whose visible `port` is not the one in use.
///
/// Both were read off the upstream documents rather than recalled, and this test is where that
/// reading is written down.
#[test]
fn hy2_port_hopping_is_written_into_the_port_component_and_clash_ports() {
    let (uri_text, clash_text) = render(|face| {
        face.wires = IngressWires::Hysteria2(Hysteria2 {
            port: 50000,
            hop: Some(brocade_core::model::HysteriaPortHop {
                start: 50000,
                end: 50009,
            }),
            ..Default::default()
        });
    });

    assert!(
        uri_text.contains("hysteria2://uuid-alice@203.0.113.7:50000-50009/"),
        "{uri_text}"
    );
    assert!(
        clash_text.contains("ports: \"50000-50009\""),
        "{clash_text}"
    );
    // The single-port key would be ignored by mihomo, and read as authoritative by a person.
    assert!(!clash_text.contains("port: 50000"), "{clash_text}");
}

/// Hopping off is the ordinary case and must stay byte-identical to what it was before hopping
/// existed: one number in the port component, `port:` in Clash, no range key anywhere.
#[test]
fn without_hopping_a_subscription_names_one_port() {
    let (uri_text, clash_text) = render(|face| {
        face.wires = IngressWires::Hysteria2(Hysteria2 {
            port: 50000,
            hop: None,
            ..Default::default()
        });
    });

    assert!(
        uri_text.contains("hysteria2://uuid-alice@203.0.113.7:50000/"),
        "{uri_text}"
    );
    assert!(clash_text.contains("port: 50000"), "{clash_text}");
    assert!(!clash_text.contains("\n    ports:"), "{clash_text}");
}

/// Two wires, two entries — and two distinct names.
///
/// A client speaks one or the other, never both, so the subscription lists them side by side and
/// lets the person choose. The suffix is not decoration: mihomo refuses a proxy list outright
/// when two entries share a name, so without it a two-wire ingress produces a Clash file that
/// will not load at all.
#[test]
fn an_ingress_serving_both_wires_lists_both_and_keeps_the_names_apart() {
    let (uri_text, clash_text) = render(|face| {
        let vless = face.wires.vless().unwrap().clone();
        face.wires = IngressWires::Both {
            vless,
            hysteria2: Hysteria2::default(),
        };
    });

    assert_eq!(uri_text.matches("vless://").count(), 1, "{uri_text}");
    assert_eq!(uri_text.matches("hysteria2://").count(), 1, "{uri_text}");
    assert_eq!(clash_text.matches("type: vless").count(), 1, "{clash_text}");
    assert_eq!(
        clash_text.matches("type: hysteria2").count(),
        1,
        "{clash_text}"
    );

    let proxy_section = clash_text
        .split_once("\nproxy-groups:")
        .map(|(proxies, _)| proxies)
        .unwrap_or(&clash_text);
    let names = proxy_section
        .lines()
        .filter(|line| line.trim_start().starts_with("- name:"))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "{clash_text}");
    assert_ne!(names[0], names[1], "两条线重名，mihomo 会整份拒掉");
    assert!(names.iter().any(|name| name.contains("QUIC")), "{names:#?}");
}

#[test]
fn a_dual_wire_subscription_can_be_narrowed_to_either_protocol() {
    let make_plan = || {
        plan(|face| {
            let vless = face.wires.vless().unwrap().clone();
            face.wires = IngressWires::Both {
                vless,
                hysteria2: Hysteria2::default(),
            };
        })
    };

    let mut vless = make_plan();
    vless.retain_protocol(SubscriptionProtocol::Vless);
    let vless = subscription::build(&vless);
    let vless_uri = uri::subscription(&vless);
    let vless_clash = yaml::clash_subscription(&vless);
    assert!(vless_uri.contains("vless://"), "{vless_uri}");
    assert!(!vless_uri.contains("hysteria2://"), "{vless_uri}");
    assert!(vless_clash.contains("type: vless"), "{vless_clash}");
    assert!(!vless_clash.contains("type: hysteria2"), "{vless_clash}");

    let mut hysteria = make_plan();
    hysteria.retain_protocol(SubscriptionProtocol::Hysteria2);
    let hysteria = subscription::build(&hysteria);
    let hysteria_uri = uri::subscription(&hysteria);
    let hysteria_clash = yaml::clash_subscription(&hysteria);
    assert!(!hysteria_uri.contains("vless://"), "{hysteria_uri}");
    assert!(hysteria_uri.contains("hysteria2://"), "{hysteria_uri}");
    assert!(!hysteria_clash.contains("type: vless"), "{hysteria_clash}");
    assert!(
        hysteria_clash.contains("type: hysteria2"),
        "{hysteria_clash}"
    );
}

/// No subscription this fleet hands out may tell a client to skip certificate verification, on
/// either wire. xray removed `allowInsecure` in v26.2.6 and rejects the whole config from v26.6.1
/// on, so the fleet's own core cannot load a configuration carrying the equivalent field; a
/// subscription offering it would describe a client this deployment cannot run. A certificate
/// that does not verify is a fault to fix on the machine, and the setting no longer exists.
#[test]
fn no_subscription_ever_tells_a_client_to_skip_verification() {
    let (uri_text, clash_text) = render(|face| {
        let vless = face.wires.vless().unwrap().clone();
        face.wires = IngressWires::Both {
            vless,
            hysteria2: Hysteria2 {
                port: 50000,
                hop: None,
                ..Hysteria2::default()
            },
        };
    });

    assert!(uri_text.contains("hysteria2://"), "{uri_text}");
    assert!(uri_text.contains("vless://"), "{uri_text}");
    assert!(!uri_text.contains("insecure"), "{uri_text}");
    assert!(clash_text.contains("type: hysteria2"), "{clash_text}");
    assert!(clash_text.contains("type: vless"), "{clash_text}");
    assert!(!clash_text.contains("skip-cert-verify"), "{clash_text}");
}

fn render_with_tls(xhttp: Option<Xhttp>) -> (String, String) {
    render(|face| {
        let tls = Tls {
            // Off: this shape's XHTTP half refuses flow control, and the TCP half is not what
            // these two cases are about.
            flow: Some(String::new()),
            fingerprint: "chrome".to_owned(),
        };
        face.wires = IngressWires::Vless(match xhttp.clone() {
            None => Transport::VlessTls(tls),
            Some(xhttp) => Transport::VlessTlsXhttp(TlsXhttp { tls, xhttp }),
        });
    })
}

#[test]
fn node_certificate_fallback_uses_the_certificate_name_as_reality_sni() {
    let (uri_text, clash_text) = render(|face| {
        face.wires.reality_mut().unwrap().fallback_mode = RealityFallbackMode::NodeCertificate;
    });

    assert!(uri_text.contains("sni=hk-cert.example.net"), "{uri_text}");
    assert!(!uri_text.contains("sni=www.example.com"), "{uri_text}");
    assert!(
        clash_text.contains("servername: hk-cert.example.net"),
        "{clash_text}"
    );
    assert!(
        clash_text.contains("  skip-domain:\n    - \"hk-cert.example.net\""),
        "{clash_text}"
    );
    assert!(
        !clash_text
            .split("proxies:")
            .next()
            .unwrap_or_default()
            .contains("www.example.com"),
        "节点证书回落不能把外部站点名留在 skip-domain：{clash_text}"
    );
}

fn render_with_xhttp(xhttp: Option<Xhttp>) -> (String, String) {
    render(|face| {
        if let Some(xhttp) = xhttp.clone() {
            face.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
                reality: face.wires.reality().unwrap().clone(),
                xhttp,
            }));
        }
    })
}

fn render(shape: impl FnOnce(&mut Ingress)) -> (String, String) {
    let artifact = subscription::build(&plan(shape));
    (
        uri::subscription(&artifact),
        yaml::clash_subscription(&artifact),
    )
}

fn plan(shape: impl FnOnce(&mut Ingress)) -> UserPlan {
    let mut hk = node("hk", "203.0.113.7", [10, 66, 0, 1]);
    hk.certificate_name = Some("hk-cert.example.net".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let mut face = ingress("i-hk", "c-hk", "hk", None);
    shape(&mut face);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hk", "香港")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-hk")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error),
        "{diagnostics:#?}"
    );
    project_user(&[ir], "platform.acme", "alice")
}

fn render_with_projection(projection: Projection) -> (String, String) {
    let mut hk = node("hk", "203.0.113.7", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let mut face = ingress("i-hk", "c-hk", "hk", None);
    face.projection = projection;
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hk", "香港")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-hk")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error),
        "{diagnostics:#?}"
    );
    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    (
        uri::subscription(&artifact),
        yaml::clash_subscription(&artifact),
    )
}

#[test]
fn no_projection_keeps_the_node_public_addresses() {
    let (uri_text, clash_text) = render_with_projection(Projection::default());

    assert!(uri_text.contains("@203.0.113.7:443?"));
    assert!(uri_text.contains("@[2001:db8::10]:443?"));
    assert!(clash_text.contains("server: 203.0.113.7"));
    assert!(clash_text.contains("server: 2001:db8::10"));
}

#[test]
fn projecting_one_family_leaves_the_other_on_its_public_address() {
    let (uri_text, clash_text) = render_with_projection(Projection {
        v4: projected("cu.acc.example.net", 20443),
        v6: None,
    });

    // v4 becomes the projected address and port — note that the port follows the address
    // and is no longer the ingress's 443.
    assert!(uri_text.contains("@cu.acc.example.net:20443?"));
    assert!(!uri_text.contains("203.0.113.7"));
    // v6 is unprojected and stays the node's own address plus the listening port.
    assert!(uri_text.contains("@[2001:db8::10]:443?"));

    assert!(clash_text.contains("server: cu.acc.example.net"));
    assert!(clash_text.contains("port: 20443"));
    assert!(clash_text.contains("server: 2001:db8::10"));
}

#[test]
fn projecting_both_families_replaces_both_addresses() {
    let (uri_text, clash_text) = render_with_projection(Projection {
        v4: projected("cu.acc.example.net", 20443),
        v6: projected("v6.acc.example.net", 30443),
    });

    assert!(uri_text.contains("@cu.acc.example.net:20443?"));
    assert!(uri_text.contains("@v6.acc.example.net:30443?"));
    assert!(!uri_text.contains("203.0.113.7"));
    assert!(!uri_text.contains("2001:db8::10"));

    // Both artifacts take the same entry.server, so the Clash one follows automatically.
    assert!(clash_text.contains("server: cu.acc.example.net"));
    assert!(clash_text.contains("server: v6.acc.example.net"));
    assert!(!clash_text.contains("server: 203.0.113.7"));
}

#[test]
fn an_ipv6_literal_projection_still_gets_brackets_in_the_uri() {
    let (uri_text, clash_text) = render_with_projection(Projection {
        v4: None,
        v6: projected("2001:db8:acc::9", 30443),
    });

    // uri_host keys on containing a colon, regardless of whether the address came from a
    // projection.
    assert!(uri_text.contains("@[2001:db8:acc::9]:30443?"));
    // Clash's server carries no brackets, matching how v6 is written without a
    // projection.
    assert!(clash_text.contains("server: 2001:db8:acc::9"));
}

#[test]
fn projection_ignores_nat_and_a_missing_public_address() {
    // This machine is behind NAT and has no public v6 yet: without a projection it emits
    // nothing at all (leaving only the "?" fallback). The optimized line, meanwhile, can be
    // dialed — a projection is an external line's endpoint and has nothing to do with this
    // machine's own position on the network.
    let mut hk = node("hk", "10.0.0.7", [10, 66, 0, 1]);
    hk.public_ipv4_nat = true;
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let mut face = ingress("i-hk", "c-hk", "hk", None);
    face.projection = Projection {
        v4: projected("cu.acc.example.net", 20443),
        v6: None,
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hk", "香港")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-hk")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    let artifact = subscription::build(&project_user(&[ir], "platform.acme", "alice"));
    let uri_text = uri::subscription(&artifact);

    assert!(uri_text.contains("@cu.acc.example.net:20443?"));
    // The address behind NAT must not leak, and the "?" fallback must not appear — the
    // projection has already answered this question.
    assert!(!uri_text.contains("10.0.0.7"));
    assert!(!uri_text.contains("@?:"));
}

/// A dual-stack ingress rendered for one family only. Both are compiled first and one is then
/// dropped, which is what the console hands an operator whose subscriber reaches only one.
fn render_for_family(family: IpFamily) -> (String, String) {
    let mut hk = node("hk", "203.0.113.7", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hk", "香港")],
        ingresses: vec![ingress("i-hk", "c-hk", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-hk")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    let mut plan = project_user(&[ir], "platform.acme", "alice");
    plan.retain_family(family);
    let artifact = subscription::build(&plan);
    (
        uri::subscription(&artifact),
        yaml::clash_subscription(&artifact),
    )
}

#[test]
fn retaining_v4_drops_the_v6_entries() {
    let (uri_text, clash_text) = render_for_family(IpFamily::V4);

    assert!(uri_text.contains("@203.0.113.7:443?"));
    assert!(!uri_text.contains("2001:db8::10"));
    assert!(clash_text.contains("server: 203.0.113.7"));
    assert!(!clash_text.contains("server: 2001:db8::10"));
}

#[test]
fn retaining_v6_drops_the_v4_entries() {
    let (uri_text, clash_text) = render_for_family(IpFamily::V6);

    assert!(uri_text.contains("@[2001:db8::10]:443?"));
    assert!(!uri_text.contains("203.0.113.7"));
    assert!(clash_text.contains("server: 2001:db8::10"));
    assert!(!clash_text.contains("server: 203.0.113.7"));
}

/// A projected host says nothing about which family it resolves to, so the family has to travel
/// with the entry from the projection slot it came from. Parsing it back out of `server` would
/// keep both of these lines under either filter.
#[test]
fn a_projected_host_is_filtered_by_the_slot_it_came_from() {
    let mut hk = node("hk", "203.0.113.7", [10, 66, 0, 1]);
    hk.public_ipv6 = Some("2001:db8::10".to_owned());
    let mut doc = doc(vec![hk]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let mut face = ingress("i-hk", "c-hk", "hk", None);
    face.projection = Projection {
        v4: projected("v4.acc.example.net", 20443),
        v6: projected("v6.acc.example.net", 30443),
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hk", "香港")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-hk")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);
    let mut plan = project_user(&[ir], "platform.acme", "alice");
    plan.retain_family(IpFamily::V6);
    let uri_text = uri::subscription(&subscription::build(&plan));

    assert!(uri_text.contains("@v6.acc.example.net:30443?"));
    assert!(!uri_text.contains("v4.acc.example.net"));
}

/// The front group's only member serves v4 alone. Filtering to v6 empties the group, and the
/// exit that dials through it has to go too: mihomo would import a proxy whose `dialer-proxy`
/// names a group that is not there.
#[test]
fn a_family_that_empties_a_front_group_drops_the_exits_behind_it() {
    let mut front_node = node("hk", "203.0.113.7", [10, 66, 0, 1]);
    front_node.public_ipv6 = None;
    let mut exit = node("us", "198.51.100.9", [10, 66, 0, 2]);
    exit.public_ipv6 = Some("2001:db8:us::9".to_owned());
    let mut doc = doc(vec![front_node, exit]);
    doc.users.push(user("platform.acme", "alice", "uuid-alice"));
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front", "香港入口"), chain("c-us", "美国出口")],
        ingresses: vec![
            ingress("i-front", "c-front", "hk", None),
            ingress("i-us", "c-us", "us", Some("f")),
        ],
        fronts: vec![Front {
            id: "f".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "入口组".to_owned(),
            via: vec!["i-front".to_owned()],
            external_via: Vec::new(),
            strategy: FrontStrategy::UrlTest,
        }],
        steps: Vec::new(),
        grants: vec![grant("alice", "i-front"), grant("alice", "i-us")],
    };
    let mut diagnostics = Vec::new();
    let ir = compile_app(&doc, &app, &mut diagnostics);

    let mut both = project_user(std::slice::from_ref(&ir), "platform.acme", "alice");
    both.retain_family(IpFamily::V4);
    let v4_clash = yaml::clash_subscription(&subscription::build(&both));
    assert!(v4_clash.contains("proxies: [\"香港入口\"]"));
    assert!(v4_clash.contains("dialer-proxy: \"入口组\""));

    let mut v6 = project_user(&[ir], "platform.acme", "alice");
    v6.retain_family(IpFamily::V6);
    let v6_clash = yaml::clash_subscription(&subscription::build(&v6));
    assert!(!v6_clash.contains("入口组"));
    assert!(!v6_clash.contains("dialer-proxy"));
    assert!(!v6_clash.contains("2001:db8:us::9"));
}
