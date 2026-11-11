mod common;
use brocade_deployment::cvm::*;
use common::*;

#[test]
fn quota_and_attachment_workers_share_one_network_mutation_queue() {
    let fixture = Fixture::new();
    fixture.state.lock().unwrap().network_delay = true;
    let barrier = std::sync::Barrier::new(9);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let barrier = &barrier;
            let backend = fixture.backend();
            scope.spawn(move || {
                barrier.wait();
                assert!(backend
                    .apply_network_quotas(&NetworkQuotaDesired::default())
                    .unwrap()
                    .is_empty());
            });
        }
        barrier.wait();
        fixture.backend().submit(&fixture.desired()).unwrap();
        assert_eq!(fixture.report(1)["status"], "applied");
    });
    let state = fixture.state.lock().unwrap();
    assert!(
        !state.network_overlap,
        "quota and attachment requests raced at Cubelet"
    );
    assert!(state
        .network_actions
        .iter()
        .any(|action| action == "prepare"));
    assert!(state
        .network_actions
        .iter()
        .any(|action| action == "reconcile"));
    assert_eq!(
        state
            .network_actions
            .iter()
            .filter(|action| action.as_str() == "quota")
            .count(),
        8
    );
}
use std::{
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

#[test]
fn storage_inventory_requires_matching_backend_evidence_for_each_local_backend() {
    let fixture = Fixture::new();
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.config).unwrap()).unwrap();
    for configured in ["plain", "xfs"] {
        config["storage_backend"] = serde_json::json!(configured);
        std::fs::write(&fixture.config, serde_json::to_vec(&config).unwrap()).unwrap();
        for reported in [None, Some(""), Some("s3"), Some("plain"), Some("xfs")] {
            fixture.state.lock().unwrap().storage_backend = reported.map(str::to_owned);
            assert_eq!(
                brocade_cube::Backend::inspect(&fixture.config).is_ok(),
                reported == Some(configured),
                "configured={configured}, reported={reported:?}"
            );
        }
    }
}

#[test]
fn virtualization_switch_requires_exclusive_drained_state_and_no_runtime() {
    let mut fixture = Fixture::new();
    assert!(brocade_cube::Backend::inspect_empty(&fixture.config, &fixture.records).is_err());
    fixture.backend.take();
    brocade_cube::Backend::inspect_empty(&fixture.config, &fixture.records).unwrap();
    let record = fixture.records.join("owned.json");
    std::fs::write(&record, br#"{"instance":"owned","runtime":"","volume":"","launch_sha256":"","boot_sha256":"","spec_sha256":"","contact_id":"","native_shim_version":"","actual_vcpus":0,"actual_memory_bytes":0,"generation":0,"restart_epoch":0,"reinstall_epoch":0,"reinstall_reset_epoch":0,"transition":"","host_boot_id":"","attempted":false,"initialized":false,"deleted":false,"detached_interfaces":[],"objects":[],"last_interfaces":[],"report":null}"#).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(brocade_cube::Backend::inspect_empty(&fixture.config, &fixture.records).is_err());
    std::fs::write(&record, br#"{"instance":"owned","runtime":"","volume":"","launch_sha256":"","boot_sha256":"","spec_sha256":"","contact_id":"","native_shim_version":"","actual_vcpus":0,"actual_memory_bytes":0,"generation":0,"restart_epoch":0,"reinstall_epoch":0,"reinstall_reset_epoch":0,"transition":"","host_boot_id":"","attempted":false,"initialized":false,"deleted":true,"detached_interfaces":[],"objects":[],"last_interfaces":[],"report":null}"#).unwrap();
    brocade_cube::Backend::inspect_empty(&fixture.config, &fixture.records).unwrap();
    fixture
        .state
        .lock()
        .unwrap()
        .boxes
        .push(serde_json::json!({"id":"foreign","labels":{}}));
    assert!(brocade_cube::Backend::inspect_empty(&fixture.config, &fixture.records).is_err());
}

#[test]
fn pvm_configuration_reports_its_backend_and_rejects_unknown_modes() {
    let fixture = Fixture::new();
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&fixture.config).unwrap()).unwrap();
    config["virtualization_mode"] = serde_json::json!("pvm");
    std::fs::write(&fixture.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(
        brocade_cube::Backend::inspect(&fixture.config)
            .unwrap()
            .backend,
        "cube-pvm-v1"
    );
    config["virtualization_mode"] = serde_json::json!("unknown");
    std::fs::write(&fixture.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(brocade_cube::Backend::inspect(&fixture.config).is_err());
}

#[test]
fn direct_rpc_creates_once_and_verifies_frozen_capacity_and_ports() {
    let fixture = Fixture::new();
    let desired = fixture.desired();
    assert!(fixture.backend().inventory().unwrap().guest_terminal);
    assert!(
        fixture
            .backend()
            .inventory()
            .unwrap()
            .guest_terminal_persistent
    );
    assert!(fixture.backend().inventory().unwrap().configure_network);
    assert_eq!(
        fixture.backend().submit(&desired).unwrap().status,
        ApplyStatus::Pending
    );
    let report = fixture.report(1);
    assert_eq!(report["status"], "applied", "{report}");
    assert_eq!(report["power"], "running");
    assert_eq!(fixture.state.lock().unwrap().creates, 1);
    assert_eq!(fixture.guest_calls.load(Ordering::Acquire), 1);
    fixture.backend().submit(&desired).unwrap();
    thread::sleep(Duration::from_millis(50));
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.creates, 1);
    assert_eq!(state.capacity, desired.spec.resources.root_disk_bytes);
    assert_eq!(state.vcpu_request[0]["ports"], serde_json::json!([]));
}

#[test]
fn closed_stdin_does_not_mask_exec_success_but_unconfirmed_delete_retains_cleanup() {
    let fixture = Fixture::new();
    fixture.close_io_failure.store(true, Ordering::Release);
    let mut desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    assert_eq!(fixture.live_execs.load(Ordering::Acquire), 0);
    fixture.delete_exec_failure.store(true, Ordering::Release);
    desired.generation = 2;
    desired.operation_id = "op-stop".into();
    desired.power = PowerState::Stopped;
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(2)["status"], "unknown");
    assert!(fixture.live_execs.load(Ordering::Acquire) > 0);
    assert!(std::fs::read_dir(&fixture.records)
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".guest-io-")));
    fixture.delete_exec_failure.store(false, Ordering::Release);
    fixture.backend().inventory().unwrap();
    assert_eq!(fixture.live_execs.load(Ordering::Acquire), 0);
    fixture.backend().submit(&desired).unwrap();
    fixture
        .backend()
        .wait_idle(Duration::from_secs(10))
        .unwrap();
    assert_eq!(fixture.report(2)["status"], "applied");
}

