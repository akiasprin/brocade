use super::*;
use brocade_store::{ForwardingEntitlementWrite, UserServicePolicyWrite, UserServiceState};

const TENANT: &str = "platform.acme";

fn policy(day: u8, start: Option<&str>, end: Option<&str>) -> UserServicePolicyWrite {
    UserServicePolicyWrite {
        traffic_reset_day: day,
        service_starts_at: start.map(str::to_owned),
        service_expires_at: end.map(str::to_owned),
    }
}

async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    seed_subscription_serving(db).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_rollback_keeps_expiry_and_configured_grants_for_renewal() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let target: i64 = sqlx::query_scalar(
        "INSERT INTO deployments(revision_id,status,activation_status)
        SELECT current_revision,'succeeded','activated' FROM control_state RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(15, None, Some("2020-01-01T00:00:00Z")),
        )
        .await
        .unwrap();
    db.store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(target, "service-expiry-rollback"),
        )
        .await
        .unwrap();
    assert_eq!(live_grants(db.pool()).await, 1);
    let user = db
        .store
        .user_profile(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(user.status, "active");
    assert_eq!(user.service_state, UserServiceState::Expired);
    assert_eq!(user.traffic_reset_day, 15);
    assert!(db
        .store
        .materialize_snapshot(None)
        .await
        .unwrap()
        .users
        .is_empty());
    db.store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(15, None, None))
        .await
        .unwrap();
    assert_eq!(
        db.store.materialize_snapshot(None).await.unwrap().apps[0]
            .grants
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_access_and_forwarding_share_cycle_and_double_accounting() {
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
            serde_json::from_value::<ForwardingEntitlementWrite>(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    let rule:i64=sqlx::query_scalar("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
        VALUES('platform.acme','alice','n1','cycle test','tcp',40001,'1.1.1.1',443) RETURNING id").fetch_one(db.pool()).await.unwrap();
    let day: i32 =
        sqlx::query_scalar("SELECT extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')::integer")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let day = u8::try_from(day).unwrap();
    db.store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(day, None, None))
        .await
        .unwrap();
    for (offset, amount) in [(0, 60), (-1, 1000)] {
        sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
            SELECT 'platform.acme','alice','app-main',starts_at+$2*interval '1 day',starts_at+($2+1)*interval '1 day',0,$3
            FROM user_traffic_cycle($1,now())").bind(i32::from(day)).bind(offset).bind(i64::from(amount)).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
            SELECT starts_at+$2*interval '1 day','n1','platform.acme','alice',$4,0,$3,false FROM user_traffic_cycle($1,now())")
            .bind(i32::from(day)).bind(offset).bind(i64::from(amount)).bind(rule).execute(db.pool()).await.unwrap();
    }
    let preview = db
        .store
        .preview_user_service_policy(&system_admin(), TENANT, "alice", &policy(day, None, None))
        .await
        .unwrap();
    assert_eq!(
        (preview.access_used_bytes, preview.forwarding_used_bytes),
        (60, 60)
    );
    let usage = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(usage.views[0].downlink_bytes, 60);
    assert_eq!(usage.forwarding[0].downlink_bytes, 60);
    assert!(db
        .store
        .list_user_cycle_usage(&publisher("platform.other"))
        .await
        .unwrap()
        .periods
        .is_empty());
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
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 0);
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
            TENANT,
            "alice",
            UpdateUserProfileRequest {
                account_type: UserAccountType::Formal,
                traffic_accounting_mode: Some(UserTrafficAccountingMode::Double),
            },
        )
        .await
        .unwrap();
    let preview = db
        .store
        .preview_user_service_policy(&system_admin(), TENANT, "alice", &policy(day, None, None))
        .await
        .unwrap();
    assert_eq!(
        (preview.access_used_bytes, preview.forwarding_used_bytes),
        (120, 120)
    );
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    assert_eq!(
        db.store
            .enforce_forwarding_quotas()
            .await
            .unwrap()
            .suspended,
        1
    );
    let view = db
        .store
        .forwarding_overview(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(view.entitlements[0].cycle_used_bytes, 120);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_0002_upgrade_preserves_users_usage_and_initial_migration() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut initial = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .unwrap();
    assert!(initial.iter().any(|migration| migration.version == 2));
    initial.migrations = std::borrow::Cow::Owned(
        initial
            .iter()
            .filter(|migration| migration.version == 1)
            .cloned()
            .collect(),
    );
    initial.run(db.pool()).await.unwrap();
    let before: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id='alice'")
        .execute(db.pool())
        .await
        .unwrap();
    insert_month_sample(db.pool(), "ing-a1b2", "app-main", 1, 40, 60).await;
    db.store.migrate().await.unwrap();
    let after: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before);
    let user = db
        .store
        .user_profile(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(user.status, "disabled");
    assert_eq!(user.traffic_reset_day, 1);
    assert_eq!(user.service_state, UserServiceState::Active);
    let total: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM usage_rollups")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(total, 100);
    db.store.migrate().await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_subscriptions_use_cycle_and_real_expiry_and_reject_expired_serving_credentials(
) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(15, None, Some("2099-01-01T00:00:00Z")),
        )
        .await
        .unwrap();
    let subscription = db
        .store
        .clash_subscription_by_uuid("2d2304da-f114-4574-8d44-625afdb1db5c")
        .await
        .unwrap();
    let preview = db
        .store
        .preview_user_service_policy(&system_admin(), TENANT, "alice", &policy(15, None, None))
        .await
        .unwrap();
    let reset: i64 = sqlx::query_scalar("SELECT extract(epoch FROM $1::timestamptz)::bigint")
        .bind(&subscription.usage.reset_at)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let end: i64 = sqlx::query_scalar("SELECT extract(epoch FROM $1::timestamptz)::bigint")
        .bind(&preview.ends_at)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(reset, end);
    assert_eq!(
        subscription.usage.expires_at.as_deref(),
        Some("2099-01-01T00:00:00Z")
    );
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(15, None, Some("2020-01-01T00:00:00Z")),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            db.store
                .clash_subscription_by_uuid("2d2304da-f114-4574-8d44-625afdb1db5c")
                .await,
            Err(StoreError::NotFound(_))
        ),
        "a stale serving snapshot must not expose expired credentials"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_cycles_clamp_each_month_and_use_exact_utc8_boundaries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    for (day, now, start, end) in [
        (31, "2026-02-28T00:00:00+08:00", "2026-02-28", "2026-03-31"),
        (31, "2026-03-30T23:59:59+08:00", "2026-02-28", "2026-03-31"),
        (31, "2026-03-31T00:00:00+08:00", "2026-03-31", "2026-04-30"),
        (31, "2028-02-29T00:00:00+08:00", "2028-02-29", "2028-03-31"),
        (15, "2026-01-14T23:59:59+08:00", "2025-12-15", "2026-01-15"),
        (15, "2026-01-15T00:00:00+08:00", "2026-01-15", "2026-02-15"),
    ] {
        let got: (String,String) = sqlx::query_as("SELECT to_char(starts_at AT TIME ZONE 'Asia/Hong_Kong','YYYY-MM-DD'),
            to_char(ends_at AT TIME ZONE 'Asia/Hong_Kong','YYYY-MM-DD') FROM user_traffic_cycle($1,$2::timestamptz)")
            .bind(day).bind(now).fetch_one(db.pool()).await.unwrap();
        assert_eq!(
            got,
            (start.to_owned(), end.to_owned()),
            "day {day}, at {now}"
        );
    }
    let states: (String, String, String) = sqlx::query_as(
        "SELECT
        user_service_state('2026-01-01T00:00:00Z','2026-02-01T00:00:00Z','2025-12-31T00:00:00Z'),
        user_service_state('2026-01-01T00:00:00Z','2026-02-01T00:00:00Z','2026-01-01T00:00:00Z'),
        user_service_state('2026-01-01T00:00:00Z','2026-02-01T00:00:00Z','2026-02-01T00:00:00Z')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        states,
        ("pending".into(), "active".into(), "expired".into())
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_policy_validates_defaults_scope_and_preview_without_mutating_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let user = db
        .store
        .user_profile(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(user.traffic_reset_day, 1);
    assert_eq!(user.service_state, UserServiceState::Active);
    assert!(user.service_expires_at.is_none());
    let revisions = revision_count(db.pool()).await;
    let preview = db
        .store
        .preview_user_service_policy(&system_admin(), TENANT, "alice", &policy(15, None, None))
        .await
        .unwrap();
    assert_eq!(preview.access_used_bytes, 0);
    assert_eq!(revision_count(db.pool()).await, revisions);
    for invalid in [
        policy(0, None, None),
        policy(32, None, None),
        policy(1, Some("not a date"), None),
        policy(1, None, Some("2026-02-30T00:00:00Z")),
        policy(1, None, Some("2026-02-20T00:00:00")),
        policy(
            1,
            Some("2026-03-01T00:00:00Z"),
            Some("2026-03-01T00:00:00Z"),
        ),
    ] {
        assert!(matches!(
            db.store
                .update_user_service_policy(&system_admin(), TENANT, "alice", invalid)
                .await,
            Err(StoreError::InvalidData(_))
        ));
    }
    assert!(matches!(
        db.store
            .update_user_service_policy(
                &publisher("platform.other"),
                TENANT,
                "alice",
                policy(15, None, None)
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let updated = db
        .store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(15, None, None))
        .await
        .unwrap();
    assert_eq!(updated.traffic_reset_day, 15);
    assert_eq!(
        revision_count(db.pool()).await,
        revisions,
        "reset-day-only edits are operational"
    );
    db.store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(15, None, None))
        .await
        .unwrap();
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM user_service_policy_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(audit, 1, "unchanged writes are idempotent");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_expiry_and_renewal_preserve_grants_rules_and_manual_disable() {
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
            serde_json::from_value::<ForwardingEntitlementWrite>(json!({"enabled":true})).unwrap(),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port,enabled)
        VALUES('platform.acme','alice','n1','active','tcp',40001,'1.1.1.1',443,true),
              ('platform.acme','alice','n1','manual','tcp',40002,'1.1.1.1',443,false)")
        .execute(db.pool()).await.unwrap();
    let before = db.store.forwarding_desired_for_node("n1").await.unwrap();
    let expired = db
        .store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(1, None, Some("2020-01-01T00:00:00Z")),
        )
        .await
        .unwrap();
    assert_eq!(expired.service_state, UserServiceState::Expired);
    assert_eq!(expired.status, "active");
    assert_eq!(
        live_grants(db.pool()).await,
        1,
        "service expiry keeps configured grants"
    );
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(snapshot.users.is_empty());
    assert!(snapshot.apps[0].grants.is_empty());
    let paused = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert!(paused.generation > before.generation);
    assert!(paused.rules.is_empty());
    let view = db
        .store
        .forwarding_overview(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert_eq!(view.rules[0].runtime_status, "service-paused");
    assert_eq!(view.rules[1].runtime_status, "disabled");
    assert_eq!(db.store.enforce_user_service_periods().await.unwrap(), 0);
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind='grants-deployment' AND status='queued'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(jobs > 0, "expiry and its grant outbox commit together");
    db.store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(1, None, None))
        .await
        .unwrap();
    assert_eq!(
        db.store
            .materialize_snapshot(None)
            .await
            .unwrap()
            .users
            .len(),
        1
    );
    let restored = db.store.forwarding_desired_for_node("n1").await.unwrap();
    assert_eq!(
        restored.rules.len(),
        1,
        "manual rule disable survives renewal"
    );
    db.store
        .update_user_status(
            &system_admin(),
            TENANT,
            "alice",
            UpdateUserStatusRequest {
                status: "disabled".into(),
                note: None,
            },
        )
        .await
        .unwrap();
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(1, None, Some("2020-01-01T00:00:00Z")),
        )
        .await
        .unwrap();
    db.store
        .update_user_service_policy(&system_admin(), TENANT, "alice", policy(1, None, None))
        .await
        .unwrap();
    assert!(db
        .store
        .materialize_snapshot(None)
        .await
        .unwrap()
        .users
        .is_empty());
    assert!(db
        .store
        .forwarding_desired_for_node("n1")
        .await
        .unwrap()
        .rules
        .is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_scheduler_recovers_due_boundaries_and_is_idempotent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(1, Some("2099-01-01T00:00:00Z"), None),
        )
        .await
        .unwrap();
    assert!(db
        .store
        .materialize_snapshot(None)
        .await
        .unwrap()
        .users
        .is_empty());
    // Emulate a boundary passed while Console was stopped, without a timing-sensitive sleep.
    sqlx::query("UPDATE users SET service_starts_at=now()-interval '1 day' WHERE id='alice'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(db.store.enforce_user_service_periods().await.unwrap(), 1);
    assert_eq!(
        db.store
            .materialize_snapshot(None)
            .await
            .unwrap()
            .users
            .len(),
        1
    );
    sqlx::query("UPDATE users SET service_expires_at=now()-interval '1 second' WHERE id='alice'")
        .execute(db.pool())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        db.store.enforce_user_service_periods(),
        db.store.enforce_user_service_periods()
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert!(db
        .store
        .materialize_snapshot(None)
        .await
        .unwrap()
        .users
        .is_empty());
    assert_eq!(db.store.enforce_user_service_periods().await.unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_service_reset_policy_reuses_ledgers_and_keeps_expiry_when_quota_recovers() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    set_quota(&db, Some(100)).await;
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
        SELECT 'platform.acme','alice','app-main',starts_at,starts_at+interval '1 day',40,60 FROM user_traffic_cycle(1,now())")
        .execute(db.pool()).await.unwrap();
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    let day: i32 =
        sqlx::query_scalar("SELECT extract(day FROM now() AT TIME ZONE 'Asia/Hong_Kong')::integer")
            .fetch_one(db.pool())
            .await
            .unwrap();
    // This fixture is independent of the wall date: move the ledger to one day before the
    // chosen cycle, retaining the exact same recorded bytes.
    sqlx::query(
        "UPDATE usage_rollups SET period_start=c.starts_at-interval '1 day',period_end=c.starts_at
        FROM user_traffic_cycle($1,now()) c",
    )
    .bind(day)
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .update_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            policy(
                u8::try_from(day).unwrap(),
                None,
                Some("2020-01-01T00:00:00Z"),
            ),
        )
        .await
        .unwrap();
    assert_eq!(db.store.enforce_quotas().await.unwrap().restored, 1);
    assert!(
        db.store
            .materialize_snapshot(None)
            .await
            .unwrap()
            .users
            .is_empty(),
        "quota recovery cannot bypass expiry"
    );
    let usage = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert!(usage.views.is_empty());
    assert_eq!(usage.periods.len(), 1);
    let total: i64 =
        sqlx::query_scalar("SELECT sum(uplink_bytes+downlink_bytes)::bigint FROM usage_rollups")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(total, 100);
    let preview = db
        .store
        .preview_user_service_policy(
            &system_admin(),
            TENANT,
            "alice",
            &policy(u8::try_from(day).unwrap(), None, None),
        )
        .await
        .unwrap();
    assert_eq!(preview.access_used_bytes, 0);
}
