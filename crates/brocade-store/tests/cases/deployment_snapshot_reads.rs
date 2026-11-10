use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn revision_coverage_metadata_is_lightweight_and_does_not_treat_grants_as_config() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO deployments (revision_id, kind, status, activation_status, active)
        VALUES (1, 'grants', 'succeeded', 'activated', FALSE),
               (1, 'config', 'running', 'waiting', TRUE)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE apps IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *gate)
        .await
        .unwrap();
    let revisions = tokio::time::timeout(
        Duration::from_secs(2),
        db.store.list_revisions(&system_admin(), 50),
    )
    .await
    .unwrap()
    .unwrap();
    gate.rollback().await.unwrap();
    assert_eq!(revisions.last_config_revision_id, None);
    sqlx::query("UPDATE deployments SET status='succeeded',activation_status='activated',active=FALSE WHERE kind='config'")
        .execute(db.pool()).await.unwrap();
    assert_eq!(
        db.store
            .list_revisions(&system_admin(), 50)
            .await
            .unwrap()
            .last_config_revision_id,
        Some(1)
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn planning_refreshes_operational_certificate_slots_without_rebuilding_the_model() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.ensure_default_self_signed_pool().await.unwrap();
    let group: String = sqlx::query_scalar("SELECT id FROM cert_labels WHERE is_default")
        .fetch_one(db.pool())
        .await
        .unwrap();
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let certificates: Vec<String> =
        sqlx::query_scalar("SELECT id FROM certificates WHERE label_id=$1 ORDER BY runtime_slot")
            .bind(&group)
            .fetch_all(db.pool())
            .await
            .unwrap();
    for (id, fingerprint) in certificates.iter().zip(['a', 'b']) {
        db.store
            .record_certificate(IssuedCertificate {
                certificate_id: id,
                acme_directory: brocade_store::SELF_SIGNED_DIRECTORY,
                cert_pem: "fixture certificate",
                key_pem: "fixture private key",
                not_after: "2126-01-01T00:00:00Z",
                issuer: "Fixture CA",
                peer_sha256: &fingerprint.to_string().repeat(64),
            })
            .await
            .unwrap();
    }
    // REALITY's node-certificate cover advertises the live certificate-name set.
    sqlx::query("UPDATE ingresses SET reality_fallback_mode='node-certificate'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .set_node_cert_label(&system_admin(), "n1", Some(&group))
        .await
        .unwrap();
    seed_subscription_serving(&db).await;
    let before = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    let changed = sqlx::query(
        "UPDATE certificates SET expires_at=now()-interval '1 hour'
        WHERE label_id=$1 AND status='ready'",
    )
    .bind(&group)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    let live = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        live.revision, 1,
        "certificate assignment is operational state"
    );
    let expected = brocade_deployment::plan::plan_deployment(&live, &[]).unwrap();
    let actual = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    assert_ne!(
        expected.targets[0].desired.xray,
        before.targets[0].desired.xray
    );
    assert_eq!(
        actual.targets[0].desired.xray,
        expected.targets[0].desired.xray
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn planning_reads_the_frozen_revision_without_waiting_for_live_app_tables() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    seed_subscription_serving(&db).await;
    let expected = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE apps IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *gate)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        db.store.plan_deployment(&system_admin(), 1),
    )
    .await;
    gate.rollback().await.unwrap();
    let actual = result
        .expect("release reads must not wait for mutable application tables")
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual.targets.len(), 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn planning_keeps_the_committed_model_when_live_rows_are_changed_out_of_band() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    seed_subscription_serving(&db).await;
    let expected = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    sqlx::query(
        "UPDATE steps SET rules='[{\"m\":{\"t\":\"any\"},\"a\":{\"t\":\"block\"}}]'::jsonb",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_ne!(
        db.store.materialize_snapshot(None).await.unwrap().apps[0].steps[0].rules[0].action,
        brocade_core::model::Action::Egress { send_through: None }
    );
    assert_eq!(
        db.store.plan_deployment(&system_admin(), 1).await.unwrap(),
        expected
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn notification_listener_does_not_reserve_a_business_connection_and_closes_cleanly() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let mut listener = sqlx::postgres::PgListener::connect_with(db.store.listener_pool())
        .await
        .unwrap();
    listener.listen("brocade_test_capacity").await.unwrap();
    let mut held = tokio::time::timeout(Duration::from_secs(2), async {
        let mut held = Vec::new();
        for _ in 0..5 {
            held.push(db.pool().acquire().await.unwrap());
        }
        held
    })
    .await
    .expect("LISTEN must leave all five business connections available");
    assert_eq!(db.store.listener_pool().size(), 1);
    sqlx::query("SELECT pg_notify('brocade_test_capacity', 'ready')")
        .execute(&mut **held.first_mut().unwrap())
        .await
        .unwrap();
    let notification = tokio::time::timeout(Duration::from_secs(2), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notification.payload(), "ready");
    drop(held);
    let close = db.store.listener_pool().close();
    let receive = async move {
        let result = listener.try_recv().await;
        drop(listener);
        result
    };
    let (_, result) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(close, receive)
    })
    .await
    .unwrap();
    assert!(result.is_err());
}
