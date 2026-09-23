use brocade_core::{
    artifacts::tunnel_probe,
    format::json,
    model::{
        ExternalOutbound, ExternalOutboundProtocol, ExternalOutboundSecurity,
        ExternalVlessTransport,
    },
};
use serde_json::Value;

fn outbound() -> ExternalOutbound {
    ExternalOutbound {
        id: "custom-1111-1111".to_owned(),
        tenant: "platform".to_owned(),
        name: "Vendor".to_owned(),
        address: "edge.example.com".to_owned(),
        port: 443,
        protocol: ExternalOutboundProtocol::Vless {
            credential: "secret-uuid".to_owned(),
            encryption: "none".to_owned(),
            flow: None,
            transport: ExternalVlessTransport::Raw,
        },
        security: ExternalOutboundSecurity::Tls {
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
    assert_eq!(value["routing"]["rules"][0]["outboundTag"], "probe-out");
    assert!(!rendered.contains("freedom"));
    assert_eq!(
        rendered,
        json::tunnel_probe(&artifact, "/run/brocade/probe.log")
    );
}

#[test]
fn managed_warp_is_refused_without_a_console_identity() {
    let mut value = outbound();
    value.protocol = ExternalOutboundProtocol::Warp {
        mtu: 1280,
        keep_alive: 25,
        allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
        no_kernel_tun: false,
        domain_strategy: "ForceIP".to_owned(),
        workers: 0,
    };
    assert_eq!(
        tunnel_probe::build(&value, 19080).unwrap_err(),
        tunnel_probe::TunnelProbeBuildError::ManagedWarpNeedsDedicatedIdentity
    );
}
