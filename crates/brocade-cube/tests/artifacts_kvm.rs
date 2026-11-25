//! Explicit disposable KVM acceptance for the artifact/native-executor boundary.
use brocade_cube::Backend;
use brocade_cvm_compile::compile;
use brocade_deployment::cvm::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn apply(backend: &Backend, desired: &InstanceDesired) -> InstanceReport {
    let until = Instant::now() + Duration::from_secs(240);
    loop {
        let report = backend.submit(desired).unwrap();
        if !matches!(report.status, ApplyStatus::Pending | ApplyStatus::Unknown) {
            return report;
        }
        assert!(
            Instant::now() < until,
            "actual Guest did not converge before deadline: {:?}",
            report.error
        );
        backend.wait_idle(Duration::from_secs(240)).unwrap();
        // A newly booted Guest can be visible before its network confirmation
        // succeeds. Replay the same frozen intent, as the Agent does, until the
        // outcome is proven; a definite failure still returns immediately.
        if report.status == ApplyStatus::Unknown {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
fn wait_guest_ssh(address: &str) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if std::net::TcpStream::connect_timeout(
            &format!("{address}:22").parse().unwrap(),
            Duration::from_secs(1),
        )
        .is_ok()
        {
            return;
        }
        assert!(Instant::now() < until, "Guest SSH did not become ready");
        std::thread::sleep(Duration::from_millis(200));
    }
}
fn guest_ssh(root: &std::path::Path, address: &str, command: &str) -> String {
    let out = std::process::Command::new("timeout")
        .args([
            "20",
            "ssh",
            "-o",
            "BatchMode=no",
            "-o",
            "PreferredAuthentications=password",
            "-o",
            "PubkeyAuthentication=no",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "HostKeyAlgorithms=ssh-ed25519",
            "-o",
            "ConnectTimeout=5",
        ])
        .env("SSH_ASKPASS", root.join("ssh-askpass"))
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", ":0")
        .env("BROCADE_CVM_KVM_PASSWORD_FILE", root.join("guest-password"))
        .stdin(std::process::Stdio::null())
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile={}",
            root.join("known-hosts").display()
        ))
        .arg(format!("cube@{address}"))
        .arg(command)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "Guest SSH did not confirm command: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn host_key(root: &std::path::Path) -> String {
    let text = std::fs::read_to_string(root.join("known-hosts")).unwrap();
    text.lines()
        .rfind(|l| !l.starts_with('#'))
        .unwrap()
        .split_whitespace()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
}
#[test]
#[ignore = "requires explicitly selected disposable KVM and BROCADE_CVM_ARTIFACT_KVM_CONFIG"]
fn cube_artifact_real_guest_lifecycle() {
    let config = PathBuf::from(
        std::env::var("BROCADE_CVM_ARTIFACT_KVM_CONFIG")
            .expect("explicit disposable KVM config required"),
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = Backend::open(&config, root.path()).unwrap();
    let caps = backend.inventory().unwrap();
    assert!(caps.stable_storage && caps.stop && caps.start && caps.delete);
    let image = caps.images[0].clone();
    let manifest = backend
        .image_manifests()
        .into_iter()
        .find(|m| m.sha256 == image.sha256)
        .unwrap();
    let limits = caps.resource_limits.as_ref().unwrap();
    let id = format!(
        "cvm-artifacts-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let memory_mib: u64 = std::env::var("BROCADE_CVM_ARTIFACT_KVM_MEMORY_MIB")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(512);
    let memory = (memory_mib * 1024 * 1024)
        .max(limits.min_memory_bytes)
        .div_ceil(limits.memory_step_bytes)
        * limits.memory_step_bytes;
    let spec = InstanceSpec {
        execution: ExecutionTarget::Cube,
        name: "artifact-kvm-test".into(),
        image: Some(image),
        resources: Resources {
            vcpus: limits.min_vcpus,
            memory_bytes: memory,
            root_disk_bytes: limits.min_root_disk_bytes,
        },

        login_revision: 1,
        legacy_ssh_public_key: None,
        interfaces: vec![InterfaceSpec {
            id: "nic-contact".into(),
            allocated: None,
            connection: InterfaceRequest {
                source_id: caps
                    .network_sources
                    .iter()
                    .find(|s| s.kind == NetworkKind::Public)
                    .unwrap()
                    .id
                    .clone(),
                purpose: InterfacePurpose::Contact,
                default_ipv4: false,
                default_ipv6: false,
            },
        }],
        routing: ContainerRoutes::default(),
    };
    let mut desired:InstanceDesired=serde_json::from_value(json!({"id":id,"operation_id":"op-create","generation":1,"spec_sha256":brocade_core::hash::sha256_hex(&serde_json::to_vec(&spec).unwrap()),"spec":spec,"power":"running","delete_data":false,"restart_epoch":0,"reinstall_epoch":0})).unwrap();
    use base64::Engine;
    let mut entropy = [0; 24];
    getrandom::fill(&mut entropy).unwrap();
    let password = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entropy);
    std::fs::write(root.path().join("guest-password"), &password).unwrap();
    std::fs::set_permissions(
        root.path().join("guest-password"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(
        root.path().join("ssh-askpass"),
        b"#!/bin/sh\nexec cat -- \"$BROCADE_CVM_KVM_PASSWORD_FILE\"\n",
    )
    .unwrap();
    std::fs::set_permissions(
        root.path().join("ssh-askpass"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    backend
        .stage_credential(&GuestCredential {
            instance_id: desired.id.clone(),
            revision: desired.spec.login_revision,
            password,
        })
        .unwrap();
    let freeze = |d: &mut InstanceDesired| {
        d.spec_sha256 = brocade_core::hash::sha256_hex(&serde_json::to_vec(&d.spec).unwrap());
        d.artifacts = Some(
            compile(&CvmArtifactInput {
                instance_id: d.id.clone(),
                spec: d.spec.clone(),
                image_manifest: manifest.clone(),
                capabilities: caps.clone(),
            })
            .unwrap(),
        );
    };
    freeze(&mut desired);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let created = apply(&backend, &desired);
        assert_eq!(created.status, ApplyStatus::Applied, "{:?}", created.error);
        let address = created.access_address.as_ref().unwrap();
        wait_guest_ssh(address);
        assert_eq!(guest_ssh(root.path(), address, "id -un").trim(), "cube");
        let original_dns = guest_ssh(root.path(), address, "cat /etc/resolv.conf");
        guest_ssh(
            root.path(),
            address,
            "printf %s compiler-persisted > /home/cube/artifact-proof",
        );
        let original_host_key = host_key(root.path());
        let receipt = created.artifacts.unwrap();
        assert!(receipt.mismatches.is_empty());
        assert!(receipt.files.iter().all(|f| f.effective_sha256.is_some()));
        let sandbox = desired
            .artifacts
            .as_ref()
            .unwrap()
            .file(CvmArtifactKind::Sandbox)
            .unwrap()
            .sha256
            .clone();
        let volume = created.volume_id.clone();
        println!("PASS: native artifact create and all four executor acknowledgements");
        desired.generation += 1;
        desired.operation_id = "op-network".into();
        desired.spec.routing.revision += 1;
        desired.spec.routing.dns = vec!["1.1.1.1".parse().unwrap()];
        freeze(&mut desired);
        assert_eq!(
            desired
                .artifacts
                .as_ref()
                .unwrap()
                .file(CvmArtifactKind::Sandbox)
                .unwrap()
                .sha256,
            sandbox
        );
        let hot = apply(&backend, &desired);
        assert_eq!(hot.status, ApplyStatus::Applied, "{:?}", hot.error);
        assert_eq!(hot.runtime_id, created.runtime_id);
        assert_eq!(hot.volume_id, volume);
        assert!(guest_ssh(
            root.path(),
            hot.access_address.as_ref().unwrap(),
            "cat /etc/resolv.conf"
        )
        .contains("nameserver 1.1.1.1"));
        println!("PASS: hot DNS attachment update preserves actual runtime and disk");
        desired.generation += 1;
        desired.operation_id = "op-dns-restore".into();
        desired.spec.routing.dns.clear();
        desired.spec.routing.revision += 1;
        freeze(&mut desired);
        let restored = apply(&backend, &desired);
        assert_eq!(
            restored.status,
            ApplyStatus::Applied,
            "{:?}",
            restored.error
        );
        assert_eq!(
            guest_ssh(
                root.path(),
                restored.access_address.as_ref().unwrap(),
                "cat /etc/resolv.conf"
            ),
            original_dns
        );
        println!("PASS: clearing DNS restores actual original Guest resolver configuration");
        desired.generation += 1;
        desired.operation_id = "op-stop".into();
        desired.power = PowerState::Stopped;
        freeze(&mut desired);
        desired
            .artifacts
            .as_mut()
            .unwrap()
            .input
            .image_manifest
            .content = "damaged creation manifest".into();
        let stopped = apply(&backend, &desired);
        assert_eq!(stopped.status, ApplyStatus::Applied, "{:?}", stopped.error);
        assert!(stopped.volume_retained && stopped.network_absent);
        desired.generation += 1;
        desired.operation_id = "op-configure".into();
        if caps.configure
            && limits.max_vcpus > limits.min_vcpus
            && limits.max_memory_bytes >= limits.min_memory_bytes + limits.memory_step_bytes
        {
            desired.spec.resources.vcpus += 1;
            desired.spec.resources.memory_bytes += limits.memory_step_bytes;
            freeze(&mut desired);
            assert_ne!(
                desired
                    .artifacts
                    .as_ref()
                    .unwrap()
                    .file(CvmArtifactKind::Sandbox)
                    .unwrap()
                    .sha256,
                sandbox
            );
            let configured = apply(&backend, &desired);
            assert_eq!(
                configured.status,
                ApplyStatus::Applied,
                "{:?}",
                configured.error
            );
            desired.generation += 1;
        }
        desired.operation_id = "op-start".into();
        desired.power = PowerState::Running;
        freeze(&mut desired);
        let started = apply(&backend, &desired);
        assert_eq!(started.status, ApplyStatus::Applied, "{:?}", started.error);
        assert_eq!(started.volume_id, volume);
        wait_guest_ssh(started.access_address.as_ref().unwrap());
        assert_eq!(
            guest_ssh(
                root.path(),
                started.access_address.as_ref().unwrap(),
                "cat /home/cube/artifact-proof"
            ),
            "compiler-persisted"
        );
        assert_eq!(host_key(root.path()), original_host_key);
        println!(
            "PASS: stop ignores damaged creation manifest; configure/start retains physical disk, Guest data and SSH host identity"
        );
        desired.generation += 1;
        desired.operation_id = "op-corrupt".into();
        desired.artifacts.as_mut().unwrap().files[0]
            .content
            .push(' ');
        let rejected = apply(&backend, &desired);
        assert_eq!(rejected.status, ApplyStatus::Failed);
        println!("PASS: tampered artifacts rejected before privileged running convergence");
    }));
    if outcome.is_err()
        && std::env::var("BROCADE_CVM_ARTIFACT_KVM_KEEP_FAILURE").as_deref() == Ok("1")
    {
        let evidence = root.path().join("failed-desired");
        std::fs::write(&evidence, serde_json::to_vec_pretty(&desired).unwrap()).unwrap();
        std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o600)).unwrap();
        let path = root.keep();
        panic!(
            "KVM failure retained with running instance and private evidence at {}",
            path.display()
        );
    }
    desired.generation += 1;
    desired.operation_id = "op-delete".into();
    desired.power = PowerState::Deleted;
    desired.delete_data = true;
    let removed = apply(&backend, &desired);
    if removed.status != ApplyStatus::Applied || outcome.is_err() {
        let path = root.keep();
        panic!(
            "KVM acceptance/cleanup incomplete: {:?}; evidence retained at {}",
            removed.error,
            path.display()
        );
    }
    assert!(removed.volume_deleted && removed.network_absent);
    println!(
        "PASS: owned runtime/network/physical disk cleanup survives invalid creation products"
    );
}
