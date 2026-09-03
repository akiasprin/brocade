use std::net::{IpAddr, Ipv4Addr};

use brocade_core::{
    compile::compile,
    ir::{
        routing::compile_app,
        system::compile_system,
        validate::{validate_app, validate_app_set, validate_model_snapshot, validate_system},
    },
    model::{
        Accept, Action, AnyTls, AnyTlsMasquerade, AppView, Chain, DestMatch, Dns, DomainStrategy,
        EgressDnsAddressStrategy, EgressDnsFallback, EgressDnsResolution, EgressDnsTransport,
        ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalVlessTransport, ExternalVlessXhttp, ExternalVlessXhttpDownload,
        ExternalWarpBinding, Front, FrontStrategy, Grant, HopDial, HopIn, HopPool, HopWire,
        Hysteria2, HysteriaBandwidth, HysteriaMasquerade, HysteriaObfs, HysteriaPortHop, Ingress,
        IngressWires, IpFamily, ModelSettings, ModelSnapshot, Node, NodeEgressDnsPolicy,
        OverlaySettings, Projection, ProjectionDownloadEndpoint, ProjectionEndpoint, Reality,
        RealityClientPolicy, RealityFallbackMode, RealitySite, RealityXhttp, Rule, Step, Tls,
        Transport, User, WireGuardKeys, Xhttp, XhttpDownload, XhttpMode, XhttpXmux,
    },
    Diagnostic, Level,
};
use ipnet::Ipv4Net;

#[test]
fn legacy_egress_dns_binding_is_ignored_and_cleaned_when_serialized() {
    let action: Action = serde_json::from_value(serde_json::json!({
        "t": "egress",
        "send_through": null,
        "dns": true,
        "resolution": { "address": "192.0.2.53", "port": 53 }
    }))
    .unwrap();
    assert_eq!(action, Action::Egress { send_through: None });
    assert_eq!(
        serde_json::to_value(action).unwrap(),
        serde_json::json!({ "t": "egress", "send_through": null })
    );
}

#[test]
fn legacy_external_vless_without_transport_deserializes_as_raw() {
    let protocol: ExternalOutboundProtocol = serde_json::from_value(serde_json::json!({
        "t": "vless",
        "v": {
            "credential": "legacy-uuid",
            "encryption": "none",
            "flow": null
        }
    }))
    .unwrap();
    assert!(matches!(
        protocol,
        ExternalOutboundProtocol::Vless {
            transport: ExternalVlessTransport::Raw,
            ..
        }
    ));
}

#[test]
fn external_shadowsocks_is_explicitly_ss2022_raw_with_a_sized_psk() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "ss".to_owned(),
                },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    doc.external_outbounds = vec![ExternalOutbound {
        id: "ss".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "SS".to_owned(),
        address: "ss.example.net".to_owned(),
        port: 8388,
        protocol: ExternalOutboundProtocol::Shadowsocks2022 {
            credential: "ordinary-password".to_owned(),
            method: "aes-256-gcm".to_owned(),
        },
        security: ExternalOutboundSecurity::Tls {
            server_name: "ss.example.net".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        bindings: Vec::new(),
    }];

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert_has(
        &diagnostics,
        Level::Error,
        "external-outbound.shadowsocks2022-method",
    );
    assert_has(
        &diagnostics,
        Level::Error,
        "external-outbound.shadowsocks2022-transport",
    );

    doc.external_outbounds[0].protocol = ExternalOutboundProtocol::Shadowsocks2022 {
        credential: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
        method: "2022-blake3-aes-256-gcm".to_owned(),
    };
    doc.external_outbounds[0].security = ExternalOutboundSecurity::None;
    let mut valid = Vec::new();
    let sys = compile_system(&doc, &mut valid);
    let app_ir = compile_app(&doc, &app, &mut valid);
    validate_app(&sys, &app_ir, &mut valid);
    assert!(
        valid.iter().all(|diagnostic| !diagnostic
            .code
            .starts_with("external-outbound.shadowsocks2022")),
        "{valid:#?}"
    );
}

#[test]
fn managed_warp_runtime_overrides_are_validated() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "warp".to_owned(),
                },
            }],
            None,
        )],
        grants: Vec::new(),
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
            no_kernel_tun: false,
            domain_strategy: "ForceIP".to_owned(),
            workers: 257,
        },
        security: ExternalOutboundSecurity::None,
        bindings: vec![ExternalWarpBinding {
            node: "hk".to_owned(),
            device_id: "device-hk".to_owned(),
            account_id: "account-hk".to_owned(),
            registered_at: "2026-08-28T00:00:00.000Z".to_owned(),
            endpoint_address: None,
            endpoint_port: None,
            mtu: None,
            keep_alive: None,
            allowed_ips: Some(vec!["0.0.0.0/0".to_owned()]),
            no_kernel_tun: None,
            domain_strategy: None,
            workers: Some(257),
            private_key: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
            peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
            local_addresses: vec!["172.16.0.2/32".to_owned()],
            reserved: vec![0, 0, 0],
        }],
    }];
    let mut invalid_warp = Vec::new();
    let sys = compile_system(&doc, &mut invalid_warp);
    let app_ir = compile_app(&doc, &app, &mut invalid_warp);
    validate_app(&sys, &app_ir, &mut invalid_warp);
    for code in [
        "external-outbound.warp-workers",
        "external-outbound.warp-binding-address-policy",
        "external-outbound.warp-binding-workers",
    ] {
        assert_has(&invalid_warp, Level::Error, code);
    }
}

#[test]
fn external_tunnel_visibility_follows_tenant_ancestry() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "shared".to_owned(),
                },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    doc.external_outbounds = vec![ExternalOutbound {
        id: "shared".to_owned(),
        tenant: "platform".to_owned(),
        name: "共享出口".to_owned(),
        address: "proxy.example.net".to_owned(),
        port: 1080,
        protocol: ExternalOutboundProtocol::Socks5 {
            username: None,
            credential: String::new(),
        },
        security: ExternalOutboundSecurity::None,
        bindings: Vec::new(),
    }];

    let mut ancestor_diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut ancestor_diagnostics);
    let app_ir = compile_app(&doc, &app, &mut ancestor_diagnostics);
    validate_app(&sys, &app_ir, &mut ancestor_diagnostics);
    assert!(
        ancestor_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "tenant.scope"),
        "{ancestor_diagnostics:#?}"
    );

    doc.external_outbounds[0].tenant = "platform.beta".to_owned();
    let mut sibling_diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut sibling_diagnostics);
    let app_ir = compile_app(&doc, &app, &mut sibling_diagnostics);
    validate_app(&sys, &app_ir, &mut sibling_diagnostics);
    assert_has(&sibling_diagnostics, Level::Error, "tenant.scope");
}

#[test]
fn external_vless_xhttp_validates_the_complete_upload_and_download_shape() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "external".to_owned(),
                },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    doc.external_outbounds = vec![ExternalOutbound {
        id: "external".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "External XHTTP".to_owned(),
        address: "upload.example.net".to_owned(),
        port: 443,
        protocol: ExternalOutboundProtocol::Vless {
            credential: "6f9d1a8e-2b3c-4d5e-8f70-1a2b3c4d5e6f".to_owned(),
            encryption: "none".to_owned(),
            flow: Some("xtls-rprx-vision".to_owned()),
            transport: ExternalVlessTransport::Xhttp(ExternalVlessXhttp {
                path: "bad path?query".to_owned(),
                host: Some("   ".to_owned()),
                mux: Some(0),
                mode: XhttpMode::StreamOne,
                download: Some(ExternalVlessXhttpDownload {
                    address: String::new(),
                    port: 0,
                    security: ExternalOutboundSecurity::None,
                    path: "download".to_owned(),
                    host: Some(" ".to_owned()),
                    mux: Some(129),
                    mode: XhttpMode::Auto,
                }),
            }),
        },
        security: ExternalOutboundSecurity::Tls {
            server_name: "upload.example.net".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        bindings: Vec::new(),
    }];

    let mut invalid = Vec::new();
    let sys = compile_system(&doc, &mut invalid);
    let app_ir = compile_app(&doc, &app, &mut invalid);
    validate_app(&sys, &app_ir, &mut invalid);
    for code in [
        "external-outbound.xhttp-flow-conflict",
        "external-outbound.xhttp-path",
        "external-outbound.xhttp-host",
        "external-outbound.xhttp-mux-range",
        "external-outbound.xhttp-download-stream-one",
        "external-outbound.xhttp-download-endpoint",
        "external-outbound.xhttp-download-path",
        "external-outbound.xhttp-download-host",
        "external-outbound.xhttp-download-mux-range",
        "external-outbound.xhttp-download-security",
    ] {
        assert_has(&invalid, Level::Error, code);
    }

    let ExternalOutboundProtocol::Vless {
        flow, transport, ..
    } = &mut doc.external_outbounds[0].protocol
    else {
        unreachable!()
    };
    *flow = None;
    let ExternalVlessTransport::Xhttp(xhttp) = transport else {
        unreachable!()
    };
    xhttp.path = "/upload".to_owned();
    xhttp.host = None;
    xhttp.mux = Some(4);
    xhttp.mode = XhttpMode::StreamUp;
    xhttp.download = Some(ExternalVlessXhttpDownload {
        address: "download.example.net".to_owned(),
        port: 443,
        security: ExternalOutboundSecurity::Tls {
            server_name: "download.example.net".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        path: "/download".to_owned(),
        host: None,
        mux: Some(2),
        mode: XhttpMode::Auto,
    });

    let mut valid = Vec::new();
    let sys = compile_system(&doc, &mut valid);
    let app_ir = compile_app(&doc, &app, &mut valid);
    validate_app(&sys, &app_ir, &mut valid);
    assert!(
        valid
            .iter()
            .all(|diagnostic| !diagnostic.code.starts_with("external-outbound.xhttp")),
        "{valid:#?}"
    );
}

#[test]
fn external_socks_auth_and_wireguard_shape_are_validated() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "external".to_owned(),
                },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    doc.external_outbounds = vec![ExternalOutbound {
        id: "external".to_owned(),
        tenant: "platform.acme".to_owned(),
        name: "External".to_owned(),
        address: "proxy.example.net".to_owned(),
        port: 1080,
        protocol: ExternalOutboundProtocol::Socks5 {
            username: Some("operator".to_owned()),
            credential: String::new(),
        },
        security: ExternalOutboundSecurity::Tls {
            server_name: "proxy.example.net".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        bindings: Vec::new(),
    }];

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert_has(&diagnostics, Level::Error, "external-outbound.proxy-auth");
    assert_has(
        &diagnostics,
        Level::Error,
        "external-outbound.raw-transport",
    );

    doc.external_outbounds[0].protocol = ExternalOutboundProtocol::Wireguard {
        credential: "not-a-key".to_owned(),
        peer_public_key: "also-not-a-key".to_owned(),
        local_addresses: vec!["not-a-cidr".to_owned()],
        mtu: 100,
        reserved: vec![1, 2],
        keep_alive: 0,
        allowed_ips: Vec::new(),
        no_kernel_tun: true,
        domain_strategy: "AsIs".to_owned(),
    };
    let mut invalid_wireguard = Vec::new();
    let sys = compile_system(&doc, &mut invalid_wireguard);
    let app_ir = compile_app(&doc, &app, &mut invalid_wireguard);
    validate_app(&sys, &app_ir, &mut invalid_wireguard);
    for code in [
        "external-outbound.wireguard-private-key",
        "external-outbound.wireguard-public-key",
        "external-outbound.wireguard-addresses",
        "external-outbound.wireguard-mtu",
        "external-outbound.wireguard-reserved",
        "external-outbound.wireguard-allowed-ips",
        "external-outbound.wireguard-domain-strategy",
    ] {
        assert_has(&invalid_wireguard, Level::Error, code);
    }

    doc.external_outbounds[0].protocol = ExternalOutboundProtocol::Wireguard {
        credential: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
        peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
        local_addresses: vec!["172.16.0.2/32".to_owned()],
        mtu: 1420,
        reserved: vec![0, 0, 0],
        keep_alive: 25,
        allowed_ips: vec!["0.0.0.0/0".to_owned()],
        no_kernel_tun: true,
        domain_strategy: "ForceIPv4".to_owned(),
    };
    doc.external_outbounds[0].security = ExternalOutboundSecurity::None;
    let mut valid = Vec::new();
    let sys = compile_system(&doc, &mut valid);
    let app_ir = compile_app(&doc, &app, &mut valid);
    validate_app(&sys, &app_ir, &mut valid);
    assert!(
        valid.iter().all(
            |diagnostic| !diagnostic.code.starts_with("external-outbound.wireguard")
                && diagnostic.code != "external-outbound.raw-transport"
        ),
        "{valid:#?}"
    );
}

