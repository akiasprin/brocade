use super::*;
use brocade_deployment::protocol::ForwardingReconcileReport;
use brocade_store::{ForwardingEntitlementWrite, ForwardingQuotaWrite};

async fn fixture(db: &TestPg) -> (i64, i64, i64, i64) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','20111111-1111-4111-8111-111111111111')").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,tenant_id,name,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers)
        SELECT 'n2',tenant_id,'Second','10.66.0.2','private2','public2',51820,10085,true,true,dns_kind,dns_servers FROM nodes WHERE id='n1'")
        .execute(db.pool()).await.unwrap();
    for (user, node) in [("alice", "n1"), ("bob", "n1"), ("alice", "n2")] {
        db.store
            .set_forwarding_entitlement(
                &system_admin(),
                "platform.acme",
                user,
                node,
                serde_json::from_value::<ForwardingEntitlementWrite>(json!({"enabled":true}))
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let mut ids = Vec::new();
    for (user, node, port, enabled) in [
        ("alice", "n1", 40001, true),
        ("alice", "n1", 40002, false),
        ("bob", "n1", 40003, true),
        ("alice", "n2", 40004, true),
    ] {
        ids.push(sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port,enabled)
            VALUES('platform.acme',$1,$2,'quota test','tcp',$3,'example.net',443,$4) RETURNING id")
            .bind(user).bind(node).bind(port).bind(enabled).fetch_one(db.pool()).await.unwrap());
    }
    (ids[0], ids[1], ids[2], ids[3])
}

