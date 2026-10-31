use super::*;

const RELEASED_SECOND: &str = include_str!("../../migrations/0002_forwarding_rule_results.sql");
const OLD_SECOND: &str = include_str!("../fixtures/migrations/deployed_0002_user_cycle_reset.sql");
const OLD_THIRD: &str =
    include_str!("../fixtures/migrations/deployed_0003_before_user_cycle_reset.sql");

async fn install(db: &TestPg, reset: bool, third: bool) {
    use sha2::{Digest, Sha384};
    assert_eq!(
        format!("{:x}", Sha384::digest(RELEASED_SECOND)),
        "54300599c9b4cce71f9e574b3f43141fee2fbd50279f829feda08f537bcde441a725ff1af4e0fbbf96fb9d80966552f1"
    );
    assert_eq!(
        format!("{:x}", Sha384::digest(OLD_SECOND)),
        "a28f8fca174df0c854d77581b7cf3f73e85e9df99dbf65e54eed7d8536420a3dc43389948cfdfbfd2d5f003fb32ec7db"
    );
    assert_eq!(
        format!("{:x}", Sha384::digest(OLD_THIRD)),
        "0c8f0c30cb8cfc07fead32837a7090b13f4ef5bd8e812c9a6c4818db370bfcc8e937ea7562209955aa96c94eb3a12347"
    );
    let mut migrations = vec![
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
            (if reset { OLD_SECOND } else { RELEASED_SECOND }).into(),
            false,
        ),
    ];
    if third {
        migrations.push(sqlx::migrate::Migration::new(
            3,
            "unlock probe schedule".into(),
            sqlx::migrate::MigrationType::Simple,
            OLD_THIRD.into(),
            false,
        ));
    }
    let mut migrator = sqlx::migrate::Migrator::DEFAULT;
    migrator.migrations = std::borrow::Cow::Owned(migrations);
    migrator.run(db.pool()).await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
        SELECT 'platform.acme','alice','app-main',start,start+interval '24 hours',10,90 FROM
          (SELECT date_trunc('day',now() AT TIME ZONE 'Asia/Hong_Kong') AT TIME ZONE 'Asia/Hong_Kong' AS start) day")
        .execute(db.pool()).await.unwrap();
}

async fn receipts(db: &TestPg) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY version) FROM _sqlx_migrations m")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn billing(db: &TestPg) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object(
      'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY tenant_id,id) FROM users u),
      'usage',(SELECT jsonb_agg(to_jsonb(s) ORDER BY tenant_id,user_id,app_id,period_start) FROM usage_rollups s),
      'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM user_cycle_resets e),
      'state',(SELECT jsonb_agg(to_jsonb(s) ORDER BY tenant_id,user_id) FROM user_cycle_reset_state s),
      'access',(SELECT jsonb_agg(to_jsonb(s) ORDER BY tenant_id,user_id,app_id,period_start) FROM user_cycle_access_baselines s),
      'forwarding',(SELECT jsonb_agg(to_jsonb(s) ORDER BY tenant_id,user_id,node_id,rule_id,period_start) FROM user_cycle_forwarding_baselines s),
      'heads',(SELECT jsonb_agg(to_jsonb(h) ORDER BY node_id,label) FROM usage_counter_heads h),
      'sequence',(SELECT jsonb_build_object('last',last_value,'called',is_called) FROM user_cycle_resets_id_seq),
      'used',user_traffic_usage('platform.acme','alice','access','app-main'))")
        .fetch_one(db.pool()).await.unwrap()
}

