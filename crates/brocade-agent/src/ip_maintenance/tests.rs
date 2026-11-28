use super::*;

#[tokio::test]
async fn normal_shutdown_does_not_publish_worker_recovery_failure() {
    let fixture = Fixture::new();
    fixture.0.synced().unwrap();
    let desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Waiting, None, None)
        .unwrap();
    let (sender, receiver) = watch::channel(Some(desired));
    let task = tokio::spawn(supervise(Arc::clone(&fixture.0), receiver));
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.0.locked().unwrap().current.as_ref().unwrap().phase
            != MaintenancePhase::Disabled
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.0.stop.store(true, Ordering::Release);
    drop(sender);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    let state = fixture.0.locked().unwrap();
    assert_eq!(
        state.current.as_ref().unwrap().phase,
        MaintenancePhase::Disabled
    );
    assert_eq!(state.current.as_ref().unwrap().reason, None);
    assert!(state.terminal.is_empty());
}

#[tokio::test]
async fn recovered_worker_leaves_recovery_state_and_preserves_history_and_usage() {
    let fixture = Fixture::new();
    fixture.0.synced().unwrap();
    let desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    fixture
        .0
        .mutate(|state| {
            state.daily_requests = 17;
            state.daily_bytes = 2345;
            Ok(())
        })
        .unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Failed, Some("worker_recovering"), None)
        .unwrap();
    let failed = fixture.0.locked().unwrap().terminal[0].clone();
    let state = fixture
        .run_until(desired, |state| {
            state
                .current
                .as_ref()
                .is_some_and(|r| r.phase == MaintenancePhase::Disabled && r.round_id.is_none())
        })
        .await;
    assert_eq!(state.daily_requests, 17);
    assert_eq!(state.daily_bytes, 2345);
    assert_eq!(state.terminal.len(), 1);
    assert_eq!(state.terminal[0], failed);
    assert_eq!(state.current.as_ref().unwrap().reason, None);
}

pub(super) struct Fixture(pub(super) Arc<Context>);
impl Fixture {
    pub(super) fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "brocade-maintenance-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(Arc::new(Context {
            directory,
            checkpoint: Arc::new(Mutex::new(Checkpoint::default())),
            last_sync: Arc::new(Mutex::new(None)),
            assets: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            instance: "fixture-instance".into(),
            family: None,
        }))
    }
    fn disk(&self) -> Checkpoint {
        serde_json::from_slice(&fs::read(self.0.directory.join("checkpoint.json")).unwrap())
            .unwrap()
    }
    async fn run_until(
        &self,
        desired: IpMaintenanceDesired,
        ready: impl Fn(&Checkpoint) -> bool,
    ) -> Checkpoint {
        let (_sender, receiver) = watch::channel(Some(desired));
        let task = tokio::spawn(worker::run(Arc::clone(&self.0), receiver));
        let outcome = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let matches = ready(&self.0.locked().unwrap());
                if matches {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        task.abort();
        let _ = task.await;
        outcome.expect("maintenance worker must publish the expected transition");
        self.disk()
    }
}

#[tokio::test]
async fn policy_changes_keep_absolute_start_and_cooldown_across_reload() {
    for minutes in [5, 30, 1440] {
        let fixture = Fixture::new();
        let mut desired = IpMaintenanceDesired {
            generation: 1,
            ..Default::default()
        };
        desired.policy.behavior.timer_jitter_secs = 0;
        desired.policy.behavior.runner_jitter_secs = 0;
        fixture.0.apply(&desired).unwrap();
        fixture
            .0
            .begin_round(&desired, MaintenanceModule::Google, 5)
            .unwrap();
        // Setup can cross a wall-clock second; compare the start that was actually persisted.
        let stamp = fixture.disk().last_started_at;
        fixture
            .0
            .mutate(|state| {
                state.cooldown_until = stamp + 3600;
                Ok(())
            })
            .unwrap();
        fixture
            .0
            .emit(MaintenancePhase::Cooldown, Some("service_restricted"), None)
            .unwrap();
        desired.generation = 2;
        desired.policy.interval_minutes = minutes;
        let reload_started_at = now().unwrap();
        let state = fixture
            .run_until(desired, |state| {
                state.current.as_ref().is_some_and(|r| r.generation == 2)
            })
            .await;
        let reload_finished_at = now().unwrap();
        assert_eq!(state.last_started_at, stamp);
        assert_eq!(state.cooldown_until, stamp + 3600);
        assert!(
            state.next_scheduled_at > reload_started_at
                && state.next_scheduled_at <= reload_finished_at + i64::from(minutes) * 60
        );
        assert_eq!(
            state.current.as_ref().unwrap().phase,
            MaintenancePhase::Cooldown
        );
        *fixture.0.checkpoint.lock().unwrap() = state.clone();
        fixture.0.apply(state.desired.as_ref().unwrap()).unwrap();
        assert_eq!(fixture.disk().cooldown_until, stamp + 3600);
        assert_eq!(fixture.disk().daily_requests, 0);
    }
}

