use super::*;

fn movement(source: &str, target: &str) -> ModelOp {
    ModelOp::MoveChain {
        app_id: source.to_owned(),
        chain_id: "chn-a1b2-c3d4".to_owned(),
        target_app_id: target.to_owned(),
    }
}

async fn setup() -> Option<TestPg> {
    let db = TestPg::start_if_enabled().await?;
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO apps (id,label,position) VALUES ('app-next','Next',1)")
        .execute(db.pool())
        .await
        .unwrap();
    seed_subscription_serving(&db).await;
    Some(db)
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_group_move_preserves_identity_authorization_history_and_usage_generations() {
    let Some(db) = setup().await else {
        return;
    };
    let base = usage_report_base_unix();
    let old_bindings = alice_usage_bindings();
    let old = install_usage_generation(db.pool(), "n1", old_bindings.clone(), base - 60).await;
    db.store
        .record_usage_report(
            "n1",
            current_usage(old, 1, base, "journal:stable", 100, 200),
        )
        .await
        .unwrap();
    db.store
        .record_usage_report(
            "n1",
            current_usage(old, 2, base + 30, "journal:stable", 110, 220),
        )
        .await
        .unwrap();
    let before = db.store.materialize_snapshot(None).await.unwrap();
    let preview = db
        .store
        .preview_draft(&system_admin(), vec![movement("app-main", "app-next")])
        .await
        .unwrap();
    assert_eq!(
        preview.snapshot.snapshot["apps"][1]["chains"][0]["id"],
        "chn-a1b2-c3d4"
    );
    assert_eq!(
        db.store.materialize_snapshot(None).await.unwrap(),
        before,
        "preview rolls back every association"
    );

    let changed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![movement("app-main", "app-next")],
            None,
        )
        .await
        .unwrap();
    let after = db.store.materialize_snapshot(None).await.unwrap();
    assert!(after.apps[0].chains.is_empty());
    assert_eq!(after.apps[1].chains, before.apps[0].chains);
    assert_eq!(after.apps[1].ingresses, before.apps[0].ingresses);
    assert_eq!(after.apps[1].steps, before.apps[0].steps);
    assert_eq!(after.apps[1].grants, before.apps[0].grants);
    let saved_old = db
        .store
        .materialize_snapshot(Some(before.revision))
        .await
        .unwrap();
    assert_eq!(saved_old, before, "old revisions remain immutable");
    assert!(changed.revision_id > before.revision);

    // A committed authoring move does not rewrite a still-running generation's billing owner.
    db.store
        .record_usage_report(
            "n1",
            current_usage(old, 3, base + 60, "journal:stable", 120, 240),
        )
        .await
        .unwrap();
    let mut new_bindings = old_bindings;
    new_bindings["alice@platform.acme#ing-a1b2"]["app_id"] = json!("app-next");
    let new = install_usage_generation(db.pool(), "n1", new_bindings, base + 60).await;
    let result = db
        .store
        .record_usage_report(
            "n1",
            current_usage(new, 4, base + 90, "journal:stable", 125, 250),
        )
        .await
        .unwrap();
    assert_eq!(result.rejected_counters, 0);
    let totals: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT app_id,sum(uplink_bytes)::bigint,sum(downlink_bytes)::bigint
         FROM usage_samples GROUP BY app_id ORDER BY app_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        totals,
        [
            ("app-main".to_owned(), 20, 40),
            ("app-next".to_owned(), 5, 10)
        ]
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_group_move_rejects_stale_source_permissions_and_pool_links_atomically() {
    let Some(db) = setup().await else {
        return;
    };
    for actor in [
        tenant_admin("platform.acme"),
        AdminContext::new("editor", AdminRole::Editor, None),
    ] {
        assert!(matches!(
            db.store
                .apply_draft(&actor, vec![movement("app-main", "app-next")], None)
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    assert!(matches!(
        db.store
            .apply_draft(
                &system_admin(),
                vec![movement("app-next", "app-main")],
                None
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(db
        .store
        .apply_draft(&system_admin(), vec![movement("app-main", "missing")], None)
        .await
        .is_err());
    sqlx::query("INSERT INTO fronts (id,app_id,tenant_id,name,strategy) VALUES ('front-test','app-main','platform.acme','Pool','select')").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE ingresses SET front_id='front-test' WHERE id='ing-a1b2'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .apply_draft(
                &system_admin(),
                vec![movement("app-main", "app-next")],
                None
            )
            .await,
        Err(StoreError::Unsupported(_))
    ));
    let owners: (String,String,String) = sqlx::query_as("SELECT c.app_id,i.app_id,g.app_id FROM chains c JOIN ingresses i ON i.chain_id=c.id JOIN grants g ON g.ingress_id=i.id").fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        owners,
        (
            "app-main".to_owned(),
            "app-main".to_owned(),
            "app-main".to_owned()
        )
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_group_move_into_new_group_and_repeated_moves_replay_in_order() {
    let Some(db) = setup().await else {
        return;
    };
    let create = ModelOp::CreateApp {
        app: CreateAppRequest {
            id: "app-a1b2".to_owned(),
            label: "New".to_owned(),
            note: None,
        },
    };
    db.store
        .apply_draft(
            &system_admin(),
            vec![
                create,
                movement("app-main", "app-a1b2"),
                movement("app-a1b2", "app-next"),
                movement("app-next", "app-main"),
                movement("app-main", "app-a1b2"),
            ],
            None,
        )
        .await
        .unwrap();
    let owner: String = sqlx::query_scalar("SELECT app_id FROM chains WHERE id='chn-a1b2-c3d4'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(owner, "app-a1b2");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_group_move_preserves_diagnostic_results_and_suspended_grants() {
    let Some(db) = setup().await else {
        return;
    };
    sqlx::raw_sql(
        "INSERT INTO e2e_probes (chain_id,app_id,node_id,status,ttfb_ms,exit_verdict,probed_at)
         VALUES ('chn-a1b2-c3d4','app-main','n1','ok',22,'match',now());
         INSERT INTO unlock_probe_jobs (scope_key,node_id,app_id,chain_id,configuration_revision,job_id,state,trigger)
         VALUES ('chain:app-main/chn-a1b2-c3d4:v4','n1','app-main','chn-a1b2-c3d4',1,'test-job','complete','manual');
         DELETE FROM grants WHERE ingress_id='ing-a1b2';
         INSERT INTO quota_suspensions (tenant_id,user_id,app_id,ingress_id)
         VALUES ('platform.acme','alice','app-main','ing-a1b2');",
    ).execute(db.pool()).await.unwrap();
    db.store
        .apply_draft(
            &system_admin(),
            vec![movement("app-main", "app-next")],
            None,
        )
        .await
        .unwrap();
    let result: (String, String, String, i32) = sqlx::query_as(
        "SELECT j.scope_key,j.app_id,s.app_id,p.ttfb_ms
         FROM unlock_probe_jobs j JOIN e2e_probes p ON p.chain_id=j.chain_id
         JOIN quota_suspensions s ON s.ingress_id='ing-a1b2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        result,
        (
            "chain:app-next/chn-a1b2-c3d4:v4".to_owned(),
            "app-next".to_owned(),
            "app-next".to_owned(),
            22
        )
    );
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM grants")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        active, 0,
        "moving must not reactivate a suspended authorization"
    );
}
