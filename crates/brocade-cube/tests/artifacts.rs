mod common;
use brocade_cvm_compile::{compile, digest};
use brocade_deployment::cvm::*;
use common::*;
use serde_json::json;
fn compiled(f: &Fixture, mode: CvmArtifactMode) -> InstanceDesired {
    let mut d = f.desired();
    let caps = f.backend().inventory().unwrap();
    let manifest = f.backend().image_manifests().into_iter().next().unwrap();
    d.artifacts = Some(
        compile(&CvmArtifactInput {
            instance_id: d.id.clone(),
            spec: d.spec.clone(),
            image_manifest: manifest,
            capabilities: caps,
        })
        .unwrap(),
    );
    d.artifact_mode = mode;
    d
}
#[test]
fn shadow_reports_comparison_without_changing_legacy_labels() {
    let f = Fixture::new();
    let d = compiled(&f, CvmArtifactMode::Shadow);
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "applied");
    assert_eq!(report["artifacts"]["mismatches"], json!([]));
    assert_eq!(report["artifacts"]["mode"], "shadow");
    assert!(f.state.lock().unwrap().boxes[0]["labels"]
        .get("brocade.cvm.sandbox_sha256")
        .is_none());
}
#[test]
fn enforced_native_artifacts_report_reproducible_bindings_and_effective_digests() {
    let f = Fixture::new();
    let d = compiled(&f, CvmArtifactMode::Enforce);
    f.backend().submit(&d).unwrap();
    let report = f.report(1);
    assert_eq!(report["status"], "applied", "{report}");
    let r: CvmArtifactReceipt = serde_json::from_value(report["artifacts"].clone()).unwrap();
    assert_eq!(r.files.len(), 4);
    assert!(r.files.iter().all(|f| f.effective_sha256.is_some()));
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
fn tampered_shadow_is_visible_and_enforce_never_creates_a_runtime() {
    for mode in [CvmArtifactMode::Shadow, CvmArtifactMode::Enforce] {
        let f = Fixture::new();
        let mut d = compiled(&f, mode);
        d.artifacts.as_mut().unwrap().files[0].content.push(' ');
        f.backend().submit(&d).unwrap();
        let report = f.report(1);
        if mode == CvmArtifactMode::Shadow {
            assert_eq!(report["status"], "applied");
            assert_eq!(
                report["artifacts"]["mismatches"].as_array().unwrap().len(),
                4
            );
        } else {
            assert_eq!(report["status"], "failed");
            assert_eq!(f.state.lock().unwrap().creates, 0);
        }
    }
}
#[test]
fn enforce_missing_products_does_not_fall_back_to_spec_execution() {
    let f = Fixture::new();
    let mut d = f.desired();
    d.artifact_mode = CvmArtifactMode::Enforce;
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(1)["status"], "failed");
    assert_eq!(f.state.lock().unwrap().creates, 0);
}
#[test]
fn corrupt_products_cannot_trap_owned_runtime_cleanup() {
    let f = Fixture::new();
    let mut d = compiled(&f, CvmArtifactMode::Enforce);
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
    assert!(f.state.lock().unwrap().boxes.is_empty());
}
#[test]
fn shadow_comparison_outage_preserves_legacy_identity_validation() {
    let mut f = Fixture::new();
    let d = compiled(&f, CvmArtifactMode::Shadow);
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(1)["status"], "applied");
    f.backend.take();
    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.config).unwrap()).unwrap();
    cfg["capabilities"]["images"][0]["id"] = json!("new-image");
    let path = cfg["image_files"]["debian"].take();
    cfg["image_files"] = json!({"new-image":path});
    std::fs::write(&f.config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    f.backend = Some(brocade_cube::Backend::open(&f.config, &f.records).unwrap());
    let mut legacy = d.clone();
    legacy.artifacts = None;
    f.backend().submit(&legacy).unwrap();
    let baseline = f.report(1);
    f.backend().submit(&d).unwrap();
    let r = f.report(1);
    assert_eq!(r["status"], baseline["status"]);
    assert_eq!(r["error"], baseline["error"]);
    assert_eq!(r["artifacts"]["mismatches"].as_array().unwrap().len(), 4);
    assert_eq!(f.state.lock().unwrap().creates, 1);
}
#[test]
fn enforced_frozen_products_survive_catalogue_replacement_and_retained_disk_restart() {
    let mut f = Fixture::new();
    let mut d = compiled(&f, CvmArtifactMode::Enforce);
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
    let manifest = d.artifacts.as_ref().unwrap().input.image_manifest.clone();
    d.artifacts.as_mut().unwrap().input.image_manifest.content = "damaged manifest".into();
    d.generation = 2;
    d.operation_id = "op-stop".into();
    d.power = PowerState::Stopped;
    f.backend().submit(&d).unwrap();
    assert_eq!(f.report(2)["status"], "applied");
    d.artifacts.as_mut().unwrap().input.image_manifest = manifest;
    d.generation = 3;
    d.operation_id = "op-start".into();
    d.power = PowerState::Running;
    f.backend().submit(&d).unwrap();
    let started = f.report(3);
    assert_eq!(started["status"], "applied", "{started}");
    assert_eq!(started["volume_id"], volume);
    assert_eq!(started["artifacts"]["mismatches"], json!([]));
}
