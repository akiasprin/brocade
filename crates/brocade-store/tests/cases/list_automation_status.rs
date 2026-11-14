use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn list_automation_status_is_batched_scoped_and_contains_no_connector_secrets() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO tenants(id,name) VALUES ('customer.alpha','Other tenant')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES ('customer.alpha','alice','b04fa0cd-827f-44ee-9025-54c61fb89dde')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES ('platform.acme','bob','b8dfa1b1-2f29-47f5-8a4e-5b27b3b8d55c'),('platform.acme','carol','5cb8f614-98ba-4dd1-a57a-7650b2a43100')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO subscription_connectors(tenant_id,user_id,sealed,metadata,remote_name,auto) VALUES ('platform.acme','alice','sealed-fixture-not-decrypted','{\"endpoint\":\"https://private.example.test/private-token\",\"login\":\"private-login\"}','fixture-alice',true),('platform.acme','bob','sealed-fixture-not-decrypted','{}','fixture-bob',false)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO subscription_connectors(tenant_id,user_id,sealed,metadata,remote_name,auto) VALUES ('customer.alpha','alice','foreign-sealed-fixture','{}','foreign-alice',false)")
        .execute(db.pool()).await.unwrap();
    let actor = system_admin();
    let users = db.store.list_users(&actor, None, true).await.unwrap();
    let summary = |id: &str| {
        users
            .users
            .iter()
            .find(|user| user.tenant_id == "platform.acme" && user.id == id)
            .unwrap()
            .substore
            .unwrap()
    };
    assert!(summary("alice").configured && summary("alice").auto);
    assert!(summary("bob").configured && !summary("bob").auto);
    assert!(!summary("carol").configured && !summary("carol").auto);
    let scoped = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    let scoped_users = db.store.list_users(&scoped, None, true).await.unwrap();
    assert!(scoped_users
        .users
        .iter()
        .all(|user| user.tenant_id == "platform.acme"));
    assert!(
        scoped_users
            .users
            .iter()
            .find(|user| user.id == "alice")
            .unwrap()
            .substore
            .unwrap()
            .auto
    );
    let serialized = serde_json::to_string(&users).unwrap();
    for secret in [
        "private.example.test",
        "private-token",
        "private-login",
        "sealed-fixture-not-decrypted",
    ] {
        assert!(!serialized.contains(secret));
    }
    let self_actor = AdminContext::from_authenticated(&brocade_store::AuthenticatedAdmin {
        operator_id: "self-alice".into(),
        role: AdminRole::User,
        tenant_scope: Some("platform.acme".into()),
        token_prefix: None,
        self_user: Some(brocade_store::AuthenticatedUser {
            tenant_id: "platform.acme".into(),
            user_id: "alice".into(),
        }),
    });
    let own = db.store.list_users(&self_actor, None, true).await.unwrap();
    assert!(
        own.users
            .iter()
            .find(|user| user.id == "alice")
            .unwrap()
            .substore
            .unwrap()
            .auto
    );
    assert!(own
        .users
        .iter()
        .filter(|user| user.id != "alice")
        .all(|user| user.substore.is_none()));
    let public = AdminContext::new("public", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(db
        .store
        .list_users(&public, None, true)
        .await
        .unwrap()
        .users
        .iter()
        .all(|user| user.substore.is_none()));
    assert!(
        db.store
            .self_user_profile(&self_actor)
            .await
            .unwrap()
            .substore
            .unwrap()
            .auto
    );

    let node = db
        .store
        .list_node_agent_states(&actor)
        .await
        .unwrap()
        .nodes
        .remove(0);
    assert!(!node.ip_maintenance.unwrap().configured);
    let policy = brocade_deployment::ip_maintenance::IpMaintenancePolicy {
        enabled: true,
        ..Default::default()
    };
    sqlx::query("INSERT INTO ip_maintenance_policies(node_id,generation,policy,actor_id) VALUES ('n1',1,$1,'test')")
        .bind(serde_json::to_value(policy).unwrap()).execute(db.pool()).await.unwrap();
    let node = db
        .store
        .list_node_agent_states(&actor)
        .await
        .unwrap()
        .nodes
        .remove(0);
    assert!(node.ip_maintenance.unwrap().configured && node.ip_maintenance.unwrap().enabled);
    sqlx::query("UPDATE ip_maintenance_policies SET policy=jsonb_set(policy,'{enabled}','false') WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let node = db
        .store
        .list_node_agent_states(&actor)
        .await
        .unwrap()
        .nodes
        .remove(0);
    assert!(node.ip_maintenance.unwrap().configured && !node.ip_maintenance.unwrap().enabled);
}