/// An unencrypted hop dialing a concrete address warns without blocking the release: the
/// address written on the chain may equally be a leased line or a private datacenter
/// network, the compiler cannot determine where it is exposed, and an error would wall off a
/// legitimate configuration.
#[test]
fn validate_notes_an_unencrypted_direct_hop() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = two_hop_ir(
        HopDial::Addr("relay.example.net:20000".to_owned()),
        HopWire::None,
        &mut diagnostics,
    );

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Info, "hop.plaintext");
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "只该是警告：{diagnostics:#?}"
    );
}

/// An unencrypted hop over the overlay reports nothing: WireGuard already wraps it, and
/// another layer burns CPU for nothing.
///
/// The test is how the hop actually dials rather than what the machine looks like — one
/// relay can perfectly well take the overlay on one chain and dial bare on another, and
/// testing by machine would collapse the two into one statement.
#[test]
fn validate_stays_quiet_about_an_unencrypted_hop_inside_the_overlay() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        !diagnostics.iter().any(|d| d.code == "hop.plaintext"),
        "包在 wg 里的一跳不该报明文：{diagnostics:#?}"
    );
}

/// An unencrypted reverse-access hop must still be reported — all that separates it from
/// direct dialing is who opens the TCP connection.
///
/// `HopDial::Reverse` offers no "reverse over the overlay" combination, so this hop is as
/// bare as direct dialing: the downstream takes its UUID to the upstream's public address.
/// Filtering on "is it Direct" misses the entire reverse family, and what gets missed is a
/// chain running in the clear over the public internet without a word.
#[test]
fn validate_notes_an_unencrypted_reverse_hop() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = reverse_hop_ir(HopWire::None, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Info, "hop.plaintext");
    let plaintext = diagnostics
        .iter()
        .find(|d| d.code == "hop.plaintext")
        .expect("上面刚断言过");
    // The wording must make clear who connects to whom: on the reverse variant the address
    // is the upstream's own, and phrased as for direct dialing it reads "hk dials hk in the
    // clear", which has the reader doubting the compiler first.
    assert!(
        plaintext
            .message
            .contains("sg 明文连入 hk.example.net:20000"),
        "{plaintext:#?}"
    );
}

/// Reverse access's downstream must not be judged unreachable for relaying.
///
/// Its `accept` is its own identity rather than a key others dial it with, and it does not
/// listen at all — it dials the upstream. Requiring reachability of it would require it to
/// join the backbone or open a public port, and "an exit need not join the backbone" is
/// precisely why this variant exists. Should this break, reverse access as designed becomes
/// entirely unshippable and the only configuration that passes is one where the downstream
/// joins the backbone too — exactly what it exists to avoid.
#[test]
fn validate_accepts_a_reverse_downstream_that_is_off_the_backbone() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = reverse_hop_ir(HopWire::None, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    let errors = diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "反向接入的合法配置报了 Error：{errors:#?}"
    );
}

/// A REALITY relay hop on a non-443 port. xray says as much itself under -test.
#[test]
fn validate_warns_about_reality_on_a_giveaway_port() {
    let reality = Reality {
        private_key: "priv".to_owned(),
        public_key: "pub".to_owned(),
        short_ids: vec!["sid".to_owned()],
        dest: "apps.apple.com:443".to_owned(),
        server_names: vec!["apps.apple.com".to_owned()],
        fingerprint: "chrome".to_owned(),
        flow: None,
    };

    let mut odd = Vec::new();
    let (sys, app_ir) = two_hop_ir(
        HopDial::Addr("odd.example.net:20000".to_owned()),
        HopWire::Reality(reality.clone()),
        &mut odd,
    );
    validate_app(&sys, &app_ir, &mut odd);
    assert_has(&odd, Level::Warn, "hop.reality-port");

    let mut tidy = Vec::new();
    let (sys, app_ir) = two_hop_ir(
        HopDial::Addr("tidy.example.net:443".to_owned()),
        HopWire::Reality(reality),
        &mut tidy,
    );
    validate_app(&sys, &app_ir, &mut tidy);
    assert!(
        !tidy.iter().any(|d| d.code == "hop.reality-port"),
        "443 上的 REALITY 不该报：{tidy:#?}"
    );
}

/// A reverse-access chain, compiled as far as hops: hk is the upstream, sg the downstream,
/// and sg dials hk.
///
/// sg is neither on the backbone nor opens a relay port — which is precisely why this variant
/// exists (an exit machine sitting in someone's home should not gain the whole backbone's
/// reach). `security` hangs off the upstream's relay port, because on the reverse variant the
/// Shadowsocks 2022 on the end a reverse hop connects to is refused outright.
///
/// The failure it prevents is a quiet one: xray would start on both machines, every artifact
/// would look correct, and the downstream would sit there having attached no tunnel — because
/// the tunnel's name is hung off a VLESS account and a shadowsocks inbound has no accounts.
///
/// An error rather than a warning. A release that goes out with this pairing produces a chain
/// that carries nothing, and there is no partial value in letting it through.
#[test]
fn validate_refuses_shadowsocks_on_the_end_a_reverse_hop_connects_to() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = reverse_hop_ir(
        HopWire::Shadowsocks2022 {
            server_psk: "0000000000000000000000==".to_owned(),
            user_psk: "1111111111111111111111==".to_owned(),
        },
        &mut diagnostics,
    );

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "hop.reverse-needs-vless");
}

/// The same wire format on a hop that is dialed rather than connected to raises nothing. Were
/// the check written against "this chain has a shadowsocks port anywhere" instead of against
/// the reverse hop's accepting end, this would fail — and the feature would be unusable on
/// exactly the chains it was asked for.
#[test]
fn validate_allows_shadowsocks_on_a_forward_hop() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = two_hop_ir(
        HopDial::Overlay,
        HopWire::Shadowsocks2022 {
            server_psk: "0000000000000000000000==".to_owned(),
            user_psk: "1111111111111111111111==".to_owned(),
        },
        &mut diagnostics,
    );

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == "hop.reverse-needs-vless"),
        "{diagnostics:#?}"
    );
}

/// upstream is the one connected to.
fn reverse_hop_ir(
    security: HopWire,
    diagnostics: &mut Vec<Diagnostic>,
) -> (
    brocade_core::ir::system::SystemIr,
    brocade_core::ir::routing::AppIr,
) {
    let mut sg = node(
        "sg",
        "platform.acme",
        Some("sg.example.net"),
        [10, 66, 0, 2],
        true,
    );
    sg.overlay = false;
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            false,
        ),
        sg,
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            Step {
                chain: "c".to_owned(),
                node: "hk".to_owned(),
                accept: None,
                // A head acting as reverse upstream must have this port open — it is what
                // the downstream connects to.
                hop_in: Some(HopIn {
                    port: 20000,
                    security,
                }),
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Forward {
                        to: "sg".to_owned(),
                        dial: HopDial::Reverse(IpFamily::V4),
                        pool: HopPool::None,
                    },
                }],
            },
            Step {
                chain: "c".to_owned(),
                node: "sg".to_owned(),
                accept: Some(Accept {
                    uuid: "uuid-sg".to_owned(),
                    label: "c@sg".to_owned(),
                }),
                hop_in: None,
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Egress { send_through: None },
                }],
            },
        ],
        grants: Vec::new(),
    };
    let sys = compile_system(&doc, diagnostics);
    let app_ir = compile_app(&doc, &app, diagnostics);
    let app_ir = brocade_core::ir::hops::compile_hops(app_ir, &sys, diagnostics);
    (sys, app_ir)
}

/// A two-hop chain, compiled as far as hops: hk dials relay per `dial`, and that hop's relay
/// port uses `security`. Every relay-hop check builds on this shape.
/// XHTTP and Vision cannot be combined, and xray will not tell anyone: fed both, `xray -test`
/// answers `Configuration OK` and the running server then refuses every connection with
/// `XTLS only supports TLS and REALITY directly for now`. Measured on 26.4.25.
///
/// That makes this rule the only thing standing between an operator and an ingress that deploys
/// green, passes its own configuration check, and carries nothing — and flow defaults to Vision,
/// so it is the state somebody reaches by turning XHTTP on and touching nothing else.
#[test]
fn validate_refuses_xhttp_together_with_vision() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: ingress.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/probe".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
            download: None,
        },
    }));
    ingress.wires.set_flow(Some("xtls-rprx-vision".to_owned()));

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.xhttp-flow-conflict");
}

