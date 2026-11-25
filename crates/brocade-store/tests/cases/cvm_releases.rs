use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_stopped_retry_requires_a_compiled_product_even_without_a_previous_bundle() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(
            &system_admin(),
            create_request("review-stop-retry-create-01"),
        )
        .await
        .unwrap();
    let create = approve(&db, "review-stop-retry-create-order-01").await;
    db.store
        .report_target_result(applied_report(&create))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let stopped = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "review-stop-retry-stop-01".into(),
                expected_generation: item.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    let release = approve(&db, "review-stop-retry-stop-order-01").await;
    db.store
        .report_target_result(applied_report(&release))
        .await
        .unwrap();
    let mut failed = evidence(&stopped);
    failed.status = ApplyStatus::Failed;
    failed.error = Some("fixture stop failure".into());
    db.store.cvm_record_report("n1", failed).await.unwrap();

    // Fault injection in the disposable database only. Production history remains immutable.
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    for table in ["cvm_spec_artifacts", "cvm_spec_artifact_sets"] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE instance_id=$1 AND generation=$2"
        ))
        .bind(&item.desired.id)
        .bind(stopped.desired.generation as i64)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    db.store
        .retry_target(&system_admin(), release.deployment_id, "n1")
        .await
        .unwrap();
    let desired = db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .remove(0);
    assert_eq!(desired.power, PowerState::Stopped);
    assert!(desired.generation > stopped.desired.generation);
    let bundle = desired
        .artifacts
        .expect("a retry must not fall back to an uncompiled stopped specification");
    assert_eq!(bundle.files.len(), 4);
}

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
        .create_cvm(&system_admin(), create_request("mixed-resign-create-01"))
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
        .create_cvm(&system_admin(), create_request("mixed-release-create-01"))
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
        .create_cvm(&system_admin(), create_request("mixed-release-create-01"))
        .await
        .unwrap();
    assert_eq!(
        replay.desired.operation_id, item.desired.operation_id,
        "idempotent replay remains available during publication"
    );
    let blocked = db
        .store
        .command_cvm(
            &system_admin(),
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
    let claimed = db
        .store
        .claim_desired_for_node_with_protocol(
            "n1",
            brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
        )
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
            &system_admin(),
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
        .create_cvm(&system_admin(), create_request("mixed-cancel-create-01"))
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
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) SELECT id,77 FROM nodes WHERE id IN ('n1','n2') ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=77").execute(db.pool()).await.unwrap();
    let first = db
        .store
        .create_cvm(&system_admin(), create_request("mixed-failure-node1-01"))
        .await
        .unwrap();
    let mut request = create_request("mixed-failure-node2-01");
    request.node_id = "n2".into();
    db.store.create_cvm(&system_admin(), request).await.unwrap();
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
        .create_cvm(&system_admin(), create_request("review-audit-create-01"))
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
            &system_admin(),
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

async fn change(db: &TestPg, item: &CvmInstance, key: &str, action: CvmChange) -> CvmInstance {
    db.store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: key.into(),
                expected_generation: item.desired.generation,
                change: action,
            },
        )
        .await
        .unwrap()
}

