use super::*;
use brocade_deployment::product::ProductFlavor;
use brocade_store::{
    BinaryBuildInfo, BinaryComponent, BinaryReleaseArtifact, CreateBinaryReleaseRequest,
};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn product_variants_require_reported_identity_and_explicit_release_intent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO admin_operators (id,display_name,role,password_hash) VALUES ('test-system','Test','system-admin','test-only')")
        .execute(db.pool()).await.unwrap();
    let before = "1".repeat(64);
    let build_id = "3".repeat(64);
    sqlx::query("INSERT INTO node_agent_state (node_id,token_last_used_at,runtime_reported_at,runtime_versions) VALUES ('n1',now(),now(),$1)")
        .bind(json!({"agent":before})).execute(db.pool()).await.unwrap();
    let artifacts = [BinaryReleaseArtifact {
        arch: "x86_64".into(),
        sha256: "2".repeat(64),
    }];
    let build = BinaryBuildInfo {
        component: BinaryComponent::Agent,
        build_id: &build_id,
        version: "test",
        artifacts: &artifacts,
    };
    let mut request = CreateBinaryReleaseRequest {
        agent_flavor: Some(ProductFlavor::Cvm),
        allow_flavor_change: true,
        idempotency_key: "variant-test".into(),
        build_id: build_id.clone(),
        nodes: vec!["n1".into()],
        note: None,
        xray_strategy: None,
        xray_drain_timeout_secs: None,
    };
    assert!(
        matches!(db.store.create_binary_release(&system_admin(), request.clone(), build).await,
        Err(StoreError::InvalidData(message)) if message.contains("product flavor"))
    );
    sqlx::query("UPDATE node_agent_state SET runtime_versions=runtime_versions||'{\"agent_flavor\":\"base\"}'::jsonb WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    request.allow_flavor_change = false;
    assert!(matches!(
        db.store
            .create_binary_release(&system_admin(), request.clone(), build)
            .await,
        Err(StoreError::Conflict(_))
    ));
    request.allow_flavor_change = true;
    let order = db
        .store
        .create_binary_release(&system_admin(), request.clone(), build)
        .await
        .unwrap();
    assert_eq!(order.agent_flavor, Some(ProductFlavor::Cvm));
    assert!(order.allow_flavor_change);
    let offer = db
        .store
        .claim_agent_release_for_flavor("n1", "x86_64", &build_id, ProductFlavor::Base)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offer.agent_flavor, Some(ProductFlavor::Cvm));
    request.allow_flavor_change = false;
    assert!(matches!(
        db.store
            .create_binary_release(&system_admin(), request.clone(), build)
            .await,
        Err(StoreError::Conflict(_))
    ));
    db.store
        .cancel_binary_release(BinaryComponent::Agent, &system_admin(), order.id)
        .await
        .unwrap();
    sqlx::query("UPDATE node_agent_state SET runtime_versions=runtime_versions||'{\"agent_flavor\":\"cvm\"}'::jsonb WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    request.idempotency_key = "downgrade-test".into();
    request.agent_flavor = Some(ProductFlavor::Base);
    request.allow_flavor_change = true;
    assert!(
        matches!(db.store.create_binary_release(&system_admin(), request, build).await,
        Err(StoreError::Conflict(message)) if message.contains("stopped-node installer"))
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn product_variants_base_guard_distinguishes_live_resources_from_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.ensure_cvm_released().await.unwrap();
    sqlx::query("INSERT INTO cvm_nic_publications (id,provider_node,revision,spec,actor_id) VALUES ('pub','n1',1,'{}','test')")
        .execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store.ensure_cvm_released().await,
        Err(StoreError::Conflict(_))
    ));
    sqlx::query("UPDATE cvm_nic_publications SET admitting=false,deleted=true WHERE id='pub'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.ensure_cvm_released().await.unwrap();
    sqlx::query("INSERT INTO cvm_user_host_network_quotas (tenant_id,user_id,node_id,id,revision,limits,held_states) VALUES ('platform.acme','alice','n1','1111111111111111',1,'{}',100)")
        .execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store.ensure_cvm_released().await,
        Err(StoreError::Conflict(_))
    ));
    sqlx::query("UPDATE cvm_user_host_network_quotas SET held_states=0")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.ensure_cvm_released().await.unwrap();
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(users, 1);
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_publications")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(history, 1);
}