#[test]
fn validate_accepts_xhttp_with_flow_turned_off() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: ingress.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/probe".to_owned(),
            host: None,
            xmux: Some(XhttpXmux::with_concurrency(16)),
            tuning: None,
            mode: XhttpMode::Auto,
            download: None,
        },
    }));
    // Both spellings of "off" reach here from the model — an explicit empty string set by the
    // operator, and nothing configured at all — and neither may be reported as a conflict.
    ingress.wires.set_flow(Some(String::new()));

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|d| !d.code.starts_with("ingress.xhttp")),
        "不该有 XHTTP 相关的抱怨：{diagnostics:#?}"
    );
}

/// `packet-up` is the one upload shape that turns the server into a door the default client
/// cannot open: measured across all sixteen server/client pairs on 26.4.25, a `packet-up` server
/// refuses a client left at `auto` — which is every client holding a subscription issued before
/// the change. A warning rather than an error, because it is still the only shape a caching CDN
/// in front will tolerate.
#[test]
fn validate_warns_about_an_upload_mode_that_locks_out_existing_clients() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: ingress.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/probe".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::PacketUp,
            download: None,
        },
    }));
    ingress.wires.set_flow(Some(String::new()));

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Warn, "ingress.xhttp-mode-packet-up");
}

/// The other three say nothing: two of them are as reachable as the default, and the default
/// itself is what every client resolves to on its own.
#[test]
fn validate_says_nothing_about_the_other_upload_modes() {
    for mode in [XhttpMode::Auto, XhttpMode::StreamUp, XhttpMode::StreamOne] {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        let ingress = &mut app_ir.ingresses[0];
        ingress.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
            reality: ingress.wires.reality().unwrap().clone(),
            xhttp: Xhttp {
                path: "/probe".to_owned(),
                host: None,
                xmux: None,
                tuning: None,
                mode,
                download: None,
            },
        }));
        ingress.wires.set_flow(Some(String::new()));

        validate_app(&sys, &app_ir, &mut diagnostics);

        assert!(
            diagnostics
                .iter()
                .all(|d| !d.code.starts_with("ingress.xhttp")),
            "{mode:?} 不该有抱怨：{diagnostics:#?}"
        );
    }
}

/// A shape presenting the machine's own certificate on a machine that has none is an ingress
/// nobody can reach: the inbound names files that are not there so xray refuses to start, and the
/// subscription carries an empty name so the client fails its own certificate check first.
#[test]
fn validate_refuses_a_tls_ingress_on_a_machine_without_a_certificate() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.wires = IngressWires::Vless(Transport::VlessTls(Tls {
        flow: Some(String::new()),
    }));
    ingress.certificate_name = None;

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.tls-no-certificate");
}

#[test]
fn validate_node_certificate_fallback_requires_a_certificate_but_not_an_external_site() {
    for (certificate_name, wants_error) in [(None, true), (Some("cover.example.net"), false)] {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        let ingress = &mut app_ir.ingresses[0];
        let reality = ingress.wires.reality_mut().unwrap();
        reality.fallback_mode = RealityFallbackMode::NodeCertificate;
        reality.dest.clear();
        reality.server_names.clear();
        ingress.certificate_name = certificate_name.map(str::to_owned);

        validate_app(&sys, &app_ir, &mut diagnostics);

        assert_eq!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ingress.tls-no-certificate"),
            wants_error,
            "{certificate_name:?}: {diagnostics:#?}"
        );
        assert!(
            diagnostics.iter().all(|diagnostic| {
                diagnostic.code != "reality.dest" && diagnostic.code != "reality.no-sni"
            }),
            "{certificate_name:?}: {diagnostics:#?}"
        );
    }
}

/// And says nothing once the machine has one. Blank counts as none — a certificate row that
/// exists with an empty name is not a name anything answers to.
#[test]
fn validate_accepts_a_tls_ingress_once_the_machine_holds_a_certificate() {
    for (name, wanted) in [(Some("a1b2.example.net"), false), (Some("  "), true)] {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        let ingress = &mut app_ir.ingresses[0];
        ingress.wires = IngressWires::Vless(Transport::VlessTls(Tls {
            flow: Some(String::new()),
        }));
        ingress.certificate_name = name.map(str::to_owned);

        validate_app(&sys, &app_ir, &mut diagnostics);

        let complained = diagnostics
            .iter()
            .any(|d| d.code == "ingress.tls-no-certificate");
        assert_eq!(complained, wanted, "{name:?}: {diagnostics:#?}");
    }
}

#[test]
fn validate_reports_each_invalid_hysteria2_operator_field() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.certificate_name = Some("hy2.example.net".to_owned());
    ingress.wires = IngressWires::Hysteria2(Hysteria2 {
        bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
        quic: brocade_core::model::HysteriaQuic::default(),
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth {
            up: Some("32 kbps".to_owned()),
            down: None,
        },
        congestion: Default::default(),
        obfs: Some(HysteriaObfs::Salamander {
            password: "  ".to_owned(),
        }),
        masquerade: HysteriaMasquerade::Proxy {
            url: "http://cover.example.net/".to_owned(),
        },
    });

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.hy2-bandwidth");
    assert_has(&diagnostics, Level::Error, "ingress.hy2-obfs-blank");
    assert_has(&diagnostics, Level::Error, "ingress.hy2-masquerade");
}

/// The two rules Xray enforces while building the config, checked here instead of on the machine.
///
/// Both are fatal at startup: `force-brutal` without a rate, and any QUIC knob outside its range,
/// stop xray from coming up. Learning that from a node whose entrances just went dark is the
/// expensive way; the compile diagnostic names the ingress and the field.
#[test]
fn validate_rejects_force_brutal_without_a_rate_and_out_of_range_quic_knobs() {
    use brocade_core::model::{HysteriaCongestion, HysteriaQuic};

    let case = |congestion, bandwidth, quic| {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        let ingress = &mut app_ir.ingresses[0];
        ingress.certificate_name = Some("hy2.example.net".to_owned());
        ingress.wires = IngressWires::Hysteria2(Hysteria2 {
            bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
            quic,
            port: 50000,
            hop: None,
            bandwidth,
            congestion,
            obfs: None,
            masquerade: HysteriaMasquerade::NotFound,
        });
        validate_app(&sys, &app_ir, &mut diagnostics);
        diagnostics
    };

    let no_rate = case(
        HysteriaCongestion::ForceBrutal,
        HysteriaBandwidth::default(),
        HysteriaQuic::default(),
    );
    assert_has(&no_rate, Level::Error, "ingress.hy2-force-brutal-needs-up");

    // The same case with a rate is fine, so the diagnostic is about the missing value and not about
    // force-brutal itself.
    let with_rate = case(
        HysteriaCongestion::ForceBrutal,
        HysteriaBandwidth {
            up: Some("20 mbps".to_owned()),
            down: Some("100 mbps".to_owned()),
        },
        HysteriaQuic::default(),
    );
    assert!(
        !with_rate
            .iter()
            .any(|d| d.code == "ingress.hy2-force-brutal-needs-up"),
        "{with_rate:#?}"
    );

    // Every bound, each one wrong by one step in the direction xray rejects.
    for quic in [
        HysteriaQuic {
            init_stream_receive_window: Some(HysteriaQuic::MIN_RECEIVE_WINDOW - 1),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            max_connection_receive_window: Some(0),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            max_idle_timeout_secs: Some(HysteriaQuic::MIN_IDLE_TIMEOUT_SECS - 1),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            max_idle_timeout_secs: Some(HysteriaQuic::MAX_IDLE_TIMEOUT_SECS + 1),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            keep_alive_period_secs: Some(HysteriaQuic::MIN_KEEP_ALIVE_SECS - 1),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            keep_alive_period_secs: Some(HysteriaQuic::MAX_KEEP_ALIVE_SECS + 1),
            ..HysteriaQuic::default()
        },
        HysteriaQuic {
            max_incoming_streams: Some(HysteriaQuic::MIN_INCOMING_STREAMS - 1),
            ..HysteriaQuic::default()
        },
    ] {
        let diagnostics = case(HysteriaCongestion::Bbr, HysteriaBandwidth::default(), quic);
        assert_has(&diagnostics, Level::Error, "ingress.hy2-quic-range");
    }

    // On the boundary is accepted — the bounds are inclusive, same as xray's.
    let edges = case(
        HysteriaCongestion::Bbr,
        HysteriaBandwidth::default(),
        HysteriaQuic {
            init_stream_receive_window: Some(HysteriaQuic::MIN_RECEIVE_WINDOW),
            max_idle_timeout_secs: Some(HysteriaQuic::MAX_IDLE_TIMEOUT_SECS),
            keep_alive_period_secs: Some(HysteriaQuic::MIN_KEEP_ALIVE_SECS),
            max_incoming_streams: Some(HysteriaQuic::MIN_INCOMING_STREAMS),
            ..HysteriaQuic::default()
        },
    );
    assert!(
        !edges.iter().any(|d| d.code == "ingress.hy2-quic-range"),
        "{edges:#?}"
    );
}

/// The two ways a port pair goes wrong, neither of which the server would ever complain about:
/// it binds the one port it was told to and starts cleanly in both cases.
///
/// Sharing a number with the TCP wire is legal at the socket layer — separate spaces — and is
/// refused because a hop range is one redirect over a run of ports; the first one written without
/// `-p udp` takes the TCP wire with it. A range excluding the listener leaves the only port that
/// actually answers outside the set clients rotate through.
#[test]
fn validate_refuses_a_shared_hysteria2_port_and_a_hop_range_without_its_listener() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.certificate_name = Some("hy2.example.net".to_owned());
    let vless = ingress.wires.vless().unwrap().clone();
    ingress.wires = IngressWires::Both {
        vless,
        hysteria2: Hysteria2 {
            port: ingress.port,
            hop: Some(brocade_core::model::HysteriaPortHop {
                start: 50_000,
                end: 50_009,
            }),
            ..Default::default()
        },
    };

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.hy2-port-shared");
    assert_has(&diagnostics, Level::Error, "ingress.hy2-hop-listener");
}

#[test]
fn validate_anytls_checks_padding_masquerade_and_tcp_port_collisions() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.certificate_name = Some("anytls.example.net".to_owned());
    let vless = ingress.wires.vless().unwrap().clone();
    ingress.wires = IngressWires::VlessAndAnyTls {
        vless,
        anytls: AnyTls {
            // AnyTLS and VLESS are both TCP, so an equal port is a real socket collision.
            port: ingress.port,
            padding_scheme: vec!["0=30-30".to_owned(), "0=40-40".to_owned()],
            masquerade: AnyTlsMasquerade::String {
                content: String::new(),
                headers: [("Bad Header".to_owned(), "line\nfeed".to_owned())]
                    .into_iter()
                    .collect(),
                status_code: 199,
            },
        },
    };

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.anytls-padding");
    assert_has(&diagnostics, Level::Error, "ingress.anytls-masquerade");
    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