#[test]
fn lost_create_response_is_reconciled_by_owner_without_second_allocation() {
    let fixture = Fixture::new();
    fixture.state.lock().unwrap().fail_create_reply = true;
    let desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "unknown");
    fixture.backend().submit(&desired).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.report(1)["status"] != "applied" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fixture.state.lock().unwrap().creates, 1);
}

#[test]
fn missing_physical_root_never_allocates_an_empty_replacement() {
    let fixture = Fixture::new();
    let mut desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    {
        let mut state = fixture.state.lock().unwrap();
        state.boxes.clear();
        state.root_exists = false;
    }
    desired.generation = 2;
    desired.operation_id = "op-2".into();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(2)["status"], "unknown");
    assert_eq!(fixture.state.lock().unwrap().creates, 1);
}

#[test]
fn queued_newer_generation_cannot_be_replaced_by_stale_or_changed_identity() {
    let fixture = Fixture::new();
    fixture.state.lock().unwrap().block_create = true;
    let desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.state.lock().unwrap().create_entered {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let mut newer = desired.clone();
    newer.generation = 2;
    newer.operation_id = "op-2".into();
    fixture.backend().submit(&newer).unwrap();
    assert_eq!(
        fixture.backend().submit(&desired).unwrap().status,
        ApplyStatus::Failed
    );
    let mut changed = newer.clone();
    changed.operation_id = "op-other".into();
    assert_eq!(
        fixture.backend().submit(&changed).unwrap().status,
        ApplyStatus::Failed
    );
    fixture.state.lock().unwrap().block_create = false;
    assert_eq!(fixture.report(2)["status"], "applied");
    assert_eq!(fixture.state.lock().unwrap().creates, 1);
}

#[test]
fn shutdown_and_boot_retain_root_and_reinstall_runs_once() {
    let fixture = Fixture::new();
    let mut desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    desired.generation = 2;
    desired.operation_id = "op-2".into();
    desired.power = PowerState::Stopped;
    fixture.backend().submit(&desired).unwrap();
    let stopped = fixture.report(2);
    assert_eq!(stopped["power"], "stopped");
    assert_eq!(stopped["volume_retained"], true);
    assert!(fixture.state.lock().unwrap().root_exists);
    desired.generation = 3;
    desired.operation_id = "op-3".into();
    desired.power = PowerState::Running;
    desired.restart_epoch = 1;
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(3)["status"], "applied");
    assert_eq!(fixture.state.lock().unwrap().creates, 2);
    desired.generation = 4;
    desired.operation_id = "op-4".into();
    desired.reinstall_epoch = 1;
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(4)["status"], "applied");
    fixture.backend().submit(&desired).unwrap();
    thread::sleep(Duration::from_millis(50));
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.creates, 3);
    assert_eq!(state.root_deletes, 1);
}

