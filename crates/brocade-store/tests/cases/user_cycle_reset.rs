use super::*;
use brocade_store::{UserCycleResetRequest, UserCycleResetResult};

const TENANT: &str = "platform.acme";

async fn request(db: &TestPg, key: u8, previous: Option<i64>) -> UserCycleResetRequest {
    let periods = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    let period = periods
        .periods
        .iter()
        .find(|p| p.tenant_id == TENANT && p.user_id == "alice")
        .unwrap();
    UserCycleResetRequest {
        request_id: format!("{key:032x}"),
        expected_reset_id: previous,
        expected_period_start: period.starts_at.clone(),
        expected_period_end: period.ends_at.clone(),
    }
}

async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    // Keep the preceding two minutes inside the active cycle even at a month boundary.
    sqlx::query("UPDATE users SET traffic_reset_day=CASE WHEN extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')=1 THEN 2 ELSE 1 END")
        .execute(db.pool()).await.unwrap();
    seed_subscription_serving(db).await;
}

async fn used(db: &TestPg) -> i64 {
    sqlx::query_scalar("SELECT user_traffic_usage($1,'alice','access','app-main')")
        .bind(TENANT)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn reset(db: &TestPg, key: u8, previous: Option<i64>) -> UserCycleResetResult {
    db.store
        .reset_user_cycle_usage(
            &system_admin(),
            TENANT,
            "alice",
            request(db, key, previous).await,
        )
        .await
        .unwrap()
}

async fn add_raw(db: &TestPg, amount: i64) {
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
        SELECT $1,'alice','app-main',start,start+interval '24 hours',0,$2 FROM
          (SELECT date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong' AS start) day
        ON CONFLICT(tenant_id,user_id,app_id,period_start,period_end) DO UPDATE SET downlink_bytes=usage_rollups.downlink_bytes+EXCLUDED.downlink_bytes")
        .bind(TENANT).bind(amount).execute(db.pool()).await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_keeps_real_reports_heads_history_and_subscription_consistent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let base = usage_report_base_unix();
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), base - 60).await;
    db.store
        .record_usage_report(
            "n1",
            current_usage(generation, 1, base, "reset:stable", 100, 200),
        )
        .await
        .unwrap();
    db.store
        .record_usage_report(
            "n1",
            current_usage(generation, 2, base + 30, "reset:stable", 150, 280),
        )
        .await
        .unwrap();
    assert_eq!(used(&db).await, 130);
    let audit_sql = "SELECT json_build_object(
      'rollups',(SELECT json_agg(t ORDER BY period_start) FROM usage_rollups t),
      'heads',(SELECT json_agg(t ORDER BY node_id,label) FROM usage_counter_heads t),
      'readings',(SELECT json_agg(t ORDER BY id) FROM usage_readings t),
      'samples',(SELECT json_agg(t ORDER BY id) FROM usage_samples t))";
    let before: serde_json::Value = sqlx::query_scalar(audit_sql)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let first = reset(&db, 1, None).await;
    assert!(!first.duplicate);
    assert_eq!(used(&db).await, 0);
    let after: serde_json::Value = sqlx::query_scalar(audit_sql)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "reset must not rewrite any billing fact or counter head"
    );
    let cycle = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(
        cycle.views[0].uplink_bytes + cycle.views[0].downlink_bytes,
        0
    );
    assert_eq!(cycle.periods[0].reset_id, Some(first.reset_id));
    assert!(cycle.periods[0].reset_at.is_some());
    let sub = db
        .store
        .clash_subscription_by_uuid("2d2304da-f114-4574-8d44-625afdb1db5c")
        .await
        .unwrap();
    assert_eq!(sub.usage.upload_bytes + sub.usage.download_bytes, 0);
    // This real cumulative report was measured before reset but delivered afterwards.
    let late = current_usage(generation, 3, base + 60, "reset:stable", 170, 300);
    db.store
        .record_usage_report("n1", late.clone())
        .await
        .unwrap();
    assert!(
        db.store
            .record_usage_report("n1", late)
            .await
            .unwrap()
            .duplicate
    );
    assert_eq!(used(&db).await, 0);
    let raw: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM usage_rollups")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(raw, 170, "late historical bytes remain in the real ledger");
    add_raw(&db, 25).await;
    assert_eq!(used(&db).await, 25);
    let retry = reset(&db, 1, None).await;
    assert!(retry.duplicate);
    assert_eq!(retry.reset_id, first.reset_id);
    assert_eq!(
        used(&db).await,
        25,
        "a lost-response retry must not reset new traffic"
    );
    let second = reset(&db, 2, Some(first.reset_id)).await;
    assert_eq!(used(&db).await, 0);
    assert!(second.reset_id > first.reset_id);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_enforces_scope_and_serializes_concurrent_requests() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    add_raw(&db, 100).await;
    assert!(matches!(
        db.store
            .reset_user_cycle_usage(
                &publisher("platform.other"),
                TENANT,
                "alice",
                request(&db, 1, None).await
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let readonly = AdminContext::new("reader", AdminRole::Readonly, None);
    assert!(matches!(
        db.store
            .reset_user_cycle_usage(&readonly, TENANT, "alice", request(&db, 1, None).await)
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let actor = system_admin();
    let (a, b) = tokio::join!(
        db.store
            .reset_user_cycle_usage(&actor, TENANT, "alice", request(&db, 1, None).await),
        db.store
            .reset_user_cycle_usage(&actor, TENANT, "alice", request(&db, 2, None).await)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(StoreError::Conflict(_))) || matches!(b, Err(StoreError::Conflict(_))));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_cycle_resets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(used(&db).await, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_splits_measured_windows_and_survives_raw_retention() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let base = usage_report_base_unix();
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), base - 60).await;
    db.store
        .record_usage_report(
            "n1",
            current_usage(generation, 1, base, "reset:stable", 0, 0),
        )
        .await
        .unwrap();
    let first = reset(&db, 1, None).await;
    // Fixture-controlled cutoff, halfway through a real 30-second cumulative report window.
    sqlx::query("UPDATE user_cycle_resets SET reset_at=to_timestamp($2) WHERE id=$1")
        .bind(first.reset_id)
        .bind((base + 15) as f64)
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .record_usage_report(
            "n1",
            current_usage(generation, 2, base + 30, "reset:stable", 30, 60),
        )
        .await
        .unwrap();
    assert_eq!(used(&db).await, 45);
    sqlx::query("DELETE FROM usage_samples")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        used(&db).await,
        45,
        "quota calculations cannot depend on expiring raw samples"
    );
    sqlx::query(
        "UPDATE users SET traffic_accounting_mode='double' WHERE tenant_id=$1 AND id='alice'",
    )
    .bind(TENANT)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        used(&db).await,
        90,
        "the current accounting mode applies after the raw baseline"
    );
    // A future natural cycle ignores this earlier reset.
    let next:(i64,i64)=sqlx::query_as("SELECT extract(epoch FROM ends_at)::bigint,extract(epoch FROM ends_at+interval '1 day')::bigint FROM users u CROSS JOIN user_traffic_cycle(u.traffic_reset_day,now()) WHERE u.id='alice'")
        .fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes) VALUES($1,'alice','app-main',to_timestamp($2),to_timestamp($3),0,50)")
        .bind(TENANT).bind(next.0 as f64).bind(next.1 as f64).execute(db.pool()).await.unwrap();
    let next_used:i64=sqlx::query_scalar("SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM user_cycle_access_usage($1,'alice',to_timestamp($2),to_timestamp($3))")
        .bind(TENANT).bind(next.0 as f64).bind(next.1 as f64).fetch_one(db.pool()).await.unwrap();
    assert_eq!(next_used, 50);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_preserves_forwarding_history_and_restores_only_quota_suspensions() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            serde_json::from_value(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    let rule:i64=sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
        VALUES($1,'alice','n1','cycle reset','tcp',40001,'1.1.1.1',443) RETURNING id")
        .bind(TENANT).fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
        VALUES(date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong','n1',$1,'alice',$2,40,160,false)")
        .bind(TENANT).bind(rule).execute(db.pool()).await.unwrap();
    add_raw(&db, 200).await;
    set_quota(&db, Some(100)).await;
    db.store
        .set_forwarding_quota(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            brocade_store::ForwardingQuotaWrite {
                limit_bytes: Some(100),
            },
        )
        .await
        .unwrap();
    db.store.enforce_quotas().await.unwrap();
    db.store.enforce_forwarding_quotas().await.unwrap();
    reset(&db, 1, None).await;
    assert_eq!(db.store.enforce_quotas().await.unwrap().restored, 1);
    assert_eq!(
        db.store.enforce_forwarding_quotas().await.unwrap().restored,
        1
    );
    let cycle = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(
        cycle.forwarding[0].uplink_bytes + cycle.forwarding[0].downlink_bytes,
        0
    );
    let raw: i64 = sqlx::query_scalar(
        "SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM forwarding_usage_rollups",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(raw, 200);
    let rules: bool = sqlx::query_scalar("SELECT enabled FROM forwarding_rules WHERE id=$1")
        .bind(rule)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(rules);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_handles_real_forwarding_late_reports_and_crossing_windows() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            serde_json::from_value(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    let rule:i64=sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
        VALUES($1,'alice','n1','real reset','tcp',40001,'1.1.1.1',443) RETURNING id")
        .bind(TENANT).fetch_one(db.pool()).await.unwrap();
    let base = usage_report_base_unix();
    let report = |sequence, at, bytes| brocade_store::ForwardingUsageReportRequest {
        agent_instance_id: "forwarding-reset".into(),
        sequence,
        read_at_unix_secs: at,
        xray_epoch: "stable-reset".into(),
        counters: vec![brocade_store::ForwardingUsageCounter {
            rule_id: rule,
            uplink_bytes: 0,
            downlink_bytes: bytes,
        }],
    };
    db.store
        .record_forwarding_usage("n1", report(1, base, 0))
        .await
        .unwrap();
    db.store
        .record_forwarding_usage("n1", report(2, base + 30, 100))
        .await
        .unwrap();
    let first = reset(&db, 1, None).await;
    let late = report(3, base + 60, 130);
    db.store
        .record_forwarding_usage("n1", late.clone())
        .await
        .unwrap();
    assert!(
        db.store
            .record_forwarding_usage("n1", late)
            .await
            .unwrap()
            .duplicate
    );
    let cycle = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(cycle.forwarding[0].downlink_bytes, 0);
    sqlx::query("UPDATE user_cycle_resets SET reset_at=to_timestamp($2) WHERE id=$1")
        .bind(first.reset_id)
        .bind((base + 75) as f64)
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .record_forwarding_usage("n1", report(4, base + 90, 160))
        .await
        .unwrap();
    let cycle = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(cycle.forwarding[0].downlink_bytes, 15);
    let raw: i64 =
        sqlx::query_scalar("SELECT sum(downlink_bytes)::bigint FROM forwarding_usage_rollups")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(raw, 160);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_report_race_has_no_lost_or_double_counted_bytes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let base = usage_report_base_unix();
    let generation =
        install_usage_generation(db.pool(), "n1", alice_usage_bindings(), base - 60).await;
    db.store
        .record_usage_report(
            "n1",
            current_usage(generation, 1, base, "race:stable", 0, 0),
        )
        .await
        .unwrap();
    let actor = system_admin();
    let (reset, report) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            db.store
                .reset_user_cycle_usage(&actor, TENANT, "alice", request(&db, 1, None).await),
            db.store.record_usage_report(
                "n1",
                current_usage(generation, 2, base + 30, "race:stable", 100, 200)
            )
        )
    })
    .await
    .expect("reset and billing must not deadlock");
    reset.unwrap();
    report.unwrap();
    assert_eq!(used(&db).await, 0);
    let raw: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM usage_rollups")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(raw, 300);
    let head: (i64, i64) = sqlx::query_as(
        "SELECT uplink_bytes,downlink_bytes FROM usage_counter_heads WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(head, (100, 200));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_survives_changing_reset_day_without_resurrecting_old_usage() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    add_raw(&db, 100).await;
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
      SELECT $1,'alice','app-main',start-interval '1 day',start,0,300 FROM
        (SELECT date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong' AS start) day")
      .bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE users SET traffic_reset_day=extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')::int WHERE tenant_id=$1 AND id='alice'")
      .bind(TENANT).execute(db.pool()).await.unwrap();
    assert_eq!(used(&db).await, 100);
    reset(&db, 1, None).await;
    sqlx::query("UPDATE users SET traffic_reset_day=CASE WHEN extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')=1 THEN 2 ELSE 1 END WHERE tenant_id=$1 AND id='alice'")
      .bind(TENANT).execute(db.pool()).await.unwrap();
    assert_eq!(used(&db).await, 0);
    add_raw(&db, 25).await;
    assert_eq!(used(&db).await, 25);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_rejects_stale_periods_and_invalid_or_unprivileged_requests() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    add_raw(&db, 100).await;
    let planned = request(&db, 1, None).await;
    assert!(matches!(
        db.store
            .reset_user_cycle_usage(&ordinary_user(TENANT), TENANT, "alice", planned.clone())
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let mut invalid = planned.clone();
    invalid.request_id = "not-a-reset-request".into();
    assert!(matches!(
        db.store
            .reset_user_cycle_usage(&system_admin(), TENANT, "alice", invalid)
            .await,
        Err(StoreError::InvalidData(_))
    ));
    sqlx::query("UPDATE users SET traffic_reset_day=extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')::int WHERE tenant_id=$1 AND id='alice'")
      .bind(TENANT).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store
            .reset_user_cycle_usage(&system_admin(), TENANT, "alice", planned)
            .await,
        Err(StoreError::Conflict(_))
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM user_cycle_resets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(used(&db).await, 100);
    let fresh = request(&db, 2, None).await;
    db.store
        .reset_user_cycle_usage(&system_admin(), TENANT, "alice", fresh.clone())
        .await
        .unwrap();
    add_raw(&db, 25).await;
    sqlx::query("UPDATE users SET traffic_reset_day=CASE WHEN extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')=1 THEN 2 ELSE 1 END WHERE tenant_id=$1 AND id='alice'")
      .bind(TENANT).execute(db.pool()).await.unwrap();
    let replay = db
        .store
        .reset_user_cycle_usage(&system_admin(), TENANT, "alice", fresh)
        .await
        .unwrap();
    assert!(
        replay.duplicate,
        "a replay remains a replay after a billing-policy change"
    );
    assert_eq!(used(&db).await, 25);
}

async fn wait_for_cycle_control_waiters(pool: &PgPool, minimum: i64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname=current_database() AND pid<>pg_backend_pid()
                   AND wait_event_type='Lock' AND query LIKE '%FROM control_state%'",
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
    .expect("reset and quota workers must reach the control lock");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_invalidates_a_waiting_access_suspension_plan() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    add_raw(&db, 200).await;
    set_quota(&db, Some(100)).await;
    let planned = request(&db, 10, None).await;
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("SELECT current_revision FROM control_state WHERE id=TRUE FOR UPDATE")
        .execute(&mut *gate)
        .await
        .unwrap();
    let store = db.store.clone();
    let reset = tokio::spawn(async move {
        store
            .reset_user_cycle_usage(&system_admin(), TENANT, "alice", planned)
            .await
    });
    wait_for_cycle_control_waiters(db.pool(), 1).await;
    let store = db.store.clone();
    let worker = tokio::spawn(async move { store.enforce_quotas().await });
    wait_for_cycle_control_waiters(db.pool(), 2).await;
    gate.commit().await.unwrap();
    let (reset, outcome) = tokio::time::timeout(Duration::from_secs(5), async {
        (
            reset.await.unwrap().unwrap(),
            worker.await.unwrap().unwrap(),
        )
    })
    .await
    .expect("reset and access enforcement must not deadlock");
    assert!(!reset.duplicate);
    assert_eq!(used(&db).await, 0);
    assert_eq!(
        outcome.suspended, 0,
        "the pre-reset stop plan must be discarded"
    );
    assert_eq!(live_grants(db.pool()).await, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM quota_suspensions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    assert!(outcome.deployment_id.is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_rechecks_access_restoration_after_new_usage() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    add_raw(&db, 200).await;
    set_quota(&db, Some(100)).await;
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    reset(&db, 11, None).await;
    assert_eq!(used(&db).await, 0);
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("SELECT current_revision FROM control_state WHERE id=TRUE FOR UPDATE")
        .execute(&mut *gate)
        .await
        .unwrap();
    let store = db.store.clone();
    let worker = tokio::spawn(async move { store.enforce_quotas().await });
    wait_for_cycle_control_waiters(db.pool(), 1).await;
    add_raw(&db, 200).await;
    gate.commit().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("access enforcement must finish")
        .unwrap()
        .unwrap();
    assert_eq!(used(&db).await, 200);
    assert_eq!(
        outcome.restored, 0,
        "newly exhausted usage must invalidate the old restore plan"
    );
    assert_eq!(live_grants(db.pool()).await, 0);
    let markers: i64 = sqlx::query_scalar("SELECT count(*) FROM quota_suspensions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        markers, 1,
        "the quota worker must retain ownership of the suspension"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_reset_invalidates_a_waiting_forwarding_suspension_plan() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .set_forwarding_entitlement(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            serde_json::from_value(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    let rule: i64 = sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
        VALUES($1,'alice','n1','reset concurrency','tcp',40001,'1.1.1.1',443) RETURNING id")
        .bind(TENANT).fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
        SELECT starts_at,'n1',$1,'alice',$2,0,200,false FROM users u CROSS JOIN user_traffic_cycle(u.traffic_reset_day,now()) c WHERE u.tenant_id=$1 AND u.id='alice'")
        .bind(TENANT).bind(rule).execute(db.pool()).await.unwrap();
    db.store
        .set_forwarding_quota(
            &system_admin(),
            TENANT,
            "alice",
            "n1",
            brocade_store::ForwardingQuotaWrite {
                limit_bytes: Some(100),
            },
        )
        .await
        .unwrap();
    let generation: i64 = sqlx::query_scalar(
        "SELECT desired_generation FROM forwarding_node_state WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let planned = request(&db, 12, None).await;
    let mut gate = db.pool().begin().await.unwrap();
    sqlx::query("SELECT current_revision FROM control_state WHERE id=TRUE FOR UPDATE")
        .execute(&mut *gate)
        .await
        .unwrap();
    let store = db.store.clone();
    let reset = tokio::spawn(async move {
        store
            .reset_user_cycle_usage(&system_admin(), TENANT, "alice", planned)
            .await
    });
    wait_for_cycle_control_waiters(db.pool(), 1).await;
    let store = db.store.clone();
    let worker = tokio::spawn(async move { store.enforce_forwarding_quotas().await });
    wait_for_cycle_control_waiters(db.pool(), 2).await;
    gate.commit().await.unwrap();
    let (_, outcome) = tokio::time::timeout(Duration::from_secs(5), async {
        (
            reset.await.unwrap().unwrap(),
            worker.await.unwrap().unwrap(),
        )
    })
    .await
    .expect("reset and forwarding enforcement must not deadlock");
    assert_eq!(outcome.suspended, 0);
    let state: (bool, Option<i64>) =
        sqlx::query_as("SELECT suspended,last_job_id FROM forwarding_node_quotas")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, (false, None));
    let after: i64 = sqlx::query_scalar(
        "SELECT desired_generation FROM forwarding_node_state WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        generation, after,
        "a stale stop must not produce node desired state"
    );
}