async fn usage(db: &TestPg, rule: i64, up: i64, down: i64) {
    sqlx::query("INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
        SELECT date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong',node_id,tenant_id,user_id,id,$2,$3,false
        FROM forwarding_rules WHERE id=$1").bind(rule).bind(up).bind(down).execute(db.pool()).await.unwrap();
}

async fn limit(db: &TestPg, value: Option<u64>) {
    db.store
        .set_forwarding_quota(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            ForwardingQuotaWrite { limit_bytes: value },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_jobs_pause_only_the_user_machine_and_preserve_manual_flags() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, manual, bob, other) = fixture(&db).await;
    usage(&db, active, 40, 60).await;
    usage(&db, bob, 1000, 1000).await;
    usage(&db, other, 1000, 1000).await;
    limit(&db, Some(100)).await;
    let original = db.store.forwarding_desired_for_node("n1").await.unwrap();
    let (a, b) = tokio::join!(
        db.store.enforce_forwarding_quotas(),
        db.store.enforce_forwarding_quotas()
    );
    assert_eq!(a.unwrap().suspended + b.unwrap().suspended, 1);
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        0
    );
    let after = db
        .store
        .forwarding_overview(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    let entry = after
        .entitlements
        .iter()
        .find(|e| e.node_id == "n1")
        .unwrap();
    assert_eq!(entry.cycle_used_bytes, 100);
    assert!(entry.quota_suspended);
    assert_eq!(entry.quota_job_status.as_deref(), Some("queued"));
    assert_eq!(
        after
            .rules
            .iter()
            .find(|r| r.id == active)
            .unwrap()
            .runtime_status,
        "quota-paused"
    );
    assert!(after.rules.iter().find(|r| r.id == active).unwrap().enabled);
    assert!(!after.rules.iter().find(|r| r.id == manual).unwrap().enabled);
    let paused = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(
        paused.rules.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![bob]
    );
    assert_eq!(
        db.store
            .forwarding_desired_for_node("n2")
            .await
            .unwrap()
            .rules[0]
            .id,
        other
    );
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                rule_results: None,
                generation: original.generation,
                applied_rule_ids: vec![active, bob],
                error: None,
            },
        )
        .await
        .unwrap();
    let jobs = db
        .store
        .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
        .await
        .unwrap();
    assert_eq!(jobs.jobs[0].status, "running");
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                rule_results: None,
                generation: paused.generation,
                applied_rule_ids: vec![active, bob],
                error: Some("remove failed; retrying".into()),
            },
        )
        .await
        .unwrap();
    assert!(db
        .store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                rule_results: None,
                generation: paused.generation,
                applied_rule_ids: vec![],
                error: None
            }
        )
        .await
        .is_err());
    let job = db
        .store
        .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
        .await
        .unwrap()
        .jobs
        .remove(0);
    assert_eq!(job.status, "running");
    assert!(job.last_error.is_some());
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                rule_results: None,
                generation: paused.generation,
                applied_rule_ids: vec![bob],
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
            .await
            .unwrap()
            .jobs[0]
            .status,
        "succeeded"
    );
    limit(&db, Some(1000)).await;
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
    let restored = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(
        restored.rules.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![active, bob]
    );
    let disabled: bool = sqlx::query_scalar("SELECT NOT enabled FROM forwarding_rules WHERE id=$1")
        .bind(manual)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(disabled);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_uses_the_users_single_or_double_accounting_mode() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, _, _, _) = fixture(&db).await;
    usage(&db, active, 40, 60).await;
    limit(&db, Some(150)).await;
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        0
    );

    db.store
        .update_user_profile(
            &system_admin(),
            "platform.acme",
            "alice",
            UpdateUserProfileRequest {
                account_type: UserAccountType::Formal,
                traffic_accounting_mode: Some(UserTrafficAccountingMode::Double),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        1
    );
    let overview = db
        .store
        .forwarding_overview(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    let entry = overview
        .entitlements
        .iter()
        .find(|entry| entry.node_id == "n1")
        .unwrap();
    assert_eq!(entry.cycle_used_bytes, 200);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_month_reset_deleted_history_and_superseded_jobs() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, _, _, _) = fixture(&db).await;
    usage(&db, active, 90, 10).await;
    sqlx::query("UPDATE forwarding_rules SET deleted_at=now() WHERE id=$1")
        .bind(active)
        .execute(db.pool())
        .await
        .unwrap();
    limit(&db, Some(100)).await;
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        1
    );
    let paused = db.store.forwarding_desired_for_node("n1").await.unwrap();
    sqlx::query("UPDATE forwarding_usage_rollups SET period_start=(date_trunc('month',now() AT TIME ZONE 'Asia/Hong_Kong')-interval '1 day') AT TIME ZONE 'Asia/Hong_Kong'").execute(db.pool()).await.unwrap();
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
    db.store
        .record_forwarding_reconcile(
            "n1",
            ForwardingReconcileReport {
                rule_results: None,
                generation: paused.generation,
                applied_rule_ids: vec![],
                error: None,
            },
        )
        .await
        .unwrap();
    let jobs = db
        .store
        .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
        .await
        .unwrap();
    assert_eq!(jobs.jobs[0].status, "queued");
    assert_eq!(jobs.jobs[1].status, "canceled");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM forwarding_usage_rollups"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        100
    );
    assert!(
        !db.store
            .forwarding_overview(&system_admin(), "platform.acme", "alice")
            .await
            .unwrap()
            .entitlements
            .iter()
            .find(|e| e.node_id == "n1")
            .unwrap()
            .quota_suspended
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_administration_limits_and_no_reenable_bypass() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, _, _, _) = fixture(&db).await;
    for role in [AdminRole::Readonly, AdminRole::Editor, AdminRole::Publisher] {
        let actor = AdminContext::new("viewer", role, Some("platform.acme".into()));
        assert!(matches!(
            db.store
                .set_forwarding_quota(
                    &actor,
                    "platform.acme",
                    "alice",
                    "n1",
                    ForwardingQuotaWrite {
                        limit_bytes: Some(100)
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    let outside = AdminContext::new("outside", AdminRole::TenantAdmin, Some("outside".into()));
    assert!(matches!(
        db.store
            .set_forwarding_quota(
                &outside,
                "platform.acme",
                "alice",
                "n1",
                ForwardingQuotaWrite {
                    limit_bytes: Some(100)
                }
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    for invalid in [0, u64::MAX] {
        assert!(matches!(
            db.store
                .set_forwarding_quota(
                    &system_admin(),
                    "platform.acme",
                    "alice",
                    "n1",
                    ForwardingQuotaWrite {
                        limit_bytes: Some(invalid)
                    }
                )
                .await,
            Err(StoreError::InvalidData(_))
        ));
    }
    usage(&db, active, 30, 70).await;
    limit(&db, Some(100)).await;
    let write = brocade_store::ForwardingRuleWrite {
        node_id: "n1".into(),
        name: "bypass".into(),
        protocol: brocade_deployment::protocol::ForwardingProtocol::Tcp,
        listen_port: 40005,
        target_host: "example.net".into(),
        target_port: 443,
        enabled: true,
    };
    assert!(matches!(
        db.store
            .create_forwarding_rule(&system_admin(), "platform.acme", "alice", write.clone())
            .await,
        Err(StoreError::Conflict(_))
    ));
    db.store.enforce_forwarding_quotas().await.unwrap();
    let mut disabled = write;
    disabled.enabled = false;
    db.store
        .create_forwarding_rule(&system_admin(), "platform.acme", "alice", disabled)
        .await
        .unwrap();
    limit(&db, None).await;
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_recovery_never_regrants_revoked_access_and_removal_preserves_audit() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, _, bob, _) = fixture(&db).await;
    usage(&db, active, 100, 0).await;
    limit(&db, Some(100)).await;
    db.store.enforce_forwarding_quotas().await.unwrap();
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            serde_json::from_value(json!({"enabled":false})).unwrap(),
        )
        .await
        .unwrap();
    limit(&db, None).await;
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
    assert_eq!(
        db.store
            .forwarding_desired_for_node("n1")
            .await
            .unwrap()
            .rules
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![bob]
    );
    db.store
        .delete_forwarding_entitlement(&system_admin(), "platform.acme", "alice", "n1")
        .await
        .unwrap();
    let jobs = db
        .store
        .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
        .await
        .unwrap();
    assert_eq!(jobs.jobs.len(), 2);
    assert!(jobs.jobs.iter().all(|j| j.status == "canceled"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM forwarding_usage_rollups"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        100
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM forwarding_node_quotas")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_job_history_is_bounded_and_scoped() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    for user in ["alice", "bob"] {
        for _ in 0..23 {
            sqlx::query("INSERT INTO jobs(kind,status,payload) VALUES('forwarding-quota','succeeded',$1)")
                .bind(json!({"tenant_id":"platform.acme","user_id":user,"node_id":"n1","generation":1,"suspended":true}))
                .execute(db.pool()).await.unwrap();
        }
    }
    let page = db
        .store
        .forwarding_quota_jobs(&system_admin(), "platform.acme", "alice", "n1", None)
        .await
        .unwrap();
    assert_eq!(page.jobs.len(), 20);
    let next = db
        .store
        .forwarding_quota_jobs(
            &system_admin(),
            "platform.acme",
            "alice",
            "n1",
            page.next_before_id,
        )
        .await
        .unwrap();
    assert_eq!(next.jobs.len(), 3);
    assert!(next.next_before_id.is_none());
    assert!(next
        .jobs
        .iter()
        .all(|j| j.id < page.jobs.last().unwrap().id));
    assert!(db
        .store
        .forwarding_quota_jobs(
            &AdminContext::new("outside", AdminRole::TenantAdmin, Some("outside".into())),
            "platform.acme",
            "alice",
            "n1",
            None
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_quota_compatibility_preserves_existing_rules_and_usage() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let (active, _, _, _) = fixture(&db).await;
    usage(&db, active, 40, 60).await;
    // Emulate the pre-quota schema only inside this isolated, disposable test database.
    sqlx::raw_sql("DROP TABLE forwarding_node_quotas; DROP INDEX forwarding_quota_jobs_by_owner; DROP INDEX forwarding_quota_jobs_pending_node;").execute(db.pool()).await.unwrap();
    let ddl = include_str!("../../../../scripts/compat-forwarding-quota.sql")
        .split_once("-- BEGIN FORWARDING QUOTA SCHEMA")
        .unwrap()
        .1
        .split_once("-- END FORWARDING QUOTA SCHEMA")
        .unwrap()
        .0;
    sqlx::raw_sql(ddl).execute(db.pool()).await.unwrap();
    let view = db
        .store
        .forwarding_overview(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    assert_eq!(view.rules.len(), 3);
    let entry = view
        .entitlements
        .iter()
        .find(|e| e.node_id == "n1")
        .unwrap();
    assert_eq!(entry.cycle_used_bytes, 100);
    assert!(entry.cycle_limit_bytes.is_none());
    assert!(!entry.quota_suspended);
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap(),
        brocade_store::ForwardingQuotaOutcome::default()
    );
}
