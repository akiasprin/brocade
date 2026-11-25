mod common;
use brocade_cvm_compile::digest;
use brocade_deployment::cvm::*;
use common::*;
use serde_json::json;

#[test]
fn preparation_without_a_pinned_shim_never_creates_a_runtime() {
    let f = Fixture::new();
    f.state.lock().unwrap().omit_prepared_shim = true;
    f.backend().submit(&f.desired()).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "unknown", "{report}");
    assert!(report["error"].as_str().unwrap().contains("pinned shim"));
    let creates = f.state.lock().unwrap().creates;
    assert_eq!(creates, 0);
}

#[test]
fn guest_initialization_failure_preserves_the_closed_boot_policy() {
    let f = Fixture::new();
    let mut desired = f.desired();

    desired.spec.login_revision = 1;
    desired.spec.routing = serde_json::from_value(json!({"revision":1,"priorities":[{"chain_id":"chain-test","ipv4":100}],"dns":[],"routes":[{"destination":"0.0.0.0/0","chain_id":"chain-test"}]})).unwrap();
    desired.spec_sha256 =
        brocade_core::hash::sha256_hex(&serde_json::to_vec(&desired.spec).unwrap());
    let mut input = desired.artifacts.take().unwrap().input;
    input.spec = desired.spec.clone();
    input.capabilities.chain_egress = true;
    desired.artifacts = Some(brocade_cvm_compile::compile(&input).unwrap());
    // Creation succeeds, but the missing credential prevents initialization.
    std::fs::remove_file(f.records.join("credentials/cvm-test.json")).unwrap();
    f.backend().submit(&desired).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "unknown", "{report}");
    let (creates, initial_internet_access, initial_deny_out, actions) = {
        let state = f.state.lock().unwrap();
        (
            state.creates,
            state.initial_internet_access,
            state.initial_deny_out.clone(),
            state.network_actions.clone(),
        )
    };
    assert_eq!(creates, 1);
    assert_eq!(initial_internet_access, Some(false));
    assert_eq!(initial_deny_out, ["0.0.0.0/0"]);
    assert_eq!(actions, ["prepare"]);
}

#[test]
fn running_runtime_without_a_creation_receipt_is_rejected_after_restart() {
    let mut f = Fixture::new();
    let d = f.desired();
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(1)["status"], "applied");
    f.backend.take();
    let path = f.records.join("cvm-test.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["sandbox_sha256"] = json!("");
    record["sandbox_effective_sha256"] = json!("");
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let actions = f.state.lock().unwrap().network_actions.clone();
    f.backend = Some(brocade_cube::Backend::open(&f.config, &f.records).unwrap());
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "unknown", "{report}");
    assert!(report["error"].as_str().unwrap().contains("沙箱产物回执"));
    let (creates, network_actions) = {
        let state = f.state.lock().unwrap();
        (state.creates, state.network_actions.clone())
    };
    assert_eq!(creates, 1);
    assert_eq!(network_actions, actions);
}

