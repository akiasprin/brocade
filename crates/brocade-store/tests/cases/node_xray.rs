use super::*;
use brocade_deployment::protocol::ForwardingReconcileReport;
use brocade_store::{ForwardingEntitlementWrite, ForwardingRuleWrite};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_xray_switch_preserves_editing_snapshots_and_independent_forwarding() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            serde_json::from_value::<ForwardingEntitlementWrite>(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    db.store.create_forwarding_rule(&system_admin(), "platform.acme", "alice",
        serde_json::from_value::<ForwardingRuleWrite>(json!({"node_id":"n1","name":"retained","protocol":"tcp","listen_port":40001,"target_host":"1.1.1.1","target_port":443,"enabled":true})).unwrap()).await.unwrap();
    let initial = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(initial.rules.len(), 1);
    let revision = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                xray_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .revision_id;
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(!snapshot.nodes[0].xray_enabled);
    assert!(!snapshot.nodes[0].retired);
    assert!(snapshot.nodes[0].overlay);
    assert!(db.store.materialize_snapshot(Some(1)).await.unwrap().nodes[0].xray_enabled);
    assert!(matches!(
        db.store
            .update_node(
                &tenant_admin("platform.acme"),
                "n1",
                UpdateNodeRequest {
                    xray_enabled: Some(true),
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                xray_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.materialize_snapshot(None).await.unwrap().revision,
        revision
    );
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                dns: Some(Dns::Servers(vec!["9.9.9.9".into()])),
                connection: Some(NodeConnection {
                    conn_idle_secs: Some(600),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let edited = db.store.materialize_snapshot(None).await.unwrap();
    let stopped = compile(&edited).project_node("n1").unwrap();
    assert!(stopped.xray.is_none() && stopped.wireguard.is_some());
    assert!(!edited.apps[0].ingresses.is_empty());
    // A model edit cannot stop the independent live forwarding lane before publication.
    assert_eq!(
        db.store
            .forwarding_desired_for_node("n1")
            .await
            .unwrap()
            .rules,
        initial.rules
    );
    sqlx::query("INSERT INTO node_applied_state(node_id,wireguard_state,xray_state,grants_state,observed_at) VALUES('n1','unknown','disabled','disabled',now())")
        .execute(db.pool()).await.unwrap();
    let disabled = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert!(disabled.rules.is_empty());
    assert!(disabled.generation > initial.generation);
    let view = db
        .store
        .forwarding_overview(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    assert!(view.rules[0].enabled);
    assert_eq!(view.rules[0].runtime_status, "xray-paused");
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                generation: initial.generation,
                applied_rule_ids: initial.rules.iter().map(|rule| rule.id).collect(),
                error: None,
                rule_results: None,
            },
        )
        .await
        .unwrap();
    // Re-enable uses all edits made while stopped and generates another fenced desired set.
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                xray_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(db
        .store
        .forwarding_desired_for_node("n1")
        .await
        .unwrap()
        .rules
        .is_empty());
    let resumed = db.store.materialize_snapshot(None).await.unwrap();
    let plan = compile(&resumed).project_node("n1").unwrap().xray.unwrap();
    assert_eq!(plan.connection.conn_idle_secs, 600);
    assert_eq!(plan.dns, Dns::Servers(vec!["9.9.9.9".into()]));
    sqlx::query("UPDATE node_applied_state SET xray_state='present', xray_sha256=repeat('a',64) WHERE node_id='n1'").execute(db.pool()).await.unwrap();
    let active = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(active.rules, initial.rules);
    assert!(active.generation > disabled.generation);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_xray_configuration_publication_and_rollback_restore_the_switch() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let base = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "xray-base"))
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    let revision = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                xray_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .revision_id;
    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "xray-stop"),
        )
        .await
        .unwrap();
    let stopped = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        stopped.desired.xray,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        stopped.desired.wireguard,
        DesiredArtifact::Unmanaged { .. }
    ));
    assert!(matches!(
        stopped.desired.grants,
        DesiredGrants::Disabled { .. }
    ));
    db.store
        .report_target_result(applied_report(&stopped))
        .await
        .unwrap();
    db.store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(base.deployment_id, "restore Xray"),
        )
        .await
        .unwrap();
    assert!(db.store.materialize_snapshot(None).await.unwrap().nodes[0].xray_enabled);
    let restored = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        restored.desired.xray,
        DesiredArtifact::Present { .. }
    ));
    db.store
        .report_target_result(applied_report(&restored))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_xray_stopped_binary_release_confirms_installation_without_launch() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO admin_operators(id,display_name,role,password_hash) VALUES('test-system','Test System','system-admin','test-only')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version,runtime_versions,runtime_reported_at,last_poll_at) VALUES('n1',57,$1,now(),now())")
        .bind(json!({"xray_installed_sha256":"a".repeat(64)})).execute(db.pool()).await.unwrap();
    let release_id = "e".repeat(64);
    let artifacts = vec![XrayReleaseArtifact {
        arch: "x86_64".into(),
        sha256: "b".repeat(64),
    }];
    let build = XrayBuildInfo {
        release_id: &release_id,
        version: "26.10.31",
        artifacts: &artifacts,
    };
    let request = CreateXrayReleaseRequest {
        idempotency_key: "stopped-update".into(),
        release_id: release_id.clone(),
        nodes: vec!["n1".into()],
        note: None,
        xray_strategy: XrayReleaseStrategy::Restart,
        xray_drain_timeout_secs: None,
    };
    assert!(
        db.store
            .create_xray_release(&system_admin(), request.clone(), build)
            .await
            .is_err(),
        "an unexplained stopped process must remain ineligible"
    );
    sqlx::query("INSERT INTO node_applied_state(node_id,wireguard_state,xray_state,grants_state,observed_at) VALUES('n1','unknown','disabled','disabled',now())").execute(db.pool()).await.unwrap();
    let release = db
        .store
        .create_xray_release(&system_admin(), request, build)
        .await
        .unwrap();
    assert!(db
        .store
        .claim_xray_release("n1", "x86_64", &release_id)
        .await
        .unwrap()
        .is_some());
    let report = XrayReleaseReport {
        release_id: release.id,
        attempt: 1,
        outcome: XrayReleaseOutcome::Succeeded,
        performed_update: true,
        strategy: XrayReleaseStrategy::Restart,
        forced_retirement: false,
        xray_enabled: false,
        installed_sha256: Some("b".repeat(64)),
        running_sha256: None,
        error: None,
    };
    assert!(db.store.report_xray_release("n1", &report).await.unwrap());
    assert_eq!(
        db.store.xray_release(release.id).await.unwrap().status,
        XrayReleaseStatus::Succeeded
    );
}
