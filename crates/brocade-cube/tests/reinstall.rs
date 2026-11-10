mod common;
use brocade_deployment::cvm::*;
use common::*;
use serde_json::Value;
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn checkpoint(fixture: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(fixture.records.join("cvm-test.json")).unwrap()).unwrap()
}

fn apply(fixture: &Fixture, desired: &InstanceDesired) -> Value {
    fixture.backend().submit(desired).unwrap();
    fixture
        .backend()
        .wait_idle(Duration::from_secs(10))
        .unwrap();
    let saved = checkpoint(fixture);
    assert_eq!(saved["generation"], desired.generation);
    saved["report"].clone()
}

fn restart_executor(fixture: &mut Fixture) {
    fixture
        .backend()
        .wait_idle(Duration::from_secs(10))
        .unwrap();
    fixture.backend.take();
    // The idle worker still owns its Arc until its thread returns and releases the lock.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match brocade_cube::Backend::open(&fixture.config, &fixture.records) {
            Ok(backend) => {
                fixture.backend = Some(backend);
                return;
            }
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "executor did not release its lock: {error}"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn stopped_instance() -> (Fixture, InstanceDesired) {
    let fixture = Fixture::new();
    let mut desired = fixture.desired();
    assert_eq!(apply(&fixture, &desired)["status"], "applied");
    desired.generation = 2;
    desired.operation_id = "op-stop".into();
    desired.power = PowerState::Stopped;
    assert_eq!(apply(&fixture, &desired)["power"], "stopped");
    assert_eq!(checkpoint(&fixture)["transition"], "boot");
    (fixture, desired)
}

fn reinstall_stopped_instance(restart: bool) {
    let (mut fixture, mut desired) = stopped_instance();
    if restart {
        restart_executor(&mut fixture);
    }
    desired.generation = 3;
    desired.operation_id = "op-reinstall".into();
    desired.power = PowerState::Running;
    desired.reinstall_epoch = 1;
    let report = apply(&fixture, &desired);
    assert_eq!(report["status"], "applied", "{report}");
    assert_eq!(report["reinstall_epoch"], 1);
    let (deletes, creates, expected_root) = {
        let state = fixture.state.lock().unwrap();
        (
            state.root_deletes,
            state.creates,
            state.vcpu_request.last().unwrap()["annotations"]["brocade.cvm.expected_root"].clone(),
        )
    };
    assert_eq!(deletes, 1, "reinstall must clear the retained disk");
    assert_eq!(creates, 2);
    assert!(
        expected_root.is_null(),
        "reinstall must not request the old disk"
    );
    restart_executor(&mut fixture);
    assert_eq!(apply(&fixture, &desired)["status"], "applied");
    let deletes = fixture.state.lock().unwrap().root_deletes;
    assert_eq!(deletes, 1, "completed reinstall must not erase data again");
    desired.generation = 4;
    desired.operation_id = "op-reinstall-again".into();
    desired.reinstall_epoch = 2;
    assert_eq!(apply(&fixture, &desired)["status"], "applied");
    let deletes = fixture.state.lock().unwrap().root_deletes;
    assert_eq!(deletes, 2, "a new reinstall must clear the disk again");
}

#[test]
fn stopped_reinstall_clears_the_retained_disk() {
    reinstall_stopped_instance(false);
}

#[test]
fn stopped_reinstall_after_executor_restart_clears_the_retained_disk() {
    reinstall_stopped_instance(true);
}

#[test]
fn reinstall_does_not_boot_before_old_disk_deletion_is_confirmed() {
    let (fixture, mut desired) = stopped_instance();
    desired.generation = 3;
    desired.operation_id = "op-reinstall".into();
    desired.power = PowerState::Running;
    desired.reinstall_epoch = 1;
    fixture.state.lock().unwrap().suppress_delete = true;
    assert_eq!(apply(&fixture, &desired)["status"], "unknown");
    let (creates, root_exists) = {
        let state = fixture.state.lock().unwrap();
        (state.creates, state.root_exists)
    };
    assert_eq!(creates, 1);
    assert!(root_exists);
    assert_eq!(checkpoint(&fixture)["transition"], "reset");
    fixture.state.lock().unwrap().suppress_delete = false;
    assert_eq!(apply(&fixture, &desired)["status"], "applied");
}

#[test]
fn reinstall_recovery_never_erases_the_new_disk_after_lost_boot_receipts() {
    {
        let mut fixture = Fixture::new();
        let mut desired = fixture.desired();
        assert_eq!(apply(&fixture, &desired)["status"], "applied");
        desired.generation = 2;
        desired.operation_id = "op-reinstall".into();
        desired.reinstall_epoch = 1;
        fixture.state.lock().unwrap().fail_create_reply = true;
        assert_eq!(apply(&fixture, &desired)["status"], "unknown");
        assert_eq!(checkpoint(&fixture)["transition"], "fresh");
        // Recovery finds the freshly installed runtime stopped, then loses the
        // retained-disk boot response too. The next retry sees "boot", not "fresh".
        {
            let mut state = fixture.state.lock().unwrap();
            state.boxes[0]["containers"][0]["state"] = serde_json::json!(0);
            state.fail_create_reply = true;
        }
        restart_executor(&mut fixture);
        assert_eq!(apply(&fixture, &desired)["status"], "unknown");
        assert_eq!(checkpoint(&fixture)["transition"], "boot");
        restart_executor(&mut fixture);
        let report = apply(&fixture, &desired);
        assert_eq!(report["status"], "applied", "{report}");
        assert_eq!(report["reinstall_epoch"], 1);
        let (deletes, creates) = {
            let state = fixture.state.lock().unwrap();
            (state.root_deletes, state.creates)
        };
        assert_eq!(
            deletes, 1,
            "recovery must preserve the freshly installed disk"
        );
        assert_eq!(creates, 3);
    }
}