#[test]
fn validate_accepts_a_valid_anytls_only_ingress_with_a_certificate() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.certificate_name = Some("anytls.example.net".to_owned());
    ingress.wires = IngressWires::AnyTls(AnyTls {
        port: 19443,
        padding_scheme: vec!["stop=2".to_owned(), "0=30-30".to_owned()],
        masquerade: AnyTlsMasquerade::NotFound {
            headers: Default::default(),
        },
    });

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "{diagnostics:#?}"
    );
}

/// A coherent pair produces neither complaint, and the ports the hop covers are claimed on this
/// machine — a relay port inside the range would stop receiving with nothing in its own config
/// wrong, so the clash has to be reported here rather than discovered on the machine.
#[test]
fn a_hop_range_claims_every_port_it_covers() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let node = app_ir.ingresses[0].node.clone();
    app_ir.ingresses[0].certificate_name = Some("hy2.example.net".to_owned());
    app_ir.ingresses[0].wires = IngressWires::Hysteria2(Hysteria2 {
        port: 50_000,
        hop: Some(brocade_core::model::HysteriaPortHop {
            start: 50_000,
            end: 50_009,
        }),
        ..Default::default()
    });
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code.starts_with("ingress.hy2")),
        "{diagnostics:#?}"
    );

    // A second ingress landing inside the range, on the same machine.
    let mut second = app_ir.ingresses[0].clone();
    second.id = format!("{}-2", second.id);
    second.node = node;
    second.certificate_name = Some("hy2.example.net".to_owned());
    second.wires = IngressWires::Hysteria2(Hysteria2 {
        port: 50_005,
        hop: None,
        ..Default::default()
    });
    app_ir.ingresses.push(second);
    let mut diagnostics = Vec::new();
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

/// REALITY's own checks have nothing to say about a shape that borrows no site. Left applying to
/// every ingress they would refuse the TLS shapes outright — no server_names, no dest, no
/// short_id — which is three errors describing a configuration that is entirely correct.
#[test]
fn validate_holds_reality_to_account_only_where_a_site_is_borrowed() {
    let mut diagnostics = Vec::new();
    let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
    let ingress = &mut app_ir.ingresses[0];
    ingress.wires = IngressWires::Vless(Transport::VlessTls(Tls {
        flow: Some(String::new()),
    }));
    ingress.certificate_name = Some("a1b2.example.net".to_owned());

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        diagnostics.iter().all(|d| !d.code.starts_with("reality.")),
        "{diagnostics:#?}"
    );
}

/// A path the two ends cannot agree on is an ingress nobody reaches: the server matches it
/// literally and answers `failed to validate path` to anything else.
#[test]
fn validate_refuses_a_path_that_cannot_round_trip() {
    for path in ["probe", "/probe?x=1", "/pr obe"] {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        app_ir.ingresses[0].wires =
            IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
                reality: app_ir.ingresses[0].wires.reality().unwrap().clone(),
                xhttp: Xhttp {
                    path: path.to_owned(),
                    host: None,
                    xmux: None,
                    tuning: None,
                    mode: XhttpMode::Auto,
                    download: None,
                },
            }));

        validate_app(&sys, &app_ir, &mut diagnostics);

        assert_has(&diagnostics, Level::Error, "ingress.xhttp-path");
    }
}

/// Refused rather than clamped, at both ends. Zero is the one to watch: xray reads it as "no
/// limit", which is already what leaving the field out says, so accepting it would give one
/// meaning two spellings.
#[test]
fn validate_refuses_a_concurrency_outside_the_range() {
    for mux in [0u16, 1000] {
        let mut diagnostics = Vec::new();
        let (sys, mut app_ir) = two_hop_ir(HopDial::Overlay, HopWire::None, &mut diagnostics);
        app_ir.ingresses[0].wires =
            IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
                reality: app_ir.ingresses[0].wires.reality().unwrap().clone(),
                xhttp: Xhttp {
                    path: "/probe".to_owned(),
                    host: None,
                    xmux: Some(XhttpXmux::with_concurrency(mux)),
                    tuning: None,
                    mode: XhttpMode::Auto,
                    download: None,
                },
            }));

        validate_app(&sys, &app_ir, &mut diagnostics);

        assert_has(&diagnostics, Level::Error, "ingress.xhttp-xmux-concurrency");
    }
}

fn two_hop_ir(
    dial: HopDial,
    security: HopWire,
    diagnostics: &mut Vec<Diagnostic>,
) -> (
    brocade_core::ir::system::SystemIr,
    brocade_core::ir::routing::AppIr,
) {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "relay",
            "platform.acme",
            Some("relay.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            Step {
                chain: "c".to_owned(),
                node: "hk".to_owned(),
                accept: None,
                hop_in: None,
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Forward {
                        to: "relay".to_owned(),
                        dial,
                        pool: HopPool::None,
                    },
                }],
            },
            Step {
                chain: "c".to_owned(),
                node: "relay".to_owned(),
                accept: Some(Accept {
                    uuid: "uuid-relay".to_owned(),
                    label: "c@relay".to_owned(),
                }),
                hop_in: Some(HopIn {
                    port: 20000,
                    security,
                }),
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Egress { send_through: None },
                }],
            },
        ],
        grants: Vec::new(),
    };
    let sys = compile_system(&doc, diagnostics);
    let app_ir = compile_app(&doc, &app, diagnostics);
    let app_ir = brocade_core::ir::hops::compile_hops(app_ir, &sys, diagnostics);
    (sys, app_ir)
}

#[test]
fn validate_system_reports_overlay_address_problems() {
    let doc = doc(vec![
        node(
            "a",
            "platform.acme",
            Some("a.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "b",
            "platform.acme",
            Some("b.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "c",
            "platform.acme",
            Some("c.example.net"),
            [10, 99, 0, 1],
            true,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    validate_system(&sys, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.overlay-dup");
    assert_has(&diagnostics, Level::Error, "node.overlay-out");
}

#[test]
fn validate_model_snapshot_reports_duplicate_user_uuid() {
    let mut snapshot = doc(Vec::new());
    snapshot.users = vec![
        User {
            tenant: "platform.acme".to_owned(),
            id: "alice".to_owned(),
            uuid: "same-uuid".to_owned(),
        },
        User {
            tenant: "platform.beta".to_owned(),
            id: "bob".to_owned(),
            uuid: "same-uuid".to_owned(),
        },
    ];
    let mut diagnostics = Vec::new();

    validate_model_snapshot(&snapshot, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "user.uuid-dup");
}

#[test]
fn compile_blocks_a_chain_with_multiple_ingresses() {
    let mut snapshot = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    snapshot.apps = vec![AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![
            ingress("i-hk", "c", "hk", None),
            ingress("i-sg", "c", "sg", None),
        ],
        fronts: Vec::new(),
        steps: vec![
            step("c", "hk", vec![any_egress()], None),
            step("c", "sg", vec![any_egress()], None),
        ],
        grants: Vec::new(),
    }];

    let output = compile(&snapshot);

    assert!(!output.can_publish(), "{:#?}", output.diagnostics);
    assert_has(&output.diagnostics, Level::Error, "chain.multi-ingress");
}

#[test]
fn validate_app_reports_duplicate_ids_no_ingress_and_empty_match() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        // Two chains collide on the id c; c has no ingress (its head does not exist, so the
        // compiler leaves its steps alone); c-ok has one, and validation finds the empty
        // geosite rule in its step. None of the three masks another.
        chains: vec![chain("c"), chain("c"), chain("c-ok")],
        ingresses: vec![ingress("i", "c-ok", "hk", None)],
        fronts: Vec::new(),
        steps: vec![Step {
            chain: "c-ok".to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Geosite(Vec::new()),
                action: Action::Block,
            }],
        }],
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "id.dup");
    assert_has(&diagnostics, Level::Error, "chain.no-ingress");
    assert_has(&diagnostics, Level::Error, "rule.empty-match");
}

#[test]
fn validate_app_reports_unrepresentable_all_match() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
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
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "rule.all-unrepresentable");
}

#[test]
fn validate_app_reports_port_and_label_collisions() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-in"), chain("c-relay")],
        ingresses: vec![
            // The same number as c-relay's relay port on sg — both TCP, so a genuine
            // conflict. This used to be 51820 (wg's port), which does not count: wg is UDP
            // and a TCP port of the same number coexists with it perfectly well (see Proto
            // in validate.rs).
            Ingress {
                port: 20000,
                ..ingress("i-sg", "c-in", "sg", None)
            },
            // c-relay's head is hk: a head accepts no relay credential, so sg has to be its
            // relay for the accept to compile
            ingress("i-relay", "c-relay", "hk", None),
        ],
        fronts: Vec::new(),
        steps: vec![
            Step {
                chain: "c-relay".to_owned(),
                node: "hk".to_owned(),
                accept: None,
                hop_in: None,
                rules: vec![forward("sg")],
            },
            Step {
                chain: "c-relay".to_owned(),
                node: "sg".to_owned(),
                accept: Some(Accept {
                    uuid: "uuid-relay".to_owned(),
                    label: "alice@platform.acme#i-sg".to_owned(),
                }),
                hop_in: Some(HopIn {
                    port: 20000,
                    security: HopWire::None,
                }),
                rules: vec![any_egress()],
            },
        ],
        grants: vec![Grant {
            tenant: "platform.acme".to_owned(),
            user: "alice".to_owned(),
            ingress: "i-sg".to_owned(),
        }],
    };
    let mut doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    doc.users.push(user("platform.acme", "alice"));
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
    assert_has(&diagnostics, Level::Error, "label.duplicate");
}

#[test]
fn validate_app_rejects_multiple_dials_to_the_same_target() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            Step {
                chain: "c".to_owned(),
                node: "hk".to_owned(),
                accept: None,
                hop_in: None,
                rules: vec![
                    Rule {
                        dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                        action: Action::Forward {
                            to: "sg".to_owned(),
                            dial: HopDial::Overlay,
                            pool: HopPool::None,
                        },
                    },
                    Rule {
                        dest_match: DestMatch::Any,
                        action: Action::Forward {
                            to: "sg".to_owned(),
                            dial: HopDial::Addr("sg.example.net:20000".to_owned()),
                            pool: HopPool::None,
                        },
                    },
                ],
            },
            step(
                "c",
                "sg",
                vec![any_egress()],
                Some(Accept {
                    uuid: "uuid-sg".to_owned(),
                    label: "c@sg".to_owned(),
                }),
            ),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = brocade_core::ir::hops::compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "rule.forward-dial-conflict");
}

