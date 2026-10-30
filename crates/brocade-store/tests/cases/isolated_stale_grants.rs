use super::*;

async fn wait_for_lock(pool: &PgPool, pattern: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity
                  WHERE datname = current_database() AND pid <> pg_backend_pid()
                    AND wait_event_type = 'Lock' AND query LIKE $1)",
            )
            .bind(pattern)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("concurrent isolated-node operation must reach the intended lock wait");
}

async fn setup_isolated_grants(db: &TestPg, key: &str) -> (u64, i64) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let revision = db.store.materialize_snapshot(None).await.unwrap().revision;
    db.store
        .create_deployment(&system_admin(), create_deployment_request(revision, key))
        .await
        .unwrap();
    let claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&claim))
        .await
        .unwrap();
    db.store
        .isolate_node(
            &system_admin(),
            "n1",
            IsolateNodeRequest {
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();
    let topology = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpdateNode {
                node_id: "n1".to_owned(),
                node: UpdateNodeRequest {
                    mtu: Some(1300),
                    ..Default::default()
                },
            }],
            None,
        )
        .await
        .unwrap();
    db.store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "ing-a1b2".to_owned(),
                enabled: false,
                note: Some("revoke before isolated topology changes".to_owned()),
            },
        )
        .await
        .unwrap();
    let grants = db.store.process_grant_automation().await.unwrap();
    assert!(grants.waiting.is_none(), "{:?}", grants.waiting);
    (topology.revision_id, grants.deployment_id.unwrap())
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_config_replaces_grants_for_a_deleted_inbound() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (_, grants_id) = setup_isolated_grants(&db, "isolation-stale-grants").await;
    let removed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteChain {
                app_id: "app-main".to_owned(),
                chain_id: "chn-a1b2-c3d4".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(removed.revision_id, "isolation-remove-inbound"),
        )
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), grants_id, "n1").await,
        "superseded"
    );
    let claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.deployment_id, config.deployment_id);
    assert_eq!(
        claim.desired.grants,
        DesiredGrants::Present { inbounds: vec![] }
    );
    db.store
        .report_target_result(applied_report(&claim))
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM node_convergence_obligations WHERE node_id='n1' AND status IN ('pending','dispatched','converging','failed-recovered','failed-dirty')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(debt, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_config_preserves_a_newer_permission_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (topology_revision, grants_id) = setup_isolated_grants(&db, "isolation-newer-grants").await;
    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(topology_revision, "isolation-older-topology"),
        )
        .await
        .unwrap();
    assert_eq!(target_status(db.pool(), grants_id, "n1").await, "deferred");
    let config_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config_claim.deployment_id, config.deployment_id);
    db.store
        .report_target_result(applied_report(&config_claim))
        .await
        .unwrap();
    let grants_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grants_claim.deployment_id, grants_id);
    assert_no_alice(&grants_claim.desired.grants);
    db.store
        .report_target_result(applied_report(&grants_claim))
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_config_preserves_failure_evidence_and_fences_superseded_grants() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (_, grants_id) = setup_isolated_grants(&db, "isolation-failed-grants").await;
    let grants_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grants_claim.deployment_id, grants_id);
    let mut failed = failed_recovered_report(&grants_claim);
    failed.result = TargetApplyResult::FailedDirty;
    failed.error = Some("inbound no longer exists".to_owned());
    db.store.report_target_result(failed).await.unwrap();
    let changed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteChain {
                app_id: "app-main".to_owned(),
                chain_id: "chn-a1b2-c3d4".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(changed.revision_id, "isolation-after-grants-failure"),
        )
        .await
        .unwrap();
    let previous = sqlx::query("SELECT status, last_error, settled_at IS NULL AS unsettled, superseded_at IS NOT NULL AS superseded FROM node_convergence_obligations WHERE source_deployment_id=$1 AND node_id='n1'")
        .bind(grants_id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        previous.try_get::<String, _>("status").unwrap(),
        "superseded"
    );
    assert_eq!(
        previous.try_get::<String, _>("last_error").unwrap(),
        "inbound no longer exists"
    );
    assert!(previous.try_get::<bool, _>("unsettled").unwrap());
    assert!(previous.try_get::<bool, _>("superseded").unwrap());
    assert!(db
        .store
        .report_target_result(applied_report(&grants_claim))
        .await
        .is_err());
    let claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.deployment_id, config.deployment_id);
    db.store
        .report_target_result(failed_recovered_report(&claim))
        .await
        .unwrap();
    assert!(matches!(
        db.store.restore_node_service(&system_admin(), "n1").await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        target_status(db.pool(), config.deployment_id, "n1").await,
        "failed-recovered"
    );
    db.store
        .retry_target(&system_admin(), config.deployment_id, "n1")
        .await
        .unwrap();
    let retry = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&retry))
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_config_supersession_waits_for_the_old_report_without_deadlock() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (_, grants_id) = setup_isolated_grants(&db, "isolation-report-lock-order").await;
    let claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.deployment_id, grants_id);
    let removed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteChain {
                app_id: "app-main".to_owned(),
                chain_id: "chn-a1b2-c3d4".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("SELECT node_id FROM node_applied_state WHERE node_id='n1' FOR UPDATE")
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    let reporter_store = db.store.clone();
    let reporter = tokio::spawn(async move {
        reporter_store
            .report_target_result(applied_report(&claim))
            .await
    });
    wait_for_lock(db.pool(), "%FROM node_applied_state%FOR UPDATE%").await;
    let creator_store = db.store.clone();
    let creator = tokio::spawn(async move {
        creator_store
            .create_deployment(
                &system_admin(),
                create_deployment_request(removed.revision_id, "isolation-replace-during-report"),
            )
            .await
    });
    wait_for_lock(db.pool(), "%SELECT deployment.id%FOR UPDATE OF deployment%").await;
    gate.commit().await.unwrap();
    let created = tokio::time::timeout(Duration::from_secs(5), async {
        reporter.await.unwrap().unwrap();
        creator.await.unwrap().unwrap()
    })
    .await
    .expect("config supersession and grants report must follow one lock order");
    let config = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config.deployment_id, created.deployment_id);
    db.store
        .report_target_result(applied_report(&config))
        .await
        .unwrap();
}
