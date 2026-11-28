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

async fn removal_jobs(db: &TestPg, node: &str) -> Vec<(String, i32, Option<String>)> {
    sqlx::query_as(
        "SELECT status, attempts, last_error FROM jobs
          WHERE kind = 'node-removal' AND payload->>'node_id' = $1
          ORDER BY id",
    )
    .bind(node)
    .fetch_all(db.pool())
    .await
    .unwrap()
}

async fn listed_removal(
    db: &TestPg,
    node: &str,
) -> (Option<brocade_store::NodeRemovalStatus>, Option<String>) {
    let list = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    let item = list
        .nodes
        .into_iter()
        .find(|item| item.node_id == node)
        .expect("listed node");
    (item.removal_status, item.removal_last_error)
}

async fn node_count(db: &TestPg, node: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM nodes WHERE id = $1")
        .bind(node)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn report_teardown(db: &TestPg, node: &str) {
    let teardown = db
        .store
        .claim_desired_for_node(node)
        .await
        .unwrap()
        .expect("retirement creates a teardown target");
    db.store
        .report_target_result(applied_report(&teardown))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn scheduled_removal_waits_for_teardown_then_removes_machine() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    let forbidden = db
        .store
        .schedule_node_removal(&tenant_admin("platform.acme"), "n1")
        .await
        .unwrap_err();
    assert!(matches!(forbidden, StoreError::Forbidden(_)));
    let missing = db
        .store
        .schedule_node_removal(&system_admin(), "missing")
        .await
        .unwrap_err();
    assert!(matches!(missing, StoreError::NotFound(_)));

    // An active machine retires in the same action, with the ordinary teardown deployment.
    let scheduled = db
        .store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    assert_eq!(
        scheduled.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Retiring
    );
    assert!(scheduled.deployment_id.is_some());
    let again = db
        .store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    assert_eq!(again.deployment_id, scheduled.deployment_id);
    assert_eq!(
        removal_jobs(&db, "n1").await,
        [("queued".to_owned(), 0, None)]
    );
    assert_eq!(
        listed_removal(&db, "n1").await,
        (Some(brocade_store::NodeRemovalStatus::Queued), None)
    );
    assert_eq!(listed_removal(&db, "n-other").await, (None, None));

    // Waiting for the machine is not an attempt.
    assert!(db.store.process_node_removal().await.unwrap().is_none());
    assert_eq!(removal_jobs(&db, "n1").await[0].1, 0);
    assert_eq!(node_count(&db, "n1").await, 1);

    report_teardown(&db, "n1").await;
    let removed = db
        .store
        .process_node_removal()
        .await
        .unwrap()
        .expect("terminal retirement is removed");
    assert_eq!(removed.removed_nodes, ["n1"]);
    assert_eq!(removed.removed_chains, ["chn-a1b2-c3d4"]);
    assert_eq!(node_count(&db, "n1").await, 0);
    assert_eq!(node_count(&db, "n-other").await, 1);
    assert_eq!(
        removal_jobs(&db, "n1").await,
        [("succeeded".to_owned(), 0, None)]
    );
    let note: String = sqlx::query_scalar("SELECT note FROM revisions WHERE id = $1")
        .bind(i64::try_from(removed.revision_id).unwrap())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(note.contains("requested by"), "{note}");
    assert!(db.store.process_node_removal().await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn restoring_a_machine_cancels_its_scheduled_removal() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    db.store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    let restored = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "active".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        restored.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Active
    );
    let jobs = removal_jobs(&db, "n1").await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].0, "canceled");
    assert_eq!(listed_removal(&db, "n1").await, (None, None));

    // A later ordinary retirement is a new intent and must keep the machine.
    retire_for_removal(&db, "n1").await;
    assert_eq!(
        db.store.node_lifecycle("n1").await.unwrap().phase,
        brocade_store::NodeLifecyclePhase::Retired
    );
    assert!(db.store.process_node_removal().await.unwrap().is_none());
    assert_eq!(node_count(&db, "n1").await, 1);
    assert_eq!(listed_removal(&db, "n1").await, (None, None));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forced_retirement_completes_a_scheduled_removal() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    db.store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    let abandoned = db.store.abandon_node(&system_admin(), "n1").await.unwrap();
    assert_eq!(
        abandoned.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Abandoned
    );
    let removed = db
        .store
        .process_node_removal()
        .await
        .unwrap()
        .expect("abandoned retirement is terminal");
    assert_eq!(removed.removed_nodes, ["n1"]);
    assert_eq!(node_count(&db, "n1").await, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn scheduled_removal_retries_with_backoff_then_gives_up() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    retire_for_removal(&db, "n1").await;

    // A retired machine only gains the intent; no new revision or deployment is created.
    let before: i64 = sqlx::query_scalar("SELECT current_revision FROM control_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let scheduled = db
        .store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    assert_eq!(scheduled.revision_id, u64::try_from(before).unwrap());
    assert!(scheduled.canceled_deployment_ids.is_empty());

    sqlx::query(
        "CREATE FUNCTION reject_node_delete() RETURNS trigger LANGUAGE plpgsql AS
         $$ BEGIN RAISE EXCEPTION 'node delete rejected by test'; END $$",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_node_delete BEFORE DELETE ON nodes
         FOR EACH ROW EXECUTE FUNCTION reject_node_delete()",
    )
    .execute(db.pool())
    .await
    .unwrap();

    db.store.process_node_removal().await.unwrap_err();
    let (status, error) = listed_removal(&db, "n1").await;
    assert_eq!(status, Some(brocade_store::NodeRemovalStatus::Queued));
    assert!(error.unwrap().contains("自动移出失败"));
    // Backed off: the next tick does not retry immediately.
    assert!(db.store.process_node_removal().await.unwrap().is_none());

    for _ in 1..5 {
        sqlx::query("UPDATE jobs SET run_after = now() WHERE kind = 'node-removal'")
            .execute(db.pool())
            .await
            .unwrap();
        db.store.process_node_removal().await.unwrap_err();
    }
    let jobs = removal_jobs(&db, "n1").await;
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].0.as_str(), jobs[0].1), ("failed", 5));
    assert_eq!(
        listed_removal(&db, "n1").await.0,
        Some(brocade_store::NodeRemovalStatus::Failed)
    );
    assert_eq!(node_count(&db, "n1").await, 1);

    // Scheduling again after giving up starts a fresh attempt series.
    sqlx::query("DROP TRIGGER reject_node_delete ON nodes")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .schedule_node_removal(&system_admin(), "n1")
        .await
        .unwrap();
    assert_eq!(
        listed_removal(&db, "n1").await,
        (Some(brocade_store::NodeRemovalStatus::Queued), None)
    );
    let removed = db.store.process_node_removal().await.unwrap().unwrap();
    assert_eq!(removed.removed_nodes, ["n1"]);
    assert_eq!(node_count(&db, "n1").await, 0);
}