#[test]
fn validate_app_set_reports_cross_view_port_and_label_collisions() {
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    doc.users.push(user("platform.acme", "alice"));
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![ingress("i-shared", "c-a", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-shared")],
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![ingress("i-shared", "c-b", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: vec![grant("alice", "i-shared")],
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);
    validate_app(&sys, &ir_a, &mut diagnostics);
    validate_app(&sys, &ir_b, &mut diagnostics);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
    assert_has(&diagnostics, Level::Error, "label.duplicate");
}

/// The cross-app layer must judge per protocol, exactly as the per-machine one does.
///
/// A Hysteria 2 ingress listens on UDP; a VLESS ingress in another view on the same machine
/// listens on TCP. One number, two ports, no contention — and reported as a clash it would
/// block a release that has nothing wrong with it. The note on `validate_app_set_ports`
/// predicted this the day the first UDP ingress arrived.
#[test]
fn validate_app_set_does_not_clash_a_udp_ingress_with_a_tcp_one_in_another_view() {
    let mut hk = node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);

    let mut quic = ingress("i-a", "c-a", "hk", None);
    quic.port = 9443;
    quic.wires = IngressWires::Hysteria2(Hysteria2 {
        port: 8443,
        ..Hysteria2::default()
    });
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![quic],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i-b", "c-b", "hk", None)
        }],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert!(
        !diagnostics.iter().any(|d| d.code == "node.port-clash"),
        "UDP 的 hy2 接入面跟另一个项目里 TCP 的接入面被判成冲突了：{diagnostics:#?}"
    );
}

/// And a real collision within one protocol still has to be blocked.
#[test]
fn validate_app_set_still_clashes_two_udp_ingresses_across_views() {
    let mut hk = node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);

    let quic_ingress = |id: &str, chain_id: &str, unused_ingress_port: u16| {
        let mut ingress = ingress(id, chain_id, "hk", None);
        ingress.port = unused_ingress_port;
        ingress.wires = IngressWires::Hysteria2(Hysteria2 {
            port: 18000,
            ..Hysteria2::default()
        });
        ingress
    };
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![quic_ingress("i-a", "c-a", 8443)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![quic_ingress("i-b", "c-b", 8444)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

#[test]
fn validate_app_set_does_not_clash_distinct_hy2_ports_with_equal_unused_ingress_ports() {
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let quic_ingress = |id: &str, chain_id: &str, hy2_port: u16| {
        let mut ingress = ingress(id, chain_id, "hk", None);
        ingress.port = 8443;
        ingress.wires = IngressWires::Hysteria2(Hysteria2 {
            port: hy2_port,
            ..Hysteria2::default()
        });
        ingress
    };
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![quic_ingress("i-a", "c-a", 18000)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![quic_ingress("i-b", "c-b", 18001)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert!(
        !diagnostics.iter().any(|d| d.code == "node.port-clash"),
        "没有实际监听者的 Ingress.port 不应产生冲突：{diagnostics:#?}"
    );
}

#[test]
fn validate_app_set_counts_both_wires_and_hy2_hop_ranges_across_views() {
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);

    let mut both = ingress("i-both", "c-both", "hk", None);
    let vless = both.wires.vless().unwrap().clone();
    both.port = 8443;
    both.wires = IngressWires::Both {
        vless,
        hysteria2: Hysteria2 {
            port: 18000,
            hop: Some(HysteriaPortHop {
                start: 18000,
                end: 18009,
            }),
            ..Hysteria2::default()
        },
    };
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-both")],
        ingresses: vec![both],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };

    let mut tcp = ingress("i-tcp", "c-tcp", "hk", None);
    tcp.port = 8443;
    let mut hopped_udp = ingress("i-udp", "c-udp", "hk", None);
    hopped_udp.port = 9443;
    hopped_udp.wires = IngressWires::Hysteria2(Hysteria2 {
        port: 18005,
        ..Hysteria2::default()
    });
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-tcp"), chain("c-udp")],
        ingresses: vec![tcp, hopped_udp],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    let clashes = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "node.port-clash")
        .collect::<Vec<_>>();
    assert!(
        clashes
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TCP 端口 8443")),
        "Both 的 TCP 半边没有登记：{diagnostics:#?}"
    );
    assert!(
        clashes
            .iter()
            .any(|diagnostic| diagnostic.message.contains("UDP 端口 18005")),
        "Hysteria 2 跳转区间没有登记：{diagnostics:#?}"
    );
}

#[test]
fn validate_app_set_reports_a_split_download_port_used_by_another_view() {
    let mut hk = node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.certificate_name = Some("hk.example.net".to_owned());
    let doc = doc(vec![hk]);

    let mut split = ingress("i-a", "c-a", "hk", None);
    split.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: split.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/split".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
            download: Some(XhttpDownload {
                v4: Some(ProjectionDownloadEndpoint {
                    host: "cdn.example.net".to_owned(),
                    port: 443,
                    origin_port: Some(8443),
                    http_host: None,
                    mux: None,
                }),
                v6: None,
            }),
        },
    }));
    split.projection.v4 = Some(ProjectionEndpoint {
        host: "198.51.100.10".to_owned(),
        port: 443,
        download: None,
    });
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![split],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i-b", "c-b", "hk", None)
        }],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let ir_a = compile_app(&doc, &app_a, &mut diagnostics);
    let ir_b = compile_app(&doc, &app_b, &mut diagnostics);

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

#[test]
fn validate_rejects_stream_one_with_an_independent_download() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: projected.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/split".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::StreamOne,
            download: Some(XhttpDownload {
                v4: Some(ProjectionDownloadEndpoint {
                    host: "cdn.example.net".to_owned(),
                    port: 443,
                    origin_port: Some(8443),
                    http_host: None,
                    mux: None,
                }),
                v6: None,
            }),
        },
    }));
    projected.wires.set_flow(None);
    projected.projection.v4 = Some(ProjectionEndpoint {
        host: "198.51.100.10".to_owned(),
        port: 443,
        download: None,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(
        &diagnostics,
        Level::Error,
        "ingress.xhttp-download-stream-one",
    );
}

// A reverse-access upstream has both an ingress and a relay port open — as a head it should
// have no relay port, and this variant relaxes that specifically for it (ir/routing.rs). Both
// ports really bind on this machine, so a collision must be reported.
//
// What this watches is whether that relaxation let the head slip out of the port check:
// `validate_ports` keys on `step.hop_in` rather than dial, so a head that acquires a hop_in
// should participate as usual. The symptom of missing it is a green compile and an xray that
// will not start.
#[test]
fn validate_app_reports_reverse_upstream_hop_in_clashing_with_its_own_ingress() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        // The ingress sits on the head hk, port 443 (the helper's default)
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            // The head acting as reverse upstream: its relay port deliberately collides
            // with its own ingress on 443
            Step {
                hop_in: Some(HopIn {
                    port: 443,
                    security: HopWire::None,
                }),
                ..step(
                    "c",
                    "hk",
                    vec![Rule {
                        dest_match: DestMatch::Any,
                        action: Action::Forward {
                            to: "sg".to_owned(),
                            dial: HopDial::Reverse(IpFamily::V4),
                            pool: HopPool::None,
                        },
                    }],
                    Some(Accept {
                        uuid: "uuid-hk".to_owned(),
                        label: "c@hk".to_owned(),
                    }),
                )
            },
            Step {
                hop_in: None,
                ..step(
                    "c",
                    "sg",
                    vec![Rule {
                        dest_match: DestMatch::Any,
                        action: Action::Egress { send_through: None },
                    }],
                    Some(Accept {
                        uuid: "uuid-sg".to_owned(),
                        label: "c@sg".to_owned(),
                    }),
                )
            },
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir = brocade_core::ir::hops::compile_hops(
        compile_app(&doc, &app, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    // First confirm the reverse edge really compiled — otherwise "no conflict reported"
    // might merely mean the edge never formed
    assert_eq!(ir.hops.len(), 1, "{diagnostics:#?}");
    assert_eq!(ir.hops[0].port, 443, "拨的是上游那个口");

    validate_app(&sys, &ir, &mut diagnostics);
    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

#[test]
fn validate_app_set_reports_cross_view_hop_in_port_collisions() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let app_a = AppView {
        id: "a".to_owned(),
        label: "A".to_owned(),
        chains: vec![chain("c-a")],
        ingresses: vec![ingress("i-a", "c-a", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step("c-a", "hk", vec![forward("sg")], None),
            step(
                "c-a",
                "sg",
                vec![any_egress()],
                Some(Accept {
                    uuid: "uuid-a-sg".to_owned(),
                    label: "c-a@sg".to_owned(),
                }),
            ),
        ],
        grants: Vec::new(),
    };
    let app_b = AppView {
        id: "b".to_owned(),
        label: "B".to_owned(),
        chains: vec![chain("c-b")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i-b", "c-b", "hk", None)
        }],
        fronts: Vec::new(),
        steps: vec![
            step("c-b", "hk", vec![forward("sg")], None),
            step(
                "c-b",
                "sg",
                vec![any_egress()],
                Some(Accept {
                    uuid: "uuid-b-sg".to_owned(),
                    label: "c-b@sg".to_owned(),
                }),
            ),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir_a = brocade_core::ir::hops::compile_hops(
        compile_app(&doc, &app_a, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    let ir_b = brocade_core::ir::hops::compile_hops(
        compile_app(&doc, &app_b, &mut diagnostics),
        &sys,
        &mut diagnostics,
    );
    validate_app(&sys, &ir_a, &mut diagnostics);
    validate_app(&sys, &ir_b, &mut diagnostics);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    validate_app_set(&[ir_a, ir_b], &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

#[test]
fn validate_app_reports_front_open_default() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c-front"), chain("c-us")],
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
        steps: vec![Step {
            chain: "c-front".to_owned(),
            node: "hk".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![any_egress()],
        }],
        grants: Vec::new(),
    };
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "front.via-open");
}

#[test]
fn validate_app_reports_tenant_scope_and_bad_dns_form() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![Node {
        dns: Dns::Servers(vec!["local".to_owned()]),
        domain_strategy: DomainStrategy::default(),
        ..node(
            "hk",
            "platform.beta",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        )
    }]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "tenant.scope");
    assert_has(&diagnostics, Level::Error, "node.dns-form");
}

#[test]
fn validate_rejects_invalid_or_non_domain_machine_dns() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![Rule {
                dest_match: DestMatch::IpCidr(vec!["203.0.113.0/24".to_owned()]),
                action: Action::Egress { send_through: None },
            }],
            None,
        )],
        grants: Vec::new(),
    };
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    doc.node_egress_dns = vec![NodeEgressDnsPolicy {
        node: "hk".to_owned(),
        position: 0,
        selector: DestMatch::IpCidr(vec!["203.0.113.0/24".to_owned()]),
        resolution: EgressDnsResolution {
            address: "resolver.example.com".to_owned(),
            port: 0,
            transport: EgressDnsTransport::Udp,
            address_strategy: EgressDnsAddressStrategy::UseIpv4,
            fallback: EgressDnsFallback::Stop,
        },
    }];
    let mut diagnostics = Vec::new();
    validate_model_snapshot(&doc, &mut diagnostics);
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "dns.selector-unsupported");
    assert_has(&diagnostics, Level::Error, "dns.address");
    assert_has(&diagnostics, Level::Error, "dns.port");
}

