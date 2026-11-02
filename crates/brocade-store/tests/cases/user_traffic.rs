use super::*;
use brocade_deployment::user_traffic::*;
const TENANT: &str = "platform.acme";
async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    seed_subscription_serving(db).await;
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||'{\"id\":\"n2\",\"name\":\"Node 2\",\"public_ipv4\":\"n2.example.net\",\"overlay_addr\":\"10.66.0.2\",\"wg_public_key\":\"wg-public2\"}'::jsonb)).* FROM nodes n WHERE id='n1'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO ingresses SELECT (jsonb_populate_record(NULL::ingresses,to_jsonb(i)||'{\"id\":\"ing-n2\",\"node_id\":\"n2\"}'::jsonb)).* FROM ingresses i WHERE id='ing-a1b2'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-main','platform.acme','alice','ing-n2')").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE nodes SET user_traffic_supported=true,user_traffic_support_at=now()")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) SELECT id,44 FROM nodes ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=44").execute(db.pool()).await.unwrap();
}
fn policy(quota: u64, rate: Option<u64>) -> UserTrafficPolicy {
    UserTrafficPolicy {
        base_rate_bps: None,
        quota_bytes: Some(quota.to_string()),
        exhausted_action: rate.map_or(ExhaustedAction::Suspend, |rate_bps| {
            ExhaustedAction::Throttle { rate_bps }
        }),
    }
}
async fn usage(db: &TestPg, bytes: i64) {
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
  SELECT 'platform.acme','alice','app-main',starts_at,starts_at+interval '1 day',0,$1 FROM user_traffic_cycle(1,now())").bind(bytes).execute(db.pool()).await.unwrap();
}
fn request(source: &str, id: u8) -> UserBudgetRequest {
    UserBudgetRequest {
        source: source.into(),
        request_id: format!("{id:032x}"),
        bytes: 16 * 1024 * 1024,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_app_quotas_keep_usage_budgets_suspension_and_recovery_independent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("DELETE FROM ingresses WHERE node_id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET retired_at=now() WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO apps(id,label,position) VALUES('app-other','Other app',1)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO chains SELECT (jsonb_populate_record(NULL::chains,to_jsonb(c)||'{\"id\":\"chain-other\",\"app_id\":\"app-other\",\"name\":\"Other Chain\"}'::jsonb)).* FROM chains c WHERE app_id='app-main'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO steps SELECT (jsonb_populate_record(NULL::steps,to_jsonb(s)||'{\"chain_id\":\"chain-other\"}'::jsonb)).* FROM steps s JOIN chains c ON c.id=s.chain_id WHERE c.app_id='app-main'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO ingresses SELECT (jsonb_populate_record(NULL::ingresses,to_jsonb(i)||'{\"id\":\"ing-other\",\"app_id\":\"app-other\",\"chain_id\":\"chain-other\",\"port\":24443}'::jsonb)).* FROM ingresses i WHERE id='ing-a1b2'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO ingress_client_settings SELECT (jsonb_populate_record(NULL::ingress_client_settings,to_jsonb(s)||'{\"ingress_id\":\"ing-other\"}'::jsonb)).* FROM ingress_client_settings s WHERE ingress_id='ing-a1b2'").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-other',$1,'alice','ing-other')").bind(TENANT).execute(db.pool()).await.unwrap();
    for (app, rate) in [("app-main", 8000), ("app-other", 16000)] {
        db.store
            .update_user_traffic_policy(
                &system_admin(),
                TENANT,
                "alice",
                UserTrafficKind::Access,
                Some(app),
                policy(1024, Some(rate)),
            )
            .await
            .unwrap();
    }
    usage(&db, 1024).await;
    db.store.enforce_user_traffic_policies().await.unwrap();
    let views = db
        .store
        .user_traffic_policies(&system_admin(), TENANT, "alice")
        .await
        .unwrap();
    assert!(
        views
            .iter()
            .find(|v| v.app_id.as_deref() == Some("app-main"))
            .unwrap()
            .exhausted
    );
    let other = views
        .iter()
        .find(|v| v.app_id.as_deref() == Some("app-other"))
        .unwrap();
    assert_eq!(other.used_bytes, "0");
    assert!(!other.exhausted);
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 1))
            .await
            .unwrap(),
        UserBudgetGrant::Limited { rate_bps: 8000, .. }
    ));
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-other", 2))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { .. }
    ));
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes) SELECT $1,'alice','app-other',starts_at,starts_at+interval '1 day',512,512 FROM user_traffic_cycle(1,now())").bind(TENANT).execute(db.pool()).await.unwrap();
    db.store.enforce_user_traffic_policies().await.unwrap();
    // A's pacing clock is still occupied. B receives its independent budget immediately.
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-other", 3))
            .await
            .unwrap(),
        UserBudgetGrant::Limited {
            rate_bps: 16000,
            ..
        }
    ));
    let scopes: Vec<String> = sqlx::query_scalar(
        "SELECT scope FROM user_traffic_budgets WHERE kind='access' ORDER BY scope",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(scopes, ["app-main", "app-other"]);
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, None),
        )
        .await
        .unwrap();
    let compiled =
        brocade_core::compile::compile(&db.store.materialize_snapshot(None).await.unwrap());
    assert_eq!(
        compiled.summary.errors, 0,
        "fixture must compile: {:?}",
        compiled.diagnostics
    );
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.users.len(), 1);
    assert!(snapshot
        .apps
        .iter()
        .find(|a| a.id == "app-main")
        .unwrap()
        .grants
        .is_empty());
    assert_eq!(
        snapshot
            .apps
            .iter()
            .find(|a| a.id == "app-other")
            .unwrap()
            .grants
            .len(),
        1
    );
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            UserTrafficPolicy {
                base_rate_bps: None,
                quota_bytes: None,
                exhausted_action: ExhaustedAction::Suspend,
            },
        )
        .await
        .unwrap();
    assert_eq!(db.store.enforce_quotas().await.unwrap().restored, 1);
    let scopes: Vec<String> = sqlx::query_scalar(
        "SELECT scope FROM user_traffic_budgets WHERE kind='access' ORDER BY scope",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(scopes, ["app-other"]);
    assert_eq!(live_grants(db.pool()).await, 2);
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 4))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { managed: false, .. }
    ));
    assert!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()
            .iter()
            .find(|v| v.app_id.as_deref() == Some("app-other"))
            .unwrap()
            .exhausted
    );
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_budget_replays_earlier_receipt_after_other_requests_and_bounds_live_receipts()
{
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    usage(&db, 1024).await;
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, Some(8_000_000)),
        )
        .await
        .unwrap();
    for id in 1..=8 {
        let mut tiny = request("alice@platform.acme#ing-a1b2", id);
        tiny.bytes = 1;
        assert!(matches!(
            db.store.user_traffic_budget("n1", tiny).await.unwrap(),
            UserBudgetGrant::Limited { bytes: 1, .. }
        ));
    }
    let at: String = sqlx::query_scalar("SELECT next_at::text FROM user_traffic_budgets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    // Replaying an older request must neither issue fresh bytes nor advance the shared clock.
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 1))
            .await
            .unwrap(),
        UserBudgetGrant::Limited { bytes: 1, .. }
    ));
    assert_eq!(
        at,
        sqlx::query_scalar::<_, String>("SELECT next_at::text FROM user_traffic_budgets")
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 9))
            .await
            .unwrap(),
        UserBudgetGrant::Retry { .. }
    ));
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM user_traffic_budget_receipts")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 8);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_quota_only_throttles_after_exhaustion_and_nodes_share_durable_budget() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, Some(8000)),
        )
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 1))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { .. }
    ));
    usage(&db, 1024).await;
    assert_eq!(db.store.enforce_user_traffic_policies().await.unwrap(), 1);
    let first = db
        .store
        .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 2))
        .await
        .unwrap();
    assert!(matches!(
        first,
        UserBudgetGrant::Limited {
            bytes: 1000,
            rate_bps: 8000,
            ..
        }
    ));
    let at: String = sqlx::query_scalar("SELECT next_at::text FROM user_traffic_budgets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let replay = db
        .store
        .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 2))
        .await
        .unwrap();
    assert!(matches!(
        replay,
        UserBudgetGrant::Limited {
            bytes: 1000,
            rate_bps: 8000,
            valid_ms: 1..=2000
        }
    ));
    assert_eq!(
        at,
        sqlx::query_scalar::<_, String>("SELECT next_at::text FROM user_traffic_budgets")
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    let restarted = PgStore::from_pool(db.pool().clone());
    assert!(matches!(
        restarted
            .user_traffic_budget("n2", request("alice@platform.acme#ing-n2", 3))
            .await
            .unwrap(),
        UserBudgetGrant::Retry { .. }
    ));
    tokio::time::sleep(Duration::from_millis(1050)).await;
    // The oldest waiting node receives the next turn; an existing busy node cannot starve it.
    assert!(matches!(
        restarted
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 4))
            .await
            .unwrap(),
        UserBudgetGrant::Retry { .. }
    ));
    assert!(matches!(
        restarted
            .user_traffic_budget("n2", request("alice@platform.acme#ing-n2", 5))
            .await
            .unwrap(),
        UserBudgetGrant::Limited { bytes: 500, .. }
    ));
    assert_eq!(live_grants(db.pool()).await, 2);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_quota_suspend_throttle_reset_and_manual_disable_compose() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("DELETE FROM ingresses WHERE node_id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET retired_at=now() WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    usage(&db, 2000).await;
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, None),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            db.store
                .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 7))
                .await
                .unwrap(),
            UserBudgetGrant::Suspended { .. }
        ),
        "an exhausted stop policy cannot briefly release traffic while authorization catches up"
    );
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot.users.len(),
        1,
        "only this app's grants are paused, not the user"
    );
    assert!(snapshot.apps[0].grants.is_empty());
    assert_eq!(live_grants(db.pool()).await, 0);
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 1))
            .await
            .unwrap(),
        UserBudgetGrant::Suspended { .. }
    ));
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, Some(8000)),
        )
        .await
        .unwrap();
    assert_eq!(db.store.enforce_quotas().await.unwrap().restored, 1);
    assert_eq!(
        db.store
            .materialize_snapshot(None)
            .await
            .unwrap()
            .users
            .len(),
        1
    );
    sqlx::query("DELETE FROM grants WHERE ingress_id='ing-n2'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(4096, None),
        )
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 2))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { .. }
    ));
    assert_eq!(live_grants(db.pool()).await, 1);
    sqlx::query("UPDATE users SET service_suspended=true WHERE id='alice'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 3))
            .await
            .unwrap(),
        UserBudgetGrant::Suspended { .. }
    ));
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_quota_scopes_capabilities_and_legacy_policy_are_enforced() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    assert!(db
        .store
        .update_user_traffic_policy(
            &publisher("platform.other"),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1, None)
        )
        .await
        .is_err());
    sqlx::query("UPDATE nodes SET user_traffic_supported=false WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1, Some(8000))
        )
        .await
        .is_err());
    sqlx::query("UPDATE nodes SET user_traffic_supported=true WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    // The duplicated n2 ingress is useful for capability checks, but is not a compiled chain fixture.
    sqlx::query("DELETE FROM ingresses WHERE node_id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET retired_at=now() WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_app_quotas(tenant_id,user_id,app_id,limit_bytes) VALUES('platform.acme','alice','app-main',1)").execute(db.pool()).await.unwrap();
    assert!(
        db.store
            .user_traffic_assignments("n1")
            .await
            .unwrap()
            .sources
            .is_empty(),
        "legacy stop quotas do not require the new Xray feature"
    );
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 8))
            .await
            .unwrap(),
        UserBudgetGrant::Unlimited { managed: false, .. }
    ));
    usage(&db, 2000).await;
    assert_eq!(db.store.enforce_quotas().await.unwrap().suspended, 1);
    assert_eq!(live_grants(db.pool()).await, 0);
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, Some(8000)),
        )
        .await
        .unwrap();
    assert!(db
        .store
        .plan_quota_enforcement()
        .await
        .unwrap()
        .suspend
        .is_empty());
    // A new global throttle policy also restores entries previously removed by legacy child quotas.
    assert_eq!(db.store.enforce_quotas().await.unwrap().restored, 1);
    assert_eq!(live_grants(db.pool()).await, 1);
    assert!(matches!(
        db.store
            .user_traffic_budget("n2", request("alice@platform.acme#ing-a1b2", 1))
            .await
            .unwrap(),
        UserBudgetGrant::Suspended { .. }
    ));
    let mut invalid = request("alice@platform.acme#ing-a1b2", 2);
    invalid.bytes = MAX_BUDGET_BYTES + 1;
    assert!(db.store.user_traffic_budget("n1", invalid).await.is_err());
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_scoped_quota_policy_receipts_match_exact_snapshot_and_show_offline_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let views = db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            policy(1024, Some(8000)),
        )
        .await
        .unwrap();
    assert_eq!(views[0].pending_nodes, vec!["n1", "n2"]);
    let before = db.store.user_traffic_assignments("n1").await.unwrap();
    assert_eq!(before.sources.len(), 1);
    assert!(matches!(
        before.sources[0].effect,
        UserTrafficEffect::Unlimited { managed: true }
    ));
    db.store
        .acknowledge_user_traffic("n1", &before.sha256)
        .await
        .unwrap();
    assert_eq!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()[0]
            .pending_nodes,
        vec!["n2"]
    );
    usage(&db, 1024).await;
    db.store.enforce_user_traffic_policies().await.unwrap();
    assert!(db
        .store
        .acknowledge_user_traffic("n1", &before.sha256)
        .await
        .is_err());
    let after = db.store.user_traffic_assignments("n1").await.unwrap();
    assert_ne!(after.sha256, before.sha256);
    assert!(matches!(
        after.sources[0].effect,
        UserTrafficEffect::Limited { rate_bps: 8000 }
    ));
    db.store
        .acknowledge_user_traffic("n1", &after.sha256)
        .await
        .unwrap();
    assert_eq!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()[0]
            .pending_nodes,
        vec!["n2"]
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn constant_app_speed_without_quota_is_shared_audited_and_capability_checked() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let cap = UserTrafficPolicy {
        quota_bytes: None,
        base_rate_bps: Some(8000),
        exhausted_action: ExhaustedAction::Suspend,
    };
    let view = db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap()
        .remove(0);
    assert!(view.base_rate_supported);
    assert!(!view.exhausted);
    assert_eq!(view.used_bytes, "0");
    assert_eq!(view.pending_nodes, ["n1", "n2"]);
    assert_eq!(
        view.effective,
        UserTrafficEffect::Limited { rate_bps: 8000 }
    );
    assert!(db
        .store
        .report_user_traffic_support("n1", true)
        .await
        .unwrap_or(false));
    let assignment = db.store.user_traffic_assignments("n1").await.unwrap();
    assert_eq!(assignment.sources[0].effect, view.effective);
    db.store
        .acknowledge_user_traffic("n1", &assignment.sha256)
        .await
        .unwrap();
    assert_eq!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()[0]
            .pending_nodes,
        ["n2"]
    );
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 21))
            .await
            .unwrap(),
        UserBudgetGrant::Limited {
            rate_bps: 8000,
            bytes: 1000,
            ..
        }
    ));
    assert!(matches!(
        db.store
            .user_traffic_budget("n2", request("alice@platform.acme#ing-n2", 22))
            .await
            .unwrap(),
        UserBudgetGrant::Retry { .. }
    ));
    let status = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap();
    assert_eq!(status.traffic_policies[0].rate_bps, Some(8000));
    assert!(!status.traffic_policies[0].exhausted);
    assert!(db
        .store
        .update_user_traffic_policy(
            &publisher("platform.other"),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone()
        )
        .await
        .is_err());
    sqlx::query("UPDATE nodes SET user_traffic_supported=false WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    let mut changed = cap.clone();
    changed.base_rate_bps = Some(16000);
    assert!(db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            changed
        )
        .await
        .is_err());
    assert_eq!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()[0]
            .policy
            .as_ref()
            .unwrap()
            .base_rate_bps,
        Some(8000)
    );
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM user_traffic_policy_events")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(audits, 1, "failed changes must roll back without audits");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn constant_app_speed_survives_quota_removal_reset_and_composes_with_exhaustion() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let mut cap = policy(1024, Some(4000));
    cap.base_rate_bps = Some(8000);
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .user_traffic_budget("n1", request("alice@platform.acme#ing-a1b2", 31))
            .await
            .unwrap(),
        UserBudgetGrant::Limited { rate_bps: 8000, .. }
    ));
    let clock: String = sqlx::query_scalar("SELECT next_at::text FROM user_traffic_budgets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    cap.quota_bytes = None;
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        clock,
        sqlx::query_scalar::<_, String>("SELECT next_at::text FROM user_traffic_budgets")
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_traffic_budget_receipts")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    cap.quota_bytes = Some("1024".into());
    usage(&db, 2048).await;
    let views = db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        views[0].effective,
        UserTrafficEffect::Limited { rate_bps: 4000 }
    );
    cap.base_rate_bps = Some(2000);
    let views = db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        views[0].effective,
        UserTrafficEffect::Limited { rate_bps: 2000 }
    );
    cap.exhausted_action = ExhaustedAction::Suspend;
    let views = db
        .store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert_eq!(views[0].effective, UserTrafficEffect::Suspended);
    let period = db
        .store
        .list_user_cycle_usage(&system_admin())
        .await
        .unwrap()
        .periods
        .remove(0);
    db.store
        .reset_user_cycle_usage(
            &system_admin(),
            TENANT,
            "alice",
            brocade_store::UserCycleResetRequest {
                request_id: format!("{:032x}", 91),
                expected_reset_id: None,
                expected_period_start: period.starts_at,
                expected_period_end: period.ends_at,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .user_traffic_assignments("n1")
            .await
            .unwrap()
            .sources[0]
            .effect,
        UserTrafficEffect::Limited { rate_bps: 2000 }
    );
    cap.base_rate_bps = None;
    cap.exhausted_action = ExhaustedAction::Throttle { rate_bps: 4000 };
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        db.store
            .user_traffic_assignments("n1")
            .await
            .unwrap()
            .sources[0]
            .effect,
        UserTrafficEffect::Unlimited { managed: true }
    );
    cap.quota_bytes = None;
    db.store
        .update_user_traffic_policy(
            &system_admin(),
            TENANT,
            "alice",
            UserTrafficKind::Access,
            Some("app-main"),
            cap,
        )
        .await
        .unwrap();
    assert!(db
        .store
        .user_traffic_assignments("n1")
        .await
        .unwrap()
        .sources
        .is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_traffic_budgets")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn constant_app_speed_new_migration_preserves_restored_existing_database_and_rolls_back_conflicts(
) {
    use sha2::{Digest, Sha384};
    let Some(mut db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut old = sqlx::migrate::Migrator::DEFAULT;
    old.migrations = std::borrow::Cow::Owned(
        [
            (1, "init", include_str!("../../migrations/0001_init.sql")),
            (
                2,
                "forwarding rule results",
                include_str!("../../migrations/0002_forwarding_rule_results.sql"),
            ),
            (
                3,
                "unlock probe schedule",
                include_str!("../../migrations/0003_unlock_probe_schedule.sql"),
            ),
        ]
        .into_iter()
        .map(|(version, description, sql)| {
            sqlx::migrate::Migration::new(
                version,
                description.into(),
                sqlx::migrate::MigrationType::Simple,
                sql.into(),
                false,
            )
        })
        .collect(),
    );
    old.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    assert_eq!(
        format!(
            "{:x}",
            Sha384::digest(include_bytes!(
                "../../migrations/0003_unlock_probe_schedule.sql"
            ))
        ),
        "6b9c70bc99b3ecd499b33fea34c0a927d8dcaa604d90e09937899bfee2415b20a21d81d2c599de4c18222ace1f2204e8"
    );
    sqlx::query("INSERT INTO user_app_quotas(tenant_id,user_id,app_id,limit_bytes,exhausted_action,rate_bps) VALUES($1,'alice','app-main',1024,'throttle',8000)").bind(TENANT).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO user_traffic_budgets(tenant_id,user_id,kind,scope,next_at) VALUES($1,'alice','access','app-main',now()+interval '30 seconds')").bind(TENANT).execute(db.pool()).await.unwrap();
    let admin = super::user_cycle_reset_upgrade_tests::restore_billing_backup(&mut db).await;
    let before:serde_json::Value=sqlx::query_scalar("SELECT jsonb_build_object('users',(SELECT jsonb_agg(to_jsonb(u)) FROM users u),'quota',(SELECT jsonb_agg(to_jsonb(q)) FROM user_app_quotas q),'clock',(SELECT jsonb_agg(to_jsonb(b)) FROM user_traffic_budgets b),'receipts',(SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m))").fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        db.store
            .check_database_upgrade()
            .await
            .unwrap()
            .pending_versions,
        [4]
    );
    sqlx::query("CREATE TABLE user_app_rate_limits(conflict TEXT)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.migrate_database_schema().await.is_err());
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(versions, [1, 2, 3]);
    sqlx::query("DROP TABLE user_app_rate_limits")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate_database_schema().await.unwrap();
    let after:serde_json::Value=sqlx::query_scalar("SELECT jsonb_build_object('users',(SELECT jsonb_agg(to_jsonb(u)) FROM users u),'quota',(SELECT jsonb_agg(to_jsonb(q)) FROM user_app_quotas q),'clock',(SELECT jsonb_agg(to_jsonb(b)) FROM user_traffic_budgets b),'receipts',(SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m WHERE version<4))").fetch_one(db.pool()).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_app_rate_limits")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    db.store.migrate_database_schema().await.unwrap();
    assert_eq!(
        db.store
            .user_traffic_policies(&system_admin(), TENANT, "alice")
            .await
            .unwrap()[0]
            .policy
            .as_ref()
            .unwrap()
            .base_rate_bps,
        None
    );
    admin.close().await;
}
