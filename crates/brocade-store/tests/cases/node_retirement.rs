use super::*;

async fn partially_disabled_node(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO node_applied_state (
             node_id, phantun_state, hy2_port_hop_state, wireguard_state,
             xray_state, xray_sha256, grants_state, observed_at
         ) VALUES ('n1', 'disabled', 'disabled', 'disabled', 'present', $1, 'disabled', now())",
    )
    .bind("1".repeat(64))
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retirement_after_partial_disable_keeps_full_teardown_and_revokes_token() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    partially_disabled_node(&db).await;
    let token = db.store.issue_node_token("n1").await.unwrap();
    let retired = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
            },
        )
        .await
        .unwrap();
    let teardown = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("partially disabled retirement still requires a teardown receipt");
    for (artifact, desired) in teardown.desired.artifacts() {
        assert!(
            matches!(desired, DesiredArtifact::Disabled { .. }),
            "retirement must retain the disabled contract for {artifact:?}: {desired:?}"
        );
    }
    db.store
        .report_target_result(applied_report(&teardown))
        .await
        .unwrap();
    let lifecycle = db.store.node_lifecycle("n1").await.unwrap();
    assert_eq!(lifecycle.phase, brocade_store::NodeLifecyclePhase::Retired);
    assert_eq!(lifecycle.deployment_id, retired.deployment_id);
    assert!(lifecycle.completed_at.is_some());
    assert!(db
        .store
        .authenticate_node_token(&token.token)
        .await
        .unwrap()
        .is_none());
    let settled = db
        .store
        .plan_deployment(&system_admin(), retired.revision_id)
        .await
        .unwrap();
    assert!(settled.targets.iter().all(|target| target.node_id != "n1"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retirement_retry_settles_legacy_success_without_rewriting_history() {
    for unrelated_changes in [false, true] {
        let Some(db) = TestPg::start_if_enabled().await else {
            return;
        };
        partially_disabled_node(&db).await;
        let token = db.store.issue_node_token("n1").await.unwrap();
        let retired = db
            .store
            .update_node_status(
                &system_admin(),
                "n1",
                brocade_store::UpdateNodeStatusRequest {
                    status: "retired".to_owned(),
                },
            )
            .await
            .unwrap();
        let deployment_id = retired.deployment_id.unwrap();
        // Recreate a frozen target written by the old planner before the Agent claims it.
        // Its successful partial receipt leaves the complete applied baseline disabled, while
        // the lifecycle still waits for a full-teardown receipt that can never arrive.
        sqlx::query(
            "UPDATE deployment_target_state SET desired_structure = desired_structure || $2
             WHERE deployment_id = $1 AND node_id = 'n1'",
        )
        .bind(deployment_id)
        .bind(json!({
            "actions": ["disable-xray"],
            "phantun": {"state": "unmanaged", "reason": "legacy narrowing"},
            "hy2_port_hop": {"state": "unmanaged", "reason": "legacy narrowing"},
            "wireguard": {"state": "unmanaged", "reason": "legacy narrowing"}
        }))
        .execute(db.pool())
        .await
        .unwrap();
        let legacy = db
            .store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .unwrap();
        db.store
            .report_target_result(applied_report(&legacy))
            .await
            .unwrap();
        assert_eq!(
            db.store.node_lifecycle("n1").await.unwrap().phase,
            brocade_store::NodeLifecyclePhase::Retiring
        );
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(s) FROM deployment_target_state s
             WHERE deployment_id = $1 AND node_id = 'n1'",
        )
        .bind(deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(db
            .store
            .authenticate_node_token(&token.token)
            .await
            .unwrap()
            .is_some());
        if unrelated_changes {
            insert_other_tenant_fixture(db.pool()).await;
        }
        let repaired = db
            .store
            .update_node_status(
                &system_admin(),
                "n1",
                brocade_store::UpdateNodeStatusRequest {
                    status: "retired".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(repaired.revision_id, retired.revision_id);
        assert_eq!(repaired.lifecycle.lifecycle_epoch, 1);
        assert_eq!(
            repaired.lifecycle.phase,
            brocade_store::NodeLifecyclePhase::Retired,
            "known-disabled legacy teardown must settle even with unrelated_changes={unrelated_changes}"
        );
        assert_eq!(repaired.lifecycle.deployment_id, Some(deployment_id));
        assert!(repaired.lifecycle.completed_at.is_some());
        assert_eq!(repaired.deployment_id.is_some(), unrelated_changes);
        assert!(db
            .store
            .authenticate_node_token(&token.token)
            .await
            .unwrap()
            .is_none());
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(s) FROM deployment_target_state s
             WHERE deployment_id = $1 AND node_id = 'n1'",
        )
        .bind(deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(
            after, before,
            "repair must preserve the accepted legacy receipt"
        );
        assert_eq!(
            target_status(db.pool(), deployment_id, "n1").await,
            "succeeded"
        );
    }
}