#[test]
fn route_matches_neither_require_nor_activate_machine_dns() {
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![step(
            "c",
            "hk",
            vec![
                Rule {
                    dest_match: DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
                Rule {
                    dest_match: DestMatch::IpCidr(vec!["203.0.113.0/24".to_owned()]),
                    action: Action::Egress { send_through: None },
                },
            ],
            None,
        )],
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);
    validate_app_set(&[app_ir], &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.code.starts_with("dns.reference")),
        "线路动作不再持有 DNS 引用：{diagnostics:#?}"
    );
}

#[test]
fn machine_dns_overlap_is_resolved_by_machine_priority_not_chain_order() {
    let mut direct = chain("c-direct");
    direct.name = "线路 A".to_owned();
    let mut transit = chain("c-transit");
    transit.name = "线路 B".to_owned();
    let mut transit_ingress = ingress("i-transit", "c-transit", "hk", None);
    transit_ingress.port = 8443;
    let mut app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![direct, transit],
        ingresses: vec![ingress("i-direct", "c-direct", "hk", None), transit_ingress],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c-direct",
                "hk",
                vec![Rule {
                    dest_match: DestMatch::DomainSuffix(vec!["example.com".to_owned()]),
                    action: Action::Egress { send_through: None },
                }],
                None,
            ),
            step(
                "c-transit",
                "hk",
                vec![Rule {
                    dest_match: DestMatch::DomainSuffix(vec![
                        "example.com".to_owned(),
                        "other.example".to_owned(),
                    ]),
                    action: Action::Egress { send_through: None },
                }],
                None,
            ),
        ],
        grants: Vec::new(),
    };
    let mut doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    doc.nodes[0].name = "出口节点".to_owned();
    doc.node_egress_dns = vec![
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 0,
            selector: app.steps[0].rules[0].dest_match.clone(),
            resolution: EgressDnsResolution {
                address: "192.0.2.53".to_owned(),
                port: 53,
                transport: EgressDnsTransport::Tcp,
                address_strategy: EgressDnsAddressStrategy::UseIp,
                fallback: EgressDnsFallback::Machine,
            },
        },
        NodeEgressDnsPolicy {
            node: "hk".to_owned(),
            position: 1,
            selector: app.steps[1].rules[0].dest_match.clone(),
            resolution: EgressDnsResolution {
                address: "198.51.100.53".to_owned(),
                port: 53,
                transport: EgressDnsTransport::Tcp,
                address_strategy: EgressDnsAddressStrategy::UseIpv6,
                fallback: EgressDnsFallback::Machine,
            },
        },
    ];
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);
    validate_app_set(&[app_ir], &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "dns.rule-conflict"),
        "DNS 由机器策略顺序唯一决定，不应再从链路顺序推导冲突：{diagnostics:#?}"
    );

    app.steps[1].rules[0].dest_match = app.steps[0].rules[0].dest_match.clone();
    let mut duplicate_diagnostics = Vec::new();
    let duplicate_ir = compile_app(&doc, &app, &mut duplicate_diagnostics);
    validate_app(&sys, &duplicate_ir, &mut duplicate_diagnostics);
    validate_app_set(&[duplicate_ir], &mut duplicate_diagnostics);
    assert!(
        duplicate_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "dns.rule-conflict"),
        "完全相同的重复配置不应被当成冲突：{duplicate_diagnostics:#?}"
    );
}

#[test]
fn validate_app_reports_topology_cycles_and_allows_converging_forward_rules() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 2],
            true,
        ),
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 3],
            true,
        ),
    ]);
    let cycle_app = AppView {
        id: "cycle".to_owned(),
        label: "环".to_owned(),
        chains: vec![chain("c-cycle")],
        ingresses: vec![ingress("i-cycle", "c-cycle", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step("c-cycle", "hk", vec![forward("sg")], None),
            step("c-cycle", "sg", vec![forward("hk")], None),
        ],
        grants: Vec::new(),
    };
    let converging_app = AppView {
        id: "converging".to_owned(),
        label: "汇合".to_owned(),
        chains: vec![chain("c-converging")],
        ingresses: vec![ingress("i-converging", "c-converging", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            step(
                "c-converging",
                "hk",
                vec![
                    Rule {
                        dest_match: DestMatch::DomainSuffix(vec!["video.example".to_owned()]),
                        action: Action::Forward {
                            to: "us".to_owned(),
                            dial: HopDial::Overlay,
                            pool: HopPool::None,
                        },
                    },
                    forward("sg"),
                ],
                None,
            ),
            step("c-converging", "sg", vec![forward("us")], None),
            step("c-converging", "us", vec![any_egress()], None),
        ],
        grants: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let cycle_ir = compile_app(&doc, &cycle_app, &mut diagnostics);
    validate_app(&sys, &cycle_ir, &mut diagnostics);
    let converging_ir = compile_app(&doc, &converging_app, &mut diagnostics);
    validate_app(&sys, &converging_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "chain.cycle");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "chain.not-tree"),
        "本地转发规则允许汇到同一个下游，不该再报非树：{diagnostics:#?}"
    );
}

#[test]
fn validate_app_reports_front_blocked_and_unproven_paths() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);

    let blocked_ir = compile_app(
        &doc,
        &front_app(
            vec![step("c-front", "hk", vec![any_block()], None)],
            Vec::new(),
        ),
        &mut diagnostics,
    );
    validate_app(&sys, &blocked_ir, &mut diagnostics);

    let unproven_ir = compile_app(
        &doc,
        &front_app(
            vec![step(
                "c-front",
                "hk",
                vec![
                    Rule {
                        dest_match: DestMatch::Geosite(vec!["netflix".to_owned()]),
                        action: Action::Egress { send_through: None },
                    },
                    any_block(),
                ],
                None,
            )],
            Vec::new(),
        ),
        &mut diagnostics,
    );
    validate_app(&sys, &unproven_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "front.blocked");
    assert_has(&diagnostics, Level::Info, "front.unproven");
}

#[test]
fn validate_app_reports_front_grant_without_via_grant_and_via_without_egress() {
    let mut doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            false,
        ),
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    doc.users.push(user("platform.acme", "alice"));
    let app = front_app(
        vec![step("c-front", "hk", vec![any_block()], None)],
        vec![grant("alice", "i-us")],
    );
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "front.grant-via");
    assert_has(&diagnostics, Level::Error, "front.via-no-egress");
}

/// Decommissioning a machine that hosts front ingresses must not deadlock the release.
///
/// `compile_app` drops vias pointing at ingresses that are already gone — a consequence of
/// the decommissioning rather than a mistyped group (`front.unknown-via` already established
/// this). Blocking afterwards on "the user has no grant for the via" means decommissioning
/// that machine requires manually deleting a swathe of grants first — the same trap in a new
/// place.
///
/// An empty group should still be visible, but as a warning: blocking the release would once
/// again leave the machine undecommissionable.
#[test]
fn validate_does_not_block_publish_when_retirement_empties_a_front_group() {
    let mut hk = node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.retired = true;
    let mut doc = doc(vec![
        hk,
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    doc.users.push(user("platform.acme", "alice"));
    let app = front_app(Vec::new(), vec![grant("alice", "i-us")]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    // Precondition: the group really was emptied, or this test measures something else
    assert!(
        app_ir.fronts.iter().all(|front| front.via.is_empty()),
        "{:#?}",
        app_ir.fronts
    );

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        !diagnostics.iter().any(|d| d.code == "front.grant-via"),
        "退役摘空的组不该反过来拦授权：{diagnostics:#?}"
    );
    assert_has(&diagnostics, Level::Warn, "front.no-via");
    assert!(
        diagnostics.iter().all(|d| d.level != Level::Error),
        "退役得发得出去：{diagnostics:#?}"
    );
}

/// The front-admission test follows Forwards downward, and the diagnostic points at the
/// machine that actually decides.
///
/// A hop writing `any → Forward` decides neither to admit nor to block; it defers to the next
/// machine. Looking only at the head, a multi-hop front chain gets reported as "the head
/// blocked it" — which is false, and has someone poring over a rule table that only forwards
/// while the machine that really blocked it goes unexamined.
#[test]
fn front_verdict_follows_forward_to_the_node_that_actually_decides() {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "sg",
            "platform.acme",
            Some("sg.example.net"),
            [10, 66, 0, 3],
            true,
        ),
        node(
            "us",
            "platform.acme",
            Some("us.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    // The head only forwards; the decision to block falls on sg
    let app = front_app(
        vec![
            step("c-front", "hk", vec![forward("sg")], None),
            step(
                "c-front",
                "sg",
                vec![any_block()],
                Some(Accept {
                    uuid: "uuid-sg".to_owned(),
                    label: "c-front@sg".to_owned(),
                }),
            ),
        ],
        Vec::new(),
    );
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &ir, &mut diagnostics);

    let blocked = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "front.blocked")
        .unwrap_or_else(|| panic!("没报 front.blocked：{diagnostics:#?}"));
    assert_eq!(
        blocked.location, "sg",
        "该指着真正挡掉它的那台，不是只写了转发的链头：{blocked:#?}"
    );
}

#[test]
fn validate_app_reports_slug_and_reality_shape_errors() {
    let bad_transport = Transport::VlessReality(brocade_core::model::RealitySettings {
        dest: "missing-port".to_owned(),
        server_names: Vec::new(),
        fingerprint: "chrome".to_owned(),
        flow: None,
        fallback_mode: Default::default(),
        fallback_guard: true,
        fallback_limits: Default::default(),
    });
    let mut bad_ingress = ingress("i", "c", "hk", None);
    bad_ingress.id = "bad@email".to_owned();
    bad_ingress.identity.short_ids = vec!["not-hex".to_owned()];
    bad_ingress.wires = IngressWires::Vless(bad_transport);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![bad_ingress],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "label.charset");
    assert_has(&diagnostics, Level::Error, "reality.no-sni");
    assert_has(&diagnostics, Level::Error, "reality.dest");
    assert_has(&diagnostics, Level::Error, "reality.short-id");
}