#[tokio::test]
async fn manual_only_cooldown_expires_without_replaying_the_consumed_round() {
    for expired in [false, true] {
        let fixture = Fixture::new();
        let stamp = now().unwrap();
        let mut desired = IpMaintenanceDesired {
            generation: 1,
            manual_sequence: 1,
            manual_expires_at: Some(stamp - 120),
            ..Default::default()
        };
        fixture.0.apply(&desired).unwrap();
        fixture
            .0
            .begin_round(&desired, MaintenanceModule::Trust, 3)
            .unwrap();
        fixture
            .0
            .mutate(|state| {
                state.cooldown_until = if expired { stamp - 1 } else { stamp + 3600 };
                Ok(())
            })
            .unwrap();
        fixture
            .0
            .emit(MaintenancePhase::Cooldown, Some("service_restricted"), None)
            .unwrap();
        desired.report_sequence_floor = 100;
        let state = fixture
            .run_until(desired, |state| {
                state.sequence >= 100
                    && (!expired
                        || state
                            .current
                            .as_ref()
                            .is_some_and(|report| report.phase == MaintenancePhase::Disabled))
            })
            .await;
        assert_eq!(
            state.round.current.as_ref().unwrap().phase,
            if expired {
                MaintenancePhase::Disabled
            } else {
                MaintenancePhase::Cooldown
            }
        );
        assert_eq!(state.manual_sequence, 1);
        assert_eq!(state.daily_requests, 0);
        assert_eq!(state.terminal.len(), 1);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0.directory).unwrap();
    }
}

#[test]
fn over_budget_download_remains_counted_on_disk_and_a_backward_day_cannot_refill_it() {
    let fixture = Fixture::new();
    let policy = IpMaintenancePolicy {
        daily_download_mib: 1,
        ..Default::default()
    };
    fixture.0.reserve_request(&policy).unwrap();
    assert_eq!(
        fixture.0.received(1024 * 1024 + 1, &policy).unwrap_err(),
        "daily_budget"
    );
    assert_eq!(fixture.disk().daily_bytes, 1024 * 1024 + 1);
    assert_eq!(
        fixture.0.reserve_request(&policy).unwrap_err(),
        "daily_budget"
    );
    fixture
        .0
        .mutate(|state| {
            state.day += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fixture.0.reserve_request(&policy).unwrap_err(),
        "daily_budget"
    );
    assert_eq!(fixture.disk().daily_requests, 1);
}

#[test]
fn idle_receipts_preserve_evidence_time_but_do_not_reuse_it_for_a_new_policy() {
    let fixture = Fixture::new();
    let mut desired = IpMaintenanceDesired {
        generation: 1,
        report_sequence_floor: 20,
        ..Default::default()
    };
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .begin_round(&desired, MaintenanceModule::Google, 5)
        .unwrap();
    fixture
        .0
        .mutate(|state| {
            let report = state.current.as_mut().unwrap();
            report.evidence_observed_at_unix_secs = Some(100);
            report.evidence.push(MaintenanceEvidence {
                source: "google_jump".into(),
                country: Some("JP".into()),
                status: Some(302),
            });
            state.last_evidence = Some(report.clone());
            Ok(())
        })
        .unwrap();
    fixture.0.clear_round().unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Waiting, None, None)
        .unwrap();
    let report = fixture.disk().round.current.unwrap();
    assert!(report.sequence > 20);
    assert_eq!(report.evidence_observed_at_unix_secs, Some(100));
    assert_eq!(report.evidence[0].country.as_deref(), Some("JP"));
    desired.generation = 2;
    fixture.0.apply(&desired).unwrap();
    fixture.0.clear_round().unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Disabled, None, None)
        .unwrap();
    assert!(fixture.disk().round.current.unwrap().evidence.is_empty());
}

