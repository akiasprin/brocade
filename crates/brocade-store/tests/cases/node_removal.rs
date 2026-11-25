use super::*;

async fn retire_for_removal(db: &TestPg, node: &str) {
    db.store
        .update_node_status(
            &system_admin(),
            node,
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
            },
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node(node)
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
}

async fn cvm_node_snapshots(db: &TestPg) {
    for statement in [
        "INSERT INTO cvm_hosts(node_id,capabilities) VALUES ($1,'{}')",
        "INSERT INTO cvm_nic_nodes(node_id,capabilities) VALUES ($1,'{}')",
        "INSERT INTO cvm_entry_nodes(node_id) VALUES ($1)",
        "INSERT INTO cvm_network_quota_hosts(node_id,capacity) VALUES ($1,'{}')",
    ] {
        for node in ["n1", "n-other"] {
            sqlx::query(statement)
                .bind(node)
                .execute(db.pool())
                .await
                .unwrap();
        }
    }
}

async fn assert_cvm_snapshots(db: &TestPg, removed: i64) {
    for table in [
        "cvm_hosts",
        "cvm_nic_nodes",
        "cvm_entry_nodes",
        "cvm_network_quota_hosts",
    ] {
        let counts = sqlx::query(&format!(
            "SELECT count(*) FILTER (WHERE node_id='n1') AS removed,
                    count(*) FILTER (WHERE node_id='n-other') AS preserved FROM {table}"
        ))
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(counts.get::<i64, _>("removed"), removed, "{table}");
        assert_eq!(counts.get::<i64, _>("preserved"), 1, "{table}");
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_removal_cleans_cvm_snapshots_and_preserves_other_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    retire_for_removal(&db, "n1").await;
    cvm_node_snapshots(&db).await;

    let mixed = db
        .store
        .remove_retired_nodes(
            &system_admin(),
            RemoveRetiredNodesRequest {
                node_ids: vec!["n1".to_owned(), "n-other".to_owned()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(mixed, StoreError::Conflict(_)));
    assert_cvm_snapshots(&db, 1).await;

    let result = db
        .store
        .remove_retired_nodes(
            &system_admin(),
            RemoveRetiredNodesRequest {
                node_ids: vec!["n1".to_owned()],
            },
        )
        .await
        .unwrap();
    assert_eq!(result.removed_nodes, ["n1"]);
    assert_cvm_snapshots(&db, 0).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n-other'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_removal_detaches_and_preserves_cvm_publication_history() {
    for table in ["cvm_nic_publications", "cvm_entry_publications"] {
        let Some(db) = TestPg::start_if_enabled().await else {
            return;
        };
        db.store.migrate().await.unwrap();
        insert_minimal_fixture(db.pool()).await;
        insert_other_tenant_fixture(db.pool()).await;
        retire_for_removal(&db, "n1").await;
        cvm_node_snapshots(&db).await;
        sqlx::query(&format!(
            "INSERT INTO {table}(id,provider_node,revision,admitting,deleted,spec,actor_id)
             VALUES ('history','n1',1,FALSE,TRUE,'{{}}','test-system')"
        ))
        .execute(db.pool())
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
        assert_cvm_snapshots(&db, 0).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!(
                "SELECT count(*) FROM {table} WHERE id='history' AND deleted"
            ))
            .fetch_one(db.pool())
            .await
            .unwrap(),
            1
        );
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_removal_detaches_and_preserves_cvm_instance_execution_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    retire_for_removal(&db, "n1").await;
    sqlx::query(
        "INSERT INTO cvm_instances (
             id,tenant_id,user_id,node_id,name,generation,desired,status,reserved
         ) VALUES (
             'cvm-11111111-1111-4111-8111-111111111111','platform.acme','alice','n1',
             'retired history',1,
             '{\"id\":\"cvm-11111111-1111-4111-8111-111111111111\",\"power\":\"deleted\"}',
             'unknown',FALSE
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cvm_spec_versions(instance_id,generation,spec,sha256)
         VALUES ('cvm-11111111-1111-4111-8111-111111111111',1,'{}',repeat('a',64))",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cvm_operations(
             id,instance_id,generation,kind,actor_id,request_id,request_sha256,status
         ) VALUES (
             'op-11111111-1111-4111-8111-111111111111',
             'cvm-11111111-1111-4111-8111-111111111111',1,'force_node_retirement',
             'test-system','remove-history',repeat('b',64),'unknown'
         )",
    )
    .execute(db.pool())
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
    let history = sqlx::query(
        "SELECT i.node_id,i.reserved,i.desired->>'power' AS power,o.kind
           FROM cvm_instances i
           JOIN cvm_operations o ON o.instance_id=i.id
          WHERE i.id='cvm-11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(history.get::<String, _>("node_id"), "n1");
    assert!(!history.get::<bool, _>("reserved"));
    assert_eq!(history.get::<String, _>("power"), "deleted");
    assert_eq!(history.get::<String, _>("kind"), "force_node_retirement");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn prerelease_0009_upgrade_resets_cvm_data_and_detaches_exact_node_foreign_keys() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    for statement in [
        "ALTER TABLE cvm_instances ADD CONSTRAINT cvm_instances_node_id_fkey FOREIGN KEY(node_id) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_nic_publications ADD CONSTRAINT cvm_nic_publications_provider_node_fkey FOREIGN KEY(provider_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_nic_allocations ADD CONSTRAINT cvm_nic_allocations_provider_node_fkey FOREIGN KEY(provider_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_nic_allocations ADD CONSTRAINT cvm_nic_allocations_consumer_node_fkey FOREIGN KEY(consumer_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_nic_bindings ADD CONSTRAINT cvm_nic_bindings_provider_node_fkey FOREIGN KEY(provider_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_nic_bindings ADD CONSTRAINT cvm_nic_bindings_consumer_node_fkey FOREIGN KEY(consumer_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_entry_publications ADD CONSTRAINT cvm_entry_publications_provider_node_fkey FOREIGN KEY(provider_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_entry_tunnels ADD CONSTRAINT cvm_entry_tunnels_entry_node_fkey FOREIGN KEY(entry_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_entry_tunnels ADD CONSTRAINT cvm_entry_tunnels_target_node_fkey FOREIGN KEY(target_node) REFERENCES nodes(id) ON DELETE RESTRICT",
        "ALTER TABLE cvm_entries ADD CONSTRAINT cvm_entries_provider_node_fkey FOREIGN KEY(provider_node) REFERENCES nodes(id) ON DELETE RESTRICT",
    ] {
        sqlx::query(statement).execute(db.pool()).await.unwrap();
    }
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO cvm_instances(id,tenant_id,node_id,name,generation,desired)
         VALUES('legacy-cvm','platform.acme','n1','Legacy CVM',1,'{}')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations
            SET checksum=decode('3955d2e313a1e967cf6602843d774a45698fa5eb7cdc0ed8d5ebff081cbce1ce2ddbd1dcdc69b5d1099b206cd08ba1a4','hex')
          WHERE version=9",
    )
    .execute(db.pool())
    .await
    .unwrap();

    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let compatible = db.store.check_prerelease_0009_upgrade().await.unwrap();
    assert!(compatible.compatible);
    assert_eq!(compatible.pending_versions, [9]);
    db.store.migrate_prerelease_0009_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_constraint
          WHERE conname=ANY($1) AND confrelid='nodes'::regclass",
    )
    .bind(vec![
        "cvm_entries_provider_node_fkey",
        "cvm_entry_publications_provider_node_fkey",
        "cvm_entry_tunnels_entry_node_fkey",
        "cvm_entry_tunnels_target_node_fkey",
        "cvm_instances_node_id_fkey",
        "cvm_nic_allocations_consumer_node_fkey",
        "cvm_nic_allocations_provider_node_fkey",
        "cvm_nic_bindings_consumer_node_fkey",
        "cvm_nic_bindings_provider_node_fkey",
        "cvm_nic_publications_provider_node_fkey",
    ])
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(remaining, 0);
    let rows: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM cvm_instances),
             (SELECT count(*) FROM nodes),
             (SELECT count(*) FROM users)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rows, (0, 1, 1));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn prerelease_detached_0009_upgrade_resets_cvm_data() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO cvm_instances(id,tenant_id,node_id,name,generation,desired)
         VALUES('legacy-cvm','platform.acme','n1','Legacy CVM',1,'{}')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations
            SET checksum=decode('c9ca869a99860a6565f5ce2f6f490fae2d9b62b4dd9a5b80cca60fbe87ba4b672d3de9e1bb29d23f973f499189c4e579','hex')
          WHERE version=9",
    )
    .execute(db.pool())
    .await
    .unwrap();

    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let compatible = db.store.check_prerelease_0009_upgrade().await.unwrap();
    assert!(compatible.compatible);
    assert_eq!(compatible.pending_versions, [9]);
    db.store.migrate_prerelease_0009_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    let rows: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM cvm_instances),
             (SELECT count(*) FROM nodes),
             (SELECT count(*) FROM users)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rows, (0, 1, 1));
}

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
