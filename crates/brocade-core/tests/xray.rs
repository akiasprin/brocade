use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    artifacts::{grants, xray},
    format::json,
    ir::{
        hops::compile_hops,
        routing::{compile_app, AppIr},
        system::compile_system,
    },
    model::{
        Accept, Action, AnyTls, AnyTlsMasquerade, AppView, Chain, DestMatch, Dns, DomainStrategy,
        EgressDnsAddressStrategy, EgressDnsFallback, EgressDnsResolution, EgressDnsTransport,
        ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalVlessTransport, ExternalVlessXhttp, ExternalVlessXhttpDownload,
        ExternalWarpBinding, Grant, HopDial, HopEncryption, HopIn, HopPool, HopWire, Hysteria2,
        HysteriaBandwidth, HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, Ingress,
        IngressWires, IpFamily, ModelSettings, ModelSnapshot, Network, Node, NodeEgressDnsPolicy,
        OverlaySettings, Reality, RealityClientPolicy, RealityFallbackLimits, RealityFallbackMode,
        RealitySite, RealityXhttp, Rule, Step, Transport, User, WireGuardKeys, Xhttp, XhttpMode,
        XhttpTuning, XhttpXmuxRange,
    },
    physical::node::{project_node, reality_fallback_limits},
    Level,
};
use ipnet::Ipv4Net;
use serde_json::Value;

#[test]
fn managed_xhttp_listener_tuning_reaches_the_server_artifact() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: face.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/probe".to_owned(),
            host: None,
            xmux: None,
            tuning: Some(XhttpTuning {
                x_padding_bytes: Some(XhttpXmuxRange::new(200, 600)),
            }),
            mode: XhttpMode::PacketUp,
            download: None,
        },
    }));
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );
    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let settings = &inbound(&value, "in:app/i")["streamSettings"]["xhttpSettings"];

    assert_eq!(settings["xPaddingBytes"], "200-600");
    assert!(settings.get("scMaxEachPostBytes").is_none());
    assert!(settings.get("scMaxBufferedPosts").is_none());
    assert!(settings.get("scMinPostsIntervalMs").is_none());
    assert!(settings.get("uplinkChunkSize").is_none());
    assert!(settings.get("xmux").is_none());
}

#[test]
fn external_proxy_action_renders_protocol_security_and_only_on_the_referencing_node() {
    let mut doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    doc.external_outbounds = vec![
        ExternalOutbound {
            id: "vendor-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "供应商边缘".to_owned(),
            address: "edge.vendor.example".to_owned(),
            port: 443,
            protocol: ExternalOutboundProtocol::Vless {
                credential: "external-uuid".to_owned(),
                encryption: "none".to_owned(),
                flow: Some("xtls-rprx-vision".to_owned()),
                transport: ExternalVlessTransport::Raw,
            },
            security: ExternalOutboundSecurity::Reality {
                server_name: "www.example.com".to_owned(),
                public_key: "reality-public-key".to_owned(),
                short_id: "0123abcd".to_owned(),
                fingerprint: "chrome".to_owned(),
            },
            bindings: Vec::new(),
        },
        ExternalOutbound {
            id: "xhttp-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "XHTTP 边缘".to_owned(),
            address: "upload.vendor.example".to_owned(),
            port: 443,
            protocol: ExternalOutboundProtocol::Vless {
                credential: "external-xhttp-uuid".to_owned(),
                encryption: "none".to_owned(),
                flow: None,
                transport: ExternalVlessTransport::Xhttp(ExternalVlessXhttp {
                    path: "/external-xhttp".to_owned(),
                    host: Some("upload-http.vendor.example".to_owned()),
                    mux: Some(4),
                    mode: XhttpMode::StreamUp,
                    download: Some(ExternalVlessXhttpDownload {
                        address: "download.vendor.example".to_owned(),
                        port: 8443,
                        security: ExternalOutboundSecurity::Tls {
                            server_name: "download.vendor.example".to_owned(),
                            fingerprint: "chrome".to_owned(),
                        },
                        path: "/external-download".to_owned(),
                        host: Some("download-http.vendor.example".to_owned()),
                        mux: Some(2),
                        mode: XhttpMode::Auto,
                    }),
                }),
            },
            security: ExternalOutboundSecurity::Reality {
                server_name: "www.example.com".to_owned(),
                public_key: "reality-public-key".to_owned(),
                short_id: "0123abcd".to_owned(),
                fingerprint: "chrome".to_owned(),
            },
            bindings: Vec::new(),
        },
        ExternalOutbound {
            id: "socks-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "SOCKS5 边缘".to_owned(),
            address: "socks.vendor.example".to_owned(),
            port: 1080,
            protocol: ExternalOutboundProtocol::Socks5 {
                username: Some("proxy-user".to_owned()),
                credential: "proxy-password".to_owned(),
            },
            security: ExternalOutboundSecurity::None,
            bindings: Vec::new(),
        },
        ExternalOutbound {
            id: "http-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "HTTP CONNECT 边缘".to_owned(),
            address: "http.vendor.example".to_owned(),
            port: 8443,
            protocol: ExternalOutboundProtocol::HttpConnect {
                username: None,
                credential: String::new(),
            },
            security: ExternalOutboundSecurity::Tls {
                server_name: "http.vendor.example".to_owned(),
                fingerprint: "chrome".to_owned(),
            },
            bindings: Vec::new(),
        },
        ExternalOutbound {
            id: "ss-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "SS 边缘".to_owned(),
            address: "ss.vendor.example".to_owned(),
            port: 8388,
            protocol: ExternalOutboundProtocol::Shadowsocks2022 {
                credential: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
                method: "2022-blake3-aes-256-gcm".to_owned(),
            },
            security: ExternalOutboundSecurity::None,
            bindings: Vec::new(),
        },
        ExternalOutbound {
            id: "wireguard-edge".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "WireGuard 边缘".to_owned(),
            address: "wg.vendor.example".to_owned(),
            port: 2408,
            protocol: ExternalOutboundProtocol::Wireguard {
                credential: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
                peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
                local_addresses: vec!["172.16.0.2/32".to_owned()],
                mtu: 1420,
                reserved: vec![0, 0, 0],
                keep_alive: 25,
                allowed_ips: vec!["0.0.0.0/0".to_owned()],
                no_kernel_tun: true,
                domain_strategy: "ForceIPv4".to_owned(),
            },
            security: ExternalOutboundSecurity::None,
            bindings: Vec::new(),
        },
    ];
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c",
                "hk",
                vec![
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["openai.com".to_owned()]),
                        action: Action::Proxy {
                            outbound: "vendor-edge".to_owned(),
                        },
                    },
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["xhttp.example".to_owned()]),
                        action: Action::Proxy {
                            outbound: "xhttp-edge".to_owned(),
                        },
                    },
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["example.net".to_owned()]),
                        action: Action::Proxy {
                            outbound: "socks-edge".to_owned(),
                        },
                    },
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["example.org".to_owned()]),
                        action: Action::Proxy {
                            outbound: "http-edge".to_owned(),
                        },
                    },
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["wireguard.example".to_owned()]),
                        action: Action::Proxy {
                            outbound: "wireguard-edge".to_owned(),
                        },
                    },
                    Rule {
                        dest_match: DestMatch::Any,
                        action: Action::Proxy {
                            outbound: "ss-edge".to_owned(),
                        },
                    },
                ],
                None,
            ),
            step(
                "c",
                "sg",
                vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Block,
                }],
                None,
            ),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    let hk = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "hk",
    )));
    let external = outbound(&hk, "out:external/vendor-edge");
    assert_eq!(external["protocol"], "vless");
    assert_eq!(external["settings"]["address"], "edge.vendor.example");
    assert_eq!(external["settings"]["id"], "external-uuid");
    assert_eq!(external["settings"]["flow"], "xtls-rprx-vision");
    assert_eq!(external["streamSettings"]["security"], "reality");
    assert_eq!(
        external["streamSettings"]["realitySettings"]["publicKey"],
        "reality-public-key"
    );
    let xhttp = outbound(&hk, "out:external/xhttp-edge");
    assert_eq!(xhttp["protocol"], "vless");
    assert_eq!(xhttp["streamSettings"]["network"], "xhttp");
    assert_eq!(
        xhttp["streamSettings"]["xhttpSettings"]["path"],
        "/external-xhttp"
    );
    assert_eq!(
        xhttp["streamSettings"]["xhttpSettings"]["mode"],
        "stream-up"
    );
    assert_eq!(
        xhttp["streamSettings"]["xhttpSettings"]["xmux"]["maxConcurrency"],
        4
    );
    let download = &xhttp["streamSettings"]["xhttpSettings"]["downloadSettings"];
    assert_eq!(download["address"], "download.vendor.example");
    assert_eq!(download["port"], 8443);
    assert_eq!(download["network"], "xhttp");
    assert_eq!(download["security"], "tls");
    assert_eq!(download["xhttpSettings"]["path"], "/external-download");
    assert_eq!(download["xhttpSettings"]["xmux"]["maxConcurrency"], 2);
    let socks = outbound(&hk, "out:external/socks-edge");
    assert_eq!(socks["protocol"], "socks");
    assert_eq!(socks["settings"]["user"], "proxy-user");
    assert_eq!(socks["settings"]["pass"], "proxy-password");
    assert_eq!(socks["streamSettings"]["security"], "none");
    let http = outbound(&hk, "out:external/http-edge");
    assert_eq!(http["protocol"], "http");
    assert!(http["settings"].get("user").is_none());
    assert!(http["settings"].get("pass").is_none());
    assert_eq!(http["streamSettings"]["security"], "tls");
    assert_eq!(
        http["streamSettings"]["tlsSettings"]["serverName"],
        "http.vendor.example"
    );
    let shadowsocks = outbound(&hk, "out:external/ss-edge");
    assert_eq!(shadowsocks["protocol"], "shadowsocks");
    assert_eq!(shadowsocks["settings"]["method"], "2022-blake3-aes-256-gcm");
    assert_eq!(
        shadowsocks["settings"]["password"],
        "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY="
    );
    assert_eq!(shadowsocks["streamSettings"]["security"], "none");
    let wireguard = outbound(&hk, "out:external/wireguard-edge");
    assert_eq!(wireguard["protocol"], "wireguard");
    assert_eq!(
        wireguard["settings"]["peers"][0]["endpoint"],
        "wg.vendor.example:2408"
    );
    assert_eq!(wireguard["settings"]["address"][0], "172.16.0.2/32");
    assert_eq!(wireguard["settings"]["domainStrategy"], "ForceIPv4");
    assert!(wireguard.get("streamSettings").is_none());
    assert!(hk["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| { rule["outboundTag"] == "out:external/vendor-edge" }));

    let sg = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "sg")));
    assert!(!sg["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| { candidate["tag"] == "out:external/vendor-edge" }));
}

#[test]
fn managed_warp_lowers_to_a_distinct_wireguard_identity_on_each_machine() {
    let mut doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    let private_hk = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
    let private_sg = "QG4l1cVXHNVPQxL0FKBTaFAsuGSKLFB39JYFhTOEXFo=";
    let binding = |node: &str, private_key: &str, address: &str| ExternalWarpBinding {
        node: node.to_owned(),
        device_id: format!("device-{node}"),
        account_id: format!("account-{node}"),
        registered_at: "2026-08-27T12:00:00.000Z".to_owned(),
        endpoint_address: (node == "hk").then(|| "162.159.193.10".to_owned()),
        endpoint_port: (node == "hk").then_some(500),
        mtu: (node == "hk").then_some(1420),
        keep_alive: (node == "hk").then_some(40),
        allowed_ips: (node == "hk").then(|| vec!["::/0".to_owned()]),
        no_kernel_tun: (node == "hk").then_some(false),
        domain_strategy: (node == "hk").then(|| "ForceIPv6".to_owned()),
        workers: (node == "hk").then_some(4),
        private_key: private_key.to_owned(),
        peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
        local_addresses: vec![
            address.to_owned(),
            if node == "hk" {
                "2606:4700:110:8::2/128".to_owned()
            } else {
                "2606:4700:110:8::3/128".to_owned()
            },
        ],
        reserved: vec![1, 2, 3],
    };
    doc.external_outbounds = vec![ExternalOutbound {
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
        bindings: vec![
            binding("hk", private_hk, "172.16.0.2/32"),
            binding("sg", private_sg, "172.16.0.3/32"),
        ],
    }];
    let proxy_rule = || Rule {
        dest_match: DestMatch::Any,
        action: Action::Proxy {
            outbound: "warp".to_owned(),
        },
    };
    let app = AppView {
        id: "warp-app".to_owned(),
        label: "WARP".to_owned(),
        chains: vec![chain("c-hk"), chain("c-sg")],
        ingresses: vec![ingress("i-hk", "c-hk", "hk"), ingress("i-sg", "c-sg", "sg")],
        fronts: Vec::new(),
        steps: vec![
            step("c-hk", "hk", vec![proxy_rule()], None),
            step("c-sg", "sg", vec![proxy_rule()], None),
        ],
        grants: Vec::new(),
    };

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "{diagnostics:#?}"
    );

    // A tenant tunnel is shared rather than copied into a project. Referencing the same WARP
    // from another project on the same machine must still produce exactly one Xray outbound;
    // duplicating a WireGuard identity in one process can create competing interfaces/routes.
    let second_app = AppView {
        id: "warp-app-secondary".to_owned(),
        label: "WARP secondary".to_owned(),
        chains: vec![chain("c-hk-secondary")],
        ingresses: vec![ingress("i-hk-secondary", "c-hk-secondary", "hk")],
        fronts: Vec::new(),
        steps: vec![step("c-hk-secondary", "hk", vec![proxy_rule()], None)],
        grants: Vec::new(),
    };
    let second_app_ir = compile_hops(
        compile_app(&doc, &second_app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "{diagnostics:#?}"
    );

    let hk = parse_xray(&xray::build(&project_node(
        &sys,
        &[app_ir.clone(), second_app_ir],
        "hk",
    )));
    let sg = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "sg",
    )));
    let hk_warp = outbound(&hk, "out:external/warp");
    let sg_warp = outbound(&sg, "out:external/warp");
    assert_eq!(hk_warp["protocol"], "wireguard");
    assert_eq!(sg_warp["protocol"], "wireguard");
    assert_eq!(hk_warp["settings"]["secretKey"], private_hk);
    assert_eq!(sg_warp["settings"]["secretKey"], private_sg);
    assert_eq!(
        hk_warp["settings"]["address"],
        serde_json::json!(["2606:4700:110:8::2/128"]),
        "逐机地址策略应同时限制本地接口地址族"
    );
    assert_eq!(sg_warp["settings"]["address"][0], "172.16.0.3/32");
    assert_eq!(
        hk_warp["settings"]["peers"][0]["allowedIPs"],
        serde_json::json!(["::/0"])
    );
    assert_eq!(hk_warp["settings"]["peers"][0]["keepAlive"], 40);
    assert_eq!(hk_warp["settings"]["domainStrategy"], "ForceIPv6");
    assert_eq!(hk_warp["settings"]["noKernelTun"], false);
    assert_eq!(hk_warp["settings"]["workers"], 4);
    assert_eq!(
        hk_warp["settings"]["peers"][0]["endpoint"], "162.159.193.10:500",
        "机器级 Endpoint 应覆盖逻辑隧道默认值"
    );
    assert_eq!(hk_warp["settings"]["mtu"], 1420);
    assert_eq!(
        sg_warp["settings"]["peers"][0]["endpoint"], "engage.cloudflareclient.com:2408",
        "没有覆盖的机器应继续继承逻辑隧道默认值"
    );
    assert_eq!(sg_warp["settings"]["mtu"], 1280);
    assert_eq!(sg_warp["settings"]["peers"][0]["keepAlive"], 25);
    assert_eq!(sg_warp["settings"]["domainStrategy"], "ForceIP");
    assert_eq!(sg_warp["settings"]["noKernelTun"], true);
    assert!(sg_warp["settings"].get("workers").is_none());
    assert_eq!(
        hk["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|candidate| candidate["tag"] == "out:external/warp")
            .count(),
        1,
        "同一租户隧道被多个项目引用时，一台机器只能生成一个 Xray outbound"
    );
    assert_ne!(
        hk_warp["settings"]["secretKey"], sg_warp["settings"]["secretKey"],
        "共享一个 WARP 逻辑资源不能让两台机器复用同一个 WireGuard peer"
    );

    let ExternalOutboundProtocol::Warp {
        allowed_ips,
        domain_strategy,
        ..
    } = &mut doc.external_outbounds[0].protocol
    else {
        unreachable!()
    };
    *allowed_ips = vec!["0.0.0.0/0".to_owned()];
    *domain_strategy = "ForceIPv4".to_owned();
    doc.external_outbounds[0].bindings[0].allowed_ips = None;
    doc.external_outbounds[0].bindings[0].domain_strategy = None;
    let mut ipv4_diagnostics = Vec::new();
    let ipv4_sys = compile_system(&doc, &mut ipv4_diagnostics);
    let ipv4_app = compile_hops(
        compile_app(&doc, &app, &mut ipv4_diagnostics),
        &ipv4_sys,
        &mut ipv4_diagnostics,
    );
    assert!(
        ipv4_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "{ipv4_diagnostics:#?}"
    );
    let ipv4 = parse_xray(&xray::build(&project_node(&ipv4_sys, &[ipv4_app], "hk")));
    let ipv4_warp = outbound(&ipv4, "out:external/warp");
    assert_eq!(
        ipv4_warp["settings"]["address"],
        serde_json::json!(["172.16.0.2/32"]),
        "IPv4-only WARP must not leave the registered IPv6 address on Xray's interface"
    );
    assert_eq!(
        ipv4_warp["settings"]["peers"][0]["allowedIPs"],
        serde_json::json!(["0.0.0.0/0"])
    );
    assert_eq!(ipv4_warp["settings"]["domainStrategy"], "ForceIPv4");
}

