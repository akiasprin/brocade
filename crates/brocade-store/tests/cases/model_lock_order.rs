use super::*;

async fn fixture(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
}

fn step(send_through: &str) -> ModelOp {
    ModelOp::PutStep {
        app_id: "app-main".to_owned(),
        chain_id: "chn-a1b2-c3d4".to_owned(),
        node_id: "n1".to_owned(),
        step: serde_json::from_value(json!({
            "rules": [{"m":{"t":"any"},"a":{"t":"egress","send_through":send_through}}]
        }))
        .unwrap(),
    }
}

async fn network_gate(pool: &PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(1112687438,65)")
        .execute(&mut *gate)
        .await
        .unwrap();
    gate
}

async fn wait_for_network_waiters(pool: &PgPool, minimum: i64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                  WHERE datname=current_database() AND pid<>pg_backend_pid()
                    AND wait_event_type='Lock'
                    AND query='SELECT pg_advisory_xact_lock(1112687438,65)'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= minimum {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("model operations must reach the network resource lock");
}

async fn release_without_model_lock(mut gate: sqlx::Transaction<'_, sqlx::Postgres>) -> bool {
    // NOWAIT makes the original inverse ordering fail deterministically, without waiting for
    // PostgreSQL's deadlock detector or depending on the two request tasks being scheduled together.
    let model_available =
        sqlx::query("SELECT current_revision FROM control_state WHERE id=TRUE FOR UPDATE NOWAIT")
            .fetch_one(&mut *gate)
            .await
            .is_ok();
    gate.rollback().await.unwrap();
    model_available
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn preview_and_commit_wait_for_network_resources_before_locking_the_model() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    let revisions_before: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let gate = network_gate(db.pool()).await;
    let store = db.store.clone();
    let preview = tokio::spawn(async move {
        store
            .preview_draft(&system_admin(), vec![step("192.0.2.10")])
            .await
    });
    wait_for_network_waiters(db.pool(), 1).await;
    let store = db.store.clone();
    let commit = tokio::spawn(async move {
        store
            .apply_draft(&system_admin(), vec![step("192.0.2.20")], None)
            .await
    });
    wait_for_network_waiters(db.pool(), 2).await;
    let model_available = release_without_model_lock(gate).await;
    let (preview, committed) = tokio::time::timeout(Duration::from_secs(10), async {
        (
            preview.await.unwrap().unwrap(),
            commit.await.unwrap().unwrap(),
        )
    })
    .await
    .expect("preview and commit must both finish after the resource gate is released");
    assert!(
        model_available,
        "a waiting preview must not hold the model row lock"
    );
    assert!(preview.compile.summary.can_publish);
    assert_eq!(
        preview.snapshot.snapshot["apps"][0]["steps"][0]["rules"][0]["a"]["send_through"],
        "192.0.2.10"
    );
    assert_eq!(committed.changed, 1);
    let rules: serde_json::Value = sqlx::query_scalar(
        "SELECT rules FROM steps WHERE chain_id='chn-a1b2-c3d4' AND node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rules[0]["a"]["send_through"], "192.0.2.20");
    let revisions_after: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        revisions_after,
        revisions_before + 1,
        "only the committed draft creates a revision"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn direct_node_edits_wait_for_network_resources_before_locking_the_model() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    let gate = network_gate(db.pool()).await;
    let store = db.store.clone();
    let edit = tokio::spawn(async move {
        store
            .update_node(
                &system_admin(),
                "n1",
                UpdateNodeRequest {
                    name: Some("Updated machine".to_owned()),
                    ..Default::default()
                },
            )
            .await
    });
    wait_for_network_waiters(db.pool(), 1).await;
    let model_available = release_without_model_lock(gate).await;
    tokio::time::timeout(Duration::from_secs(10), edit)
        .await
        .expect("direct node edit must finish after the resource gate is released")
        .unwrap()
        .unwrap();
    assert!(
        model_available,
        "a waiting direct edit must not hold the model row lock"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn direct_ingress_edits_wait_for_network_resources_before_locking_the_model() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    let gate = network_gate(db.pool()).await;
    let store = db.store.clone();
    let edit = tokio::spawn(async move {
        store
            .upsert_ingress(
                &system_admin(),
                "app-main",
                CreateIngressRequest {
                    id: "ing-a1b2".to_owned(),
                    chain_id: "chn-a1b2-c3d4".to_owned(),
                    node_id: "n1".to_owned(),
                    bind: "0.0.0.0".parse().unwrap(),
                    port: 8443,
                    guard: brocade_core::model::IngressGuard::OPEN,
                    reality: CreateRealityIngressRequest {
                        fallback_mode: RealityFallbackMode::CustomSite,
                        fallback_limits: RealityFallbackLimits::Balanced,
                        fallback_guard: true,
                        dest: Some("www.example.com:443".to_owned()),
                        server_names: vec!["www.example.com".to_owned()],
                        fingerprint: Some("chrome".to_owned()),
                        flow: Some("xtls-rprx-vision".to_owned()),
                    },
                    wires: WiresRequest::default(),
                    projection: Projection::default(),
                    note: None,
                },
            )
            .await
    });
    wait_for_network_waiters(db.pool(), 1).await;
    let model_available = release_without_model_lock(gate).await;
    tokio::time::timeout(Duration::from_secs(10), edit)
        .await
        .expect("direct ingress edit must finish after the resource gate is released")
        .unwrap()
        .unwrap();
    assert!(
        model_available,
        "a waiting ingress edit must not hold the model row lock"
    );
    let port: i32 = sqlx::query_scalar("SELECT port FROM ingresses WHERE id='ing-a1b2'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(port, 8443);
}
