use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn desired_delivery_replays_a_lost_response_without_changing_the_frozen_claim() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "delivery-replay"),
        )
        .await
        .unwrap();
    // A claim dispatched before the Agent upgrade must also recover immediately.
    let first = db
        .store
        .claim_desired_for_node_with_protocol("n1", 38)
        .await
        .unwrap()
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node_with_protocol("n1", 38)
        .await
        .unwrap()
        .is_none());
    assert!(
        db.store
            .claim_desired_for_node("n2")
            .await
            .unwrap()
            .is_none(),
        "replay cannot skip the canary wave"
    );
    let stamp: String = sqlx::query_scalar(
        "SELECT dispatched_at::text FROM deployment_target_state WHERE deployment_id=$1 AND node_id='n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool()).await.unwrap();
    // An independently queued permission list must not replace what this claim already froze.
    sqlx::query("UPDATE deployment_target_state SET desired_grants=$2 WHERE deployment_id=$1 AND node_id='n1'")
        .bind(created.deployment_id)
        .bind(json!({"state":"disabled","reason":"a later permission list"}))
        .execute(db.pool()).await.unwrap();
    for _ in 0..2 {
        let replay = db
            .store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&replay).unwrap(),
            serde_json::to_value(&first).unwrap()
        );
        let replay_stamp: String = sqlx::query_scalar(
            "SELECT dispatched_at::text FROM deployment_target_state WHERE deployment_id=$1 AND node_id='n1'",
        ).bind(created.deployment_id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(replay_stamp, stamp);
    }
    db.store
        .report_target_result(applied_report(&first))
        .await
        .unwrap();
    // A lost report ACK can only hit the existing terminal-receipt rejection; it must not
    // replace the accepted observation or change the node's accounting boundary.
    assert!(matches!(
        db.store.report_target_result(applied_report(&first)).await,
        Err(StoreError::Unsupported(_))
    ));
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n1").await,
        "succeeded"
    );
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    assert!(db
        .store
        .claim_desired_for_node("n2")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn desired_delivery_replays_isolated_debt_without_fencing_its_first_report() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "delivery-isolated"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            created.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();
    let first = db
        .store
        .claim_desired_for_node_with_protocol("n1", 38)
        .await
        .unwrap()
        .unwrap();
    assert!(first.claim_generation > 0);
    assert!(db
        .store
        .claim_desired_for_node_with_protocol("n1", 38)
        .await
        .unwrap()
        .is_none());
    let stamp: String = sqlx::query_scalar(
        "SELECT claimed_at::text FROM node_convergence_obligations WHERE node_id='n1' AND status='dispatched'",
    ).fetch_one(db.pool()).await.unwrap();
    for _ in 0..2 {
        let replay = db
            .store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(replay).unwrap(),
            serde_json::to_value(&first).unwrap()
        );
    }
    let replay_stamp: String = sqlx::query_scalar(
        "SELECT claimed_at::text FROM node_convergence_obligations WHERE node_id='n1' AND status='dispatched'",
    ).fetch_one(db.pool()).await.unwrap();
    assert_eq!(replay_stamp, stamp);
    db.store
        .report_target_result(applied_report(&first))
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
async fn desired_delivery_does_not_replay_a_halted_or_canceled_deployment() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "delivery-halt"),
        )
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_some());
    db.store
        .halt_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    db.store
        .cancel_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
}