#[test]
fn node_certificate_reality_fallback_is_a_loopback_tls_403() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("cover.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    let reality = face.wires.reality_mut().unwrap();
    reality.fallback_mode = RealityFallbackMode::NodeCertificate;
    reality.fallback_limits = RealityFallbackLimits::Balanced;
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "hk",
    )));
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    // Tied to the cover's own port rather than to the number the allocator happens to start from.
    // What this test is about is the two ends agreeing; a prefix like `127.0.0.1:2` says only that
    // the base has not moved, and goes quietly false the day it does.
    assert_eq!(
        reality["dest"],
        serde_json::json!(format!(
            "127.0.0.1:{}",
            inbound(&value, "in:app/i:cover")["port"]
        )),
        "{reality:#?}"
    );
    assert_eq!(
        reality["serverNames"],
        serde_json::json!(["cover.example.net"])
    );
    assert!(
        reality["limitFallbackUpload"]["bytesPerSec"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        reality["limitFallbackDownload"]["bytesPerSec"]
            .as_u64()
            .unwrap()
            > 0
    );

    let cover = inbound(&value, "in:app/i:cover");
    assert_eq!(cover["listen"], "127.0.0.1");
    assert_eq!(cover["protocol"], "dokodemo-door");
    assert_eq!(cover["streamSettings"]["security"], "tls");
    assert_eq!(
        outbound(&value, xray::REALITY_COVER_OUTBOUND_TAG)["settings"]["response"]["type"],
        "http"
    );
    let cover_rule = value["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["outboundTag"] == xray::REALITY_COVER_OUTBOUND_TAG)
        .expect("cover routing rule");
    assert_eq!(
        cover_rule["inboundTag"],
        serde_json::json!(["in:app/i:cover"])
    );
    assert_ne!(cover_rule["outboundTag"], xray::INTERNAL_OUTBOUND_TAG);
    assert!(!value.to_string().contains("www.example.com"), "{value:#?}");
}

/// REALITY hands every unauthenticated connection to `dest` — including one asking for a name
/// this ingress never borrowed. Where the borrowed site sits on shared infrastructure, that
/// address answers for its neighbours too, and the machine becomes a free way to reach all of
/// them. The guard is the door that reads the name first.
///
/// The two rules are asserted together with their order, because order is the mechanism: the
/// blanket deny placed above the allow would drop every fallback, the borrowed site's handshake
/// among them, and REALITY would stop working while every field in this artifact still looked
/// right.
#[test]
fn reality_fallback_reaches_only_the_borrowed_name() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "hk",
    )));
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    let guard = inbound(&value, "in:app/i:guard");
    assert_eq!(
        reality["dest"],
        format!("127.0.0.1:{}", guard["port"].as_u64().unwrap())
    );
    // The name the client must present is untouched: the door moved, the borrowed identity did not.
    assert_eq!(
        reality["serverNames"],
        serde_json::json!(["www.example.com"])
    );

    assert_eq!(guard["listen"], "127.0.0.1");
    assert_eq!(guard["protocol"], "dokodemo-door");
    assert_eq!(guard["settings"]["address"], "www.example.com");
    assert_eq!(guard["settings"]["port"], 443);
    // Terminating TLS here is impossible — the certificate is somebody else's.
    assert_eq!(guard["streamSettings"]["security"], "none");
    assert_eq!(guard["sniffing"]["enabled"], true);
    assert_eq!(
        guard["sniffing"]["destOverride"],
        serde_json::json!(["tls"])
    );
    assert_eq!(guard["sniffing"]["routeOnly"], true);

    let rules = value["routing"]["rules"].as_array().unwrap();
    let position = |outbound_tag: &str| {
        rules
            .iter()
            .position(|rule| {
                rule["inboundTag"] == serde_json::json!(["in:app/i:guard"])
                    && rule["outboundTag"] == outbound_tag
            })
            .unwrap_or_else(|| panic!("没有指向 {outbound_tag} 的 guard 规则: {rules:#?}"))
    };
    let allow = position(xray::INTERNAL_OUTBOUND_TAG);
    let deny = position(xray::REALITY_GUARD_OUTBOUND_TAG);
    assert!(allow < deny, "{rules:#?}");
    assert_eq!(
        rules[allow]["domain"],
        serde_json::json!(["full:www.example.com"])
    );
    // A close, not a page: what reaches here is a stranger's TLS, and an HTTP body inside it
    // would announce that something other than the borrowed site is listening.
    assert!(
        outbound(&value, xray::REALITY_GUARD_OUTBOUND_TAG)["settings"]["response"].is_null(),
        "{value:#?}"
    );
}

