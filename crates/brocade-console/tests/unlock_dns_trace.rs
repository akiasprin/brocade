//! Actual connection-bound provenance using the exact Xray embedded by this Console.
#[test]
fn embedded_xray_reports_actual_fallback_without_guessing_from_query_strategy() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let (_, bytes, _) = brocade_console::http::EMBEDDED_XRAYS
        .iter()
        .find(|(arch, ..)| *arch == std::env::consts::ARCH)
        .expect("host Xray must be embedded");
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("xray");
    fs::write(&binary, bytes).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unlock_dns_trace.py");
    let output = Command::new("python3")
        .arg(script)
        .arg(binary)
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "simulation failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("simulation-results.json")).unwrap())
            .unwrap();
    assert_eq!(result["passed"], true);
    assert_eq!(result["results"].as_array().unwrap().len(), 11);
}
