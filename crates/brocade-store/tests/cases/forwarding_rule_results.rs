use super::*;
use brocade_deployment::protocol::{ForwardingReconcileReport, ForwardingRuleReconcileResult};
use brocade_store::{ForwardingEntitlementWrite, ForwardingRuleWrite};

async fn fixture(db: &TestPg) -> ForwardingRuleWrite {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    for user in ["alice", "bob"] {
        if user == "bob" {
            sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','20111111-1111-4111-8111-111111111111')")
                .execute(db.pool()).await.unwrap();
        }
        db.store
            .set_forwarding_entitlement(
                &system_admin(),
                "platform.acme",
                user,
                "n1",
                serde_json::from_value::<ForwardingEntitlementWrite>(json!({"enabled":true}))
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    serde_json::from_value(json!({"node_id":"n1","name":"old rule","protocol":"tcp",
        "listen_port":40001,"target_host":"1.1.1.1","target_port":443,"enabled":true}))
    .unwrap()
}

async fn view(db: &TestPg) -> brocade_store::ForwardingOverview {
    db.store
        .forwarding_overview(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap()
}

async fn acknowledge(db: &TestPg) {
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                generation: desired.generation,
                applied_rule_ids: desired.rules.iter().map(|rule| rule.id).collect(),
                error: None,
                rule_results: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_rule_results_isolate_generation_errors_and_metadata() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut write = fixture(&db).await;
    let created = db
        .store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", write.clone())
        .await
        .unwrap();
    let first = created.rules[0].id;
    acknowledge(&db).await;
    let mut bob = write.clone();
    bob.listen_port = 40002;
    db.store
        .create_forwarding_rule(&system_admin(), "platform.acme", "bob", bob)
        .await
        .unwrap();
    assert_eq!(view(&db).await.rules[0].runtime_status, "active");
    acknowledge(&db).await;
    let mut extra = write.clone();
    extra.listen_port = 40003;
    let created = db
        .store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", extra.clone())
        .await
        .unwrap();
    let second = created.rules[1].id;
    assert_eq!(created.rules[0].runtime_status, "active");
    assert_eq!(created.rules[1].runtime_status, "pending");
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    let report = ForwardingReconcileReport {
        generation: desired.generation,
        applied_rule_ids: desired
            .rules
            .iter()
            .filter(|rule| rule.id != second)
            .map(|rule| rule.id)
            .collect(),
        error: Some("one failed listener".into()),
        rule_results: Some(
            desired
                .rules
                .iter()
                .cloned()
                .map(|rule| {
                    let applied = rule.id != second;
                    ForwardingRuleReconcileResult {
                        rule,
                        applied,
                        error: (!applied).then(|| "TCP port is occupied".into()),
                    }
                })
                .collect(),
        ),
    };
    db.store
        .record_forwarding_reconcile("n1", report.clone())
        .await
        .unwrap();
    let current = view(&db).await;
    assert_eq!(current.rules[0].runtime_status, "active");
    assert_eq!(current.rules[0].runtime_error, None);
    assert_eq!(current.rules[1].runtime_status, "error");
    assert_eq!(
        current.rules[1].runtime_error.as_deref(),
        Some("TCP port is occupied")
    );
    write.name = "renamed".into();
    let renamed = db
        .store
        .update_forwarding_rule(&system_admin(), "platform.acme", "alice", first, write)
        .await
        .unwrap();
    assert_eq!(
        renamed.entitlements[0].desired_generation,
        desired.generation
    );
    assert_eq!(renamed.rules[0].runtime_status, "active");
    assert_eq!(renamed.rules[1].runtime_status, "error");

    for variant in 0..4 {
        let mut invalid = report.clone();
        let results = invalid.rule_results.as_mut().unwrap();
        match variant {
            0 => {
                results[0].rule.target_port += 1;
            }
            1 => {
                results.push(results[0].clone());
            }
            2 => {
                results.pop();
            }
            _ => {
                results
                    .iter_mut()
                    .find(|r| r.rule.id == second)
                    .unwrap()
                    .applied = true;
            }
        }
        assert!(matches!(
            db.store.record_forwarding_reconcile("n1", invalid).await,
            Err(StoreError::InvalidData(_))
        ));
    }
    extra.target_port = 8443;
    let changed = db
        .store
        .update_forwarding_rule(&system_admin(), "platform.acme", "alice", second, extra)
        .await
        .unwrap();
    assert_eq!(changed.rules[0].runtime_status, "active");
    assert_eq!(changed.rules[1].runtime_status, "pending");
    db.store
        .record_forwarding_reconcile("n1", report)
        .await
        .unwrap();
    assert_eq!(
        view(&db).await.rules[1].runtime_status,
        "pending",
        "stale receipts cannot restore an old error"
    );
    acknowledge(&db).await;
    assert!(view(&db)
        .await
        .rules
        .iter()
        .all(|rule| rule.runtime_status == "active"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_rule_results_do_not_claim_health_from_stale_or_legacy_errors() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let write = fixture(&db).await;
    db.store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", write)
        .await
        .unwrap();
    acknowledge(&db).await;
    sqlx::query("UPDATE forwarding_node_state SET reported_at=now()-interval '91 seconds' WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let stale = view(&db).await;
    assert_eq!(stale.rules[0].runtime_status, "unknown");
    assert!(stale.entitlements[0].reported_at.is_some());
    let desired = db.store.forwarding_desired_for_node("n1").await.unwrap();
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                generation: desired.generation,
                applied_rule_ids: vec![desired.rules[0].id],
                error: Some("legacy receipt has no per-rule evidence".into()),
                rule_results: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(view(&db).await.rules[0].runtime_status, "unknown");
    acknowledge(&db).await;
    assert_eq!(view(&db).await.rules[0].runtime_status, "active");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_rule_results_migration_preserves_existing_state() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut initial = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .unwrap();
    let expected_versions = initial
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    assert!(expected_versions.contains(&2));
    initial.migrations = std::borrow::Cow::Owned(
        initial
            .iter()
            .filter(|migration| migration.version == 1)
            .cloned()
            .collect(),
    );
    assert_eq!(initial.iter().count(), 1);
    initial.run(db.pool()).await.unwrap();
    let has_results: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='public'
            AND table_name='forwarding_node_state' AND column_name='rule_results')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!has_results, "the initial schema must remain unchanged");
    let initial_receipt: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM _sqlx_migrations m WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();

    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO forwarding_entitlements(tenant_id,user_id,node_id,updated_by)
        VALUES('platform.acme','alice','n1','migration fixture')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let rule_id: i64 = sqlx::query_scalar(
        "INSERT INTO forwarding_rules
        (tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
        VALUES('platform.acme','alice','n1','old rule','tcp',40001,'1.1.1.1',443) RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO forwarding_node_state
        (node_id,desired_generation,applied_generation,reconcile_status,applied_rule_ids,reported_at)
        VALUES('n1',7,7,'converged',$1,now())")
        .bind(json!([rule_id])).execute(db.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO forwarding_usage_rollups
        (period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
        VALUES(date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong',
            'n1','platform.acme','alice',$1,123,456,false)",
    )
    .bind(rule_id)
    .execute(db.pool())
    .await
    .unwrap();
    let facts_sql = "SELECT jsonb_build_object(
        'rules',(SELECT jsonb_agg(r ORDER BY id) FROM forwarding_rules r),
        'entitlements',(SELECT jsonb_agg(e ORDER BY tenant_id,user_id,node_id) FROM forwarding_entitlements e),
        'node_state',(SELECT jsonb_agg(to_jsonb(s)-'rule_results' ORDER BY node_id) FROM forwarding_node_state s),
        'usage',(SELECT jsonb_agg(u ORDER BY period_start,rule_id) FROM forwarding_usage_rollups u))";
    let before: serde_json::Value = sqlx::query_scalar(facts_sql)
        .fetch_one(db.pool())
        .await
        .unwrap();

    db.store.migrate().await.unwrap();
    db.store.migrate().await.unwrap();
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(versions, expected_versions);
    let after_receipt: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM _sqlx_migrations m WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        after_receipt, initial_receipt,
        "version 1 history must not be rewritten"
    );
    let after: serde_json::Value = sqlx::query_scalar(facts_sql)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "upgrade must preserve rules, receipts and accounting"
    );
    let missing: bool = sqlx::query_scalar(
        "SELECT rule_results IS NULL FROM forwarding_node_state WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(missing, "legacy evidence must not be fabricated");
    assert_eq!(view(&db).await.rules[0].runtime_status, "active");

    let error =
        sqlx::query("UPDATE forwarding_node_state SET rule_results='[]'::jsonb WHERE node_id='n1'")
            .execute(db.pool())
            .await
            .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    acknowledge(&db).await;
    let saved: bool = sqlx::query_scalar(
        "SELECT rule_results IS NOT NULL FROM forwarding_node_state WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(saved);
}
