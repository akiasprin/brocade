use super::*;

fn report(at: i64, since: Option<i64>, bytes: Option<u64>) -> NodeRuntimeReport {
    serde_json::from_value(json!({
        "observed_at_unix_secs": at,
        "certificate": {"t": "unmanaged"},
        "versions": {"agent": "session-history-test"},
        "spool": {"observation": 0, "usage": 0, "dropped": 0},
        "online_sources": [{
            "label": "alice@platform.acme#ing-a1b2",
            "sources": [{"ip": "1.1.1.1", "last_seen_unix_secs": at,
                "protocols": ["vless"], "online_since_unix_secs": since,
                "session_uplink_bytes": bytes, "session_downlink_bytes": bytes}]
        }]
    }))
    .unwrap()
}

async fn history(db: &TestPg) -> brocade_store::UserOnlineSourceHistory {
    db.store
        .user_online_source_history(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn online_source_sessions_survive_reconnects_and_retries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let first = report(now, Some(now - 60), Some(100));
    db.store.record_node_runtime("n1", &first).await.unwrap();
    // Equal-time retries are serialized on node_agent_state, not new epochs.
    let (a, b) = tokio::join!(
        db.store.record_node_runtime("n1", &first),
        db.store.record_node_runtime("n1", &first),
    );
    a.unwrap();
    b.unwrap();
    db.store
        .record_node_runtime("n1", &report(now + 1, Some(now - 60), Some(200)))
        .await
        .unwrap();
    assert!(history(&db).await.sources.is_empty());
    let mut unavailable = report(now + 2, None, None);
    unavailable.online_sources = None;
    db.store
        .record_node_runtime("n1", &unavailable)
        .await
        .unwrap();
    assert!(
        history(&db).await.sources.is_empty(),
        "unavailable is not offline"
    );
    let mut offline = unavailable.clone();
    offline.observed_at_unix_secs = now + 3;
    offline.online_sources = Some(vec![]);
    db.store.record_node_runtime("n1", &offline).await.unwrap();
    let ended = history(&db).await.sources.remove(0);
    assert!(ended.session_id.is_some());
    assert!(ended.offline_at.is_some());
    assert_eq!(ended.session_uplink_bytes, Some(200));
    let second = report(now + 4, Some(now + 4), Some(10));
    db.store.record_node_runtime("n1", &second).await.unwrap();
    db.store.record_node_runtime("n1", &second).await.unwrap();
    assert_eq!(
        history(&db).await.sources.as_slice(),
        std::slice::from_ref(&ended)
    );
    let presence = db.store.list_user_presence(&system_admin()).await.unwrap();
    let alice = presence
        .users
        .iter()
        .find(|u| u.user_id == "alice")
        .unwrap();
    assert_eq!(alice.sources.len(), 1);
    assert_eq!(alice.sources[0].session_uplink_bytes, Some(10));

    // Reconnect between reports: Xray's new epoch closes the preceding one even without [].
    db.store
        .record_node_runtime("n1", &report(now + 6, Some(now + 5), Some(5)))
        .await
        .unwrap();
    let sessions = history(&db).await.sources;
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_uplink_bytes, Some(10));
    assert_ne!(sessions[0].session_id, ended.session_id);
    assert_eq!(sessions[1], ended);
    // Counter reset in the same timestamp second also fences an epoch.
    db.store
        .record_node_runtime("n1", &report(now + 7, Some(now + 5), Some(1)))
        .await
        .unwrap();
    assert_eq!(history(&db).await.sources.len(), 3);
    // A delayed report cannot rewrite current state or append duplicate history.
    db.store.record_node_runtime("n1", &first).await.unwrap();
    assert_eq!(history(&db).await.sources.len(), 3);
    assert!(matches!(
        db.store
            .user_online_source_history(&tenant_admin("other"), "platform.acme", "alice")
            .await
            .unwrap_err(),
        StoreError::Forbidden(_)
    ));

    // Successful legacy reports remain one unknown-metrics epoch until explicitly absent.
    db.store
        .record_node_runtime("n1", &report(now + 8, None, None))
        .await
        .unwrap();
    db.store
        .record_node_runtime("n1", &report(now + 9, None, None))
        .await
        .unwrap();
    assert_eq!(history(&db).await.sources.len(), 3);
    offline.observed_at_unix_secs = now + 10;
    db.store.record_node_runtime("n1", &offline).await.unwrap();
    assert_eq!(history(&db).await.sources.len(), 4);
    assert_eq!(history(&db).await.sources[0].session_uplink_bytes, None);

    // Query bounds and cleanup apply to completed epochs, not just distinct addresses.
    sqlx::query("UPDATE user_online_sources SET first_observed_at=now()-interval '31 days', last_observed_at=now()-interval '31 days', online_since_at=NULL, session_uplink_bytes=NULL,session_downlink_bytes=NULL WHERE session_id=$1::bigint")
        .bind(ended.session_id.unwrap().parse::<i64>().unwrap()).execute(db.pool()).await.unwrap();
    assert_eq!(history(&db).await.sources.len(), 3);
    assert_eq!(db.store.prune_user_online_sources(30).await.unwrap(), 1);
    sqlx::query("INSERT INTO user_online_sources(node_id,tenant_id,user_id,ingress_id,source_ip,active,first_observed_at,last_observed_at,xray_last_seen_at,offline_at)
        SELECT 'n1','platform.acme','alice','ing-a1b2','1.1.1.1'::inet,FALSE,now(),now(),now(),now() FROM generate_series(1,260)")
        .execute(db.pool()).await.unwrap();
    let history = history(&db).await;
    assert!(history.truncated);
    assert_eq!(history.sources.len(), 256);
    let ids: std::collections::BTreeSet<_> = history
        .sources
        .iter()
        .map(|s| s.session_id.clone())
        .collect();
    assert_eq!(ids.len(), 256);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn online_source_sessions_upgrade_preserves_rows_and_rolls_back_ddl_failure() {
    use sha2::{Digest, Sha384};
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let source = include_str!("../../migrations/0003_unlock_probe_schedule.sql");
    let previous = source
        .split_once("\n-- Consolidated online source session history.\n")
        .unwrap()
        .0;
    assert_eq!(format!("{:x}", Sha384::digest(previous.as_bytes())), "075d809314104489326b974f7969513a0f8917d6d65f98d70056da8d982fa7d2e58daf50785931163a9832d5b9a35e5e");
    let mut migrator = sqlx::migrate::Migrator::DEFAULT;
    migrator.migrations = std::borrow::Cow::Owned(vec![
        sqlx::migrate::Migration::new(
            1,
            "init".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../../migrations/0001_init.sql").into(),
            false,
        ),
        sqlx::migrate::Migration::new(
            2,
            "forwarding rule results".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../../migrations/0002_forwarding_rule_results.sql").into(),
            false,
        ),
        sqlx::migrate::Migration::new(
            3,
            "unlock probe schedule".into(),
            sqlx::migrate::MigrationType::Simple,
            previous.into(),
            false,
        ),
    ]);
    migrator.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO user_online_sources(node_id,tenant_id,user_id,ingress_id,source_ip,active,first_observed_at,last_observed_at,xray_last_seen_at,offline_at)
        VALUES('n1','platform.acme','alice','ing-a1b2','1.1.1.1',TRUE,now(),now(),now(),NULL),
              ('n1','platform.acme','alice','ing-a1b2','8.8.8.8',FALSE,now(),now(),now(),now())")
        .execute(db.pool()).await.unwrap();
    let snapshot =
        "SELECT jsonb_agg(to_jsonb(s)-'session_id' ORDER BY source_ip) FROM user_online_sources s";
    let before: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let receipt_sql = "SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m";
    let receipts: serde_json::Value = sqlx::query_scalar(receipt_sql)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    assert!(
        db.store.migrate().await.is_err(),
        "normal startup must reject old checksum"
    );
    assert_eq!(
        db.store
            .check_prerelease_0003_upgrade()
            .await
            .unwrap()
            .pending_versions,
        [3]
    );
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(receipt_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        receipts
    );
    // Matching receipts cannot bless a drifted physical schema.
    sqlx::query("ALTER TABLE user_online_sources ADD COLUMN session_id BIGINT")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        !db.store
            .check_prerelease_0003_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_prerelease_0003_schema().await.is_err());
    sqlx::query("ALTER TABLE user_online_sources DROP COLUMN session_id")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION fixture_reject_presence_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture blocked presence DDL'; END; $$;
        CREATE EVENT TRIGGER fixture_presence_failure ON ddl_command_start WHEN TAG IN ('CREATE INDEX') EXECUTE FUNCTION fixture_reject_presence_ddl();")
        .execute(db.pool()).await.unwrap();
    assert!(db.store.migrate_prerelease_0003_schema().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(receipt_sql)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        receipts
    );
    assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT FROM pg_attribute WHERE attrelid='user_online_sources'::regclass AND attname='session_id' AND NOT attisdropped)").fetch_one(db.pool()).await.unwrap());
    sqlx::raw_sql(
        "DROP EVENT TRIGGER fixture_presence_failure; DROP FUNCTION fixture_reject_presence_ddl();",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store.migrate_prerelease_0003_schema().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(snapshot)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    assert_eq!(history(&db).await.sources.len(), 1);
    db.store.migrate_prerelease_0003_schema().await.unwrap();
    let ids: i64 = sqlx::query_scalar("SELECT count(DISTINCT session_id) FROM user_online_sources")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(ids, 2);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn online_source_sessions_keep_ended_ingress_while_same_ip_is_still_online() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();
    sqlx::query("INSERT INTO ingresses (id,app_id,chain_id,node_id,bind,port,transport_kind,
        reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode)
        SELECT 'ing-c3d4',app_id,chain_id,node_id,bind,8443,transport_kind,
        reality_private_key,reality_public_key,reality_short_ids,reality_dest,reality_server_names,reality_flow,reality_fallback_mode FROM ingresses WHERE id='ing-a1b2'")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-main','platform.acme','alice','ing-c3d4')")
        .execute(db.pool()).await.unwrap();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut both = report(now, Some(now - 60), Some(100));
    let mut other = both.online_sources.as_ref().unwrap()[0].clone();
    other.label = "alice@platform.acme#ing-c3d4".into();
    both.online_sources.as_mut().unwrap().push(other.clone());
    db.store.record_node_runtime("n1", &both).await.unwrap();
    let presence = db.store.list_user_presence(&system_admin()).await.unwrap();
    let alice = presence
        .users
        .iter()
        .find(|u| u.user_id == "alice")
        .unwrap();
    assert_eq!(alice.sources.len(), 1);
    assert_eq!(alice.sources[0].accesses.len(), 2);
    assert_eq!(alice.sources[0].session_uplink_bytes, Some(200));
    let mut remaining = report(now + 1, Some(now - 60), Some(110));
    remaining.online_sources.as_mut().unwrap()[0].label = other.label;
    db.store
        .record_node_runtime("n1", &remaining)
        .await
        .unwrap();
    let ended = history(&db).await;
    assert_eq!(ended.sources.len(), 1);
    assert_eq!(ended.sources[0].ingress_ids, ["ing-a1b2"]);
    assert_eq!(ended.sources[0].session_uplink_bytes, Some(100));
    let presence = db.store.list_user_presence(&system_admin()).await.unwrap();
    let alice = presence
        .users
        .iter()
        .find(|u| u.user_id == "alice")
        .unwrap();
    assert_eq!(alice.sources.len(), 1);
    assert_eq!(alice.sources[0].ingress_ids, ["ing-c3d4"]);
    // Stale live presence is unknown, never appended as an ended epoch.
    sqlx::query("UPDATE node_agent_state SET online_sources_reported_at=now()-interval '13 minutes' WHERE node_id='n1'").execute(db.pool()).await.unwrap();
    assert_eq!(history(&db).await.sources.len(), 1);
    sqlx::query("UPDATE user_online_sources SET first_observed_at=now()-interval '2 hours',last_observed_at=now()-interval '2 hours',online_since_at=NULL,session_uplink_bytes=NULL,session_downlink_bytes=NULL WHERE NOT active").execute(db.pool()).await.unwrap();
    sqlx::raw_sql("INSERT INTO admin_operators(id,display_name,role,password_hash) VALUES('history-retention','History retention','system-admin','test-only');
        INSERT INTO data_retention_policies(table_name,ttl_hours,updated_by) VALUES('user_online_sources',1,'history-retention');")
        .execute(db.pool()).await.unwrap();
    assert!(
        history(&db).await.sources.is_empty(),
        "one-hour policy must not round up to one day"
    );
    sqlx::query(
        "UPDATE data_retention_policies SET ttl_hours=72 WHERE table_name='user_online_sources'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(history(&db).await.sources.len(), 1);
    assert_eq!(history(&db).await.retention_days, 3);
}
