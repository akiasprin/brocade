use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn exact_deployed_unlock_history_consolidates_without_changing_observations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mut old = sqlx::migrate::Migrator::DEFAULT;
    old.migrations = std::borrow::Cow::Owned(vec![
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
            include_str!("../fixtures/migrations/deployed_0003_unlock_probe_schedule.sql").into(),
            false,
        ),
        sqlx::migrate::Migration::new(
            4,
            "unlock probe ip families".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../fixtures/migrations/deployed_0004_unlock_probe_ip_families.sql")
                .into(),
            false,
        ),
        sqlx::migrate::Migration::new(
            5,
            "unlock refresh stale".into(),
            sqlx::migrate::MigrationType::Simple,
            include_str!("../fixtures/migrations/deployed_0005_unlock_refresh_stale.sql").into(),
            false,
        ),
    ]);
    old.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO unlock_probe_discovery(node_id,next_discovery_at) VALUES('n1',now()+interval '1 hour')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO unlock_probe_jobs(scope_key,node_id,job_id,state,trigger,address_family,results) VALUES('node:n1:v4','n1','kept','complete','manual','v4','[{\"service\":\"chatgpt\",\"status\":\"available\",\"code\":\"web_available\",\"region\":\"JP\"}]')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE control_state SET unlock_refresh_stale=false WHERE id=TRUE")
        .execute(db.pool())
        .await
        .unwrap();
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('job',(SELECT to_jsonb(j)-'readiness_failures'-'dns_fingerprint' FROM unlock_probe_jobs j),
         'discovery',(SELECT to_jsonb(d) FROM unlock_probe_discovery d),
         'refresh',(SELECT unlock_refresh_stale FROM control_state WHERE id=TRUE))",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/prepare-consolidated-0003-compat.py"))
        .arg("--migration")
        .arg(root.join("crates/brocade-store/migrations/0003_unlock_probe_schedule.sql"))
        .args(["--output", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let conversion = String::from_utf8(output.stdout)
        .unwrap()
        .replace("\\set ON_ERROR_STOP on", "")
        .replace("SET LOCAL ROLE :\"app_role\";", "SET LOCAL ROLE postgres;");
    sqlx::raw_sql(&conversion).execute(db.pool()).await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert!(check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2, 3]);
    db.store.migrate().await.unwrap();
    assert_eq!(
        db.store
            .check_database_upgrade()
            .await
            .unwrap()
            .installed_versions,
        [1, 2, 3]
    );
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('job',(SELECT to_jsonb(j)-'readiness_failures'-'dns_fingerprint' FROM unlock_probe_jobs j),
         'discovery',(SELECT to_jsonb(d) FROM unlock_probe_discovery d),
         'refresh',(SELECT unlock_refresh_stale FROM control_state WHERE id=TRUE))",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(after, before);
    assert_channel_notifications_work(db.pool()).await;
    let mut connection = db.pool().acquire().await.unwrap();
    assert!(sqlx::raw_sql(&conversion)
        .execute(&mut *connection)
        .await
        .is_err());
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
}

async fn assert_channel_notifications_work(pool: &PgPool) {
    let triggers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_trigger WHERE tgfoid='public.notify_agent_channel_change()'::regprocedure
         AND NOT tgisinternal AND tgenabled='O'",
    ).fetch_one(pool).await.unwrap();
    assert_eq!(triggers, 13);
    let mut listener = sqlx::postgres::PgListener::connect_with(pool)
        .await
        .unwrap();
    listener.listen("brocade_agent_change").await.unwrap();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("UPDATE source_access_state SET revision=revision+1")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), listener.recv())
            .await
            .is_err()
    );
    sqlx::query("UPDATE source_access_state SET revision=revision+1")
        .execute(pool)
        .await
        .unwrap();
    let signal = tokio::time::timeout(std::time::Duration::from_secs(3), listener.recv())
        .await
        .unwrap()
        .unwrap();
    let signal: serde_json::Value = serde_json::from_str(signal.payload()).unwrap();
    assert_eq!(signal["domain"], "source_access");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn previous_consolidated_0003_adds_notifications_without_rewriting_business_state() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_previous_consolidated(&db).await;
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO unlock_probe_discovery(node_id,next_discovery_at) VALUES('n1',now()+interval '1 hour')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE control_state SET unlock_refresh_stale=false WHERE id=TRUE")
        .execute(db.pool())
        .await
        .unwrap();
    let snapshot = "SELECT jsonb_build_object('control',(SELECT to_jsonb(c)-'unlock_detection_mode'-'unlock_detection_interval_hours'-'unlock_detection_requested_at'-'unlock_detection_next_run_at' FROM control_state c),
        'discovery',(SELECT jsonb_agg(to_jsonb(d)) FROM unlock_probe_discovery d),
        'receipts',(SELECT jsonb_agg(to_jsonb(m)-'checksum' ORDER BY version) FROM _sqlx_migrations m))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/prepare-consolidated-0003-compat.py"))
        .arg("--migration")
        .arg(root.join("crates/brocade-store/migrations/0003_unlock_probe_schedule.sql"))
        .args(["--from-consolidated", "--output", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let conversion = String::from_utf8(output.stdout)
        .unwrap()
        .replace("\\set ON_ERROR_STOP on", "")
        .replace("SET LOCAL ROLE :\"app_role\";", "SET LOCAL ROLE postgres;");
    sqlx::raw_sql(&conversion).execute(db.pool()).await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert!(check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2, 3]);
    let after: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_channel_notifications_work(db.pool()).await;
    db.store.migrate().await.unwrap();
}

