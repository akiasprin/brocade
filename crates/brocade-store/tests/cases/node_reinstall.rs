use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_token_resign_restores_confirmed_disabled_artifacts_without_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("UPDATE nodes SET overlay = FALSE WHERE id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();
    freeze_current_fixture_snapshot(db.pool()).await;
    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "reinstall-disabled-baseline"),
        )
        .await
        .unwrap();
    let initial = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        initial.desired.wireguard,
        DesiredArtifact::Disabled { .. }
    ));
    db.store
        .report_target_result(applied_report(&initial))
        .await
        .unwrap();

    // Legacy narrow releases did not retain unchanged disabled dimensions. Their acknowledged
    // applied state is still authoritative, even when no successful target contains the artifact.
    sqlx::query(
        "UPDATE deployment_target_state
            SET desired_structure = desired_structure || $1
          WHERE node_id = 'n1'",
    )
    .bind(json!({
        "phantun": { "state": "unmanaged", "reason": "legacy narrow target" },
        "wireguard": { "state": "unmanaged", "reason": "legacy narrow target" },
        "hy2_port_hop": { "state": "unmanaged", "reason": "legacy narrow target" }
    }))
    .execute(db.pool())
    .await
    .unwrap();

    // An unshipped model edit must never turn WireGuard on during reinstall recovery.
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                overlay: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store.reissue_node_token("n1").await.unwrap();
    db.store.reissue_node_token("n1").await.unwrap();
    let repair = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repair.deployment_id, initial.deployment_id);
    assert!(initial.usage_generation_id.is_some());
    assert_eq!(repair.usage_generation_id, initial.usage_generation_id);
    for (_, artifact) in repair.desired.artifacts() {
        assert!(!matches!(artifact, DesiredArtifact::Unmanaged { .. }));
    }
    for action in [
        PlannedAction::DisablePhantun,
        PlannedAction::DisableWireGuard,
        PlannedAction::DisableHy2PortHop,
    ] {
        assert!(repair.actions.contains(&action));
    }
    db.store
        .report_target_result(failed_recovered_report(&repair))
        .await
        .unwrap();
    let retry = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.desired, repair.desired);
    assert_eq!(retry.usage_generation_id, repair.usage_generation_id);
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
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM node_applied_state,
             LATERAL unnest(ARRAY[phantun_state, wireguard_state, hy2_port_hop_state]) state
          WHERE node_id = 'n1'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(states, vec!["disabled"; 3]);

    sqlx::query(
        "UPDATE node_applied_state
            SET phantun_state = 'unknown', phantun_observed = '{}'::jsonb
          WHERE node_id = 'n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store.reissue_node_token("n1").await.unwrap();
    let unknown = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        unknown.desired.phantun,
        DesiredArtifact::Unmanaged { .. }
    ));
    assert!(!unknown.actions.contains(&PlannedAction::DisablePhantun));
}
