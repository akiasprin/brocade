use brocade_core::{
    artifacts::tunnel_probe,
    format::json,
    model::{
        DomainStrategy, ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalVlessTransport,
    },
};
use serde_json::Value;

fn outbound() -> ExternalOutbound {
    ExternalOutbound {
        target_strategy: None,
        id: "custom-1111-1111".to_owned(),
        tenant: "platform".to_owned(),
        name: "Vendor".to_owned(),
        address: "edge.example.com".to_owned(),
        port: 443,
        protocol: ExternalOutboundProtocol::Vless {
            final_mask: None,
            credential: "secret-uuid".to_owned(),
            encryption: "none".to_owned(),
            flow: None,
            transport: ExternalVlessTransport::Raw,
        },
        security: ExternalOutboundSecurity::Tls {
            allow_insecure: false,
            alpn: Vec::new(),
            ech_config_list: None,
            pinned_peer_cert_sha256: None,
            verify_peer_cert_by_name: None,
            server_name: "edge.example.com".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        bindings: Vec::new(),
    }
}

#[test]
fn probe_artifact_has_one_forced_outbound_and_no_direct_fallback() {
    let artifact = tunnel_probe::build(&outbound(), 19080).unwrap();
    let rendered = json::tunnel_probe(&artifact, "/run/brocade/probe.log");
    let value: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(value["inbounds"][0]["listen"], "127.0.0.1");
    assert_eq!(value["inbounds"][0]["port"], 19080);
    assert_eq!(value["outbounds"].as_array().unwrap().len(), 1);
    assert_eq!(value["outbounds"][0]["tag"], "probe-out");
    assert_eq!(value["outbounds"][0]["protocol"], "vless");
    assert_eq!(value["outbounds"][0]["settings"]["id"], "secret-uuid");
    assert!(value["outbounds"][0].get("targetStrategy").is_none());
    assert_eq!(value["routing"]["rules"][0]["outboundTag"], "probe-out");
    assert!(!rendered.contains("freedom"));
    assert_eq!(
        rendered,
        json::tunnel_probe(&artifact, "/run/brocade/probe.log")
    );
}

#[test]
fn target_strategy_is_optional_and_is_rendered_at_the_outbound_top_level() {
    let legacy = serde_json::to_value(outbound()).unwrap();
    assert!(legacy.get("target_strategy").is_none());
    assert!(serde_json::from_value::<ExternalOutbound>(legacy)
        .unwrap()
        .target_strategy
        .is_none());
    for (strategy, expected) in [
        (DomainStrategy::AsIs, "AsIs"),
        (DomainStrategy::UseIp, "UseIP"),
        (DomainStrategy::UseIpv4, "UseIPv4"),
        (DomainStrategy::UseIpv6, "UseIPv6"),
        (DomainStrategy::UseIpv4v6, "UseIPv4v6"),
        (DomainStrategy::UseIpv6v4, "UseIPv6v4"),
    ] {
        let mut value = outbound();
        value.protocol = ExternalOutboundProtocol::Anytls {
            credential: "test-password".to_owned(),
        };
        value.target_strategy = Some(strategy);
        let restored: ExternalOutbound =
            serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
        assert_eq!(restored, value);
        let artifact = tunnel_probe::build(&restored, 19080).unwrap();
        let config: Value =
            serde_json::from_str(&json::tunnel_probe(&artifact, "/run/brocade/probe.log")).unwrap();
        let outbound = &config["outbounds"][0];
        assert_eq!(outbound["targetStrategy"], expected);
        assert!(outbound["settings"].get("targetStrategy").is_none());
        assert!(outbound["streamSettings"].get("sockopt").is_none());
        assert_eq!(outbound["settings"]["address"], "edge.example.com");
    }
    let mut invalid = serde_json::to_value(outbound()).unwrap();
    invalid["target_strategy"] = serde_json::json!("unsupported");
    assert!(serde_json::from_value::<ExternalOutbound>(invalid).is_err());
}

#[test]
fn anytls_probe_preserves_explicit_certificate_bypass_and_keeps_verification_by_default() {
    for insecure in [false, true] {
        let mut value = outbound();
        value.protocol = ExternalOutboundProtocol::Anytls {
            credential: "test-password".to_owned(),
        };
        value.address = "192.0.2.25".to_owned();
        value.port = 771;
        if let ExternalOutboundSecurity::Tls { allow_insecure, .. } = &mut value.security {
            *allow_insecure = insecure;
        }
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(
            encoded["security"]["v"].get("allow_insecure"),
            insecure.then_some(&Value::Bool(true))
        );
        let restored: ExternalOutbound = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, value);
        let artifact = tunnel_probe::build(&restored, 19080).unwrap();
        let config: Value =
            serde_json::from_str(&json::tunnel_probe(&artifact, "/run/brocade/probe.log")).unwrap();
        let outbound = &config["outbounds"][0];
        assert_eq!(outbound["protocol"], "anytls");
        let settings = &outbound["streamSettings"]["tlsSettings"];
        assert_eq!(settings["serverName"], "edge.example.com");
        assert_eq!(settings["fingerprint"], "chrome");
        assert_eq!(
            settings.get("allowInsecure"),
            insecure.then_some(&Value::Bool(true))
        );
    }
}

#[test]
fn managed_warp_is_refused_without_a_console_identity() {
    let mut value = outbound();
    value.protocol = ExternalOutboundProtocol::Warp {
        http_version: Default::default(),
        remote_dns: brocade_core::model::WarpMasqueOptions::default().remote_dns,
    };
    assert_eq!(
        tunnel_probe::build(&value, 19080).unwrap_err(),
        tunnel_probe::TunnelProbeBuildError::ManagedWarpNeedsDedicatedIdentity
    );
}
