use super::*;
use brocade_deployment::product::ProductFlavor;
use brocade_store::{
    BinaryBuildInfo, BinaryComponent, BinaryReleaseArtifact, CreateBinaryReleaseRequest,
};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn product_variants_default_legacy_agents_to_base_and_require_explicit_switches() {
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
        agent_flavor: Some(ProductFlavor::Base),
        allow_flavor_change: false,
        idempotency_key: "legacy-base-test".into(),
        build_id: build_id.clone(),
        nodes: vec!["n1".into()],
        note: None,
        xray_strategy: None,
        xray_drain_timeout_secs: None,
    };
    let legacy_order = db
        .store
        .create_binary_release(&system_admin(), request.clone(), build)
        .await
        .unwrap();
    let legacy_offer = db
        .store
        .claim_agent_release("n1", "x86_64", &build_id, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy_offer.agent_flavor, Some(ProductFlavor::Base));
    db.store
        .cancel_binary_release(BinaryComponent::Agent, &system_admin(), legacy_order.id)
        .await
        .unwrap();
    request.idempotency_key = "variant-test".into();
    request.agent_flavor = Some(ProductFlavor::Cvm);
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

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn product_variants_unfinished_cvm_configuration_release_blocks_base_switch() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let order = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "variant-cvm-config-01"),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE deployment_target_state SET cvm_revision_id=1 WHERE deployment_id=$1 AND node_id='n1'").bind(order.deployment_id).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store.ensure_cvm_released().await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        db.store.ensure_node_cvm_released("n1").await,
        Err(StoreError::Conflict(_))
    ));
    db.store
        .cancel_deployment(&system_admin(), order.deployment_id)
        .await
        .unwrap();
    db.store.ensure_cvm_released().await.unwrap();
    db.store.ensure_node_cvm_released("n1").await.unwrap();
    let metadata = json!({"instances":[{"id":"own","tenant_id":"platform.acme","user_id":"alice"},{"id":"foreign","tenant_id":"platform.other","user_id":"bob"}]});
    sqlx::query("UPDATE deployment_target_state SET desired_structure=desired_structure||jsonb_build_object('cvm',$2::jsonb),observed_after=jsonb_build_object('cvm',jsonb_build_object('instances',jsonb_build_array(jsonb_build_object('id','own'),jsonb_build_object('id','foreign')))) WHERE deployment_id=$1 AND node_id='n1'").bind(order.deployment_id).bind(metadata).execute(db.pool()).await.unwrap();
    let scoped = AdminContext::from_authenticated(&brocade_store::AuthenticatedAdmin {
        operator_id: "tenant-a".into(),
        role: AdminRole::TenantAdmin,
        tenant_scope: Some("platform.acme".into()),
        token_prefix: None,
        self_user: None,
    });
    let detail = db
        .store
        .deployment_detail(&scoped, order.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(
        detail.targets[0].desired_structure["cvm"]["instances"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        detail.targets[0].observed_after.as_ref().unwrap()["cvm"]["instances"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // An unpublished empty-resource change (e.g. catalog removal) also needs the CVM authority.
    sqlx::query("INSERT INTO cvm_revision_nodes(revision_id,node_id,sha256,payload) VALUES (1,'n1',$1,'test')")
        .bind("0".repeat(64)).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store.ensure_cvm_released().await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        db.store.ensure_node_cvm_released("n1").await,
        Err(StoreError::Conflict(_))
    ));
    db.store.ensure_node_cvm_released("n2").await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn product_variants_model_rollback_cannot_abandon_newer_cvm_intent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "rollback-base-01"),
        )
        .await
        .unwrap();
    mark_succeeded(db.pool(), first.deployment_id).await;
    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "rollback-base-02"),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO revisions(id,author) VALUES (2,'test');")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO cvm_revision_nodes(revision_id,node_id,sha256,payload) VALUES (2,'n1',$1,'test')")
        .bind("0".repeat(64)).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store
            .create_rollback_deployment(
                &system_admin(),
                create_rollback_request(first.deployment_id, "rollback-denied-01")
            )
            .await,
        Err(StoreError::Unsupported(_))
    ));
    assert!(matches!(
        db.store
            .cancel_deployment_and_rollback(&system_admin(), second.deployment_id)
            .await,
        Err(StoreError::Unsupported(_))
    ));
    assert_eq!(
        deployment_status(db.pool(), second.deployment_id).await,
        "planned"
    );
    db.store
        .cancel_deployment(&system_admin(), second.deployment_id)
        .await
        .unwrap();
}
