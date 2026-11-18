use super::*;
use brocade_deployment::ip_maintenance::*;

fn receipt(generation: u64, sequence: u64) -> IpMaintenanceReport {
    serde_json::from_value(json!({
        "generation":generation,"run_generation":generation,"sequence":sequence,"agent_instance_id":"fixture-agent",
        "applied_control_sequence":0,"applied_stop_sequence":0,"applied_session_generation":0,
        "worker_observed_at_unix_secs":2000000000,"last_started_at_unix_secs":2000000000,"cooldown_until_unix_secs":null,
        "phase":"dwelling","round_id":"fixture-round","module":"google","actual_exit_ip":"1.1.1.1",
        "actions_total":5,"action_index":1,"phase_deadline_unix_secs":2000000060,"next_run_unix_secs":2000001200,
        "daily_requests":2,"daily_response_bytes":256,"actions":[],"evidence":[],"reason":null,"observed_at_unix_secs":2000000000
    })).unwrap()
}
async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(IP_MAINTENANCE_PROTOCOL_VERSION as i32))
        .await
        .unwrap();
}
fn command(id: &str) -> IpMaintenanceCommand {
    IpMaintenanceCommand {
        expected_generation: 1,
        request_id: id.into(),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_retains_100_rounds_per_machine_across_families() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("INSERT INTO nodes (id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind) VALUES ('n2','platform.acme','Peer','n2.example.net','10.66.0.2','peer-private','peer-public',51820,10085,TRUE,TRUE,'system')")
        .execute(db.pool()).await.unwrap();
    for sequence in 1..=105 {
        let mut report = receipt(0, sequence);
        report.phase = MaintenancePhase::Complete;
        report.round_id = Some(format!("round-{sequence}"));
        report.family = Some(if sequence % 2 == 0 {
            MaintenanceFamily::V4
        } else {
            MaintenanceFamily::V6
        });
        report.actual_exit_ip = None;
        db.store
            .report_ip_maintenance("n1", report.clone())
            .await
            .unwrap();
        if sequence <= 3 {
            db.store.report_ip_maintenance("n2", report).await.unwrap();
        }
    }
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT node_id,count(*) FROM ip_maintenance_runs GROUP BY node_id ORDER BY node_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(counts, vec![("n1".into(), 100), ("n2".into(), 3)]);
    let retained: Vec<String> = sqlx::query_scalar(
        "SELECT round_id FROM ip_maintenance_runs WHERE node_id='n1' ORDER BY id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        retained,
        (6..=105).map(|i| format!("round-{i}")).collect::<Vec<_>>()
    );

    // Simulate an offline machine's history written by the old 2000-round Console.
    sqlx::query("INSERT INTO ip_maintenance_runs(node_id,round_id,report) SELECT 'n2','old-'||i,$1 FROM generate_series(1,102) i")
        .bind(serde_json::to_value(receipt(0, 1)).unwrap()).execute(db.pool()).await.unwrap();
    db.store.run_data_retention().await.unwrap();
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT node_id,count(*) FROM ip_maintenance_runs GROUP BY node_id ORDER BY node_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(counts, vec![("n1".into(), 100), ("n2".into(), 100)]);
    let retained: Vec<String> = sqlx::query_scalar(
        "SELECT round_id FROM ip_maintenance_runs WHERE node_id='n2' ORDER BY id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        retained,
        (3..=102).map(|i| format!("old-{i}")).collect::<Vec<_>>()
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_view_preserves_latest_evidence_per_family_without_rewriting_rounds() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(70))
        .await
        .unwrap();
    let mut policy = IpMaintenancePolicy {
        family: MaintenanceFamily::Dual,
        enabled: true,
        ..Default::default()
    };
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: policy.clone(),
            },
        )
        .await
        .unwrap();
    for (family, country, ip, sequence) in [
        (MaintenanceFamily::V4, "JP", "1.1.1.1", 1),
        (MaintenanceFamily::V6, "US", "2606:4700::1111", 2),
    ] {
        let mut google = receipt(1, sequence);
        google.family = Some(family);
        google.phase = MaintenancePhase::Complete;
        google.actual_exit_ip = Some(ip.parse().unwrap());
        google.round_id = Some(format!("google-{family:?}"));
        google.evidence = vec![MaintenanceEvidence {
            source: "google_jump".into(),
            country: Some(country.into()),
            status: Some(200),
        }];
        google.evidence_observed_at_unix_secs = Some(2_000_000_000 + sequence as i64);
        db.store.report_ip_maintenance("n1", google).await.unwrap();
    }
    for (family, ip, sequence) in [
        (MaintenanceFamily::V4, "1.1.1.1", 3),
        (MaintenanceFamily::V6, "2606:4700::1111", 4),
    ] {
        let mut trust = receipt(1, sequence);
        trust.family = Some(family);
        trust.actual_exit_ip = Some(ip.parse().unwrap());
        trust.phase = MaintenancePhase::Complete;
        trust.module = Some(MaintenanceModule::Trust);
        trust.round_id = Some(format!("trust-{family:?}"));
        db.store.report_ip_maintenance("n1", trust).await.unwrap();
    }
    let view = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    for (report, country, time) in [
        (&view.family_reports[0], "JP", 2_000_000_001),
        (&view.family_reports[1], "US", 2_000_000_002),
    ] {
        assert_eq!(report.module, Some(MaintenanceModule::Trust));
        assert_eq!(report.evidence[0].country.as_deref(), Some(country));
        assert_eq!(report.evidence_observed_at_unix_secs, Some(time));
    }
    assert_eq!(
        view.report.unwrap().evidence[0].country.as_deref(),
        Some("US")
    );
    let raw: serde_json::Value = sqlx::query_scalar(
        "SELECT report FROM ip_maintenance_runs WHERE node_id=$1 AND round_id=$2",
    )
    .bind("n1")
    .bind("trust-V4")
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(raw["evidence"], json!([]));
    assert!(raw["evidence_observed_at_unix_secs"].is_null());

    // A fresh unknown result replaces the old known country; a partial postcheck
    // retains its own family's last complete observation, not the other family's.
    let mut unknown = receipt(1, 5);
    unknown.family = Some(MaintenanceFamily::V4);
    unknown.phase = MaintenancePhase::Complete;
    unknown.round_id = Some("new-unknown".into());
    unknown.evidence = vec![MaintenanceEvidence {
        source: "google_jump".into(),
        country: None,
        status: Some(200),
    }];
    unknown.evidence_observed_at_unix_secs = Some(2_000_000_100);
    db.store.report_ip_maintenance("n1", unknown).await.unwrap();
    let mut partial = receipt(1, 6);
    partial.family = Some(MaintenanceFamily::V6);
    partial.actual_exit_ip = Some("2606:4700::1111".parse().unwrap());
    partial.round_id = Some("partial".into());
    partial.phase = MaintenancePhase::Postcheck;
    partial.evidence = vec![MaintenanceEvidence {
        source: "google_jump".into(),
        country: Some("DE".into()),
        status: Some(200),
    }];
    db.store.report_ip_maintenance("n1", partial).await.unwrap();
    let view = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    assert_eq!(view.family_reports[0].evidence[0].country, None);
    assert_eq!(
        view.family_reports[0].evidence_observed_at_unix_secs,
        Some(2_000_000_100)
    );
    assert_eq!(
        view.family_reports[1].evidence[0].country.as_deref(),
        Some("US")
    );
    policy.region = "DE/Default/Frankfurt".into();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 1,
                policy,
            },
        )
        .await
        .unwrap();
    let view = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    assert!(view
        .family_reports
        .iter()
        .all(|r| r.evidence.is_empty() && r.evidence_observed_at_unix_secs.is_none()));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_readers_follow_machine_scope_and_cannot_write() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: IpMaintenancePolicy::default(),
            },
        )
        .await
        .unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("reader-round"), false)
        .await
        .unwrap();
    let mut report = receipt(1, 1);
    report.applied_control_sequence = 1;
    db.store.report_ip_maintenance("n1", report).await.unwrap();
    for reader in [
        AdminContext::new("public", AdminRole::Readonly, Some("platform".into())),
        AdminContext::new("viewer", AdminRole::Readonly, Some("platform.acme".into())),
    ] {
        assert!(db
            .store
            .ip_maintenance_view(&reader, "n1")
            .await
            .unwrap()
            .report
            .is_some());
        assert_eq!(
            db.store
                .ip_maintenance_runs(&reader, "n1", None)
                .await
                .unwrap()
                .runs
                .len(),
            1
        );
        assert_eq!(
            db.store
                .ip_maintenance_run(&reader, "n1", "fixture-round")
                .await
                .unwrap()
                .round_id
                .as_deref(),
            Some("fixture-round")
        );
        assert!(matches!(
            db.store
                .update_ip_maintenance(
                    &reader,
                    "n1",
                    UpdateIpMaintenance {
                        expected_generation: 1,
                        policy: IpMaintenancePolicy::default(),
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
        for reset in [false, true] {
            assert!(matches!(
                db.store
                    .command_ip_maintenance(&reader, "n1", command("reader-write"), reset)
                    .await,
                Err(StoreError::Forbidden(_))
            ));
        }
        assert!(matches!(
            db.store
                .stop_ip_maintenance(
                    &reader,
                    "n1",
                    IpMaintenanceStop {
                        expected_generation: 1,
                        request_id: "reader-stop".into(),
                        round_id: "fixture-round".into(),
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    let outside = AdminContext::new(
        "outside",
        AdminRole::Readonly,
        Some("platform.other".into()),
    );
    assert!(matches!(
        db.store.ip_maintenance_view(&outside, "n1").await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        db.store.ip_maintenance_runs(&outside, "n1", None).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        db.store
            .ip_maintenance_run(&outside, "n1", "fixture-round")
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .control_sequence,
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_cas_control_idempotency_and_stop_keep_auto_off() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    let readonly = AdminContext::new("viewer", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(db.store.ip_maintenance_view(&readonly, "n1").await.is_ok());
    let initial = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    assert!(initial.supported);
    assert!(!initial.desired.policy.enabled);
    db.store
        .record_node_poll_with_protocol("n1", None, Some(67))
        .await
        .unwrap();
    assert!(
        !db.store
            .ip_maintenance_view(&admin, "n1")
            .await
            .unwrap()
            .supported
    );
    let enabled = UpdateIpMaintenance {
        expected_generation: 0,
        policy: IpMaintenancePolicy {
            enabled: true,
            ..Default::default()
        },
    };
    assert!(matches!(
        db.store.update_ip_maintenance(&admin, "n1", enabled).await,
        Err(StoreError::Unsupported(_))
    ));
    let update = UpdateIpMaintenance {
        expected_generation: 0,
        policy: IpMaintenancePolicy::default(),
    };
    let (left, right) = tokio::join!(
        db.store.update_ip_maintenance(&admin, "n1", update.clone()),
        db.store.update_ip_maintenance(&admin, "n1", update)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    db.store
        .record_node_poll_with_protocol("n1", None, Some(68))
        .await
        .unwrap();
    let revision: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=true")
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("manual-one"), false)
        .await
        .unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("manual-one"), false)
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .command_ip_maintenance(&admin, "n1", command("manual-two"), false)
            .await,
        Err(StoreError::Conflict(_))
    ));
    let desired = db.store.ip_maintenance_desired("n1").await.unwrap();
    assert_eq!(desired.manual_sequence, 1);
    assert_eq!(desired.control_sequence, 1);
    assert!(!desired.policy.enabled);
    let mut running = receipt(1, 1);
    running.applied_control_sequence = 1;
    db.store.report_ip_maintenance("n1", running).await.unwrap();
    let stop = IpMaintenanceStop {
        expected_generation: 1,
        request_id: "stop-one".into(),
        round_id: "fixture-round".into(),
    };
    db.store
        .stop_ip_maintenance(&admin, "n1", stop.clone())
        .await
        .unwrap();
    db.store
        .stop_ip_maintenance(&admin, "n1", stop)
        .await
        .unwrap();
    let desired = db.store.ip_maintenance_desired("n1").await.unwrap();
    assert!(!desired.policy.enabled);
    assert!(desired.manual_expires_at.is_some());
    assert_eq!(desired.stop_sequence, 2);
    assert!(matches!(
        db.store
            .command_ip_maintenance(&admin, "n1", command("stop-one"), true)
            .await,
        Err(StoreError::Conflict(_))
    ));
    let mut stopped = receipt(1, 2);
    stopped.phase = MaintenancePhase::Interrupted;
    stopped.applied_control_sequence = 2;
    stopped.applied_stop_sequence = 2;
    db.store.report_ip_maintenance("n1", stopped).await.unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("manual-two"), false)
        .await
        .unwrap();
    let pending = db.store.ip_maintenance_desired("n1").await.unwrap();
    db.store
        .stop_ip_maintenance(
            &admin,
            "n1",
            IpMaintenanceStop {
                expected_generation: 1,
                request_id: "delayed-old-stop".into(),
                round_id: "fixture-round".into(),
            },
        )
        .await
        .unwrap();
    let after_stop = db.store.ip_maintenance_desired("n1").await.unwrap();
    assert_eq!(after_stop.manual_sequence, pending.manual_sequence);
    assert_eq!(after_stop.manual_request_id.as_deref(), Some("manual-two"));
    assert_eq!(
        after_stop.manual_expires_at, pending.manual_expires_at,
        "a delayed stop for an old round must preserve newer manual work"
    );
    let after: i64 = sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=true")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(revision, after);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_stale_round_stop_cannot_replace_a_pending_current_stop() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: IpMaintenancePolicy::default(),
            },
        )
        .await
        .unwrap();
    let stop = |request: &str, round: &str| IpMaintenanceStop {
        expected_generation: 1,
        request_id: request.into(),
        round_id: round.into(),
    };
    assert!(matches!(
        db.store
            .stop_ip_maintenance(&admin, "n1", stop("unknown-stop", "round-a"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let mut report = receipt(1, 1);
    report.round_id = Some("round-a".into());
    db.store
        .report_ip_maintenance("n1", report.clone())
        .await
        .unwrap();
    let old_stop = stop("stop-a", "round-a");
    db.store
        .stop_ip_maintenance(&admin, "n1", old_stop.clone())
        .await
        .unwrap();
    report.round_id = Some("round-b".into());
    report.sequence = 2;
    report.applied_control_sequence = 1;
    report.applied_stop_sequence = 1;
    db.store.report_ip_maintenance("n1", report).await.unwrap();
    let current_stop = stop("stop-b", "round-b");
    db.store
        .stop_ip_maintenance(&admin, "n1", current_stop.clone())
        .await
        .unwrap();
    let pending = db.store.ip_maintenance_desired("n1").await.unwrap();
    assert_eq!(pending.target_round_id.as_deref(), Some("round-b"));
    assert!(matches!(
        db.store
            .stop_ip_maintenance(&admin, "n1", stop("late-stop-a", "round-a"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    // Retrying an already-accepted request is still successful, even after its
    // round has changed. Neither retry is allowed to mutate the current intent.
    db.store
        .stop_ip_maintenance(&admin, "n1", old_stop)
        .await
        .unwrap();
    db.store
        .stop_ip_maintenance(&admin, "n1", current_stop)
        .await
        .unwrap();
    assert_eq!(
        db.store.ip_maintenance_desired("n1").await.unwrap(),
        pending
    );
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_receipts_reject_future_and_stale_control_and_history_is_lazy() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: IpMaintenancePolicy::default(),
            },
        )
        .await
        .unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("manual-one"), false)
        .await
        .unwrap();
    let mut report = receipt(1, 2);
    report.applied_control_sequence = 1;
    db.store
        .report_ip_maintenance("n1", report.clone())
        .await
        .unwrap();
    db.store
        .report_ip_maintenance("n1", report.clone())
        .await
        .unwrap();
    let mut stale = receipt(1, 3);
    stale.applied_control_sequence = 0;
    db.store.report_ip_maintenance("n1", stale).await.unwrap();
    assert_eq!(
        db.store
            .ip_maintenance_view(&admin, "n1")
            .await
            .unwrap()
            .report
            .unwrap()
            .sequence,
        2
    );
    assert!(db
        .store
        .report_ip_maintenance("n1", receipt(2, 4))
        .await
        .is_err());
    report.phase = MaintenancePhase::Interrupted;
    report.sequence = 5;
    report.reason = Some("stop_requested".into());
    db.store
        .report_ip_maintenance("n1", report.clone())
        .await
        .unwrap();
    report.sequence = 6;
    db.store.report_ip_maintenance("n1", report).await.unwrap();
    assert_eq!(
        db.store
            .ip_maintenance_run(&admin, "n1", "fixture-round")
            .await
            .unwrap()
            .sequence,
        5,
        "health receipts must not rewrite terminal history"
    );
    let view =
        serde_json::to_value(db.store.ip_maintenance_view(&admin, "n1").await.unwrap()).unwrap();
    assert!(view.get("runs").is_none());
    for seq in 7..30 {
        let mut r = receipt(1, seq);
        r.round_id = Some(format!("run-{seq}"));
        r.applied_control_sequence = 1;
        db.store.report_ip_maintenance("n1", r).await.unwrap();
    }
    let page = db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap();
    assert_eq!(page.runs.len(), 20);
    let second = db
        .store
        .ip_maintenance_runs(&admin, "n1", page.next_cursor)
        .await
        .unwrap();
    assert_eq!(second.runs.len(), 4);
    assert!(second.next_cursor.is_none());
    let first = page
        .runs
        .iter()
        .map(|r| r.round_id.clone())
        .collect::<Vec<_>>();
    assert!(second.runs.iter().all(|r| !first.contains(&r.round_id)));
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_run_pages_keep_round_actions_and_evidence() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: IpMaintenancePolicy::default(),
            },
        )
        .await
        .unwrap();
    let mut round = receipt(1, 1);
    round.phase = MaintenancePhase::Complete;
    round.round_id = Some("charted-round".into());
    round.actions_total = 2;
    round.action_index = 2;
    round.actions = vec![
        MaintenanceAction {
            index: 1,
            kind: "google_search".into(),
            host: "www.google.com".into(),
            status: Some(200),
            error: None,
            response_bytes: 4096,
        },
        MaintenanceAction {
            index: 2,
            kind: "google_news".into(),
            host: "news.google.com".into(),
            status: None,
            error: Some("network_timeout".into()),
            response_bytes: 0,
        },
    ];
    round.evidence = vec![MaintenanceEvidence {
        source: "youtube_premium".into(),
        country: Some("JP".into()),
        status: Some(200),
    }];
    round.evidence_observed_at_unix_secs = Some(2_000_000_010);
    db.store
        .report_ip_maintenance("n1", round.clone())
        .await
        .unwrap();
    let page = db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap();
    let detail = db
        .store
        .ip_maintenance_run(&admin, "n1", "charted-round")
        .await
        .unwrap();
    assert_eq!(detail.actions, round.actions);
    assert_eq!(detail.evidence, round.evidence);
    assert_eq!(page.runs, vec![detail]);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_data_atomic_publication_and_offline_retention() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: IpMaintenancePolicy::default(),
            },
        )
        .await
        .unwrap();
    let seed = db.store.ip_maintenance_data("n1").await.unwrap();
    let mut region = seed.region.clone();
    region.keywords = vec!["fixture news".into()];
    let key = format!("region:{}", region.id);
    let good = (key.clone(), serde_json::to_value(&region).unwrap());
    let first = db
        .store
        .claim_ip_maintenance_refresh()
        .await
        .unwrap()
        .unwrap();
    assert!(db
        .store
        .publish_ip_maintenance_assets(
            vec![good.clone(), ("agents".into(), json!(["bad pool"]))],
            data::UPSTREAM_COMMIT,
            first.0
        )
        .await
        .is_err());
    assert_eq!(
        db.store.ip_maintenance_data("n1").await.unwrap().digest,
        seed.digest
    );
    db.store
        .publish_ip_maintenance_assets(
            vec![
                good,
                (
                    "agents".into(),
                    serde_json::to_value(&seed.user_agents).unwrap(),
                ),
            ],
            data::UPSTREAM_COMMIT,
            first.0,
        )
        .await
        .unwrap();
    let published = db.store.ip_maintenance_data("n1").await.unwrap();
    published.validate().unwrap();
    assert_ne!(published.digest, seed.digest);
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .data_digest
            .as_deref(),
        Some(published.digest.as_str())
    );
    assert!(db
        .store
        .claim_ip_maintenance_refresh()
        .await
        .unwrap()
        .is_none());
    db.store
        .finish_ip_maintenance_refresh(first.0, false)
        .await
        .unwrap();
    assert!(
        db.store
            .publish_ip_maintenance_assets(
                vec![(
                    "agents".into(),
                    serde_json::to_value(&seed.user_agents).unwrap()
                )],
                data::UPSTREAM_COMMIT,
                first.0
            )
            .await
            .is_err(),
        "an expired refresh lease cannot publish late data"
    );
    let mut completed = receipt(1, 1);
    completed.phase = MaintenancePhase::Complete;
    db.store
        .report_ip_maintenance("n1", completed.clone())
        .await
        .unwrap();
    sqlx::query("UPDATE ip_maintenance_runs SET received_at=now()-interval '31 days'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.run_data_retention().await.unwrap();
    completed.sequence = 2;
    db.store
        .report_ip_maintenance("n1", completed)
        .await
        .unwrap();
    assert!(db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap()
        .runs
        .is_empty());
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .generation,
        1
    );
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_upgrade_from_0006_preserves_existing_receipts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "brocade-ip-maintenance-migrations-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let file = entry.unwrap();
        if file.file_name().to_string_lossy().as_ref() >= "0007_" {
            continue;
        }
        std::fs::copy(file.path(), dir.join(file.file_name())).unwrap();
    }
    sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .unwrap()
        .run(db.pool())
        .await
        .unwrap();
    let before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(before.len(), 6);
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.pending_versions, vec![7, 8]);
    // Both operational features share the same unreleased migration. A failure after the Clash
    // table was created must roll back that table and every maintenance table together.
    sqlx::query("CREATE TABLE ip_maintenance_state(conflict TEXT)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.migrate_database_schema().await.is_err());
    let absent: bool = sqlx::query_scalar(
        "SELECT to_regclass('public.user_clash_rules') IS NULL AND to_regclass('public.ip_maintenance_policies') IS NULL",
    ).fetch_one(db.pool()).await.unwrap();
    assert!(
        absent,
        "a failed combined 0007 must not leave either feature partially installed"
    );
    let failed: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(before, failed);
    sqlx::query("DROP TABLE ip_maintenance_state")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate_database_schema().await.unwrap();
    let present: bool = sqlx::query_scalar(
        "SELECT to_regclass('public.user_clash_rules') IS NOT NULL AND to_regclass('public.ip_maintenance_policies') IS NOT NULL",
    ).fetch_one(db.pool()).await.unwrap();
    assert!(present);
    let after: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT version,checksum FROM _sqlx_migrations WHERE version<=6 ORDER BY version",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
    std::fs::remove_dir_all(dir).unwrap();
}

async fn install_clash_only_0007(db: &TestPg) {
    let dir = std::env::temp_dir().join(format!(
        "brocade-clash-only-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let file = entry.unwrap();
        if file.file_name().to_string_lossy().as_ref() < "0007_" {
            std::fs::copy(file.path(), dir.join(file.file_name())).unwrap();
        }
    }
    std::fs::write(
        dir.join("0007_user_clash_rules.sql"),
        include_str!("../fixtures/migrations/deployed_0007_user_clash_rules.sql"),
    )
    .unwrap();
    sqlx::migrate::Migrator::new(dir.as_path())
        .await
        .unwrap()
        .run(db.pool())
        .await
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO user_clash_rules(tenant_id,user_id,version,rules) SELECT tenant_id,id,17,'{\"kept\":true}'::jsonb FROM users")
        .execute(db.pool()).await.unwrap();
}

async fn compat_snapshot(db: &TestPg) -> (String, String) {
    let receipts: String = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(m) ORDER BY version)::text FROM _sqlx_migrations m",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let rows: String = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY tenant_id,user_id),'[]'::jsonb)::text FROM user_clash_rules c").fetch_one(db.pool()).await.unwrap();
    (receipts, rows)
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_compat_exact_old_0007_is_read_only_atomic_and_idempotent() {
    for tag in ["16-alpine", "18"] {
        let Some(db) = TestPg::start_if_enabled_with_tag(tag).await else {
            return;
        };
        install_clash_only_0007(&db).await;
        let before = compat_snapshot(&db).await;
        assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
        let check = db.store.check_ip_maintenance_upgrade().await.unwrap();
        assert!(check.compatible, "{:?}", check);
        assert_eq!(check.pending_versions, vec![7, 8]);
        assert_eq!(
            before,
            compat_snapshot(&db).await,
            "preflight must be read-only"
        );
        let absent: bool =
            sqlx::query_scalar("SELECT to_regclass('ip_maintenance_policies') IS NULL")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(absent);
        // A real database DDL failure late in conversion must roll back earlier created tables.
        sqlx::raw_sql("CREATE FUNCTION reject_ip_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN IF to_regclass('public.ip_maintenance_commands') IS NOT NULL THEN RAISE EXCEPTION 'injected late DDL failure'; END IF; END $$; CREATE EVENT TRIGGER reject_ip_ddl ON ddl_command_end EXECUTE FUNCTION reject_ip_ddl();")
        .execute(db.pool()).await.unwrap();
        assert!(db.store.migrate_ip_maintenance_schema().await.is_err());
        assert_eq!(before, compat_snapshot(&db).await);
        let absent: bool=sqlx::query_scalar("SELECT to_regclass('ip_maintenance_policies') IS NULL AND to_regclass('ip_maintenance_refresh') IS NULL").fetch_one(db.pool()).await.unwrap();
        assert!(absent);
        sqlx::raw_sql("DROP EVENT TRIGGER reject_ip_ddl; DROP FUNCTION reject_ip_ddl();")
            .execute(db.pool())
            .await
            .unwrap();
        db.store.migrate_ip_maintenance_schema().await.unwrap();
        let after = compat_snapshot(&db).await;
        assert_eq!(before.1, after.1);
        let old: Vec<serde_json::Value> = serde_json::from_str(&before.0).unwrap();
        let new: Vec<serde_json::Value> = serde_json::from_str(&after.0).unwrap();
        for (i, (a, b)) in old.iter().zip(&new).enumerate() {
            if i < 6 {
                assert_eq!(a, b);
            } else {
                let mut a = a.clone();
                let mut b = b.clone();
                a.as_object_mut().unwrap().remove("checksum");
                b.as_object_mut().unwrap().remove("checksum");
                assert_eq!(a, b);
            }
        }
        assert!(db.store.check_database_upgrade().await.unwrap().compatible);
        assert!(db
            .store
            .check_ip_maintenance_upgrade()
            .await
            .unwrap()
            .pending_versions
            .is_empty());
        db.store.migrate_ip_maintenance_schema().await.unwrap();
        assert_eq!(after, compat_snapshot(&db).await);
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_compat_rejects_unknown_history_and_schema_without_writes() {
    for tag in ["16-alpine", "18"] {
        let Some(db) = TestPg::start_if_enabled_with_tag(tag).await else {
            return;
        };
        install_clash_only_0007(&db).await;
        let checksum: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=7")
                .fetch_one(db.pool())
                .await
                .unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET checksum=decode('00','hex') WHERE version=7")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            !db.store
                .check_ip_maintenance_upgrade()
                .await
                .unwrap()
                .compatible
        );
        let before = compat_snapshot(&db).await;
        assert!(db.store.migrate_ip_maintenance_schema().await.is_err());
        assert_eq!(before, compat_snapshot(&db).await);
        sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=7")
            .bind(checksum)
            .execute(db.pool())
            .await
            .unwrap();
        for ddl in [
            "ALTER TABLE user_clash_rules ADD COLUMN unexpected TEXT",
            "CREATE TABLE ip_maintenance_state(unexpected TEXT)",
        ] {
            sqlx::query(ddl).execute(db.pool()).await.unwrap();
            assert!(
                !db.store
                    .check_ip_maintenance_upgrade()
                    .await
                    .unwrap()
                    .compatible
            );
            let before = compat_snapshot(&db).await;
            assert!(db.store.migrate_ip_maintenance_schema().await.is_err());
            assert_eq!(before, compat_snapshot(&db).await);
            sqlx::raw_sql("DROP TABLE IF EXISTS ip_maintenance_state; ALTER TABLE user_clash_rules DROP COLUMN IF EXISTS unexpected").execute(db.pool()).await.unwrap();
        }
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_reset_clears_history_and_fences_buffered_old_sessions() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    let policy = IpMaintenancePolicy {
        enabled: true,
        interval_minutes: 30,
        daily_requests: 300,
        ..Default::default()
    };
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy: policy.clone(),
            },
        )
        .await
        .unwrap();
    let mut old = receipt(1, 1);
    old.phase = MaintenancePhase::Complete;
    db.store
        .report_ip_maintenance("n1", old.clone())
        .await
        .unwrap();
    assert_eq!(
        db.store
            .ip_maintenance_runs(&admin, "n1", None)
            .await
            .unwrap()
            .runs
            .len(),
        1
    );
    assert!(matches!(
        db.store
            .command_ip_maintenance(&admin, "n1", command("reset-one"), true)
            .await,
        Err(StoreError::Unsupported(_))
    ));
    db.store
        .record_node_poll_with_protocol(
            "n1",
            None,
            Some(IP_MAINTENANCE_RESET_PROTOCOL_VERSION as i32),
        )
        .await
        .unwrap();
    assert!(
        db.store
            .ip_maintenance_view(&admin, "n1")
            .await
            .unwrap()
            .session_reset_supported
    );
    db.store
        .command_ip_maintenance(&admin, "n1", command("reset-one"), true)
        .await
        .unwrap();
    let desired = db.store.ip_maintenance_desired("n1").await.unwrap();
    assert_eq!(desired.generation, 2);
    assert_eq!(desired.control_sequence, 1);
    assert_eq!(desired.session_generation, 1);
    let mut retained = policy.clone();
    retained.enabled = false;
    assert_eq!(desired.policy, retained);
    assert!(!desired.policy.enabled);
    assert!(desired.manual_expires_at.is_none());
    assert!(db
        .store
        .ip_maintenance_view(&admin, "n1")
        .await
        .unwrap()
        .report
        .is_none());
    assert!(db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap()
        .runs
        .is_empty());
    db.store
        .command_ip_maintenance(&admin, "n1", command("reset-one"), true)
        .await
        .unwrap();
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .session_generation,
        1
    );
    assert!(matches!(
        db.store
            .command_ip_maintenance(
                &admin,
                "n1",
                IpMaintenanceCommand {
                    expected_generation: 2,
                    request_id: "too-early-run".into()
                },
                false
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    old.sequence = 999;
    db.store
        .report_ip_maintenance("n1", old.clone())
        .await
        .unwrap();
    assert!(db
        .store
        .ip_maintenance_view(&admin, "n1")
        .await
        .unwrap()
        .report
        .is_none());
    assert!(db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap()
        .runs
        .is_empty());
    let mut fresh = receipt(2, 1000);
    fresh.phase = MaintenancePhase::Disabled;
    fresh.round_id = None;
    fresh.module = None;
    fresh.actions_total = 0;
    fresh.action_index = 0;
    fresh.applied_control_sequence = 1;
    fresh.applied_session_generation = 1;
    fresh.daily_requests = 0;
    fresh.daily_response_bytes = 0;
    db.store.report_ip_maintenance("n1", fresh).await.unwrap();
    old.sequence = 1001;
    db.store.report_ip_maintenance("n1", old).await.unwrap();
    let view = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    let report = view.report.unwrap();
    assert_eq!(report.sequence, 1000);
    assert_eq!(report.daily_requests, 0);
    assert!(report.round_id.is_none());
    assert!(db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap()
        .runs
        .is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_dual_stack_gates_legacy_agents_and_keeps_both_rounds() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let admin = system_admin();
    let mut policy = IpMaintenancePolicy {
        family: MaintenanceFamily::Dual,
        ..Default::default()
    };
    assert!(matches!(
        db.store
            .update_ip_maintenance(
                &admin,
                "n1",
                UpdateIpMaintenance {
                    expected_generation: 0,
                    policy: policy.clone()
                }
            )
            .await,
        Err(StoreError::Unsupported(_))
    ));
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .generation,
        0
    );
    db.store
        .record_node_poll_with_protocol("n1", None, Some(70))
        .await
        .unwrap();
    policy.enabled = true;
    db.store
        .update_ip_maintenance(
            &admin,
            "n1",
            UpdateIpMaintenance {
                expected_generation: 0,
                policy,
            },
        )
        .await
        .unwrap();
    let mut a = receipt(1, 1);
    a.family = Some(MaintenanceFamily::V4);
    a.round_id = Some("v4-round".into());
    a.evidence = vec![MaintenanceEvidence {
        source: "google_jump".into(),
        country: Some("JP".into()),
        status: Some(200),
    }];
    let mut b = receipt(1, 2);
    b.family = Some(MaintenanceFamily::V6);
    b.round_id = Some("v6-round".into());
    b.actual_exit_ip = Some("2606:4700::1111".parse().unwrap());
    b.evidence = vec![MaintenanceEvidence {
        source: "google_jump".into(),
        country: Some("US".into()),
        status: Some(200),
    }];
    db.store
        .report_ip_maintenance("n1", a.clone())
        .await
        .unwrap();
    db.store
        .report_ip_maintenance("n1", b.clone())
        .await
        .unwrap();
    let view = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    assert!(view.dual_stack_supported);
    assert_eq!(view.family_reports.len(), 2);
    assert_eq!(
        view.family_reports[0].evidence[0].country.as_deref(),
        Some("JP")
    );
    assert_eq!(
        view.family_reports[1].evidence[0].country.as_deref(),
        Some("US")
    );
    assert_eq!(
        db.store
            .ip_maintenance_runs(&admin, "n1", None)
            .await
            .unwrap()
            .runs
            .len(),
        2
    );
    db.store
        .stop_ip_maintenance(
            &admin,
            "n1",
            IpMaintenanceStop {
                expected_generation: 1,
                request_id: "dual-stop-v4".into(),
                round_id: "v4-round".into(),
            },
        )
        .await
        .unwrap();
    a.sequence = 3;
    a.phase = MaintenancePhase::Interrupted;
    a.applied_control_sequence = 1;
    a.applied_stop_sequence = 1;
    db.store
        .report_ip_maintenance("n1", a.clone())
        .await
        .unwrap();
    assert!(
        matches!(
            db.store
                .command_ip_maintenance(&admin, "n1", command("dual-reset"), true)
                .await,
            Err(StoreError::Conflict(_))
        ),
        "IPv6 is still running even though the latest IPv4 report is terminal"
    );
    b.sequence = 4;
    b.phase = MaintenancePhase::Complete;
    b.applied_control_sequence = 1;
    b.applied_stop_sequence = 1;
    db.store.report_ip_maintenance("n1", b).await.unwrap();
    db.store
        .command_ip_maintenance(&admin, "n1", command("dual-reset"), true)
        .await
        .unwrap();
    a.sequence = 5;
    db.store.report_ip_maintenance("n1", a).await.unwrap();
    let reset = db.store.ip_maintenance_view(&admin, "n1").await.unwrap();
    assert!(reset.report.is_none());
    assert!(reset.family_reports.is_empty());
    assert!(db
        .store
        .ip_maintenance_runs(&admin, "n1", None)
        .await
        .unwrap()
        .runs
        .is_empty());
}
