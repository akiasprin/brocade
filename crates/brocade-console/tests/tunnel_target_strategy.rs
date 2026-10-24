//! Verify the actual AnyTLS address frames, rather than only config acceptance or exit-IP echoes.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use brocade_console::http::EMBEDDED_XRAYS;
use brocade_core::{
    artifacts::tunnel_probe,
    format::json,
    model::{DomainStrategy, ExternalOutbound},
};
use serde_json::{json, Value};

#[tokio::test]
async fn embedded_xray_sends_the_selected_address_family_inside_anytls_tls() {
    let (_, bytes, _) = EMBEDDED_XRAYS
        .iter()
        .find(|(arch, ..)| *arch == std::env::consts::ARCH)
        .expect("Console must embed Xray for the test host");
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("xray");
    fs::write(&binary, bytes).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let certificate =
        rcgen::generate_simple_self_signed(vec!["capture.test.invalid".to_owned()]).unwrap();
    let certificate_path = directory.path().join("certificate.pem");
    let key_path = directory.path().join("key.pem");
    fs::write(&certificate_path, certificate.cert.pem()).unwrap();
    fs::write(&key_path, certificate.signing_key.serialize_pem()).unwrap();

    let domain = "dual.capture.test.invalid";
    let ipv4 = "198.51.100.7";
    let ipv6 = "2001:db8::7";
    let mut cases = Vec::<Value>::new();
    for (name, strategy, input, expected_types, expected_addresses) in [
        ("default", None, domain, vec![3], vec![domain]),
        (
            "explicit-asis",
            Some(DomainStrategy::AsIs),
            domain,
            vec![3],
            vec![domain],
        ),
        (
            "ipv4-first",
            Some(DomainStrategy::UseIpv4v6),
            domain,
            vec![1],
            vec![ipv4],
        ),
        (
            "ipv4",
            Some(DomainStrategy::UseIpv4),
            domain,
            vec![1],
            vec![ipv4],
        ),
        (
            "ipv6-first",
            Some(DomainStrategy::UseIpv6v4),
            domain,
            vec![4],
            vec![ipv6],
        ),
        (
            "ipv6",
            Some(DomainStrategy::UseIpv6),
            domain,
            vec![4],
            vec![ipv6],
        ),
        (
            "both",
            Some(DomainStrategy::UseIp),
            domain,
            vec![1, 4],
            vec![ipv4, ipv6],
        ),
        // A literal IP is already resolved. The setting must not rewrite it or consult DNS.
        (
            "literal-ipv4",
            Some(DomainStrategy::UseIpv6),
            ipv4,
            vec![1],
            vec![ipv4],
        ),
        (
            "literal-ipv6",
            Some(DomainStrategy::UseIpv4),
            ipv6,
            vec![4],
            vec![ipv6],
        ),
    ] {
        let mut outbound: ExternalOutbound = serde_json::from_value(json!({
            "id": "custom-1111-1111", "tenant": "platform", "name": "capture",
            "address": "127.0.0.1", "port": 1,
            "protocol": { "t": "anytls", "v": { "credential": "test-only-capture-password" } },
            "security": { "t": "tls", "v": {
                "server_name": "capture.test.invalid", "fingerprint": "chrome", "allow_insecure": true
            } }
        })).unwrap();
        outbound.target_strategy = strategy;
        let artifact = tunnel_probe::build(&outbound, 1).unwrap();
        let mut config: Value =
            serde_json::from_str(&json::tunnel_probe(&artifact, "unused.log")).unwrap();
        // Static DNS makes the experiment deterministic and entirely offline. Both
        // address families exist; the TLS peer only decodes and never dials them.
        config["dns"] = json!({ "hosts": { (domain): [ipv4, ipv6] } });
        cases.push(json!({
            "name": name, "input": input, "expected_types": expected_types,
            "expected_addresses": expected_addresses, "config": config
        }));
    }
    let cases_path = directory.path().join("cases.json");
    fs::write(&cases_path, serde_json::to_vec(&cases).unwrap()).unwrap();
    let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/tunnel-target-strategy/capture.py");
    let output = tokio::time::timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("python3")
            .arg(helper)
            .arg("--binary")
            .arg(&binary)
            .arg("--cases")
            .arg(&cases_path)
            .arg("--certificate")
            .arg(&certificate_path)
            .arg("--key")
            .arg(&key_path)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("AnyTLS wire capture timed out")
    .unwrap();
    assert!(
        output.status.success(),
        "AnyTLS wire capture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["cases"].as_array().unwrap().len(), cases.len());
    println!("{}", String::from_utf8(output.stdout).unwrap());
}