#[test]
fn stopped_detach_requires_exact_native_cleanup_proof_before_boot() {
    let fixture = Fixture::new();
    let mut desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    desired.generation = 2;
    desired.operation_id = "op-stop".into();
    desired.power = PowerState::Stopped;
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(2)["power"], "stopped");
    desired.generation = 3;
    desired.operation_id = "op-detach".into();
    // This lease may have been reserved while off, without a Guest device.
    desired.detached_interfaces = vec!["nic-stopped".into()];
    fixture.state.lock().unwrap().suppress_detach = true;
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(3)["status"], "unknown");
    fixture.state.lock().unwrap().suppress_detach = false;
    fixture.backend().submit(&desired).unwrap();
    fixture
        .backend()
        .wait_idle(Duration::from_secs(10))
        .unwrap();
    let detached = fixture.report(3);
    assert_eq!(detached["status"], "applied", "{detached}");
    assert_eq!(
        detached["detached_interfaces"],
        serde_json::json!(["nic-stopped"])
    );
    desired.generation = 4;
    desired.operation_id = "op-start".into();
    desired.power = PowerState::Running;
    desired.detached_interfaces.clear();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(4)["status"], "applied");
    assert_eq!(fixture.state.lock().unwrap().root_deletes, 0);
}

#[test]
fn deletion_retains_reservation_until_physical_absence_is_proven() {
    let fixture = Fixture::new();
    let mut desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    fixture.state.lock().unwrap().suppress_delete = true;
    desired.generation = 2;
    desired.operation_id = "op-2".into();
    desired.power = PowerState::Deleted;
    desired.delete_data = true;
    fixture.backend().submit(&desired).unwrap();
    let report = fixture.report(2);
    assert_eq!(report["status"], "unknown");
    assert_eq!(report["volume_deleted"], false);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.records.join("cvm-test.json")).unwrap())
            .unwrap();
    assert_eq!(record["deleted"], false);
}

#[test]
fn one_executor_owns_records_and_readiness_does_not_take_execution_lock() {
    let fixture = Fixture::new();
    assert!(brocade_cube::Backend::open(&fixture.config, &fixture.records).is_err());
    assert!(
        brocade_cube::Backend::inspect(&fixture.config)
            .unwrap()
            .guest_terminal
    );
    assert!(
        brocade_cube::Backend::inspect(&fixture.config)
            .unwrap()
            .stable_storage
    );
}

#[test]
fn inventory_recovers_only_proven_orphan_exec_checkpoints() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    let orphan = fixture.records.join(".guest-io-owned-orphan");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::set_permissions(&orphan, std::fs::Permissions::from_mode(0o700)).unwrap();
    let checkpoint = orphan.join("exec.json");
    std::fs::write(
        &checkpoint,
        serde_json::to_vec(&serde_json::json!({
            "runtime":"cvm-test", "exec_id":"brocade-0123456789abcdef0123456789abcdef"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    fixture.backend().inventory().unwrap();
    assert!(!orphan.exists());
    std::fs::create_dir(&orphan).unwrap();
    std::fs::set_permissions(&orphan, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&checkpoint, br#"{"runtime":"cvm-test","exec_id":""}"#).unwrap();
    std::fs::set_permissions(&checkpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    fixture.backend().inventory().unwrap();
    assert!(
        checkpoint.exists(),
        "unproven init identity must be retained, never killed"
    );
    assert_eq!(fixture.state.lock().unwrap().boxes.len(), 1);
}

#[test]
fn independent_observation_preserves_device_identity_and_cumulative_counters() {
    let fixture = Fixture::new();
    let desired = fixture.desired();
    fixture.backend().submit(&desired).unwrap();
    assert_eq!(fixture.report(1)["status"], "applied");
    let target = ObservationTarget {
        id: desired.id.clone(),
        generation: 1,
        power: PowerState::Running,
        services: vec![],
    };
    fixture
        .backend()
        .observe(
            std::slice::from_ref(&target),
            std::slice::from_ref(&desired.id),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let observations = fixture
            .backend()
            .observe(
                std::slice::from_ref(&target),
                std::slice::from_ref(&desired.id),
            )
            .unwrap();
        if let Some(metrics) = observations.last().and_then(|o| o.metrics.as_ref()) {
            assert_eq!(metrics.cpu_total_ticks, 81);
            assert_eq!(metrics.cpu_busy_ticks, 28);
            assert_eq!(metrics.interfaces[0].ifindex, 2);
            assert_eq!(metrics.interfaces[0].rx_bytes, 100);
            assert_eq!(metrics.interfaces[0].carrier, Some(true));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Guest sampling did not produce a valid observation: {observations:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