#[test]
fn failed_checkpoint_write_does_not_publish_an_unpersisted_budget() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.directory.join("checkpoint.json")).unwrap();
    assert!(fixture
        .0
        .reserve_request(&IpMaintenancePolicy::default())
        .is_err());
    assert_eq!(fixture.0.locked().unwrap().daily_requests, 0);
}
#[test]
fn terminal_health_receipts_do_not_fill_the_terminal_queue() {
    let fixture = Fixture::new();
    let desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .begin_round(&desired, MaintenanceModule::Google, 5)
        .unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Interrupted, Some("stop_requested"), None)
        .unwrap();
    for _ in 0..70 {
        fixture
            .0
            .emit(MaintenancePhase::Interrupted, Some("stop_requested"), None)
            .unwrap();
    }
    assert_eq!(fixture.disk().terminal.len(), 1);
    let report = fixture.disk().round.current.unwrap();
    assert!(report.worker_observed_at_unix_secs > 0);
    report.validate().unwrap();
}

#[tokio::test]
async fn session_reset_clears_execution_history_cookies_and_counters_once() {
    let fixture = Fixture::new();
    let old = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    fixture.0.apply(&old).unwrap();
    fixture
        .0
        .begin_round(&old, MaintenanceModule::Google, 5)
        .unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Complete, None, None)
        .unwrap();
    fixture
        .0
        .mutate(|state| {
            state.daily_requests = 17;
            state.daily_bytes = 65536;
            state.round_bytes = 4096;
            state.cooldown_until = now()? + 3600;
            state.last_evidence = state.current.clone();
            Ok(())
        })
        .unwrap();
    let cookies = fixture.0.directory.join("cookies");
    fs::create_dir(&cookies).unwrap();
    fs::write(cookies.join("old.jsonl"), b"old session").unwrap();
    let before = fixture.0.locked().unwrap().sequence;
    assert_eq!(fixture.0.locked().unwrap().terminal.len(), 1);
    let desired = IpMaintenanceDesired {
        generation: 2,
        control_sequence: 2,
        session_generation: 1,
        manual_sequence: 1,
        ..old
    };
    fixture.0.apply(&desired).unwrap();
    let cleared = fixture.disk();
    assert!(cleared.current.is_none());
    assert!(cleared.last_evidence.is_none());
    assert!(cleared.terminal.is_empty());
    assert_eq!(
        (
            cleared.daily_requests,
            cleared.daily_bytes,
            cleared.round_bytes
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (
            cleared.last_started_at,
            cleared.cooldown_until,
            cleared.next_scheduled_at
        ),
        (0, 0, 0)
    );
    assert_eq!(cleared.manual_sequence, 1);
    assert_eq!(cleared.sequence, before);
    assert_eq!(fs::read_dir(&cookies).unwrap().count(), 0);
    fixture.0.synced().unwrap();
    let acknowledged = fixture
        .run_until(desired.clone(), |state| {
            state
                .current
                .as_ref()
                .is_some_and(|r| r.applied_session_generation == 1)
        })
        .await;
    let report = acknowledged.current.as_ref().unwrap();
    assert_eq!(report.phase, MaintenancePhase::Disabled);
    assert!(report.round_id.is_none());
    assert!(report.evidence.is_empty());
    assert_eq!(report.daily_requests, 0);
    assert_eq!(report.daily_response_bytes, 0);
    assert!(report.reason.is_none());
    fixture.0.apply(&desired).unwrap();
    assert_eq!(
        fixture.disk().sequence,
        acknowledged.sequence,
        "an acknowledged reset must not run again"
    );
}

#[tokio::test(start_paused = true)]
async fn restored_policy_connection_clears_the_idle_lease_failure() {
    for enabled in [false, true] {
        let fixture = Fixture::new();
        let desired = IpMaintenanceDesired {
            generation: 1,
            policy: IpMaintenancePolicy {
                enabled,
                ..Default::default()
            },
            ..Default::default()
        };
        fixture.0.apply(&desired).unwrap();
        fixture
            .0
            .emit(MaintenancePhase::Failed, Some("policy_lease_expired"), None)
            .unwrap();
        fixture
            .0
            .mutate(|state| {
                state.next_scheduled_at = now()? + 3600;
                state.daily_requests = 17;
                Ok(())
            })
            .unwrap();
        let (_sender, receiver) = watch::channel(Some(desired));
        let task = tokio::spawn(worker::run(Arc::clone(&fixture.0), receiver));
        tokio::task::yield_now().await;
        assert_eq!(
            fixture.0.locked().unwrap().current.as_ref().unwrap().phase,
            if enabled {
                MaintenancePhase::Failed
            } else {
                MaintenancePhase::Disabled
            }
        );
        fixture.0.synced().unwrap();
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }
        let state = fixture.0.locked().unwrap().clone();
        assert_eq!(
            state.current.as_ref().unwrap().phase,
            if enabled {
                MaintenancePhase::Waiting
            } else {
                MaintenancePhase::Disabled
            }
        );
        assert!(state.current.as_ref().unwrap().reason.is_none());
        assert_eq!(
            state.daily_requests, 17,
            "reconnection is not a session reset"
        );
        assert_eq!(
            state.last_started_at, 0,
            "reconnection must not replay a round"
        );
        task.abort();
        let _ = task.await;
    }
}