#[test]
fn compiled_products_report_reproducible_bindings_and_effective_digests() {
    let f = Fixture::new();
    let d = f.desired();
    assert!(serde_json::to_value(&d)
        .unwrap()
        .get("artifact_mode")
        .is_none());
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "applied", "{report}");
    let initial_internet_access = f.state.lock().unwrap().initial_internet_access;
    assert_eq!(initial_internet_access, Some(true));
    let r: CvmArtifactReceipt = serde_json::from_value(report["artifacts"].clone()).unwrap();
    assert_eq!(r.files.len(), 4);
    assert!(r.mismatches.is_empty());
    assert!(r.files.iter().all(|f| f.effective_sha256.is_some()));
    assert!(report["artifacts"].get("mode").is_none());
    let sandbox = d
        .artifacts
        .as_ref()
        .unwrap()
        .file(CvmArtifactKind::Sandbox)
        .unwrap();
    assert_eq!(
        f.state.lock().unwrap().boxes[0]["labels"]["brocade.cvm.sandbox_sha256"],
        sandbox.sha256
    );
    let attachments = brocade_cvm_compile::bind(
        d.artifacts.as_ref().unwrap(),
        CvmArtifactKind::Attachments,
        &d,
        &brocade_cvm_compile::LocalBindings {
            runtime_id: Some("cvm-test".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        r.files
            .iter()
            .find(|f| f.kind == CvmArtifactKind::Attachments)
            .unwrap()
            .effective_sha256,
        Some(digest(&attachments).unwrap())
    );
}

#[test]
fn every_tampered_product_is_rejected_before_runtime_or_network_mutation() {
    for index in 0..4 {
        let f = Fixture::new();
        let mut d = f.desired();
        d.artifacts.as_mut().unwrap().files[index].content.push(' ');
        f.backend().submit(&d).unwrap();
        let report = f.report(1);
        assert_eq!(report["status"], "failed", "{report}");
        assert!(!report["artifacts"]["mismatches"]
            .as_array()
            .unwrap()
            .is_empty());
        let state = f.state.lock().unwrap();
        assert_eq!(state.creates, 0);
        assert!(state.network_actions.is_empty());
    }
}

#[test]
fn missing_products_does_not_fall_back_to_spec_execution() {
    let f = Fixture::new();
    let mut d = f.desired();
    d.artifacts = None;
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "failed", "{report}");
    assert!(report["error"]
        .as_str()
        .unwrap()
        .contains("artifacts missing"));
    let state = f.state.lock().unwrap();
    assert_eq!(state.creates, 0);
    assert!(state.network_actions.is_empty());
}

#[test]
fn corrupt_products_cannot_trap_owned_runtime_cleanup() {
    let f = Fixture::new();
    let mut d = f.desired();
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(1)["status"], "applied");
    d.generation = 2;
    d.operation_id = "op-delete".into();
    d.power = PowerState::Deleted;
    d.delete_data = true;
    d.artifacts.as_mut().unwrap().files[0].content = "bad JSON".into();
    f.backend().submit(&d).unwrap();
    let r = f.report(2);
    assert_eq!(r["status"], "applied", "{r}");
    assert!(r["artifacts"].is_null());
    assert!(f.state.lock().unwrap().boxes.is_empty());
}

#[test]
fn changed_local_storage_facts_reject_products_before_execution() {
    let mut f = Fixture::new();
    let d = f.desired();
    f.backend.take();
    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.config).unwrap()).unwrap();
    cfg["storage_backend"] = json!("xfs");
    f.state.lock().unwrap().storage_backend = Some("xfs".into());
    std::fs::write(&f.config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    f.backend = Some(brocade_cube::Backend::open(&f.config, &f.records).unwrap());
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "failed", "{report}");
    assert!(!report["artifacts"]["mismatches"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(f.state.lock().unwrap().creates, 0);
}

#[test]
fn frozen_products_survive_catalogue_replacement_and_retained_disk_restart() {
    let mut f = Fixture::new();
    let mut d = f.desired();
    f.backend().submit(&d).unwrap();
    let first = f.report(1);
    assert_eq!(first["status"], "applied");
    let volume = first["volume_id"].clone();
    f.backend.take();
    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.config).unwrap()).unwrap();
    cfg["capabilities"]["images"][0]["id"] = json!("replacement");
    let path = cfg["image_files"]["debian"].take();
    cfg["image_files"] = json!({"replacement":path});
    std::fs::write(&f.config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    f.backend = Some(brocade_cube::Backend::open(&f.config, &f.records).unwrap());
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(1)["status"], "applied");
    let bundle = d.artifacts.take();
    d.generation = 2;
    d.operation_id = "op-stop".into();
    d.power = PowerState::Stopped;
    f.backend().submit(&d).unwrap();
    let stopped = f.report(2);
    assert_eq!(stopped["status"], "applied", "{stopped}");
    d.artifacts = bundle;
    d.generation = 3;
    d.operation_id = "op-start".into();
    d.power = PowerState::Running;
    f.backend().submit(&d).unwrap();
    let started = f.report(3);
    assert_eq!(started["status"], "applied", "{started}");
    assert_eq!(started["volume_id"], volume);
    assert_eq!(started["artifacts"]["mismatches"], json!([]));
}
