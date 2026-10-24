use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retention_tracks_locked_backlog_isolates_failures_and_recovers() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO usage_readings (node_id,label,read_at,xray_started_at,uplink_bytes,downlink_bytes) VALUES ('n1','expired',now()-interval '2 days',now()-interval '3 days',10,20)")
        .execute(db.pool()).await.unwrap();
    let mut held = db.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM usage_readings FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    let results = db.store.run_data_retention().await.unwrap();
    assert_eq!(results.len(), 28);
    assert!(
        results.iter().all(|r| r.error_code.is_none()),
        "{results:?}"
    );
    let view = db.store.data_retention().await.unwrap();
    assert_eq!(
        view.policies
            .iter()
            .filter(|p| p.category == brocade_store::DataRetentionCategory::History)
            .count(),
        24
    );
    assert_eq!(
        view.policies
            .iter()
            .filter(|p| p.category == brocade_store::DataRetentionCategory::Temporary)
            .count(),
        4
    );
    assert!(!view
        .policies
        .iter()
        .any(|p| p.table_name == "vpngate_runtime_evidence"));
    let run = view
        .policies
        .iter()
        .find(|p| p.table_name == "usage_readings")
        .unwrap()
        .cleanup
        .as_ref()
        .unwrap();
    assert_eq!(run.deleted_rows, 0);
    assert_eq!(
        run.has_backlog,
        Some(true),
        "locked rows still count as backlog"
    );
    assert!(run.next_run_unix_secs - run.finished_at_unix_secs <= 61);
    held.rollback().await.unwrap();

    sqlx::raw_sql("CREATE FUNCTION retention_test_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'sensitive retention sentinel'; END $$;
        CREATE TRIGGER retention_test_failure BEFORE DELETE ON usage_readings FOR EACH ROW EXECUTE FUNCTION retention_test_failure();
        UPDATE data_retention_cleanup SET next_run_at = now() - interval '1 second';")
        .execute(db.pool()).await.unwrap();
    let failed = db.store.run_data_retention().await.unwrap();
    assert_eq!(
        failed
            .iter()
            .find(|r| r.table_name == "usage_readings")
            .unwrap()
            .error_code
            .as_deref(),
        Some("database_error")
    );
    assert!(
        failed
            .iter()
            .any(|r| r.table_name == "machine_events" && r.error_code.is_none()),
        "later tables must still execute"
    );
    let failed_view = db.store.data_retention().await.unwrap();
    let run = failed_view
        .policies
        .iter()
        .find(|p| p.table_name == "usage_readings")
        .unwrap()
        .cleanup
        .as_ref()
        .unwrap();
    assert!(run.last_success_unix_secs.is_some());
    assert_eq!(run.has_backlog, None);
    assert!(!serde_json::to_string(&failed_view)
        .unwrap()
        .contains("sensitive retention sentinel"));

    sqlx::raw_sql("DROP TRIGGER retention_test_failure ON usage_readings; DROP FUNCTION retention_test_failure();
        UPDATE data_retention_cleanup SET next_run_at = now() - interval '1 second';")
        .execute(db.pool()).await.unwrap();
    let recovered = db.store.run_data_retention().await.unwrap();
    let run = recovered
        .iter()
        .find(|r| r.table_name == "usage_readings")
        .unwrap();
    assert_eq!(run.deleted_rows, 1);
    assert!(run.error_code.is_none());
    let view = db.store.data_retention().await.unwrap();
    let run = view
        .policies
        .iter()
        .find(|p| p.table_name == "usage_readings")
        .unwrap()
        .cleanup
        .as_ref()
        .unwrap();
    assert_eq!(run.has_backlog, Some(false));
    assert!(
        db.store.run_data_retention().await.unwrap().is_empty(),
        "not-due jobs must not repeat"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM data_retention_cleanup")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        28
    );

    // A changed TTL must be checked even if the previous policy was not due yet.
    sqlx::raw_sql("INSERT INTO admin_operators(id,display_name,role,password_hash) VALUES ('retention-test','Retention test','system-admin','test-only'); INSERT INTO data_retention_policies (table_name,ttl_hours,updated_by) VALUES ('usage_readings',48,'retention-test')").execute(db.pool()).await.unwrap();
    let mut competing = db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('usage_readings',824519))")
        .execute(&mut *competing)
        .await
        .unwrap();
    assert!(
        db.store.run_data_retention().await.unwrap().is_empty(),
        "another Console's claimed table must be skipped"
    );
    competing.rollback().await.unwrap();
    let changed = db.store.run_data_retention().await.unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].table_name, "usage_readings");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retention_migration_preserves_distinct_runtime_evidence_before_expiry() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    // Upgrade from the released initial schema; all unreleased additions belong to 0002.
    let baseline = Migrator {
        migrations: std::borrow::Cow::Owned(vec![Migration::new(
            1,
            "init".into(),
            MigrationType::Simple,
            include_str!("../../migrations/0001_init.sql").into(),
            false,
        )]),
        ..Migrator::DEFAULT
    };
    baseline.run(db.pool()).await.unwrap();
    sqlx::raw_sql("INSERT INTO vpngate_sync_runs (id,source_url,worker_id,trigger,status,error_code,finished_at) VALUES (1,'https://example.test/','worker','startup','failed','fixture',now());
        INSERT INTO vpngate_profiles (sha256,server_id,remote_address,remote_port,transport,openvpn_config,first_seen_run_id,last_seen_run_id) VALUES (repeat('a',64),'vpn','192.0.2.1',1194,'udp','fixture',1,1);
        INSERT INTO vpngate_servers (id,hostname,ip,country_code,country_name,score,speed_bps,vpn_sessions,uptime_millis,total_users,total_traffic_bytes,log_type,operator_name,message,profile_sha256,first_seen_run_id,last_seen_run_id) VALUES ('vpn','vpn','192.0.2.1','JP','Japan',0,0,0,0,0,0,'none','fixture','fixture',repeat('a',64),1,1);
        INSERT INTO vpngate_probe_samples (node_id,outbound_id,topology_revision,catalog_generation,server_id,profile_sha256,status,exit_ip,connect_ms,download_bps,probed_at,received_at)
        VALUES ('retired','removed',1,1,'vpn',repeat('a',64),'succeeded','198.51.100.1',10,100, to_timestamp(100),now()-interval '8 days'),
               ('retired','removed',1,1,'vpn',repeat('a',64),'succeeded','198.51.100.2',0,200, to_timestamp(200),now()-interval '8 days');
        INSERT INTO vpngate_probe_samples (node_id,outbound_id,topology_revision,catalog_generation,server_id,profile_sha256,status,error_code,probed_at,received_at)
        VALUES ('retired','removed',1,1,'vpn',repeat('a',64),'failed','unreachable',to_timestamp(300),now()-interval '8 days');")
        .execute(db.pool()).await.unwrap();
    db.store.migrate().await.unwrap();
    let evidence: Vec<(String, i64)> = sqlx::query_as("SELECT evidence_kind, extract(epoch FROM probed_at)::bigint FROM vpngate_runtime_evidence ORDER BY evidence_kind")
        .fetch_all(db.pool()).await.unwrap();
    assert_eq!(
        evidence,
        vec![
            ("connect".into(), 100),
            ("latest".into(), 300),
            ("success".into(), 200)
        ]
    );
    let results = db.store.run_data_retention().await.unwrap();
    assert!(
        results.iter().all(|r| r.error_code.is_none()),
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .find(|r| r.table_name == "vpngate_probe_samples")
            .unwrap()
            .deleted_rows,
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM vpngate_runtime_evidence")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        3
    );
    db.store.migrate().await.unwrap();
}
