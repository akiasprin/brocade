//! Validate tunnel artifacts against the exact Xray shipped by this Console build.

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use brocade_console::http::EMBEDDED_XRAYS;
use brocade_core::{artifacts::tunnel_probe, format::json, model::ExternalOutbound};

#[tokio::test]
async fn embedded_xray_accepts_anytls_tunnel_with_explicit_insecure() {
    let (_, bytes, _) = EMBEDDED_XRAYS
        .iter()
        .find(|(arch, ..)| *arch == std::env::consts::ARCH)
        .expect("Console must embed Xray for the test host");
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("xray");
    fs::write(&binary, bytes).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

    for (insecure, strategy) in [
        (false, None),
        (true, None),
        (false, Some("as_is")),
        (false, Some("use_ip")),
        (false, Some("use_ipv4v6")),
        (false, Some("use_ipv6v4")),
        (false, Some("use_ipv4")),
        (false, Some("use_ipv6")),
    ] {
        let outbound: ExternalOutbound = serde_json::from_value(serde_json::json!({
            "id": "custom-1111-1111", "tenant": "platform", "name": "PH-test",
            "address": "192.0.2.25", "port": 771,
            "protocol": { "t": "anytls", "v": { "credential": "test-password" } },
            "security": { "t": "tls", "v": {
                "server_name": "tls.vendor.example", "fingerprint": "chrome",
                "allow_insecure": insecure
            } },
            "bindings": [], "target_strategy": strategy
        }))
        .unwrap();
        let artifact = tunnel_probe::build(&outbound, 19080).unwrap();
        let config = directory.path().join("probe.json");
        fs::write(
            &config,
            json::tunnel_probe(
                &artifact,
                &directory.path().join("probe.log").display().to_string(),
            ),
        )
        .unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(&binary)
                .args(["run", "-test", "-config"])
                .arg(&config)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("embedded Xray config validation timed out")
        .unwrap();
        assert!(
            output.status.success(),
            "embedded Xray rejected AnyTLS insecure={insecure}, target_strategy={strategy:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
