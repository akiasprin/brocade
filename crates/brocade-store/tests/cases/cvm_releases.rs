use super::*;

async fn revision(db: &TestPg) -> u64 {
    sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
        .fetch_one(db.pool())
        .await
        .unwrap() as u64
}
async fn approve(db: &TestPg, key: &str) -> NodeDesiredDeployment {
    let r = revision(db).await;
    db.store
        .create_deployment(&system_admin(), create_deployment_request(r, key))
        .await
        .unwrap();
    db.store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_token_resign_recovers_machine_component_while_resource_receipt_is_pending() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    db.store.issue_node_token("n1").await.unwrap();
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("mixed-resign-create-01"),
        )
        .await
        .unwrap();
    let claimed = approve(&db, "mixed-resign-order-01").await;
    db.store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "converging"
    );
    db.store.reissue_node_token("n1").await.unwrap();
    let repair = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("recover proven ordinary configuration before the CVM receipt completes");
    assert_eq!(repair.desired, claimed.desired);
    db.store
        .report_target_result(applied_report(&repair))
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].generation,
        item.desired.generation
    );
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "converging"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_global_revision_is_mixed_and_approval_fences_every_desired_endpoint() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let original = revision(&db).await;
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("mixed-release-create-01"),
        )
        .await
        .unwrap();
    let r = revision(&db).await;
    assert!(r > original);
    assert_eq!(item.revision_id, Some(r));
    assert!(db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .is_empty());
    assert!(
        !db.store
            .cvm_record_report("n1", evidence(&item))
            .await
            .unwrap(),
        "future receipt is not approved"
    );
    let plan = db.store.plan_deployment(&system_admin(), r).await.unwrap();
    let host = plan.targets.iter().find(|t| t.node_id == "n1").unwrap();
    assert!(host
        .actions
        .contains(&brocade_deployment::plan::PlannedAction::SyncCvm));
    let preview = db
        .store
        .plan_deployment_artifacts(&system_admin(), r, false)
        .await
        .unwrap();
    let host = preview.iter().find(|t| t.node_id == "n1").unwrap();
    assert_eq!(host.desired_structure["cvm"]["revision_id"], r);
    assert_eq!(
        host.desired_structure["cvm"]["instances"][0]["id"],
        item.desired.id
    );
    assert!(
        db.store
            .cvm_desired("n1")
            .await
            .unwrap()
            .instances
            .is_empty(),
        "preview has no effect"
    );
    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(r, "mixed-release-order-01"),
        )
        .await
        .unwrap();
    assert_eq!(created.plan.revision, r);
    let replay = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("mixed-release-create-01"),
        )
        .await
        .unwrap();
    assert_eq!(
        replay.desired.operation_id, item.desired.operation_id,
        "idempotent replay remains available during publication"
    );
    let blocked = db
        .store
        .command_cvm(
            &actor_user("alice"),
            &item.desired.id,
            CvmCommand {
                request_id: "mixed-blocked-delete-01".into(),
                expected_generation: 1,
                change: CvmChange::Delete { delete_data: false },
            },
        )
        .await;
    assert!(
        matches!(blocked, Err(StoreError::Conflict(_))),
        "an executing resource cannot be overwritten by a future draft"
    );
    assert_eq!(revision(&db).await, r);
    assert_eq!(
        db.store
            .cvm_instance(&actor_user("alice"), &item.desired.id)
            .await
            .unwrap()
            .desired
            .generation,
        1,
        "blocked staging rolls back immutable versions and reservations too"
    );

    assert!(
        db.store
            .cvm_desired("n1")
            .await
            .unwrap()
            .instances
            .is_empty(),
        "creation is not an executable claim"
    );
    assert!(db
        .store
        .claim_desired_for_node_with_protocol("n1", 64)
        .await
        .unwrap()
        .is_none());
    let claimed = db
        .store
        .claim_desired_for_node_with_protocol("n1", 65)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !claimed
            .actions
            .contains(&brocade_deployment::plan::PlannedAction::SyncCvm),
        "controller barrier is not an unknown Agent command"
    );
    assert!(
        db.store
            .cvm_desired("n1")
            .await
            .unwrap()
            .instances
            .is_empty(),
        "CVM waits for the ordinary machine configuration receipt"
    );
    let machine = db
        .store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].generation,
        1
    );
    assert_eq!(
        machine.target_status, "converging",
        "ordinary success must wait for CVM"
    );
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n1").await,
        "succeeded"
    );
    let audit:serde_json::Value=sqlx::query_scalar("SELECT observed_after->'cvm' FROM deployment_target_state WHERE deployment_id=$1 AND node_id='n1'").bind(created.deployment_id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audit["instances"][0]["generation"], 1);
    let next = db
        .store
        .command_cvm(
            &actor_user("alice"),
            &item.desired.id,
            CvmCommand {
                request_id: "mixed-release-stop-01".into(),
                expected_generation: 1,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    assert!(next.revision_id.unwrap() > r);
    let stop_plan = db
        .store
        .plan_deployment(&system_admin(), next.revision_id.unwrap())
        .await
        .unwrap();
    let host = stop_plan
        .targets
        .iter()
        .find(|t| t.node_id == "n1")
        .unwrap();
    assert!(
        !host
            .actions
            .contains(&brocade_deployment::plan::PlannedAction::ApplyXray)
            && !host
                .actions
                .contains(&brocade_deployment::plan::PlannedAction::ApplyWireGuard),
        "CVM-only intent must not regenerate unchanged ordinary artifacts"
    );

    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].power,
        PowerState::Running
    );
    assert!(
        db.store
            .cvm_record_report("n1", evidence(&next))
            .await
            .is_err(),
        "future generation cannot land before publish"
    );
    assert!(
        db.store
            .cvm_record_report("n1", evidence(&item))
            .await
            .unwrap(),
        "previously published generation may still report"
    );
    assert_eq!(
        db.store
            .cvm_instance(&actor_user("alice"), &item.desired.id)
            .await
            .unwrap()
            .status,
        ApplyStatus::Pending
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_cancelled_unclaimed_revision_never_becomes_desired_and_retry_keeps_global_identity() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("mixed-cancel-create-01"),
        )
        .await
        .unwrap();
    let r = revision(&db).await;
    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(r, "mixed-cancel-order-01"),
        )
        .await
        .unwrap();
    db.store
        .cancel_deployment(&system_admin(), first.deployment_id)
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .is_empty());
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    let claimed = approve(&db, "mixed-cancel-replace-01").await;
    db.store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].id,
        item.desired.id
    );
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "succeeded"
    );
    assert_eq!(
        revision(&db).await,
        r,
        "publishing and reports allocate no configuration revision"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_failure_halts_later_wave_and_retry_waits_for_fresh_receipt() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    insert_second_node(db.pool()).await;
    db.store
        .cvm_record_inventory(
            "n2",
            InventoryReport {
                capabilities: catalogue(),
                error: None,
            },
        )
        .await
        .unwrap();
    let mut q = quota();
    q.allowed_nodes.push("n2".into());
    q.max_instances = 2;
    db.store.set_cvm_quota(&system_admin(), q).await.unwrap();
    let first = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("mixed-failure-node1-01"),
        )
        .await
        .unwrap();
    let mut request = create_request("mixed-failure-node2-01");
    request.node_id = "n2".into();
    db.store
        .create_cvm(&actor_user("alice"), request)
        .await
        .unwrap();
    let claimed = approve(&db, "mixed-failure-order-01").await;
    db.store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n2")
        .await
        .unwrap()
        .is_none());
    let mut failed = evidence(&first);
    failed.status = ApplyStatus::Failed;
    failed.error = Some("test runtime unavailable".into());
    db.store
        .cvm_record_report("n1", failed.clone())
        .await
        .unwrap();
    let detail = db
        .store
        .deployment_detail(&system_admin(), claimed.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(detail.status, "halted");
    assert_eq!(
        detail
            .targets
            .iter()
            .find(|t| t.node_id == "n1")
            .unwrap()
            .verdict
            .as_ref()
            .unwrap()["cvm_status"],
        "failed-dirty"
    );
    assert!(db
        .store
        .cvm_desired("n2")
        .await
        .unwrap()
        .instances
        .is_empty());
    db.store
        .retry_target(&system_admin(), claimed.deployment_id, "n1")
        .await
        .unwrap();
    assert!(
        db.store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .is_none(),
        "CVM retry preserves the successful machine configuration and newer permissions"
    );
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "converging"
    );
    let fresh = db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .remove(0);
    assert!(fresh.generation > first.desired.generation);
    assert_eq!(fresh.spec, first.desired.spec);
    assert!(!db.store.cvm_record_report("n1", failed).await.unwrap());
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "converging"
    );
    let mut retried = first.clone();
    retried.desired = fresh;
    db.store
        .cvm_record_report("n1", evidence(&retried))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), claimed.deployment_id, "n1").await,
        "succeeded"
    );
    assert_eq!(
        revision(&db).await,
        detail.revision_id,
        "retry is not a new configuration revision"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_deleted_instance_keeps_its_terminal_receipt_in_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("review-audit-create-01"),
        )
        .await
        .unwrap();
    let create = approve(&db, "review-audit-create-order-01").await;
    db.store
        .report_target_result(applied_report(&create))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let deletion = db
        .store
        .command_cvm(
            &actor_user("alice"),
            &item.desired.id,
            CvmCommand {
                request_id: "review-audit-delete-01".into(),
                expected_generation: item.desired.generation,
                change: CvmChange::Delete { delete_data: false },
            },
        )
        .await
        .unwrap();
    let delete = approve(&db, "review-audit-delete-order-01").await;
    db.store
        .report_target_result(applied_report(&delete))
        .await
        .unwrap();
    let mut report = evidence(&deletion);
    report.runtime_id = None;
    report.network_absent = true;
    report.volume_retained = true;
    for n in &mut report.interfaces {
        n.attached = false;
    }
    db.store.cvm_record_report("n1", report).await.unwrap();
    assert_eq!(
        target_status(db.pool(), delete.deployment_id, "n1").await,
        "succeeded"
    );
    let receipt:serde_json::Value=sqlx::query_scalar("SELECT observed_after->'cvm' FROM deployment_target_state WHERE deployment_id=$1 AND node_id='n1'").bind(delete.deployment_id).fetch_one(db.pool()).await.unwrap();
    assert!(
        receipt["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == deletion.desired.id
                && r["generation"] == deletion.desired.generation),
        "successful deletion must retain its immutable completion evidence: {receipt}"
    );
}