async fn published(db: &TestPg, key: &str) -> CvmInstance {
    let item = db
        .store
        .create_cvm(&system_admin(), create_request(key))
        .await
        .unwrap();
    let claim = approve(db, &format!("{key}-order")).await;
    db.store
        .report_target_result(applied_report(&claim))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    db.store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_unpublished_create_releases_reservations_and_is_idempotent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("undo-create-request-01"))
        .await
        .unwrap();
    let undone = change(
        &db,
        &item,
        "undo-create-command-01",
        CvmChange::UndoPending {
            expected_revision: item.revision_id.unwrap(),
        },
    )
    .await;
    assert!(!undone.reserved);
    assert_eq!(undone.status, ApplyStatus::Applied);
    assert!(db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .is_empty());
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_volumes WHERE state<>'released'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(held, 0);
    let replay = change(
        &db,
        &item,
        "undo-create-command-01",
        CvmChange::UndoPending {
            expected_revision: item.revision_id.unwrap(),
        },
    )
    .await;
    assert_eq!(replay.desired.generation, undone.desired.generation);
    let another = db
        .store
        .create_cvm(&system_admin(), create_request("undo-create-request-02"))
        .await
        .unwrap();
    assert!(another.reserved);
    assert!(undone.revision_id > item.revision_id);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_delete_and_stop_preserves_running_instance_then_publishes_recovery() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = published(&db, "undo-delete-create-01").await;
    let deleted = change(
        &db,
        &item,
        "undo-delete-command-01",
        CvmChange::Delete { delete_data: true },
    )
    .await;
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].power,
        PowerState::Running
    );
    let restored = change(
        &db,
        &deleted,
        "undo-delete-restore-01",
        CvmChange::UndoPending {
            expected_revision: deleted.revision_id.unwrap(),
        },
    )
    .await;
    assert_eq!(restored.desired.power, PowerState::Running);
    let mut original_spec = item.desired.spec.clone();
    assert!(restored.desired.spec.routing.revision > original_spec.routing.revision);
    original_spec.routing.revision = restored.desired.spec.routing.revision;
    assert_eq!(restored.desired.spec, original_spec);
    assert!(restored.desired.generation > deleted.desired.generation);
    let claim = approve(&db, "undo-delete-publish-01").await;
    db.store
        .report_target_result(applied_report(&claim))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&restored))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), claim.deployment_id, "n1").await,
        "succeeded"
    );
    let ready = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    let stopped = change(&db, &ready, "undo-stop-command-01", CvmChange::Stop).await;
    let restored = change(
        &db,
        &stopped,
        "undo-stop-restore-01",
        CvmChange::UndoPending {
            expected_revision: stopped.revision_id.unwrap(),
        },
    )
    .await;
    assert_eq!(restored.desired.power, PowerState::Running);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_fences_stale_generation_active_order_and_published_intent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = published(&db, "undo-fencing-create-01").await;
    let stopped = change(&db, &item, "undo-fencing-stop-01", CvmChange::Stop).await;
    let stale = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-fencing-stale-01".into(),
                expected_generation: item.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: stopped.revision_id.unwrap(),
                },
            },
        )
        .await;
    assert!(matches!(stale, Err(StoreError::Conflict(_))));
    let order = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision(&db).await, "undo-fencing-order-01"),
        )
        .await
        .unwrap();
    let locked = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(locked.blocking_deployment_id, Some(order.deployment_id));
    let denied = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-fencing-active-01".into(),
                expected_generation: stopped.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: stopped.revision_id.unwrap(),
                },
            },
        )
        .await;
    assert!(matches!(denied, Err(StoreError::Conflict(_))));
    db.store
        .cancel_deployment(&system_admin(), order.deployment_id)
        .await
        .unwrap();
    let restored = change(
        &db,
        &stopped,
        "undo-fencing-cancel-01",
        CvmChange::UndoPending {
            expected_revision: stopped.revision_id.unwrap(),
        },
    )
    .await;
    let claim = approve(&db, "undo-fencing-publish-01").await;
    db.store
        .report_target_result(applied_report(&claim))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&restored))
        .await
        .unwrap();
    let denied = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-fencing-exposed-01".into(),
                expected_generation: restored.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: restored.revision_id.unwrap(),
                },
            },
        )
        .await;
    assert!(matches!(denied, Err(StoreError::Conflict(_))));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_routing_validation_explains_the_dns_field_in_chinese() {
    let db = TestPg::start_if_enabled()
        .await
        .expect("explicit Docker PostgreSQL acceptance");
    prepare(&db).await;
    let instance = published(&db, "review-validation-create-01").await;
    let mut routes = instance.desired.spec.routing.clone();
    routes.dns = vec!["9.9.9.9".parse().unwrap()];
    let valid = RoutesWrite {
        request_id: "review-validation-dns-valid-01".into(),
        expected_generation: instance.desired.generation,
        expected_revision: routes.revision,
        routes,
    };
    db.store
        .set_cvm_routes_reviewed(
            &system_admin(),
            &instance.desired.id,
            valid.clone(),
            CvmMutationReview {
                preview: true,
                ..Default::default()
            },
        )
        .await
        .expect("valid DNS control passes the actual compiler preview");
    let mut invalid = valid;
    invalid.request_id = "review-validation-dns-invalid-01".into();
    invalid.routes.dns = vec!["127.0.0.1".parse().unwrap()];
    let error = db
        .store
        .set_cvm_routes_reviewed(
            &system_admin(),
            &instance.desired.id,
            invalid,
            CvmMutationReview {
                preview: true,
                ..Default::default()
            },
        )
        .await
        .expect_err("loopback DNS should be rejected");
    let message = error.to_string();
    assert!(
        message.contains("容器 DNS 不能使用回环、组播或未指定地址"),
        "user-facing DNS validation still exposes internal English: {message}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_pending_shrink_does_not_create_a_user_admission_ceiling() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let first = db
        .store
        .create_cvm(&system_admin(), create_request("no-user-ceiling-first-01"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&first))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let second = db
        .store
        .create_cvm(&system_admin(), create_request("no-user-ceiling-second-01"))
        .await
        .unwrap();
    assert_ne!(first.desired.id, second.desired.id);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_cvm_repair_replaces_failed_debt_and_waits_for_resource_receipts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let instance = db
        .store
        .create_cvm(&system_admin(), create_request("isolated-cvm-create-0001"))
        .await
        .unwrap();
    let original = approve(&db, "isolated-cvm-original-0001").await;
    db.store
        .report_target_result(applied_report(&original))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&instance))
        .await
        .unwrap();
    // Adopt the pre-existing isolated/failed-debt baseline, as an older Console left it.
    // Isolation itself is covered separately; this regression starts at the recovery deadlock.
    sqlx::query("INSERT INTO node_operational_isolations(node_id) VALUES('n1')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE deployments SET active=NULL,status='succeeded' WHERE id=$1")
        .bind(original.deployment_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_convergence_obligations(node_id,kind,lifecycle_epoch,generation,source_deployment_id,source_revision_id,desired_structure,desired_grants,desired_fingerprint,status,claim_generation) SELECT 'n1','config',s.lifecycle_epoch,1,s.deployment_id,d.revision_id,s.desired_structure,s.desired_grants,repeat('a',64),'failed-dirty',1 FROM deployment_target_state s JOIN deployments d ON d.id=s.deployment_id WHERE s.deployment_id=$1 AND s.node_id='n1'").bind(original.deployment_id).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE deployment_targets SET status='failed-dirty',error='legacy associated network failure' WHERE deployment_id=$1 AND node_id='n1'").bind(original.deployment_id).execute(db.pool()).await.unwrap();
    let mut old_debt = original.clone();
    old_debt.claim_generation = 1;
    let repair = db
        .store
        .command_cvm(
            &system_admin(),
            &instance.desired.id,
            CvmCommand {
                request_id: "isolated-cvm-restart-0001".into(),
                expected_generation: instance.desired.generation,
                change: CvmChange::Restart,
            },
        )
        .await
        .unwrap();
    let order = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision(&db).await, "isolated-cvm-repair-order-0001"),
        )
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), original.deployment_id, "n1").await,
        "superseded"
    );
    assert_eq!(
        target_status(db.pool(), order.deployment_id, "n1").await,
        "pending"
    );
    // A repair must still wait for an operator-approved destructive wave.
    sqlx::query("UPDATE control_state SET deployment_unattended=FALSE WHERE id=TRUE")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE deployment_target_state SET wave=2,disruptive=TRUE WHERE deployment_id=$1")
        .bind(order.deployment_id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    assert!(db
        .store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    db.store
        .confirm_deployment_wave(&system_admin(), order.deployment_id, 2, None)
        .await
        .unwrap();
    assert!(
        db.store
            .report_target_result(applied_report(&old_debt))
            .await
            .is_err(),
        "a late old claim cannot overwrite the repair"
    );
    let claimed = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.deployment_id, order.deployment_id);
    assert!(
        matches!(claimed.desired.xray, DesiredArtifact::Present { .. }),
        "an isolated CVM repair must reconfirm the complete ordinary configuration"
    );

    assert_eq!(
        claimed.claim_generation, 0,
        "repair obeys ordinary wave claims"
    );
    let result = db
        .store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    assert_eq!(result.target_status, "converging");
    let debt: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node_convergence_obligations WHERE node_id='n1' AND status='pending'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        debt, 1,
        "machine success alone cannot clear the service re-entry barrier"
    );
    assert!(db
        .store
        .restore_node_service(&system_admin(), "n1")
        .await
        .is_err());
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    let mut failed = evidence(&repair);
    failed.status = ApplyStatus::Failed;
    failed.error = Some("temporary repair failure".into());
    db.store.cvm_record_report("n1", failed).await.unwrap();
    db.store
        .retry_target(&system_admin(), order.deployment_id, "n1")
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), order.deployment_id, "n1").await,
        "converging"
    );
    let retried = db
        .store
        .cvm_instance(&system_admin(), &repair.desired.id)
        .await
        .unwrap();
    assert!(retried.desired.generation > repair.desired.generation);
    db.store
        .cvm_record_report("n1", evidence(&retried))
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), order.deployment_id, "n1").await,
        "succeeded"
    );
    let debt: i64 = sqlx::query_scalar("SELECT count(*) FROM node_convergence_obligations WHERE node_id='n1' AND status IN ('pending','dispatched','converging','failed-recovered','failed-dirty')").fetch_one(db.pool()).await.unwrap();
    assert_eq!(debt, 0);
    let isolated: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM node_operational_isolations WHERE node_id='n1')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        isolated,
        "successful repair never restores serving automatically"
    );
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
}