pub(super) async fn restore_billing_backup(db: &mut TestPg) -> PgPool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dump = Command::new("docker")
        .args([
            "exec",
            db._container.id(),
            "pg_dump",
            "-U",
            "postgres",
            "-Fc",
            "postgres",
        ])
        .output()
        .unwrap();
    assert!(dump.status.success(), "isolated pg_dump failed");
    assert!(!dump.stdout.is_empty());
    sqlx::query("CREATE ROLE cycle_app LOGIN PASSWORD 'fixture-password'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("CREATE DATABASE cycle_restore OWNER cycle_app")
        .execute(db.pool())
        .await
        .unwrap();
    let mut restore = Command::new("docker")
        .args([
            "exec",
            "-i",
            db._container.id(),
            "pg_restore",
            "--exit-on-error",
            "--no-owner",
            "--role=cycle_app",
            "-U",
            "postgres",
            "-d",
            "cycle_restore",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    restore
        .stdin
        .take()
        .unwrap()
        .write_all(&dump.stdout)
        .unwrap();
    let result = restore.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "isolated pg_restore failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let admin = PgPool::connect(&db.url_for("cycle_restore")).await.unwrap();
    let app_url = db.url_for("cycle_restore").replacen(
        "postgres://postgres:postgres@",
        "postgres://cycle_app:fixture-password@",
        1,
    );
    db.store = PgStore::connect(&app_url).await.unwrap();
    admin
}

async fn reset(db: &TestPg, previous: Option<i64>, key: u8) -> i64 {
    // Build the request from the old cycle function: the candidate's new policy projection does
    // not exist yet while this fixture is still simulating the previously deployed Console.
    let (starts_at,ends_at):(String,String)=sqlx::query_as("SELECT to_char(c.starts_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),to_char(c.ends_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM users u CROSS JOIN LATERAL user_traffic_cycle(u.traffic_reset_day,now()) c WHERE u.tenant_id='platform.acme' AND u.id='alice'").fetch_one(db.pool()).await.unwrap();
    db.store
        .reset_user_cycle_usage(
            &system_admin(),
            "platform.acme",
            "alice",
            brocade_store::UserCycleResetRequest {
                request_id: format!("{key:032x}"),
                expected_reset_id: previous,
                expected_period_start: starts_at,
                expected_period_end: ends_at,
            },
        )
        .await
        .unwrap()
        .reset_id
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn official_v033_upgrade_preserves_published_0002_without_opt_in() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install(&db, false, false).await;
    let before = receipts(&db).await;
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.pending_versions, [3, 4]);
    db.store.migrate_database_schema().await.unwrap();
    let after = receipts(&db).await;
    assert_eq!(before[1], after[1]);
    assert_eq!(
        db.store
            .check_database_upgrade()
            .await
            .unwrap()
            .pending_versions,
        Vec::<i64>::new()
    );
    let (events,used):(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM user_cycle_resets),user_traffic_usage('platform.acme','alice','access','app-main')")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!((events, used), (0, 100));
    db.store.migrate_database_schema().await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_upgrade_is_opt_in_atomic_and_preserves_existing_billing() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install(&db, false, true).await;
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let before = receipts(&db).await;
    for _ in 0..2 {
        let check = db.store.check_user_cycle_reset_upgrade().await.unwrap();
        assert!(check.compatible);
        assert_eq!(check.pending_versions, [3, 4]);
        assert_eq!(receipts(&db).await, before);
    }
    // Reject actual DDL after the new tables were created: both DDL and receipts must roll back.
    sqlx::raw_sql("CREATE FUNCTION reject_cycle_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture blocked function'; END; $$;
        CREATE EVENT TRIGGER reject_cycle_function ON ddl_command_start WHEN TAG IN ('CREATE FUNCTION') EXECUTE FUNCTION reject_cycle_ddl();")
        .execute(db.pool()).await.unwrap();
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    assert_eq!(receipts(&db).await, before);
    assert!(!sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('public.user_cycle_resets') IS NOT NULL"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    sqlx::raw_sql("DROP EVENT TRIGGER reject_cycle_function; DROP FUNCTION reject_cycle_ddl();")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate_user_cycle_reset_schema().await.unwrap();
    let after = receipts(&db).await;
    assert_eq!(before[1], after[1]);
    let state = billing(&db).await;
    assert_eq!(state["used"], 100);
    assert!(state["events"].is_null());
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    db.store.migrate_user_cycle_reset_schema().await.unwrap();
    assert_eq!(billing(&db).await, state);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn misplaced_0002_moves_to_0003_preserving_resets_sequence_and_receipt_metadata() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    assert_misplaced_reset_roundtrip(db).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL 18"]
async fn postgres18_reset_catalog_preserves_billing_backup_and_atomic_conversion() {
    let Some(db) = TestPg::start_if_enabled_with_tag("18").await else {
        return;
    };
    assert_misplaced_reset_roundtrip(db).await;
}

async fn assert_misplaced_reset_roundtrip(mut db: TestPg) {
    install(&db, true, true).await;
    let id = reset(&db, None, 1).await;
    let before = billing(&db).await;
    let history = receipts(&db).await;
    let admin = restore_billing_backup(&mut db).await;
    assert_eq!(billing(&db).await, before);
    assert_eq!(receipts(&db).await, history);
    assert_eq!(before["used"], 0);
    assert_eq!(before["events"].as_array().unwrap().len(), 1);
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let check = db.store.check_user_cycle_reset_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.pending_versions, [3, 4]);
    assert_eq!(receipts(&db).await, history);
    sqlx::raw_sql("CREATE FUNCTION reject_cycle_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture blocked function'; END; $$;
        CREATE EVENT TRIGGER reject_cycle_function ON ddl_command_start WHEN TAG IN ('CREATE FUNCTION') EXECUTE FUNCTION reject_cycle_ddl();")
        .execute(&admin).await.unwrap();
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    assert_eq!(receipts(&db).await, history);
    assert_eq!(billing(&db).await, before);
    sqlx::raw_sql("DROP EVENT TRIGGER reject_cycle_function; DROP FUNCTION reject_cycle_ddl();")
        .execute(&admin)
        .await
        .unwrap();
    db.store.migrate_user_cycle_reset_schema().await.unwrap();
    assert!(db.store.check_database_upgrade().await.unwrap().compatible);
    assert_eq!(billing(&db).await, before);
    let after = receipts(&db).await;
    for (mut old, mut new) in history
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .zip(after.as_array().unwrap().iter().cloned())
    {
        old.as_object_mut().unwrap().remove("checksum");
        new.as_object_mut().unwrap().remove("checksum");
        assert_eq!(old, new);
    }
    db.store.migrate_user_cycle_reset_schema().await.unwrap();
    assert_eq!(billing(&db).await, before);
    assert!(
        reset(&db, Some(id), 2).await > id,
        "identity sequence must remain usable"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL 18"]
async fn postgres18_unvalidated_not_null_is_rejected_even_with_not_null_column_flag() {
    let Some(db) = TestPg::start_if_enabled_with_tag("18").await else {
        return;
    };
    install(&db, true, true).await;
    let history = receipts(&db).await;
    sqlx::raw_sql("ALTER TABLE user_cycle_resets ALTER COLUMN actor DROP NOT NULL;
        ALTER TABLE user_cycle_resets ADD CONSTRAINT user_cycle_resets_actor_not_null NOT NULL actor NOT VALID;")
        .execute(db.pool()).await.unwrap();
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT attnotnull FROM pg_attribute
        WHERE attrelid='user_cycle_resets'::regclass AND attname='actor'"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    assert!(
        !db.store
            .check_user_cycle_reset_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    assert_eq!(receipts(&db).await, history);
    sqlx::query(
        "ALTER TABLE user_cycle_resets VALIDATE CONSTRAINT user_cycle_resets_actor_not_null",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.store
            .check_user_cycle_reset_upgrade()
            .await
            .unwrap()
            .compatible
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn misplaced_reset_rejects_schema_drift_and_unknown_receipts_without_changes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install(&db, true, true).await;
    reset(&db, None, 1).await;
    let before = billing(&db).await;
    let history = receipts(&db).await;
    for (change, repair) in [
        (
            "ALTER TABLE user_cycle_access_baselines ALTER COLUMN uplink_bytes DROP NOT NULL",
            "ALTER TABLE user_cycle_access_baselines ALTER COLUMN uplink_bytes SET NOT NULL",
        ),
        (
            "ALTER TABLE usage_samples DISABLE TRIGGER user_cycle_access_late_sample",
            "ALTER TABLE usage_samples ENABLE TRIGGER user_cycle_access_late_sample",
        ),
        (
            "ALTER TABLE user_cycle_access_baselines ALTER COLUMN app_id TYPE TEXT COLLATE \"C\"",
            "ALTER TABLE user_cycle_access_baselines ALTER COLUMN app_id TYPE TEXT COLLATE \"default\"",
        ),
        (
            "ALTER FUNCTION user_cycle_excluded_bytes(bigint,timestamptz,timestamptz,timestamptz) PARALLEL SAFE",
            "ALTER FUNCTION user_cycle_excluded_bytes(bigint,timestamptz,timestamptz,timestamptz) PARALLEL UNSAFE",
        ),
        (
            "ALTER FUNCTION user_cycle_excluded_bytes(bigint,timestamptz,timestamptz,timestamptz) SECURITY DEFINER",
            "ALTER FUNCTION user_cycle_excluded_bytes(bigint,timestamptz,timestamptz,timestamptz) SECURITY INVOKER",
        ),
        (
            "CREATE INDEX user_cycle_unreviewed ON user_cycle_reset_state(reset_id)",
            "DROP INDEX user_cycle_unreviewed",
        ),
    ] {
        sqlx::raw_sql(change).execute(db.pool()).await.unwrap();
        assert!(
            !db.store
                .check_user_cycle_reset_upgrade()
                .await
                .unwrap()
                .compatible
        );
        assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
        assert_eq!(receipts(&db).await, history);
        assert_eq!(billing(&db).await, before);
        sqlx::raw_sql(repair).execute(db.pool()).await.unwrap();
    }
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=2",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        !db.store
            .check_user_cycle_reset_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    assert_eq!(billing(&db).await, before);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_cycle_upgrade_rejects_partial_schema_and_unknown_receipts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install(&db, false, true).await;
    sqlx::query("CREATE TABLE user_cycle_unknown_partial(value boolean)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        !db.store
            .check_user_cycle_reset_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    sqlx::query("DROP TABLE user_cycle_unknown_partial")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=2",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        !db.store
            .check_user_cycle_reset_upgrade()
            .await
            .unwrap()
            .compatible
    );
    assert!(db.store.migrate_user_cycle_reset_schema().await.is_err());
    assert!(!sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('public.user_cycle_resets') IS NOT NULL"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
}
