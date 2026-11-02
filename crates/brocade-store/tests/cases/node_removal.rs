use super::*;

async fn forwarding_history(db: &TestPg, node: &str, tenant: &str, user: &str) -> i64 {
    sqlx::query(
        "INSERT INTO forwarding_entitlements (tenant_id,user_id,node_id,updated_by)
         VALUES ($1,$2,$3,'test-system')",
    )
    .bind(tenant)
    .bind(user)
    .bind(node)
    .execute(db.pool())
    .await
    .unwrap();
    let rule = sqlx::query_scalar::<_, i64>(
        "INSERT INTO forwarding_rules (
             tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port
         ) VALUES ($1,$2,$3,'test rule','tcp',35000,'example.com',443) RETURNING id",
    )
    .bind(tenant)
    .bind(user)
    .bind(node)
    .fetch_one(db.pool())
    .await
    .unwrap();
    seed_rule_history(db, node, tenant, user, rule).await;
    sqlx::query(
        "INSERT INTO forwarding_usage_report_receipts (node_id,agent_instance_id,sequence,result)
         VALUES ($1,'test-instance',1,'{}'::jsonb)",
    )
    .bind(node)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO forwarding_usage_windows (
             node_id,tenant_id,window_start,window_end,uplink_bytes,downlink_bytes,has_gap
         ) VALUES ($1,$2,now()-interval '30 seconds',now(),10,20,false)",
    )
    .bind(node)
    .bind(tenant)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_user_node_rollups (
             tenant_id,user_id,node_id,period_start,uplink_bytes,downlink_bytes
         ) VALUES ($1,$2,$3,date_trunc('day',now()),10,20)",
    )
    .bind(tenant)
    .bind(user)
    .bind(node)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_user_ingress_rollups (
             node_id,period_start,tenant_id,user_id,ingress_id,app_id,uplink_bytes,downlink_bytes
         ) VALUES ($1,date_trunc('day',now()),$2,$3,'historical-ingress','historical-app',10,20)",
    )
    .bind(node)
    .bind(tenant)
    .bind(user)
    .execute(db.pool())
    .await
    .unwrap();
    rule
}

async fn seed_rule_history(db: &TestPg, node: &str, tenant: &str, user: &str, rule: i64) {
    sqlx::query("INSERT INTO forwarding_rule_nodes (rule_id,node_id) VALUES ($1,$2)")
        .bind(rule)
        .bind(node)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO forwarding_usage_counter_heads (
             node_id,rule_id,agent_instance_id,xray_epoch,uplink_bytes,downlink_bytes,read_at
         ) VALUES ($1,$2,'test-instance','test-epoch',10,20,now())",
    )
    .bind(node)
    .bind(rule)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO forwarding_usage_samples (
             node_id,rule_id,tenant_id,user_id,read_at,uplink_bytes,downlink_bytes
         ) VALUES ($1,$2,$3,$4,now(),10,20)",
    )
    .bind(node)
    .bind(rule)
    .bind(tenant)
    .bind(user)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO forwarding_usage_rollups (
             period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap
         ) VALUES (date_trunc('day',now()),$1,$2,$3,$4,10,20,false)",
    )
    .bind(node)
    .bind(tenant)
    .bind(user)
    .bind(rule)
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_removal_cleans_accounting_references_and_preserves_other_machine_history() {
    for moved_history in [false, true] {
        let Some(db) = TestPg::start_if_enabled().await else {
            return;
        };
        db.store.migrate().await.unwrap();
        insert_minimal_fixture(db.pool()).await;
        insert_other_tenant_fixture(db.pool()).await;
        let removed_rule = forwarding_history(&db, "n1", "platform.acme", "alice").await;
        let other_rule = forwarding_history(&db, "n-other", "platform.other", "charlie").await;
        if moved_history {
            seed_rule_history(&db, "n-other", "platform.acme", "alice", removed_rule).await;
        }
        db.store
            .update_node_status(
                &system_admin(),
                "n1",
                brocade_store::UpdateNodeStatusRequest {
                    status: "retired".to_owned(),
                },
            )
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
        let removed = db
            .store
            .remove_retired_nodes(
                &system_admin(),
                RemoveRetiredNodesRequest {
                    node_ids: vec!["n1".to_owned()],
                },
            )
            .await
            .unwrap();
        assert_eq!(removed.removed_nodes, ["n1"]);
        for table in [
            "forwarding_entitlements",
            "forwarding_rules",
            "forwarding_rule_nodes",
            "forwarding_usage_report_receipts",
            "forwarding_usage_counter_heads",
            "forwarding_usage_samples",
            "forwarding_usage_rollups",
            "forwarding_usage_windows",
            "usage_user_node_rollups",
            "usage_user_ingress_rollups",
        ] {
            let counts = sqlx::query(&format!(
                "SELECT count(*) FILTER (WHERE node_id='n1') AS removed,
                        count(*) FILTER (WHERE node_id='n-other') AS preserved FROM {table}"
            ))
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(counts.get::<i64, _>("removed"), 0, "{table}");
            assert!(counts.get::<i64, _>("preserved") > 0, "{table}");
        }
        let other_enabled: bool = sqlx::query_scalar(
            "SELECT enabled AND deleted_at IS NULL FROM forwarding_rules WHERE id=$1",
        )
        .bind(other_rule)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(other_enabled);
        let tombstone = sqlx::query(
            "SELECT node_id,enabled,deleted_at IS NOT NULL AS deleted FROM forwarding_rules WHERE id=$1",
        )
        .bind(removed_rule)
        .fetch_optional(db.pool())
        .await
        .unwrap();
        if moved_history {
            let tombstone = tombstone.expect("moved rule history must retain its identity");
            assert_eq!(tombstone.get::<String, _>("node_id"), "n-other");
            assert!(!tombstone.get::<bool, _>("enabled"));
            assert!(tombstone.get::<bool, _>("deleted"));
            let bytes: i64 = sqlx::query_scalar(
                "SELECT uplink_bytes+downlink_bytes FROM forwarding_usage_rollups
                 WHERE node_id='n-other' AND rule_id=$1",
            )
            .bind(removed_rule)
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(bytes, 30);
        } else {
            assert!(tombstone.is_none());
        }
    }
}