/// No names to admit means no door, rather than a door that admits everyone.
///
/// The empty list cannot be published (`reality.no-sni`), so this is about what the artifact does
/// on the paths that build without that gate. An allow rule built from no names carries no
/// `domain` key, and a rule with no condition matches everything the door saw — the deny rule
/// below it would then never fire, and the ingress would look guarded in the artifact and in the
/// console while forwarding every stranger to the borrowed site.
#[test]
fn an_ingress_with_no_borrowed_names_gets_no_door_at_all() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let mut face = ingress("i", "c", "hk");
    face.wires.reality_mut().unwrap().server_names = Vec::new();
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    assert!(!value.to_string().contains(":guard"), "{value:#?}");
    assert!(
        !value.to_string().contains(xray::REALITY_GUARD_OUTBOUND_TAG),
        "{value:#?}"
    );
    // And the fallback goes where it went before the guard existed, rather than to a loopback
    // door that would now be answering nothing.
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    assert_eq!(reality["dest"], "www.example.com:443");
}

#[test]
fn xray_reality_guard_splits_the_borrowed_site_into_address_and_port() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let mut face = ingress("i", "c", "hk");
    face.wires.reality_mut().unwrap().dest = "apps.apple.com: 8443".to_owned();
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let guard = inbound(&value, "in:app/i:guard");
    assert_eq!(guard["settings"]["address"], "apps.apple.com");
    assert_eq!(guard["settings"]["port"], 8443);
}

/// Turning it off restores exactly the artifact of before the guard existed: `dest` back on the
/// borrowed site, no extra listener, no rule and no outbound left behind naming a door that is
/// not there.
#[test]
fn an_unguarded_reality_ingress_keeps_dialling_the_borrowed_site_itself() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let mut face = ingress("i", "c", "hk");
    face.wires.reality_mut().unwrap().fallback_guard = false;
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    assert_eq!(reality["dest"], "www.example.com:443");
    assert!(!value.to_string().contains(":guard"), "{value:#?}");
    assert!(
        !value.to_string().contains(xray::REALITY_GUARD_OUTBOUND_TAG),
        "{value:#?}"
    );
}