// Dial direction is derived from per-address NAT flags: an end with only NAT'd public IPs
// dials the reachable peer one way and gets a keepalive; with neither end reachable the link
// never handshakes.
#[test]
fn dial_direction_and_keepalive_follow_public_ip_nat() {
    use brocade_core::ir::system::Dial;

    let mut nat = node(
        "nat",
        "platform",
        Some("nat.example.net"),
        [10, 66, 0, 2],
        true,
    );
    nat.public_ipv4_nat = true;
    let mut doc = doc(vec![
        node(
            "hk",
            "platform",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        nat,
    ]);
    doc.settings.overlay.keepalive_secs = 17;

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(sys.links.len(), 1);
    let link = &sys.links[0];
    // a=hk is reachable and b=nat is not → only nat can dial hk
    assert_eq!(link.dial, Dial::BtoA);
    assert_eq!(link.keepalive_secs, 17);

    // Neither end has a reachable public IP: report link.no-endpoint and generate no
    // link
    let mut both = doc.clone();
    both.nodes[0].public_ipv4_nat = true;
    let mut diagnostics = Vec::new();
    let sys = compile_system(&both, &mut diagnostics);
    assert!(sys.links.is_empty());
    assert!(
        diagnostics.iter().any(|d| d.code == "link.no-endpoint"),
        "{diagnostics:?}"
    );
}

#[test]
fn validate_model_snapshot_reports_reality_client_policy_errors() {
    let mut invalid_shape = doc(Vec::new());
    invalid_shape.settings = ModelSettings {
        connection: Default::default(),
        stats_user_online: false,
        reality_client: RealityClientPolicy {
            min_client_ver: Some("1.x.0".to_owned()),
            max_client_ver: Some("1.9".to_owned()),
            max_time_diff_ms: Some(86_400_001),
        },
        reality_site: RealitySite::default(),
        overlay: OverlaySettings::default(),
        ports: Default::default(),
        probe: Default::default(),
        geodata: Default::default(),
    };
    let mut diagnostics = Vec::new();
    validate_model_snapshot(&invalid_shape, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "reality.client-ver");
    assert_has(&diagnostics, Level::Error, "reality.max-time-diff");

    let mut invalid_range = doc(Vec::new());
    invalid_range.settings = ModelSettings {
        connection: Default::default(),
        stats_user_online: false,
        reality_client: RealityClientPolicy {
            min_client_ver: Some("1.10.0".to_owned()),
            max_client_ver: Some("1.9.9".to_owned()),
            max_time_diff_ms: None,
        },
        reality_site: RealitySite::default(),
        overlay: OverlaySettings::default(),
        ports: Default::default(),
        probe: Default::default(),
        geodata: Default::default(),
    };
    let mut diagnostics = Vec::new();
    validate_model_snapshot(&invalid_range, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "reality.client-ver-range");
}

/// What sits on a decommissioned node shuts down automatically: its ingresses are not
/// rendered, chains whose trunk includes it are disabled entirely, and compilation does not
/// error — "decommissioned means gone" is the design, and reporting "still in service" so
/// that the decommissioning cannot ship is the bug. The rendering behavior of a disabled
/// chain is verified by `retired_node_ingress_and_chains_are_not_rendered` in compile.rs;
/// this verifies that the validation layer stays quiet for decommissioning.
#[test]
fn retired_node_and_its_chains_compile_clean() {
    let mut hk = node(
        "hk",
        "platform",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.retired = true;
    let sg = node(
        "sg",
        "platform",
        Some("sg.example.net"),
        [10, 66, 0, 2],
        true,
    );
    let mut doc = doc(vec![hk, sg]);
    doc.apps = vec![AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    }];

    let output = compile(&doc);
    assert_eq!(
        output.summary.errors, 0,
        "退役不该报错：{:#?}",
        output.diagnostics
    );
}

/// A fake-TCP port colliding with an ingress must be blocked at compile time. Unblocked, the
/// symptom is phantun failing to start while all three configs look correct on their own —
/// the hardest kind to chase.
#[test]
fn validate_rejects_a_fake_tcp_port_that_clashes_with_an_ingress() {
    let mut hk = node(
        "hk",
        "platform",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    hk.wireguard.transport = brocade_core::model::WgTransport::FakeTcp { port: 8443 };
    // A peer is needed for a link, and a link is needed before a phantun server binds this
    // port — a machine on its own starts no phantun, and the port it declared occupies not
    // one byte.
    let sg = node(
        "sg",
        "platform",
        Some("sg.example.net"),
        [10, 66, 0, 2],
        true,
    );
    let mut doc = doc(vec![hk, sg]);
    doc.apps = vec![AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![Ingress {
            port: 8443,
            ..ingress("i", "c", "hk", None)
        }],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    }];

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app = compile_app(&doc, &doc.apps[0], &mut diagnostics);
    validate_app(&sys, &app, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

/// wg's UDP port does not conflict with a TCP port of the same number — the kernel permits
/// binding both, which is exactly how the fake-TCP variant works (phantun accepts TCP and
/// forwards to the local wg's UDP).
///
/// Checked together, the report reads "51820 is already taken by WireGuard" while WireGuard
/// is not on TCP at all, leaving someone unable to make sense of a legitimate
/// configuration.
#[test]
fn validate_does_not_clash_a_udp_wireguard_port_with_the_same_tcp_port() {
    let hk = node(
        "hk",
        "platform",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    let wg_port = hk.wireguard.listen_port;
    let mut doc = doc(vec![hk]);
    doc.apps = vec![AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        // The ingress (TCP) takes the same number as wg (UDP)
        ingresses: vec![Ingress {
            port: wg_port,
            ..ingress("i", "c", "hk", None)
        }],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    }];

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app = compile_app(&doc, &doc.apps[0], &mut diagnostics);
    validate_app(&sys, &app, &mut diagnostics);

    assert!(
        !diagnostics.iter().any(|d| d.code == "node.port-clash"),
        "UDP 的 wg 口跟 TCP 的接入面被判成冲突了：{diagnostics:#?}"
    );
}

/// A collision within one protocol must still be blocked: two TCP ports fighting over one
/// bind really will not start.
#[test]
fn validate_still_clashes_two_tcp_ports_on_the_same_node() {
    let mut hk = node(
        "hk",
        "platform",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    );
    // The fake-TCP port (TCP) takes the same number as the ingress (TCP)
    hk.wireguard.transport = brocade_core::model::WgTransport::FakeTcp { port: 9443 };
    let sg = node(
        "sg",
        "platform",
        Some("sg.example.net"),
        [10, 66, 0, 2],
        true,
    );
    let mut doc = doc(vec![hk, sg]);
    doc.apps = vec![AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![Ingress {
            port: 9443,
            ..ingress("i", "c", "hk", None)
        }],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    }];

    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app = compile_app(&doc, &doc.apps[0], &mut diagnostics);
    validate_app(&sys, &app, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

/// "No projection" is the switch being off (`None`), not the address being left blank.
///
/// Once an empty string lands in the database, whether the operator meant to turn it off or
/// filled it in halfway can never be established again; and the artifacts end up with an
/// undialable address while the machine side looks entirely fine — an error that surfaces
/// only when somebody tries to connect. The UI blocks it too, but the UI cannot be the only
/// guard: drafts can be pushed straight through the API.
#[test]
fn validate_app_rejects_a_projection_that_is_switched_on_but_blank() {
    let mut blank = ingress("i", "c", "hk", None);
    blank.projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: "   ".to_owned(),
            port: 20443,
            download: None,
        }),
        v6: Some(ProjectionEndpoint {
            host: "v6.acc.example.net".to_owned(),
            port: 0,
            download: None,
        }),
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![blank],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.projection-blank");
    assert_has(&diagnostics, Level::Error, "ingress.projection-port");
}

/// A properly filled projection must report nothing — least of all a check on whether the
/// address really reaches this machine: that line's relaying arrangement lies outside
/// brocade, the compiler has no basis to judge, and checking would only produce false
/// alarms.
#[test]
fn validate_app_stays_quiet_about_a_filled_in_projection() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.projection = Projection {
        v4: Some(ProjectionEndpoint {
            host: "cu.acc.example.net".to_owned(),
            port: 20443,
            download: None,
        }),
        v6: None,
    };
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.starts_with("ingress.projection")),
        "{diagnostics:#?}"
    );
}

#[test]
fn legacy_projection_download_is_not_an_xhttp_download() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.projection.v4 = Some(ProjectionEndpoint {
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
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);

    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.code.starts_with("ingress.xhttp-download")),
        "legacy projection download must not become an active XHTTP setting: {diagnostics:#?}"
    );
}

#[test]
fn validate_accepts_reality_xhttp_with_a_tls_download_front() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: projected.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/split".to_owned(),
            host: Some("upload.route.example".to_owned()),
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
            download: Some(XhttpDownload {
                v4: Some(ProjectionDownloadEndpoint {
                    host: "cdn.example.net".to_owned(),
                    port: 443,
                    origin_port: Some(8443),
                    http_host: Some("download.route.example".to_owned()),
                    mux: Some(24),
                }),
                v6: None,
            }),
        },
    }));
    projected.wires.set_flow(None);
    projected.projection.v4 = Some(ProjectionEndpoint {
        host: "198.51.100.10".to_owned(),
        port: 443,
        download: None,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let mut app_ir = compile_app(&doc, &app, &mut diagnostics);
    app_ir.ingresses[0].certificate_name = Some("hk.example.net".to_owned());
    validate_app(&sys, &app_ir, &mut diagnostics);

    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.level != Level::Error),
        "{diagnostics:#?}"
    );
}