async fn install_previous_consolidated(db: &TestPg) {
    install_previous_schema(
        db,
        include_str!("../fixtures/migrations/deployed_0003_consolidated_unlock.sql"),
    )
    .await;
}

async fn install_previous_schema(db: &TestPg, third: &str) {
    let mut old = sqlx::migrate::Migrator::DEFAULT;
    old.migrations = std::borrow::Cow::Owned(vec![
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
            third.to_owned().into(),
            false,
        ),
    ]);
    old.run(db.pool()).await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn launcher_compatible_0003_conversion_is_opt_in_read_only_and_atomic() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_previous_consolidated(&db).await;
    let receipts = "SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m";
    let before: serde_json::Value = sqlx::query_scalar(receipts)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    for _ in 0..2 {
        let check = db.store.check_prerelease_0003_upgrade().await.unwrap();
        assert!(check.compatible);
        assert_eq!(
            check.pending_versions,
            [3],
            "the old launcher must write its migration-started marker"
        );
        assert_eq!(
            sqlx::query_scalar::<_, serde_json::Value>(receipts)
                .fetch_one(db.pool())
                .await
                .unwrap(),
            before
        );
    }
    sqlx::raw_sql("CREATE FUNCTION blocked_notification() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END; $$;
        CREATE TRIGGER agent_channel_release_created AFTER INSERT ON binary_release_targets FOR EACH ROW EXECUTE FUNCTION blocked_notification();")
        .execute(db.pool()).await.unwrap();
    assert!(db.store.migrate_prerelease_0003_schema().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(receipts)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    let absent: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('public.notify_agent_channel_change()') IS NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        absent,
        "failed DDL must roll back with the unchanged receipt"
    );
    sqlx::raw_sql("DROP TRIGGER agent_channel_release_created ON binary_release_targets; DROP FUNCTION blocked_notification();")
        .execute(db.pool()).await.unwrap();
    db.store.migrate_prerelease_0003_schema().await.unwrap();
    assert_channel_notifications_work(db.pool()).await;
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    assert!(db
        .store
        .check_prerelease_0003_upgrade()
        .await
        .unwrap()
        .pending_versions
        .is_empty());
    db.store.migrate_prerelease_0003_schema().await.unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('0',96),'hex') WHERE version=1",
    )
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
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn current_channel_0003_converts_readiness_atomically_without_new_migration_version() {
    use sha2::{Digest, Sha384};
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let source = include_str!("../../migrations/0003_unlock_probe_schedule.sql");
    let previous = source
        .split_once("\n-- Consolidated path-based unlock readiness.\n")
        .unwrap()
        .0;
    assert_eq!(format!("{:x}",Sha384::digest(previous.as_bytes())),"9528c5fe2b11fb063af638d6a11a30079fd70413e91cc55ee3df1bfc5f23e274a779d877254f412623bb8cb4ee776aaf");
    install_previous_schema(&db, previous).await;
    insert_minimal_fixture(db.pool()).await;
    let receipts = "SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m";
    let before: serde_json::Value = sqlx::query_scalar(receipts)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let check = db.store.check_prerelease_0003_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.pending_versions, [3]);
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(receipts)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    // Fail during the actual ALTER, after guards, to prove DDL and receipts roll back together.
    sqlx::raw_sql("CREATE FUNCTION fixture_reject_readiness_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture blocked ALTER'; END; $$; CREATE EVENT TRIGGER fixture_readiness_failure ON ddl_command_start WHEN TAG IN ('ALTER TABLE') EXECUTE FUNCTION fixture_reject_readiness_ddl();").execute(db.pool()).await.unwrap();
    assert!(db.store.migrate_prerelease_0003_schema().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(receipts)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    assert!(!sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass AND attname='readiness_failures' AND NOT attisdropped)").fetch_one(db.pool()).await.unwrap());
    sqlx::raw_sql("DROP EVENT TRIGGER fixture_readiness_failure; DROP FUNCTION fixture_reject_readiness_ddl();").execute(db.pool()).await.unwrap();
    db.store.migrate_prerelease_0003_schema().await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible && check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2, 3]);
    assert_channel_notifications_work(db.pool()).await;
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(m)-'checksum' ORDER BY version) FROM _sqlx_migrations m",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let expected: Vec<serde_json::Value> = before
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|mut r| {
            r.as_object_mut().unwrap().remove("checksum");
            r
        })
        .collect();
    assert_eq!(after, serde_json::json!(expected));
    db.store.migrate_prerelease_0003_schema().await.unwrap();
}