/// Hysteria 2's shape is spread over four places in one inbound, and getting any of them
/// wrong fails in a way nobody can read off a log.
///
/// `alpn` is the one to watch. Xray v26.4.25 — the version this fleet pins — does not add h3
/// on the server side (`transport/internet/hysteria/hub.go` calls `GetTLSConfig()` with no
/// options, so the TLS config falls through to its `h2, http/1.1` default), while every
/// client offers h3 and nothing else. Omit it and each handshake dies with
/// `no application protocol` — reported to the client, logged nowhere on the server. Upstream
/// added the default in `#6186`, released in v26.6.1; writing it here is correct on both
/// sides of that line, which is what keeps the artifact a function of the snapshot alone.
#[test]
fn a_hysteria2_ingress_writes_h3_and_its_quic_parameters() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Hysteria2(Hysteria2 {
        bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
        quic: brocade_core::model::HysteriaQuic::default(),
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth {
            up: Some("200 mbps".to_owned()),
            down: Some("500 mbps".to_owned()),
        },
        congestion: HysteriaCongestion::Brutal,
        obfs: Some(HysteriaObfs::Salamander {
            password: "k7f2c1a9e4b8".to_owned(),
        }),
        masquerade: HysteriaMasquerade::Proxy {
            url: "https://www.bing.com".to_owned(),
        },
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    // The QUIC listener always carries the suffix, even where it is the only one. Naming it
    // after the wire rather than after "whether there happens to be a second wire" is what lets
    // a VLESS half be added later without renaming this inbound — and renaming an inbound means
    // removing it, which drops every session on it for a change that should only add.
    let face = inbound(&value, "in:app/i:hy2");
    let stream = &face["streamSettings"];

    assert_eq!(face["protocol"], "hysteria");
    assert_eq!(face["settings"]["version"], 2);
    // The account list is empty here as it is everywhere: users are pushed into the running
    // process, never written into the config.
    assert_eq!(face["settings"]["clients"], serde_json::json!([]));

    assert_eq!(stream["network"], "hysteria");
    assert_eq!(stream["security"], "tls");
    assert_eq!(stream["tlsSettings"]["alpn"], serde_json::json!(["h3"]));
    assert_eq!(stream["hysteriaSettings"]["version"], 2);
    assert_eq!(stream["hysteriaSettings"]["masquerade"]["type"], "proxy");
    assert_eq!(
        stream["hysteriaSettings"]["masquerade"]["url"],
        "https://www.bing.com"
    );

    let quic = &stream["finalmask"]["quicParams"];
    assert_eq!(quic["congestion"], "brutal");
    assert_eq!(quic["brutalUp"], "200 mbps");
    assert_eq!(quic["brutalDown"], "500 mbps");
    assert_eq!(stream["finalmask"]["udp"][0]["type"], "salamander");
    assert_eq!(
        stream["finalmask"]["udp"][0]["settings"]["password"],
        "k7f2c1a9e4b8"
    );
}

/// AnyTLS is a second TLS-protected TCP listener, not a transport variant of VLESS. Keep both
/// in the artifact at once and assert every server-side setting reaches the fork's JSON shape.
#[test]
fn an_anytls_ingress_writes_a_distinct_tls_listener_and_server_settings() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    let vless = face.wires.vless().unwrap().clone();
    face.wires = IngressWires::VlessAndAnyTls {
        vless,
        anytls: AnyTls {
            port: 19443,
            padding_scheme: vec![
                "stop=2".to_owned(),
                "0=30-30".to_owned(),
                "1=70000-70000".to_owned(),
            ],
            masquerade: AnyTlsMasquerade::String {
                content: "Forbidden".to_owned(),
                headers: [("Content-Type".to_owned(), "text/plain".to_owned())]
                    .into_iter()
                    .collect(),
                status_code: 403,
            },
            ..AnyTls::default()
        },
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let vless = inbound(&value, "in:app/i");
    let anytls = inbound(&value, "in:app/i:anytls");
    assert_eq!(vless["protocol"], "vless");
    assert_eq!(vless["port"], 443);
    assert_eq!(anytls["protocol"], "anytls");
    assert_eq!(anytls["port"], 19443);
    assert_eq!(anytls["settings"]["users"], serde_json::json!([]));
    assert_eq!(
        anytls["settings"]["paddingScheme"],
        serde_json::json!(["stop=2", "0=30-30", "1=70000-70000"])
    );
    assert_eq!(anytls["settings"]["masquerade"]["type"], "string");
    assert_eq!(anytls["settings"]["masquerade"]["content"], "Forbidden");
    assert_eq!(anytls["settings"]["masquerade"]["statusCode"], 403);
    assert_eq!(
        anytls["settings"]["masquerade"]["headers"]["Content-Type"],
        "text/plain"
    );
    assert_eq!(anytls["streamSettings"]["network"], "tcp");
    assert_eq!(anytls["streamSettings"]["security"], "tls");
    assert_eq!(
        anytls["streamSettings"]["sockopt"]["tcpFastOpen"], 256,
        "AnyTLS listener must enable server-side TFO"
    );
    assert_eq!(
        anytls["streamSettings"]["tlsSettings"]["certificates"][0]["certificateFile"],
        xray::NODE_CERTIFICATE_FILE
    );
}

#[test]
fn an_anytls_only_reality_ingress_uses_its_own_identity_and_custom_site() {
    let hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    let mut reality = face.wires.reality().unwrap().clone();
    reality.fallback_mode = RealityFallbackMode::CustomSite;
    reality.fallback_guard = false;
    let anytls_identity = brocade_core::model::IngressIdentity {
        private_key: "anytls-priv-i".to_owned(),
        public_key: "anytls-pub-i".to_owned(),
        short_ids: vec!["89abcdef".to_owned()],
    };
    face.anytls_identity = Some(anytls_identity.clone());
    face.wires = IngressWires::AnyTls(AnyTls {
        port: 19443,
        security: brocade_core::model::AnyTlsSecurity::Reality,
        reality: Some(reality),
        ..AnyTls::default()
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let anytls = inbound(&value, "in:app/i:anytls");
    let reality = &anytls["streamSettings"]["realitySettings"];
    assert_eq!(anytls["streamSettings"]["security"], "reality");
    assert_eq!(reality["privateKey"], anytls_identity.private_key);
    assert_ne!(reality["privateKey"], "priv-i");
    assert_eq!(
        reality["serverNames"],
        serde_json::json!(["www.example.com"])
    );
    assert_eq!(reality["dest"], "www.example.com:443");
    assert!(value["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .all(|candidate| candidate["tag"] != "in:app/i:anytls:cover"));
}

#[test]
fn vless_and_anytls_reality_keep_separate_sites_and_identities_when_both_are_enabled() {
    let hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    let mut anytls_reality = face.wires.reality().unwrap().clone();
    anytls_reality.dest = "cover.example.net:443".to_owned();
    anytls_reality.server_names = vec!["cover.example.net".to_owned()];
    anytls_reality.fallback_mode = RealityFallbackMode::CustomSite;
    anytls_reality.fallback_guard = false;
    let anytls_identity = brocade_core::model::IngressIdentity {
        private_key: "anytls-priv-i".to_owned(),
        public_key: "anytls-pub-i".to_owned(),
        short_ids: vec!["89abcdef".to_owned()],
    };
    face.anytls_identity = Some(anytls_identity.clone());
    face.wires = IngressWires::VlessAndAnyTls {
        vless: face.wires.vless().unwrap().clone(),
        anytls: AnyTls {
            port: 19443,
            security: brocade_core::model::AnyTlsSecurity::Reality,
            reality: Some(anytls_reality),
            ..AnyTls::default()
        },
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let vless = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    let anytls = &inbound(&value, "in:app/i:anytls")["streamSettings"]["realitySettings"];
    assert_eq!(vless["privateKey"], "priv-i");
    assert_eq!(vless["serverNames"], serde_json::json!(["www.example.com"]));
    assert_eq!(anytls["privateKey"], anytls_identity.private_key);
    assert_eq!(
        anytls["serverNames"],
        serde_json::json!(["cover.example.net"])
    );
    assert_ne!(vless["privateKey"], anytls["privateKey"]);
}

#[test]
fn an_anytls_ingress_without_an_override_uses_the_global_padding_scheme() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let mut doc = doc(vec![hk]);
    doc.settings.anytls_padding_scheme = vec![
        "stop=4".to_owned(),
        "0=21-28".to_owned(),
        "1=55-91".to_owned(),
        "2=90-125,c,180-245".to_owned(),
        "3=180-440".to_owned(),
    ];
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::AnyTls(AnyTls {
        port: 19443,
        ..AnyTls::default()
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    assert_eq!(
        inbound(&value, "in:app/i:anytls")["settings"]["paddingScheme"],
        serde_json::json!(doc.settings.anytls_padding_scheme)
    );
}

/// Two hopping ingresses on one entrance, each carrying a different chain: the property the
/// field bug broke. A client's port range folds onto a listener (`hy2_port_hop`), the listener
/// is a distinct `:hy2` inbound, and the routing table has to carry that inbound to its own
/// chain. It did not — the rule named only the base tag, so traffic on either `:hy2` inbound
/// matched nothing, fell through the chain table, and reached whichever outbound sat first.
/// Every port range arrived at one fixed node. Here the two ranges must reach two different
/// links.
#[test]
fn two_hysteria2_ingresses_route_each_port_range_to_its_own_chain() {
    let mut edge = node("edge", [10, 66, 0, 1], true, Dns::System);
    edge.certificate_name = Some("edge.example.net".to_owned());
    let hka = node("hka", [10, 66, 0, 2], true, Dns::System);
    let sga = node("sga", [10, 66, 0, 3], true, Dns::System);
    let doc = doc(vec![edge, hka, sga]);

    let mut to_hka = ingress("i-hka", "c-hka", "edge");
    to_hka.wires = IngressWires::Hysteria2(Hysteria2 {
        port: 50000,
        hop: Some(brocade_core::model::HysteriaPortHop {
            start: 50000,
            end: 50009,
        }),
        ..Hysteria2::default()
    });
    let mut to_sga = ingress("i-sga", "c-sga", "edge");
    to_sga.wires = IngressWires::Hysteria2(Hysteria2 {
        port: 50010,
        hop: Some(brocade_core::model::HysteriaPortHop {
            start: 50010,
            end: 50019,
        }),
        ..Hysteria2::default()
    });

    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-hka"), chain("c-sga")],
        ingresses: vec![to_hka, to_sga],
        fronts: Vec::new(),
        steps: vec![
            step("c-hka", "edge", vec![forward("hka")], None),
            step(
                "c-hka",
                "hka",
                vec![any_egress()],
                Some(accept("uuid-hka", "c-hka@hka")),
            ),
            step("c-sga", "edge", vec![forward("sga")], None),
            step(
                "c-sga",
                "sga",
                vec![any_egress()],
                Some(accept("uuid-sga", "c-sga@sga")),
            ),
        ],
        grants: Vec::new(),
    };

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "edge")));
    let rules = value["routing"]["rules"].as_array().unwrap();

    // The outbound a hopping inbound routes to. Before the fix this find() returned None for
    // either tag: no rule named a `:hy2` inbound at all.
    let outbound_for = |inbound: &str| -> String {
        rules
            .iter()
            .find(|rule| {
                rule["inboundTag"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|tag| tag == inbound))
            })
            .unwrap_or_else(|| panic!("没有指向 {inbound} 入站的路由规则: {rules:#?}"))
            ["outboundTag"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // Each range reaches its own chain's forward outbound — two different links, which is exactly
    // what failed in the field where both ranges landed on one node.
    assert_eq!(outbound_for("in:app/i-hka:hy2"), "out:app/c-hka>hka");
    assert_eq!(outbound_for("in:app/i-sga:hy2"), "out:app/c-sga>sga");
}

/// An ingress's refusals have to reach its Hysteria 2 half. The guard compiles to `out:block`
/// rules ahead of the chain's forwarding and selects on the inbound tag, so the same
/// base-tag-only omission that misrouted hy2 also let hy2 traffic slip every refusal. The default
/// guard already blocks private networks and bittorrent, so this is the common case; and the
/// UDP-oriented guards refuse abuse only the QUIC wire can carry.
#[test]
fn an_ingress_guard_also_covers_its_hysteria2_inbound() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);

    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Hysteria2(Hysteria2::default());
    face.guard = brocade_core::model::IngressGuard {
        no_private: true,
        ..brocade_core::model::IngressGuard::OPEN
    };

    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let rules = value["routing"]["rules"].as_array().unwrap();

    // A refusal (→ out:block) keyed on the hy2 listener. Absent before the fix: the guard named
    // only the base tag, and a Hysteria 2-only ingress has no inbound under it — so the QUIC half
    // was refused nothing.
    let blocks_hy2 = rules.iter().any(|rule| {
        rule["outboundTag"] == "out:block"
            && rule["inboundTag"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag == "in:app/i:hy2"))
    });
    assert!(blocks_hy2, "guard 未覆盖 hy2 入站: {rules:#?}");
}

/// The point of the whole shape: one ingress, two listeners, one credential.
///
/// They share a port number and nothing else. TCP and UDP have independent port spaces, so this
/// is not a collision — and both the per-machine and the cross-view port checks have to agree
/// with the kernel about that, or a legitimate release is blocked by a clash that does not exist.
#[test]
fn an_ingress_serving_both_wires_builds_two_inbounds_on_two_ports() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    let reality = face.wires.vless().unwrap().clone();
    face.wires = IngressWires::Both {
        vless: reality,
        hysteria2: Hysteria2::default(),
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let plan = project_node(&sys, &[app_ir], "hk");
    let value = parse_xray(&xray::build(&plan));
    let tcp = inbound(&value, "in:app/i");
    let udp = inbound(&value, "in:app/i:hy2");

    assert_eq!(tcp["protocol"], "vless");
    assert_eq!(tcp["streamSettings"]["security"], "reality");
    assert_eq!(udp["protocol"], "hysteria");
    assert_eq!(udp["streamSettings"]["security"], "tls");
    assert_eq!(
        udp["streamSettings"]["tlsSettings"]["alpn"],
        serde_json::json!(["h3"])
    );
    // Two wires, two numbers. They shared one until port hopping arrived: a hop range is a
    // redirect over a run of UDP ports, and the first one written without `-p udp` would take
    // the TCP wire on that same number down with it (`ingress.hy2-port-shared`).
    assert_eq!(tcp["port"], 443);
    assert_eq!(udp["port"], 18000);

    // Both listeners have to be fed, or half the subscriptions are turned away as unknown users.
    let tags = plan
        .grant_sync
        .updates
        .iter()
        .map(|update| update.inbound_tag.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tags, vec!["in:app/i", "in:app/i:hy2"]);
    // Flow is VLESS's. Carried onto a Hysteria account it would never match what the machine
    // reports back, and every convergence round would see drift it cannot resolve.
    let quic_clients = &plan.grant_sync.updates[1].clients;
    assert!(
        quic_clients.iter().all(|client| client.flow.is_none()),
        "{quic_clients:#?}"
    );
}

/// BBR ignores the operator's numbers, so writing them would hand xray a rate it will not use
/// and hand a reader a value that means nothing.
#[test]
fn a_bbr_hysteria2_ingress_writes_no_brutal_rates() {
    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Hysteria2(Hysteria2 {
        bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
        quic: brocade_core::model::HysteriaQuic::default(),
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth {
            up: Some("200 mbps".to_owned()),
            down: Some("500 mbps".to_owned()),
        },
        congestion: HysteriaCongestion::Bbr,
        obfs: None,
        masquerade: HysteriaMasquerade::NotFound,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let stream = &inbound(&value, "in:app/i:hy2")["streamSettings"];
    let quic = &stream["finalmask"]["quicParams"];

    assert_eq!(quic["congestion"], "bbr");
    assert!(quic.get("brutalUp").is_none(), "{quic:#?}");
    assert!(quic.get("brutalDown").is_none(), "{quic:#?}");
    assert!(stream["finalmask"].get("udp").is_none(), "{stream:#?}");
    assert_eq!(stream["hysteriaSettings"]["masquerade"]["type"], "404");
}

/// The tuning knobs are written only when set, and `reno` suppresses the rates the same way `bbr`
/// does.
///
/// "Only when set" is the whole point: an absent key means xray applies the default of the version
/// it is actually running. Writing today's default instead would freeze it into every artifact,
/// and the artifact would stop tracking the binary it is deployed against.
#[test]
fn hysteria2_quic_tuning_is_written_only_where_the_operator_set_it() {
    use brocade_core::model::{HysteriaBbrProfile, HysteriaQuic};

    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Hysteria2(Hysteria2 {
        bbr_profile: HysteriaBbrProfile::Conservative,
        quic: HysteriaQuic {
            init_stream_receive_window: Some(131_072),
            max_idle_timeout_secs: Some(45),
            disable_path_mtu_discovery: true,
            ..HysteriaQuic::default()
        },
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth {
            up: Some("200 mbps".to_owned()),
            down: Some("500 mbps".to_owned()),
        },
        congestion: HysteriaCongestion::Reno,
        obfs: None,
        masquerade: HysteriaMasquerade::NotFound,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let quic = &inbound(&value, "in:app/i:hy2")["streamSettings"]["finalmask"]["quicParams"];

    assert_eq!(quic["congestion"], "reno");
    // reno has no target rate at all, so the pair goes unwritten exactly as under bbr.
    assert!(quic.get("brutalUp").is_none(), "{quic:#?}");
    assert!(quic.get("brutalDown").is_none(), "{quic:#?}");

    assert_eq!(quic["bbrProfile"], "conservative");
    assert_eq!(quic["initStreamReceiveWindow"], 131_072);
    assert_eq!(quic["maxIdleTimeout"], 45);
    assert_eq!(quic["disablePathMTUDiscovery"], true);

    for absent in [
        "maxStreamReceiveWindow",
        "initConnectionReceiveWindow",
        "maxConnectionReceiveWindow",
        "keepAlivePeriod",
        "maxIncomingStreams",
    ] {
        assert!(quic.get(absent).is_none(), "{absent} 不该出现：{quic:#?}");
    }
}

/// The default profile is the one key that stays unwritten even when it is set, because xray reads
/// an absent `bbrProfile` as `standard`. Writing it would add a line to every artifact that says
/// nothing.
#[test]
fn the_standard_bbr_profile_writes_no_key() {
    use brocade_core::model::{HysteriaBbrProfile, HysteriaQuic};

    let mut hk = node("hk", [10, 66, 0, 1], true, Dns::System);
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);
    let mut face = ingress("i", "c", "hk");
    face.wires = IngressWires::Hysteria2(Hysteria2 {
        bbr_profile: HysteriaBbrProfile::Standard,
        quic: HysteriaQuic::default(),
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth::default(),
        congestion: HysteriaCongestion::Bbr,
        obfs: None,
        masquerade: HysteriaMasquerade::NotFound,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![face],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let quic = &inbound(&value, "in:app/i:hy2")["streamSettings"]["finalmask"]["quicParams"];
    assert!(quic.get("bbrProfile").is_none(), "{quic:#?}");
}

#[test]
fn fallback_limit_presets_are_stable_but_not_fleet_wide_constants() {
    let first = reality_fallback_limits(&RealityFallbackLimits::Balanced, "in:app/a").unwrap();
    let repeated = reality_fallback_limits(&RealityFallbackLimits::Balanced, "in:app/a").unwrap();
    let other = reality_fallback_limits(&RealityFallbackLimits::Balanced, "in:app/b").unwrap();

    assert_eq!(first, repeated);
    assert_ne!(first, other);
    assert!(first.upload.burst_bytes_per_sec > first.upload.bytes_per_sec);
    assert!(first.download.burst_bytes_per_sec > first.download.bytes_per_sec);
}

#[test]
fn xray_reality_settings_include_global_client_policy() {
    let mut doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    doc.settings = ModelSettings {
        connection: Default::default(),
        stats_user_online: false,
        reality_client: RealityClientPolicy {
            min_client_ver: Some("1.8.0".to_owned()),
            max_client_ver: Some("1.9.9".to_owned()),
            max_time_diff_ms: Some(30_000),
        },
        reality_site: RealitySite::default(),
        anytls_padding_scheme: brocade_core::model::default_anytls_padding_scheme(),
        overlay: OverlaySettings::default(),
        ports: Default::default(),
        probe: Default::default(),
        geodata: Default::default(),
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let plan = project_node(&sys, &[app_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];

    assert_eq!(reality["minClientVer"], "1.8.0");
    assert_eq!(reality["maxClientVer"], "1.9.9");
    assert_eq!(reality["maxTimeDiff"], 30_000);
}

#[test]
fn xray_reality_dest_removes_spaces_around_the_port_separator() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let mut ingress = ingress("i", "c", "hk");
    let reality = ingress.wires.reality_mut().unwrap();
    reality.dest = "apps.apple.com: 443".to_owned();
    // Unguarded, so that `dest` still reaches the artifact and the normalization is visible there.
    // The guarded spelling of this same case is
    // `xray_reality_guard_splits_the_borrowed_site_into_address_and_port`.
    reality.fallback_guard = false;
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress],
        fronts: Vec::new(),
        steps: vec![step("c", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let artifact = xray::build(&project_node(&sys, &[app_ir], "hk"));
    let text = json::xray(&artifact);
    assert!(!text.contains("apps.apple.com: 443"), "{text}");
    let value = parse_xray(&artifact);
    let reality = &inbound(&value, "in:app/i")["streamSettings"]["realitySettings"];
    assert_eq!(reality["dest"], "apps.apple.com:443");
}

/// The three connection settings each reach the artifact as their own shape.
///
/// Asserted together in one table rather than as three tests, because what matters is that they
/// differ: `None` writing `"enabled": false` instead of nothing, or `Pool` and `Merge(8)` both
/// arriving as 8, are the mistakes worth catching, and each is invisible when a variant is
/// checked on its own.
///
/// The absent case is asserted as an absent key, not a falsy one. A machine that was never asked
/// to pool has to read exactly as it did before this feature existed, or every golden artifact
/// gains a line that means nothing.
#[test]
fn the_connection_setting_reaches_the_hop_outbound() {
    for (pool, expected) in [
        (HopPool::None, None),
        (HopPool::Pool, Some(1)),
        (HopPool::Merge(8), Some(8)),
        (HopPool::Merge(128), Some(128)),
    ] {
        let doc = doc(vec![
            node("hk", [10, 66, 0, 1], true, Dns::System),
            node("sg", [10, 66, 0, 2], true, Dns::System),
        ]);
        let app = AppView {
            id: "relay".to_owned(),
            label: "中转".to_owned(),
            chains: vec![chain("c-relay")],
            ingresses: vec![ingress("i-relay", "c-relay", "hk")],
            fronts: Vec::new(),
            steps: vec![
                step("c-relay", "hk", vec![forward_pool("sg", pool)], None),
                step(
                    "c-relay",
                    "sg",
                    vec![any_egress()],
                    Some(accept("uuid-sg", "c-relay@sg")),
                ),
            ],
            grants: Vec::new(),
        };
        let mut diagnostics = Vec::new();
        let sys = compile_system(&doc, &mut diagnostics);
        let ir = compile_hops(
            compile_app(&doc, &app, &mut diagnostics),
            &sys,
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty(), "{pool:?}: {diagnostics:#?}");

        let hk = parse_xray(&xray::build(&project_node(
            &sys,
            std::slice::from_ref(&ir),
            "hk",
        )));
        let out = outbound(&hk, "out:relay/c-relay>sg");
        assert_eq!(
            out["streamSettings"]["sockopt"]["tcpFastOpen"], true,
            "{pool:?}: relay dialer must enable client-side TFO"
        );
        match expected {
            None => assert!(out["mux"].is_null(), "{pool:?}: {out:#?}"),
            Some(concurrency) => {
                assert_eq!(out["mux"]["enabled"], true, "{pool:?}");
                assert_eq!(out["mux"]["concurrency"], concurrency, "{pool:?}");
            }
        }

        let sg = parse_xray(&xray::build(&project_node(
            &sys,
            std::slice::from_ref(&ir),
            "sg",
        )));
        let hop_in = inbound(&sg, "in:hop:relay/c-relay");
        assert_eq!(
            hop_in["streamSettings"]["sockopt"]["tcpFastOpen"], 256,
            "{pool:?}: relay listener must offer a bounded TFO backlog"
        );

        // The subscription enables client-side TFO, so its corresponding listener has to offer
        // the same bounded server backlog. Otherwise every client silently pays the ordinary
        // handshake even though its imported profile says TFO is on.
        let user_in = inbound(&hk, "in:relay/i-relay");
        assert_eq!(
            user_in["streamSettings"]["sockopt"]["tcpFastOpen"], 256,
            "{pool:?}: subscriber listener must offer a bounded TFO backlog"
        );
    }
}

/// A relay hop speaking Shadowsocks 2022 renders as shadowsocks on both ends.
///
/// Both halves matter and they are different shapes: the listener takes a bare method and key,
/// the dialer wraps the same key in a `servers` list with the address. Testing only one would
/// leave a chain where one end is configured and the other is not, which xray reports as
/// nothing at all — it simply never completes a handshake.
///
/// The key is symmetric, so the identical string has to appear on both. That is the property
/// worth asserting rather than merely that each side has some key: a generator wired up wrong
/// gives each end its own, and the chain fails exactly as if the key were right.
#[test]
fn a_shadowsocks_hop_renders_as_shadowsocks_on_both_ends() {
    const SERVER_PSK: &str = "SGVsbG9Ccm9jYWRlS2V5MDE=";
    const USER_PSK: &str = "QnJvY2FkZVVzZXJLZXkwMDA9";
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    let app = AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay")],
        ingresses: vec![ingress("i-relay", "c-relay", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step("c-relay", "hk", vec![forward("sg")], None),
            Step {
                chain: "c-relay".to_owned(),
                node: "sg".to_owned(),
                accept: Some(accept("uuid-sg", "c-relay@sg")),
                hop_in: Some(HopIn {
                    port: 20000,
                    security: HopWire::Shadowsocks2022 {
                        server_psk: SERVER_PSK.to_owned(),
                        user_psk: USER_PSK.to_owned(),
                    },
                }),
                rules: vec![any_egress()],
            },
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    // The listener.
    let sg: serde_json::Value = serde_json::from_str(&json::xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&ir),
        "sg",
    ))))
    .unwrap();
    let hop = sg["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|inbound| inbound["tag"] == "in:hop:relay/c-relay")
        .expect("relay inbound");
    assert_eq!(hop["protocol"], "shadowsocks");
    assert_eq!(hop["settings"]["method"], "2022-blake3-aes-128-gcm");
    assert_eq!(hop["settings"]["password"], SERVER_PSK);
    // The account is what gives arriving traffic a name — the routing rules select on it and
    // the usage counters are titled after it. Its absence is invisible in the artifact and
    // costs both, so it is asserted rather than assumed.
    assert_eq!(hop["settings"]["clients"][0]["email"], "c-relay@sg");
    assert_eq!(hop["settings"]["clients"][0]["password"], USER_PSK);
    // xray refuses a per-account method, and the port already declared one.
    assert!(
        hop["settings"]["clients"][0]["method"].is_null(),
        "{hop:#?}"
    );
    assert!(hop["settings"]["decryption"].is_null(), "{hop:#?}");
    assert_eq!(hop["streamSettings"]["security"], "none");
    assert_eq!(
        hop["streamSettings"]["sockopt"]["tcpFastOpen"], 256,
        "the relay listener must enable server-side TFO"
    );
    // Both networks, explicitly. Left out, xray hears TCP alone and the chain's UDP — DNS and
    // QUIC — disappears while TCP goes on working, which is the shape of failure nobody reports
    // as "the relay is down".
    assert_eq!(hop["settings"]["network"], "tcp,udp");

    // The dialer.
    let hk: serde_json::Value =
        serde_json::from_str(&json::xray(&xray::build(&project_node(&sys, &[ir], "hk")))).unwrap();
    let out = hk["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outbound| outbound["tag"] == "out:relay/c-relay>sg")
        .expect("forward outbound");
    assert_eq!(out["protocol"], "shadowsocks");
    assert_eq!(
        out["streamSettings"]["sockopt"]["tcpFastOpen"], true,
        "the relay dialer must enable client-side TFO"
    );
    let server = &out["settings"]["servers"][0];
    assert_eq!(server["address"], "10.66.0.2");
    assert_eq!(server["port"], 20000);
    assert_eq!(server["method"], "2022-blake3-aes-128-gcm");
    // Colon-joined: the account under the port. Asserted as one string rather than by parts
    // because the separator is the whole of what tells the two keys apart on the wire.
    assert_eq!(
        server["password"],
        format!("{SERVER_PSK}:{USER_PSK}"),
        "the dialer presents the account under the port"
    );
    // A uuid here would mean the VLESS shape leaked through.
    assert!(out["settings"]["vnext"].is_null(), "{out:#?}");

    // The account's name must be the same string three places want: xray titles its traffic
    // statistics after it (which `usage.rs` matches relay volume against by `accept_label`),
    // and the routing rules select on it. All three read the one credential label, so this
    // asserts they still agree — a divergence would not fail anywhere, it would route the
    // chain somewhere else and count its bytes as nobody's.
    let selects_account = sg["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule["user"][0] == "c-relay@sg" && rule["outboundTag"] == "out:egress");
    assert!(
        selects_account,
        "the routing rules must select the account the inbound declares: {:#?}",
        sg["routing"]["rules"]
    );
}

/// Every freedom outbound on the machine takes the node's strategy, spelled the way xray spells
/// it — `out:internal` included. That outbound carries no user traffic, but resolution is a
/// property of the machine's egress and it is egress; leaving it pinned would have one machine
/// resolve two ways.
///
/// The casing is not cosmetic here even though xray lowercases before matching: the value is what
/// somebody reads out of xray.json while comparing it against the console, and the two have to be
/// the same token.
#[test]
fn every_freedom_outbound_takes_the_nodes_domain_strategy() {
    let mut doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    doc.nodes[0].domain_strategy = DomainStrategy::UseIpv6v4;
    let app = AppView {
        id: "direct".to_owned(),
        label: "直出".to_owned(),
        chains: vec![chain("c-direct")],
        ingresses: vec![ingress("i-direct", "c-direct", "hk")],
        fronts: Vec::new(),
        steps: vec![step("c-direct", "hk", vec![any_egress()], None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    let plan = project_node(&sys, &[ir], "hk");
    let value: serde_json::Value = serde_json::from_str(&json::xray(&xray::build(&plan))).unwrap();

    let freedom: Vec<_> = value["outbounds"]
        .as_array()
        .expect("outbounds")
        .iter()
        .filter(|outbound| outbound["protocol"] == "freedom")
        .collect();
    assert!(
        freedom.len() >= 2,
        "expected egress and internal: {value:#?}"
    );
    for outbound in &freedom {
        assert_eq!(
            outbound["settings"]["domainStrategy"], "UseIPv6v4",
            "outbound {} kept the old value",
            outbound["tag"]
        );
    }
    assert!(freedom
        .iter()
        .any(|outbound| outbound["tag"] == "out:internal"));

    // The routing-level field is a different knob and stays where it was: it governs whether the
    // router resolves a domain in order to match IP rules, which the rules here never need.
    assert_eq!(value["routing"]["domainStrategy"], "AsIs");
}

#[test]
fn project_node_merges_xray_structure_from_all_app_views() {
    let mut doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    doc.users.push(User {
        tenant: "platform.acme".to_owned(),
        id: "bob".to_owned(),
        uuid: "uuid-bob".to_owned(),
    });
    let relay = AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay")],
        ingresses: vec![Ingress {
            port: 443,
            ..ingress_with_flow("i-relay", "c-relay", "hk", Some("xtls-rprx-vision"))
        }],
        fronts: Vec::new(),
        steps: vec![
            step("c-relay", "hk", vec![forward("sg")], None),
            step(
                "c-relay",
                "sg",
                vec![any_egress()],
                Some(accept("uuid-sg", "c-relay@sg")),
            ),
        ],
        grants: vec![Grant {
            tenant: "platform.acme".to_owned(),
            user: "alice".to_owned(),
            ingress: "i-relay".to_owned(),
        }],
    };
    let direct = AppView {
        id: "direct".to_owned(),
        label: "直出".to_owned(),
        chains: vec![chain("c-direct")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i-direct", "c-direct", "hk")
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
        grants: vec![Grant {
            tenant: "platform.acme".to_owned(),
            user: "bob".to_owned(),
            ingress: "i-direct".to_owned(),
        }],
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let relay_ir = compile_hops(
        compile_app(&doc, &relay, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    let direct_ir = compile_hops(
        compile_app(&doc, &direct, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let plan = project_node(&sys, &[relay_ir, direct_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);
    let grant_batch = grants::build(&plan);
    let grant_value = parse_grants(&grant_batch);

    assert_eq!(
        tags(value["inbounds"].as_array().unwrap()),
        [
            "api",
            "in:direct/i-direct",
            "in:direct/i-direct:guard",
            "in:relay/i-relay",
            "in:relay/i-relay:guard"
        ]
    );
    assert_eq!(
        value["inbounds"][1]["settings"]["clients"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Index 3, not 2: each ingress is followed by its own fallback guard.
    assert_eq!(
        value["inbounds"][3]["settings"]["clients"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(value["inbounds"][1]["sniffing"]["enabled"], true);

    assert_eq!(
        tags(value["outbounds"].as_array().unwrap()),
        [
            "out:relay/c-relay>sg",
            "out:egress",
            "out:block",
            "out:reality-guard",
            "out:internal"
        ]
    );
    let forward = outbound(&value, "out:relay/c-relay>sg");
    assert_eq!(forward["settings"]["vnext"][0]["address"], "10.66.0.2");
    assert_eq!(forward["settings"]["vnext"][0]["port"], 20000);
    assert_eq!(forward["settings"]["vnext"][0]["users"][0]["id"], "uuid-sg");

    assert_eq!(value["dns"]["servers"], serde_json::json!(["localhost"]));
    assert_eq!(value["routing"]["domainStrategy"], "AsIs");
    assert_eq!(
        value["routing"]["rules"][0]["inboundTag"],
        serde_json::json!(["api"])
    );
    let block_rule = rule_to(&value, "out:block");
    assert_eq!(
        block_rule["domain"],
        serde_json::json!(["domain:ads.example"])
    );
    let relay_rule = rule_to(&value, "out:relay/c-relay>sg");
    assert_eq!(
        relay_rule["inboundTag"],
        serde_json::json!(["in:relay/i-relay"])
    );

    assert_eq!(
        tags(grant_value["inbounds"].as_array().unwrap()),
        ["in:direct/i-direct", "in:relay/i-relay"]
    );
    let direct_grants = grant_inbound(&grant_value, "in:direct/i-direct");
    assert_eq!(direct_grants["clients"][0]["id"], "uuid-bob");
    let relay_grants = grant_inbound(&grant_value, "in:relay/i-relay");
    assert_eq!(relay_grants["clients"][0]["id"], "uuid-alice");
    assert_eq!(relay_grants["clients"][0]["flow"], "xtls-rprx-vision");
    assert!(relay_grants.get("listen").is_none());
    assert!(relay_grants.get("port").is_none());
    assert!(relay_grants.get("protocol").is_none());
}

#[test]
fn xray_tags_are_namespaced_by_app() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    let make_app = |id: &str, ingress_port: u16, hop_port: u16| AppView {
        id: id.to_owned(),
        label: id.to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![Ingress {
            port: ingress_port,
            ..ingress("i", "c", "hk")
        }],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![forward("sg")], None),
            Step {
                chain: "c".to_owned(),
                node: "sg".to_owned(),
                accept: Some(accept(&format!("uuid-{id}-sg"), &format!("{id}-c@sg"))),
                hop_in: Some(HopIn {
                    port: hop_port,
                    security: HopWire::None,
                }),
                rules: vec![any_egress()],
            },
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_a = compile_hops(
        compile_app(&doc, &make_app("a", 443, 20000), &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    let app_b = compile_hops(
        compile_app(&doc, &make_app("b", 8443, 20001), &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let dialer = parse_xray(&xray::build(&project_node(
        &sys,
        &[app_a.clone(), app_b.clone()],
        "hk",
    )));
    assert_eq!(
        tags(dialer["inbounds"].as_array().unwrap()),
        ["api", "in:a/i", "in:a/i:guard", "in:b/i", "in:b/i:guard"]
    );
    assert_eq!(
        tags(dialer["outbounds"].as_array().unwrap()),
        [
            "out:a/c>sg",
            "out:b/c>sg",
            "out:reality-guard",
            "out:internal"
        ]
    );
    assert_eq!(
        outbound(&dialer, "out:a/c>sg")["settings"]["vnext"][0]["port"],
        20000
    );
    assert_eq!(
        outbound(&dialer, "out:b/c>sg")["settings"]["vnext"][0]["port"],
        20001
    );
    assert_eq!(
        rule_to(&dialer, "out:a/c>sg")["inboundTag"],
        serde_json::json!(["in:a/i"])
    );
    assert_eq!(
        rule_to(&dialer, "out:b/c>sg")["inboundTag"],
        serde_json::json!(["in:b/i"])
    );

    let receiver = parse_xray(&xray::build(&project_node(&sys, &[app_a, app_b], "sg")));
    assert_eq!(
        tags(receiver["inbounds"].as_array().unwrap()),
        ["api", "in:hop:a/c", "in:hop:b/c"]
    );
}

#[test]
fn project_node_adds_hop_inbound_and_dns_route() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node(
            "sg",
            [10, 66, 0, 2],
            true,
            Dns::Servers(vec!["8.8.8.8".to_owned()]),
        ),
    ]);
    let relay = AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay")],
        ingresses: vec![ingress("i-relay", "c-relay", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step("c-relay", "hk", vec![forward("sg")], None),
            step(
                "c-relay",
                "sg",
                vec![any_egress()],
                Some(accept("uuid-sg", "c-relay@sg")),
            ),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let relay_ir = compile_hops(
        compile_app(&doc, &relay, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let plan = project_node(&sys, &[relay_ir], "sg");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);

    assert_eq!(
        tags(value["inbounds"].as_array().unwrap()),
        ["api", "in:hop:relay/c-relay"]
    );
    let overlay = inbound(&value, "in:hop:relay/c-relay");
    assert_eq!(overlay["listen"], "10.66.0.2");
    assert_eq!(overlay["settings"]["clients"][0]["id"], "uuid-sg");
    assert_eq!(overlay["settings"]["clients"][0]["email"], "c-relay@sg");

    assert_eq!(value["dns"]["tag"], "dns-out");
    assert_eq!(value["dns"]["servers"], serde_json::json!(["8.8.8.8"]));
    assert_eq!(
        value["routing"]["rules"][1]["inboundTag"],
        serde_json::json!(["dns-out"])
    );
    assert_eq!(value["routing"]["rules"][1]["outboundTag"], "out:egress");
}

#[test]
fn machine_egress_dns_is_global_without_replacing_default_dns() {
    let mut doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    doc.node_egress_dns = vec![
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 0,
            selector: DestMatch::Geosite(vec!["netflix".to_owned()]),
            resolution: EgressDnsResolution {
                address: "192.0.2.53".to_owned(),
                port: 53,
                transport: EgressDnsTransport::Tcp,
                address_strategy: EgressDnsAddressStrategy::UseIpv4,
                fallback: EgressDnsFallback::Stop,
            },
        },
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 1,
            selector: DestMatch::DomainKeyword(vec!["disney".to_owned()]),
            resolution: EgressDnsResolution {
                address: "2001:db8::53".to_owned(),
                port: 5353,
                transport: EgressDnsTransport::Udp,
                address_strategy: EgressDnsAddressStrategy::UseIpv6,
                fallback: EgressDnsFallback::Machine,
            },
        },
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 2,
            selector: DestMatch::DomainKeyword(vec!["v4-first".to_owned()]),
            resolution: EgressDnsResolution {
                address: "198.51.100.53".to_owned(),
                port: 53,
                transport: EgressDnsTransport::Tcp,
                address_strategy: EgressDnsAddressStrategy::UseIpv4v6,
                fallback: EgressDnsFallback::Stop,
            },
        },
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 3,
            selector: DestMatch::DomainKeyword(vec!["v6-first".to_owned()]),
            resolution: EgressDnsResolution {
                address: "203.0.113.53".to_owned(),
                port: 53,
                transport: EgressDnsTransport::Tcp,
                address_strategy: EgressDnsAddressStrategy::UseIpv6v4,
                fallback: EgressDnsFallback::Stop,
            },
        },
    ];
    let app = AppView {
        id: "video".to_owned(),
        label: "视频".to_owned(),
        chains: vec![chain("stream")],
        ingresses: vec![ingress("stream-in", "stream", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "stream",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                    action: Action::Egress {
                        send_through: Some("198.51.100.9".parse().unwrap()),
                    },
                },
                Rule {
                    dest_match: DestMatch::DomainKeyword(vec!["disney".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
                Rule {
                    dest_match: DestMatch::DomainKeyword(vec!["v4-first".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
                Rule {
                    dest_match: DestMatch::DomainKeyword(vec!["v6-first".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
                any_egress(),
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let servers = value["dns"]["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 5);
    assert_eq!(
        servers[0],
        serde_json::json!({
            "address": "tcp://192.0.2.53",
            "port": 53,
            "domains": ["geosite:netflix"],
            "queryStrategy": "UseIPv4",
            "skipFallback": true,
            "finalQuery": true,
            "tag": servers[0]["tag"],
        })
    );
    assert_eq!(
        servers[1],
        serde_json::json!({
            "address": "2001:db8::53",
            "port": 5353,
            "domains": ["disney"],
            "queryStrategy": "UseIPv6",
            "skipFallback": true,
            "finalQuery": false,
            "tag": servers[1]["tag"],
        })
    );
    assert_eq!(servers[2]["queryStrategy"], "UseIP");
    assert_eq!(servers[2]["domains"], serde_json::json!(["v4-first"]));
    assert_eq!(servers[3]["queryStrategy"], "UseIP");
    assert_eq!(servers[3]["domains"], serde_json::json!(["v6-first"]));
    assert_eq!(servers[4], "localhost", "机器默认 DNS 必须保留");

    let outbounds = value["outbounds"].as_array().unwrap();
    assert!(outbounds
        .iter()
        .any(|outbound| outbound["settings"]["domainStrategy"] == "UseIPv4v6"));
    assert!(outbounds
        .iter()
        .any(|outbound| outbound["settings"]["domainStrategy"] == "UseIPv6v4"));

    let dns_tag = servers[0]["tag"].as_str().unwrap();
    assert!(dns_tag.starts_with("dns:egress:"));
    let routes = value["routing"]["rules"].as_array().unwrap();
    let dns_route = routes
        .iter()
        .find(|rule| rule["inboundTag"] == serde_json::json!([dns_tag]))
        .unwrap();
    let dns_outbound_tag = dns_route["outboundTag"].as_str().unwrap();
    let dns_outbound = outbounds
        .iter()
        .find(|outbound| outbound["tag"] == dns_outbound_tag)
        .unwrap();
    assert_eq!(dns_outbound["protocol"], "freedom");
    assert_eq!(dns_outbound["settings"]["domainStrategy"], "UseIPv4");
    assert_eq!(
        dns_outbound["sendThrough"],
        serde_json::Value::Null,
        "DNS 查询不能继承线路的源地址绑定"
    );
    let traffic_route = routes
        .iter()
        .find(|rule| rule["domain"] == serde_json::json!(["geosite:netflix"]))
        .unwrap();
    let traffic_outbound_tag = traffic_route["outboundTag"].as_str().unwrap();
    assert_ne!(traffic_outbound_tag, dns_outbound_tag);
    let traffic_outbound = outbounds
        .iter()
        .find(|outbound| outbound["tag"] == traffic_outbound_tag)
        .unwrap();
    assert_eq!(traffic_outbound["sendThrough"], "198.51.100.9");
    assert_eq!(traffic_outbound["settings"]["domainStrategy"], "UseIPv4");

    let v6_dns_tag = servers[1]["tag"].as_str().unwrap();
    let v6_outbound_tag = value["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outbound| outbound["settings"]["domainStrategy"] == "UseIPv6")
        .unwrap()["tag"]
        .as_str()
        .unwrap();
    assert_eq!(
        routes
            .iter()
            .find(|rule| rule["inboundTag"] == serde_json::json!([v6_dns_tag]))
            .unwrap()["outboundTag"],
        v6_outbound_tag
    );
    assert_eq!(
        routes
            .iter()
            .find(|rule| rule["domain"] == serde_json::json!(["disney"]))
            .unwrap()["outboundTag"],
        v6_outbound_tag
    );
    assert!(routes
        .iter()
        .any(|rule| rule["outboundTag"] == "out:egress"));
}

#[test]
fn machine_egress_dns_is_emitted_without_a_route_reference() {
    let selector = DestMatch::DomainSuffix(vec!["stream.example".to_owned()]);
    let mut doc = doc(vec![node("exit", [10, 66, 0, 1], true, Dns::System)]);
    doc.node_egress_dns = vec![NodeEgressDnsPolicy {
        node: "exit".to_owned(),
        position: 0,
        selector: selector.clone(),
        resolution: EgressDnsResolution {
            address: "192.0.2.53".to_owned(),
            port: 53,
            transport: EgressDnsTransport::Tcp,
            address_strategy: EgressDnsAddressStrategy::UseIpv4,
            fallback: EgressDnsFallback::Stop,
        },
    }];
    let app = AppView {
        id: "shared-dns".to_owned(),
        label: "Shared DNS".to_owned(),
        chains: vec![chain("stream")],
        ingresses: vec![ingress("stream-in", "stream", "exit")],
        fronts: Vec::new(),
        // No authored route mentions the selector. The machine policy must still enter Xray's
        // global DNS list; the compiler-added Any route is unrelated to that activation.
        steps: vec![step("stream", "exit", Vec::new(), None)],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    assert_eq!(app_ir.steps[0].rules.len(), 1);
    assert!(matches!(
        app_ir.steps[0].rules[0].dest_match,
        DestMatch::Any
    ));

    let value = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "exit")));
    let servers = value["dns"]["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0]["address"], "tcp://192.0.2.53");
    assert_eq!(
        servers[0]["domains"],
        serde_json::json!(["domain:stream.example"])
    );
    assert_eq!(servers[1], "localhost");

    let dns_tag = servers[0]["tag"].as_str().unwrap();
    let dns_route = value["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["inboundTag"] == serde_json::json!([dns_tag]))
        .expect("机器 DNS server 必须有独立查询路由");
    let outbound_tag = dns_route["outboundTag"].as_str().unwrap();
    let outbound = value["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outbound| outbound["tag"] == outbound_tag)
        .expect("机器 DNS 查询路由必须指向真实 Freedom outbound");
    assert_eq!(outbound["protocol"], "freedom");
    assert_eq!(outbound["sendThrough"], serde_json::Value::Null);
}

#[test]
fn machine_dns_priority_overrides_chain_rule_order() {
    let mut doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let alpha_resolution = EgressDnsResolution {
        address: "192.0.2.53".to_owned(),
        port: 53,
        transport: EgressDnsTransport::Tcp,
        address_strategy: EgressDnsAddressStrategy::UseIp,
        fallback: EgressDnsFallback::Stop,
    };
    let beta_resolution = EgressDnsResolution {
        address: "198.51.100.53".to_owned(),
        ..alpha_resolution.clone()
    };
    let alpha = DestMatch::DomainSuffix(vec!["alpha.example".to_owned()]);
    let beta = DestMatch::DomainSuffix(vec!["beta.example".to_owned()]);
    doc.node_egress_dns = vec![
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 1,
            selector: alpha.clone(),
            resolution: alpha_resolution.clone(),
        },
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 0,
            selector: beta.clone(),
            resolution: beta_resolution.clone(),
        },
    ];
    let app = AppView {
        id: "dns-priority".to_owned(),
        label: "DNS priority".to_owned(),
        chains: vec![chain("stream")],
        ingresses: vec![ingress("stream-in", "stream", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "stream",
            "hk",
            vec![
                Rule {
                    dest_match: alpha,
                    action: Action::Egress { send_through: None },
                },
                Rule {
                    dest_match: beta,
                    action: Action::Egress { send_through: None },
                },
                any_egress(),
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let value = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "hk",
    )));
    let servers = value["dns"]["servers"].as_array().unwrap();
    assert_eq!(
        servers[0]["domains"],
        serde_json::json!(["domain:beta.example"])
    );
    assert_eq!(
        servers[1]["domains"],
        serde_json::json!(["domain:alpha.example"])
    );
    assert_eq!(servers[2], "localhost");

    let mut without_machine_order = app_ir;
    without_machine_order.nodes[0].egress_dns.clear();
    let without_machine_order = parse_xray(&xray::build(&project_node(
        &sys,
        &[without_machine_order],
        "hk",
    )));
    assert_eq!(
        without_machine_order["dns"]["servers"],
        serde_json::json!(["localhost"]),
        "没有机器 DNS 表时，链路规则不能自行产生解析配置"
    );
}

/// A public relay port's two sides must agree: the receiving machine binds 0.0.0.0 and
/// carries the private key on `decryption`, the dialing machine carries the public key on
/// `encryption`, and `streamSettings.security` stays none on both — VLESS Encryption is
/// applied at the protocol layer.
#[test]
fn public_hop_renders_vless_encryption_on_both_sides() {
    let (sys, app_ir) = public_hop_fixture(HopWire::Encryption(HopEncryption {
        private_key: "PRIV-SG".to_owned(),
        public_key: "PUB-SG".to_owned(),
    }));

    let receiver = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "sg",
    )));
    let overlay = inbound(&receiver, "in:hop:relay/c-relay");
    assert_eq!(
        overlay["listen"], "0.0.0.0",
        "别人拨的是公网地址，只绑 overlay 就是 connection refused"
    );
    assert_eq!(
        overlay["port"], 20000,
        "监听口仍是本机 hop_port，对外那个口是 hop_endpoint 的事"
    );
    assert_eq!(
        overlay["settings"]["decryption"],
        "mlkem768x25519plus.native.600s.PRIV-SG"
    );
    assert_eq!(overlay["streamSettings"]["security"], "none");

    let dialer = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let forward = outbound(&dialer, "out:relay/c-relay>sg");
    assert_eq!(
        forward["settings"]["vnext"][0]["address"],
        "relay.sg.example"
    );
    assert_eq!(forward["settings"]["vnext"][0]["port"], 8443);
    assert_eq!(
        forward["settings"]["vnext"][0]["users"][0]["encryption"],
        "mlkem768x25519plus.native.0rtt.PUB-SG"
    );
    assert_eq!(forward["streamSettings"]["security"], "none");
    assert!(
        !serde_json::to_string(&dialer).unwrap().contains("PRIV-SG"),
        "拨号方的产物里不该有对端私钥"
    );
}

/// The REALITY variant lands at the transport layer, leaving `decryption` / `encryption`
/// as none.
#[test]
fn public_hop_renders_reality_on_the_stream_layer() {
    let (sys, app_ir) = public_hop_fixture(HopWire::Reality(Reality {
        private_key: "PRIV-SG".to_owned(),
        public_key: "PUB-SG".to_owned(),
        short_ids: vec!["0123456789abcdef".to_owned()],
        dest: "apps.apple.com: 443".to_owned(),
        server_names: vec!["apps.apple.com".to_owned()],
        fingerprint: "chrome".to_owned(),
        flow: None,
    }));

    let receiver = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "sg",
    )));
    let overlay = inbound(&receiver, "in:hop:relay/c-relay");
    assert_eq!(overlay["settings"]["decryption"], "none");
    assert_eq!(overlay["streamSettings"]["security"], "reality");
    assert_eq!(
        overlay["streamSettings"]["realitySettings"]["dest"],
        "apps.apple.com:443"
    );
    assert_eq!(
        overlay["streamSettings"]["realitySettings"]["privateKey"],
        "PRIV-SG"
    );

    let dialer = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
    let forward = outbound(&dialer, "out:relay/c-relay>sg");
    assert_eq!(
        forward["settings"]["vnext"][0]["users"][0]["encryption"],
        "none"
    );
    assert_eq!(forward["streamSettings"]["security"], "reality");
    let reality = &forward["streamSettings"]["realitySettings"];
    assert_eq!(reality["publicKey"], "PUB-SG");
    assert_eq!(reality["serverName"], "apps.apple.com");
    assert_eq!(reality["shortId"], "0123456789abcdef");
    assert_eq!(reality["fingerprint"], "chrome");
}

/// A relay port someone dials by address must bind 0.0.0.0, even where the machine is also
/// on the backbone.
///
/// The test is how others dial this chain, not whether the machine is on the backbone. By
/// the latter, a machine on the backbone would bind its overlay address while the dialer
/// still generated an outbound dialing 10.0.0.9 — compile all green, artifacts normal,
/// traffic dead.
#[test]
fn a_hop_dialed_by_address_binds_the_wildcard() {
    for on_overlay in [true, false] {
        let sg = node("sg", [10, 66, 0, 2], on_overlay, Dns::System);
        let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System), sg]);
        let relay = AppView {
            id: "relay".to_owned(),
            label: "中转".to_owned(),
            chains: vec![chain("c-relay")],
            ingresses: vec![ingress("i-relay", "c-relay", "hk")],
            fronts: Vec::new(),
            steps: vec![
                step(
                    "c-relay",
                    "hk",
                    vec![forward_addr("sg", "10.0.0.9:8443")],
                    None,
                ),
                step(
                    "c-relay",
                    "sg",
                    vec![any_egress()],
                    Some(accept("uuid-sg", "c-relay@sg")),
                ),
            ],
            grants: Vec::new(),
        };
        let mut diagnostics = Vec::new();
        let sys = compile_system(&doc, &mut diagnostics);
        let app_ir = compile_hops(
            compile_app(&doc, &relay, &mut diagnostics),
            &sys,
            &mut diagnostics,
        );
        assert!(
            diagnostics.iter().all(|d| d.level != Level::Error),
            "{diagnostics:#?}"
        );

        let receiver = parse_xray(&xray::build(&project_node(
            &sys,
            std::slice::from_ref(&app_ir),
            "sg",
        )));
        let hop = inbound(&receiver, "in:hop:relay/c-relay");
        assert_eq!(
            hop["listen"], "0.0.0.0",
            "在 overlay 里={on_overlay}：别人拨的是 10.0.0.9，绑 overlay 地址收不到"
        );

        let dialer = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "hk")));
        let forward = outbound(&dialer, "out:relay/c-relay>sg");
        assert_eq!(forward["settings"]["vnext"][0]["address"], "10.0.0.9");
    }
}

// Reverse access's artifacts on both sides. sg is behind NAT and off the backbone, so it
// dials hk; traffic still flows hk → sg. What this pins down is that everything is in its
// place — the upstream has a portal and no outbound dialing the downstream, the downstream
// has a bridge and one outbound dialing the upstream, and the domain agrees across the two.
// Any one of them missing and the tunnel never comes up, while each side's artifacts look
// right on their own.
#[test]
fn reverse_hop_renders_portal_upstream_and_bridge_downstream() {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], false, Dns::System),
    ]);
    let relay = AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay")],
        ingresses: vec![ingress("i-relay", "c-relay", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c-relay",
                "hk",
                vec![forward_dial("sg", HopDial::Reverse(IpFamily::V4))],
                Some(accept("uuid-hk", "c-relay@hk")),
            ),
            Step {
                hop_in: None,
                ..step(
                    "c-relay",
                    "sg",
                    vec![any_egress()],
                    Some(accept("uuid-sg", "c-relay@sg")),
                )
            },
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &relay, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );

    let portal_tag = "rev:portal:relay/c-relay>sg";

    // ── Upstream: the portal tag rides on the downstream's credential ──
    let upstream = parse_xray(&xray::build(&project_node(
        &sys,
        std::slice::from_ref(&app_ir),
        "hk",
    )));
    // No top-level block any more. Emitting one is not a cosmetic regression: xray
    // refuses to start at all on seeing the key ("legacy reverse has been removed"),
    // so this assertion is what stands between a compiler change and a dead fleet.
    assert!(
        upstream.get("reverse").is_none(),
        "顶层 reverse 块必须消失：{upstream:#?}"
    );

    let hop_in = inbound(&upstream, "in:hop:relay/c-relay");
    let clients = hop_in["settings"]["clients"].as_array().unwrap();
    let peer = clients
        .iter()
        .find(|c| c["id"] == "uuid-sg")
        .unwrap_or_else(|| panic!("下游要能连进来：{clients:#?}"));
    assert_eq!(
        peer["reverse"]["tag"], portal_tag,
        "反代身份挂在下游的凭据上：{peer:#?}"
    );
    // Everyone else on this port stays an ordinary client. The tag is the authorization
    // now, so handing it to the wrong credential would let that one take the tunnel.
    assert!(
        clients
            .iter()
            .filter(|c| c["id"] != "uuid-sg")
            .all(|c| c.get("reverse").is_none()),
        "只有那个下游带 reverse：{clients:#?}"
    );
    // Bind the wildcard, not the overlay address. The downstream takes this variant
    // precisely because it is off the backbone and connects in from the public internet;
    // bound to the overlay this port is out of its reach, while both sides' configs look
    // right.
    assert_eq!(hop_in["listen"], "0.0.0.0", "{hop_in:#?}");

    // The upstream should have no outbound dialing the downstream — it cannot reach it,
    // that machine is behind NAT
    let out_tags = tags(upstream["outbounds"].as_array().unwrap());
    assert!(
        !out_tags.contains(&"out:relay/c-relay>sg"),
        "上游不拨下游：{out_tags:#?}"
    );
    // Traffic bound for the downstream goes through the portal. The tag keeps the
    // meaning it had under the old format — it names a virtual outbound — which is why
    // the business rules needed no rewrite.
    let rules = upstream["routing"]["rules"].as_array().unwrap();
    assert!(
        rules.iter().any(|r| r["outboundTag"] == portal_tag),
        "{upstream:#?}"
    );
    // The two setup rules are gone. They used to have to sit at the top of the table to
    // catch the control connection by its agreed domain; there is no such connection
    // now, so a rule matching on that token would only ever match real user traffic.
    assert!(
        rules.iter().all(|r| r["domain"].as_array().is_none_or(|d| d
            .iter()
            .all(|v| v.as_str().is_none_or(|s| !s.contains(".internal"))))),
        "不该再有按约定域名匹配的 setup 规则：{rules:#?}"
    );

    // ── Downstream: the bridge tag rides on the dialling outbound ──
    let downstream = parse_xray(&xray::build(&project_node(&sys, &[app_ir], "sg")));
    assert!(
        downstream.get("reverse").is_none(),
        "顶层 reverse 块必须消失：{downstream:#?}"
    );

    let dial = outbound(&downstream, "out:rev:relay/c-relay<hk");
    // The flat form, and this is load-bearing: xray honours `reverse` only here and
    // rejects it under `vnext` ("please use simplified outbound's config style"). A
    // well-meaning tidy-up that reunified the two shapes would break the tunnel while
    // the artifact still read plausibly.
    assert!(
        dial["settings"].get("vnext").is_none(),
        "隧道那条 outbound 必须是极简写法：{dial:#?}"
    );
    assert_eq!(
        dial["settings"]["address"], "hk.example.net",
        "下游拨的是上游"
    );
    assert_eq!(dial["settings"]["port"], 20000);
    assert_eq!(dial["settings"]["id"], "uuid-sg", "用自己的身份连上去");
    assert_eq!(
        dial["settings"]["reverse"]["tag"], "rev:bridge:relay/c-relay<hk",
        "隧道来的流量以这个 tag 作为虚拟 inbound 落地：{dial:#?}"
    );

    // Every other VLESS outbound keeps `vnext`. One shape per purpose is what makes it
    // readable in the artifact which outbound is the tunnel.
    for other in downstream["outbounds"].as_array().unwrap() {
        if other["protocol"] == "vless" && other["tag"] != "out:rev:relay/c-relay<hk" {
            assert!(
                other["settings"].get("vnext").is_some(),
                "非隧道的 vless outbound 仍用 vnext：{other:#?}"
            );
        }
    }
}

fn public_hop_fixture(security: HopWire) -> (brocade_core::ir::system::SystemIr, AppIr) {
    let doc = doc(vec![
        node("hk", [10, 66, 0, 1], true, Dns::System),
        node("sg", [10, 66, 0, 2], true, Dns::System),
    ]);
    let relay = AppView {
        id: "relay".to_owned(),
        label: "中转".to_owned(),
        chains: vec![chain("c-relay")],
        ingresses: vec![ingress("i-relay", "c-relay", "hk")],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c-relay",
                "hk",
                vec![forward_addr("sg", "relay.sg.example:8443")],
                None,
            ),
            Step {
                hop_in: Some(HopIn {
                    port: 20000,
                    security,
                }),
                ..step(
                    "c-relay",
                    "sg",
                    vec![any_egress()],
                    Some(accept("uuid-sg", "c-relay@sg")),
                )
            },
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &relay, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "{diagnostics:#?}"
    );
    (sys, app_ir)
}

#[test]
fn unexpanded_front_downstream_renders_as_never_match() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let app = AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front")],
        ingresses: vec![ingress("i-front", "c-front", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "c-front",
            "hk",
            vec![Rule {
                dest_match: DestMatch::FrontDownstream,
                action: Action::Egress { send_through: None },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.level == Level::Error && diagnostic.code == "rule.front-scope"
    }));

    let plan = project_node(&sys, &[app_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);

    assert!(value["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| rule["domain"] == serde_json::json!(["full:invalid.brocade.never-match"])));
}

#[test]
fn unrepresentable_all_match_renders_as_never_match() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::All(vec![
                        DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                        DestMatch::Geosite(vec!["netflix".to_owned()]),
                    ]),
                    action: Action::Block,
                },
                any_egress(),
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    let plan = project_node(&sys, &[app_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);
    let block_rule = rule_to(&value, "out:block");

    assert_eq!(
        block_rule["domain"],
        serde_json::json!(["full:invalid.brocade.never-match"])
    );
}

#[test]
fn nested_empty_all_match_renders_as_never_match() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::All(vec![
                        DestMatch::All(Vec::new()),
                        DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                    ]),
                    action: Action::Block,
                },
                any_egress(),
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    let plan = project_node(&sys, &[app_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);
    let block_rule = rule_to(&value, "out:block");

    assert_eq!(
        block_rule["domain"],
        serde_json::json!(["full:invalid.brocade.never-match"])
    );
}

#[test]
fn all_match_with_distinct_xray_fields_renders_as_and() {
    let doc = doc(vec![node("hk", [10, 66, 0, 1], true, Dns::System)]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk")],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::All(vec![
                        DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                        DestMatch::Port(vec!["443".to_owned()]),
                        DestMatch::Network(Network::Tcp),
                    ]),
                    action: Action::Egress {
                        send_through: Some(IpAddr::from(Ipv4Addr::new(192, 0, 2, 10))),
                    },
                },
                any_egress(),
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    let plan = project_node(&sys, &[app_ir], "hk");
    let artifact = xray::build(&plan);
    let value = parse_xray(&artifact);
    let rule = rule_to(&value, "out:egress:192.0.2.10");

    assert_eq!(rule["domain"], serde_json::json!(["domain:example.com"]));
    assert_eq!(rule["port"], "443");
    assert_eq!(rule["network"], "tcp");
}

fn parse_xray(artifact: &xray::XrayArtifact) -> Value {
    serde_json::from_str(&json::xray(artifact)).unwrap()
}

fn parse_grants(batch: &grants::GrantSyncBatch) -> Value {
    serde_json::from_str(&json::grant_sync_batch(batch)).unwrap()
}

fn tags(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .map(|value| value["tag"].as_str().unwrap())
        .collect()
}

fn inbound<'a>(value: &'a Value, tag: &str) -> &'a Value {
    value["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|inbound| inbound["tag"] == tag)
        .unwrap()
}

fn outbound<'a>(value: &'a Value, tag: &str) -> &'a Value {
    value["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outbound| outbound["tag"] == tag)
        .unwrap()
}

fn rule_to<'a>(value: &'a Value, tag: &str) -> &'a Value {
    value["routing"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["outboundTag"] == tag)
        .unwrap()
}

fn grant_inbound<'a>(value: &'a Value, tag: &str) -> &'a Value {
    value["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|inbound| inbound["tag"] == tag)
        .unwrap()
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 31,
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

fn node(id: &str, overlay: [u8; 4], egress_allowed: bool, dns: Dns) -> Node {
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
        wireguard: WireGuardKeys {
            private_key: format!("priv-{id}"),
            public_key: format!("pub-{id}"),
            listen_port: 51820,
            transport: Default::default(),
        },
        api_port: Some(10085),
        overlay: true,
        egress_allowed,
        dns,
        domain_strategy: DomainStrategy::default(),
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

fn ingress(id: &str, chain: &str, node: &str) -> Ingress {
    ingress_with_flow(id, chain, node, None)
}

fn ingress_with_flow(id: &str, chain: &str, node: &str, flow: Option<&str>) -> Ingress {
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
        anytls_identity: None,
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

fn forward_pool(to: &str, pool: HopPool) -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Forward {
            to: to.to_owned(),
            dial: HopDial::Overlay,
            pool,
        },
    }
}