#[test]
fn validate_rejects_invalid_xhttp_client_routing_fields() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: projected.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/split".to_owned(),
            host: Some("  ".to_owned()),
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
            download: Some(XhttpDownload {
                v4: Some(ProjectionDownloadEndpoint {
                    host: "cdn.example.net".to_owned(),
                    port: 443,
                    origin_port: Some(8443),
                    http_host: Some(" ".to_owned()),
                    mux: Some(0),
                }),
                v6: None,
            }),
        },
    }));
    projected.wires.set_flow(None);
    projected.projection.v4 = Some(ProjectionEndpoint {
        host: "198.51.100.10".to_owned(),
        port: 443,
        download: None,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        Some("hk.example.net"),
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let mut app_ir = compile_app(&doc, &app, &mut diagnostics);
    app_ir.ingresses[0].certificate_name = Some("hk.example.net".to_owned());
    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.xhttp-host");
    assert_has(
        &diagnostics,
        Level::Error,
        "ingress.xhttp-download-http-host",
    );
    assert_has(
        &diagnostics,
        Level::Error,
        "ingress.xhttp-download-mux-range",
    );
}

#[test]
fn validate_reality_split_requires_a_certificate_and_a_distinct_port() {
    let mut projected = ingress("i", "c", "hk", None);
    projected.wires = IngressWires::Vless(Transport::VlessRealityXhttp(RealityXhttp {
        reality: projected.wires.reality().unwrap().clone(),
        xhttp: Xhttp {
            path: "/split".to_owned(),
            host: None,
            xmux: None,
            tuning: None,
            mode: XhttpMode::Auto,
            download: Some(XhttpDownload {
                v4: Some(ProjectionDownloadEndpoint {
                    host: "cdn.example.net".to_owned(),
                    port: 443,
                    origin_port: None,
                    http_host: None,
                    mux: None,
                }),
                v6: None,
            }),
        },
    }));
    projected.wires.set_flow(None);
    projected.projection.v4 = Some(ProjectionEndpoint {
        host: "198.51.100.10".to_owned(),
        port: 443,
        download: None,
    });
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![projected],
        fronts: Vec::new(),
        steps: Vec::new(),
        grants: Vec::new(),
    };
    let doc = doc(vec![node(
        "hk",
        "platform.acme",
        None,
        [10, 66, 0, 1],
        true,
    )]);
    let mut diagnostics = Vec::new();
    let sys = compile_system(&doc, &mut diagnostics);
    let app_ir = compile_app(&doc, &app, &mut diagnostics);
    validate_app(&sys, &app_ir, &mut diagnostics);

    assert_has(&diagnostics, Level::Error, "ingress.tls-no-certificate");
    assert_has(
        &diagnostics,
        Level::Error,
        "ingress.xhttp-download-port-clash",
    );
    assert_has(&diagnostics, Level::Error, "node.port-clash");
}

fn assert_has(diagnostics: &[Diagnostic], level: Level, code: &'static str) {
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == level && diagnostic.code == code),
        "missing {level:?} {code}; diagnostics: {diagnostics:#?}"
    );
}

fn doc(nodes: Vec<Node>) -> ModelSnapshot {
    ModelSnapshot {
        revision: 23,
        overlay_cidr: Ipv4Net::new(Ipv4Addr::new(10, 66, 0, 0), 16).unwrap(),
        settings: Default::default(),
        nodes,
        node_egress_dns: Vec::new(),
        users: Vec::new(),
        external_outbounds: Vec::new(),
        apps: Vec::new(),
    }
}

fn node(
    id: &str,
    tenant: &str,
    public_ipv4: Option<&str>,
    overlay: [u8; 4],
    egress_allowed: bool,
) -> Node {
    Node {
        mtu: None,
        connection: Default::default(),
        retired: false,
        id: id.to_owned(),
        tenant: tenant.to_owned(),
        name: id.to_owned(),
        public_ipv4: public_ipv4.map(str::to_owned),
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
        dns: Dns::System,
        domain_strategy: DomainStrategy::default(),
    }
}

fn user(tenant: &str, id: &str) -> User {
    User {
        tenant: tenant.to_owned(),
        id: id.to_owned(),
        uuid: format!("uuid-{tenant}-{id}"),
    }
}

fn front_app(steps: Vec<Step>, grants: Vec<Grant>) -> AppView {
    AppView {
        id: "front".to_owned(),
        label: "前置".to_owned(),
        chains: vec![chain("c-front"), chain("c-us")],
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
        steps,
        grants,
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
            short_ids: vec!["0123abcd".to_owned()],
        },
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
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Forward {
            to: to.to_owned(),
            dial: HopDial::Overlay,
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

fn any_block() -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Block,
    }
}

fn grant(user: &str, ingress: &str) -> Grant {
    Grant {
        tenant: "platform.acme".to_owned(),
        user: user.to_owned(),
        ingress: ingress.to_owned(),
    }
}

/// A connection setting on a reverse hop is refused rather than ignored.
///
/// Reverse has the peer open the connection; this machine holds a virtual outbound onto a
/// tunnel already up, and there is no dial to pool. Dropping the value silently would be worse
/// than refusing it — the operator picked a setting, the console keeps showing it, and nothing
/// on the machine ever acts on it.
#[test]
fn a_connection_setting_on_a_reverse_hop_is_refused() {
    for pool in [HopPool::Pool, HopPool::Merge(8)] {
        let mut diagnostics = Vec::new();
        let (sys, app_ir) = pool_ir(HopDial::Reverse(IpFamily::V4), pool, &mut diagnostics);
        validate_app(&sys, &app_ir, &mut diagnostics);
        assert_has(&diagnostics, Level::Error, "rule.pool-on-reverse");
    }

    // The same hop without one compiles clean, so the diagnostic above is about the pool and
    // not about reverse hops in general.
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = pool_ir(
        HopDial::Reverse(IpFamily::V4),
        HopPool::None,
        &mut diagnostics,
    );
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert!(
        !diagnostics.iter().any(|d| d.code == "rule.pool-on-reverse"),
        "{diagnostics:#?}"
    );
}

/// The concurrency-one pool remains valid for existing authored models, but it must not look as
/// safe as a normal connection pool. Xray selects an idle Mux.cool worker without probing the
/// underlying TCP connection first, which is the observed source of long stalls after idle reuse.
#[test]
fn a_concurrency_one_pool_warns_once_per_edge() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = pool_ir_rules(
        vec![
            forward_pool("relay", HopDial::Overlay, HopPool::Pool),
            forward_pool("relay", HopDial::Overlay, HopPool::Pool),
        ],
        &mut diagnostics,
    );
    validate_app(&sys, &app_ir, &mut diagnostics);

    let warnings = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "rule.pool-concurrency-one")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1, "{diagnostics:#?}");
    assert_eq!(warnings[0].level, Level::Warn);
}

/// The merge count is refused outside 2..=128 rather than clamped into it.
///
/// xray reads 0 as 8 and caps anything above 128, so reproducing either would leave the console
/// showing one number while the machine runs another — and 1 is not out of range so much as
/// spelled elsewhere, which the message has to say or the operator retries 1 and gets the same
/// error.
#[test]
fn a_merge_count_outside_the_range_is_refused() {
    for n in [0, 1, 129, 1000] {
        let mut diagnostics = Vec::new();
        let (sys, app_ir) = pool_ir(HopDial::Overlay, HopPool::Merge(n), &mut diagnostics);
        validate_app(&sys, &app_ir, &mut diagnostics);
        assert_has(&diagnostics, Level::Error, "rule.pool-range");
    }
    for n in [2, 8, 128] {
        let mut diagnostics = Vec::new();
        let (sys, app_ir) = pool_ir(HopDial::Overlay, HopPool::Merge(n), &mut diagnostics);
        validate_app(&sys, &app_ir, &mut diagnostics);
        assert!(
            !diagnostics.iter().any(|d| d.code == "rule.pool-range"),
            "{n} 该是合法的：{diagnostics:#?}"
        );
    }
}

/// Two rules pointing at one target must agree, the same way they already must on `dial`.
///
/// One edge compiles to one outbound, so two settings cannot both hold. Left unchecked, the
/// winner is whichever rule the compiler reaches first — a rule order away from changing how
/// every stream on that hop connects.
#[test]
fn two_rules_to_one_target_must_agree_on_the_connection_setting() {
    let mut diagnostics = Vec::new();
    let (sys, app_ir) = pool_ir_rules(
        vec![
            forward_pool("relay", HopDial::Overlay, HopPool::Pool),
            forward_pool("relay", HopDial::Overlay, HopPool::Merge(8)),
        ],
        &mut diagnostics,
    );
    validate_app(&sys, &app_ir, &mut diagnostics);
    assert_has(&diagnostics, Level::Error, "rule.forward-pool-conflict");
}

fn forward_pool(to: &str, dial: HopDial, pool: HopPool) -> Rule {
    Rule {
        dest_match: DestMatch::Any,
        action: Action::Forward {
            to: to.to_owned(),
            dial,
            pool,
        },
    }
}

fn pool_ir(
    dial: HopDial,
    pool: HopPool,
    diagnostics: &mut Vec<Diagnostic>,
) -> (
    brocade_core::ir::system::SystemIr,
    brocade_core::ir::routing::AppIr,
) {
    pool_ir_rules(vec![forward_pool("relay", dial, pool)], diagnostics)
}

/// `two_hop_ir`'s shape with the head's rules supplied, so the conflict case can write two.
///
/// The head opens a relay port unconditionally: reverse needs one (it is what the downstream
/// connects to) and a forward hop simply never looks at it, so one shape serves both and the
/// tests differ only in the rules.
fn pool_ir_rules(
    rules: Vec<Rule>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (
    brocade_core::ir::system::SystemIr,
    brocade_core::ir::routing::AppIr,
) {
    let doc = doc(vec![
        node(
            "hk",
            "platform.acme",
            Some("hk.example.net"),
            [10, 66, 0, 1],
            true,
        ),
        node(
            "relay",
            "platform.acme",
            Some("relay.example.net"),
            [10, 66, 0, 2],
            true,
        ),
    ]);
    let app = AppView {
        id: "app".to_owned(),
        label: "应用".to_owned(),
        chains: vec![chain("c")],
        ingresses: vec![ingress("i", "c", "hk", None)],
        fronts: Vec::new(),
        steps: vec![
            Step {
                chain: "c".to_owned(),
                node: "hk".to_owned(),
                accept: None,
                hop_in: Some(HopIn {
                    port: 20000,
                    security: HopWire::None,
                }),
                rules,
            },
            Step {
                chain: "c".to_owned(),
                node: "relay".to_owned(),
                accept: Some(Accept {
                    uuid: "uuid-relay".to_owned(),
                    label: "c@relay".to_owned(),
                }),
                hop_in: Some(HopIn {
                    port: 20001,
                    security: HopWire::None,
                }),
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Egress { send_through: None },
                }],
            },
        ],
        grants: Vec::new(),
    };
    let sys = compile_system(&doc, diagnostics);
    let app_ir = compile_app(&doc, &app, diagnostics);
    let app_ir = brocade_core::ir::hops::compile_hops(app_ir, &sys, diagnostics);
    (sys, app_ir)
}