#[test]
fn dual_stack_shares_one_durable_budget_and_resets_it_once() {
    let fixture = Fixture::new();
    let v4 = fixture.0.for_family(MaintenanceFamily::V4);
    let v6 = fixture.0.for_family(MaintenanceFamily::V6);
    let mut desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    desired.policy.family = MaintenanceFamily::Dual;
    desired.policy.daily_requests = 10;
    v4.apply(&desired).unwrap();
    let policy = &desired.policy;
    let results = thread::scope(|scope| {
        let a = scope.spawn(|| {
            (0..10)
                .filter(|_| v4.reserve_request(policy).is_ok())
                .count()
        });
        let b = scope.spawn(|| {
            (0..10)
                .filter(|_| v6.reserve_request(policy).is_ok())
                .count()
        });
        a.join().unwrap() + b.join().unwrap()
    });
    assert_eq!(results, 10);
    assert_eq!(fixture.disk().daily_requests, 10);
    v4.begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    v6.begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    assert_ne!(
        v4.locked().unwrap().current.as_ref().unwrap().round_id,
        v6.locked().unwrap().current.as_ref().unwrap().round_id
    );
    let obsolete = desired.clone();
    desired.session_generation = 1;
    desired.generation = 2;
    v4.apply(&desired).unwrap();
    assert_eq!(
        fixture.disk().daily_requests,
        10,
        "reset waits for both old futures to exit"
    );
    v4.emit(MaintenancePhase::Interrupted, Some("policy_changed"), None)
        .unwrap();
    v6.emit(MaintenancePhase::Interrupted, Some("policy_changed"), None)
        .unwrap();
    v4.apply(&desired).unwrap();
    v4.reserve_request(&desired.policy).unwrap();
    v6.apply(&obsolete).unwrap();
    assert_eq!(
        fixture.disk().desired.as_ref().unwrap().session_generation,
        1
    );
    v6.apply(&desired).unwrap();
    assert_eq!(
        fixture.disk().daily_requests,
        1,
        "the second family must not replenish the shared budget"
    );
    assert!(v4.locked().unwrap().current.is_none());
    assert!(v6.locked().unwrap().current.is_none());
}

#[test]
fn dual_stack_scheduling_latches_both_families_in_the_same_slot() {
    let fixture = Fixture::new();
    let v4 = fixture.0.for_family(MaintenanceFamily::V4);
    let v6 = fixture.0.for_family(MaintenanceFamily::V6);
    let mut desired = IpMaintenanceDesired {
        generation: 1,
        ..Default::default()
    };
    desired.policy.family = MaintenanceFamily::Dual;
    desired.policy.enabled = true;
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .mutate(|s| {
            s.next_scheduled_at = now()? - 1;
            Ok(())
        })
        .unwrap();
    v4.schedule_families(&desired, now().unwrap()).unwrap();
    assert!(v4.locked().unwrap().scheduled);
    assert!(v6.locked().unwrap().scheduled);
    v4.begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    v6.schedule_families(&desired, now().unwrap()).unwrap();
    assert!(!v4.locked().unwrap().scheduled);
    assert!(
        v6.locked().unwrap().scheduled,
        "starting IPv4 must not consume IPv6's scheduled run"
    );
    v6.begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    assert!(!v6.locked().unwrap().scheduled);
}

#[test]
fn pending_manual_intent_is_not_acknowledged_as_an_idle_completed_control() {
    let fixture = Fixture::new();
    let desired = IpMaintenanceDesired {
        generation: 1,
        control_sequence: 1,
        manual_sequence: 1,
        manual_expires_at: Some(now().unwrap() + 120),
        ..Default::default()
    };
    fixture.0.apply(&desired).unwrap();
    fixture
        .0
        .emit(MaintenancePhase::Waiting, None, None)
        .unwrap();
    assert_eq!(
        fixture
            .0
            .locked()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .applied_control_sequence,
        0
    );
    fixture
        .0
        .begin_round(&desired, MaintenanceModule::Google, 1)
        .unwrap();
    assert_eq!(
        fixture
            .0
            .locked()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .applied_control_sequence,
        1
    );
}
