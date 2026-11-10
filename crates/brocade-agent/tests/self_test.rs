use std::{fs, process::Command};

#[test]
fn executable_self_test_requires_no_configuration_and_leaves_no_state() {
    let root = std::env::temp_dir().join(format!("brocade-cli-self-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_brocade-agent"))
        .arg("self-test")
        .current_dir(&root)
        .env_remove("BROCADE_AGENT_SERVER")
        .env_remove("BROCADE_NODE_TOKEN")
        .env("BROCADE_NODE_TOKEN_FILE", root.join("missing-token"))
        .env("BROCADE_AGENT_STATE_DIR", root.join("state"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some(brocade_deployment::protocol::AGENT_SELF_TEST_RESPONSE)
    );
    let flavor = brocade_deployment::product::ProductFlavor::from_cvm(cfg!(feature = "cvm"));
    assert_eq!(
        lines.next().unwrap(),
        format!("brocade-agent-product:{}:v1", flavor.as_str())
    );
    let metadata: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(
        metadata,
        serde_json::json!({"flavor": flavor, "arch": std::env::consts::ARCH})
    );
    assert!(lines.next().is_none());
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(root).unwrap();
}
