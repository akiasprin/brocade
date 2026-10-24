use super::*;

async fn install_v031_schema(db: &TestPg) {
    use sha2::{Digest, Sha384};
    // Both official v0.3.1 and v0.3.2 shipped these exact bytes. Never silently widen this gate.
    let source = include_str!("../../migrations/0001_init.sql");
    assert_eq!(format!("{:x}", Sha384::digest(source.as_bytes())),
        "794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947");
    let mut migrator = sqlx::migrate::Migrator::DEFAULT;
    migrator.migrations = std::borrow::Cow::Owned(vec![sqlx::migrate::Migration::new(
        1,
        "init".into(),
        sqlx::migrate::MigrationType::Simple,
        source.into(),
        false,
    )]);
    migrator.run(db.pool()).await.unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn upgrade_v031_preflight_is_read_only_and_forward_migration_preserves_data() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let empty = db.store.check_database_upgrade().await.unwrap();
    assert!(!empty.compatible);
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(
        !exists,
        "preflight must not initialize an empty/wrong database"
    );
    install_v031_schema(&db).await;
    sqlx::query("UPDATE control_state SET site_name = 'keep existing configuration'")
        .execute(db.pool())
        .await
        .unwrap();
    let receipt: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    for _ in 0..2 {
        let check = db.store.check_database_upgrade().await.unwrap();
        assert!(check.compatible);
        assert_eq!(check.installed_versions, [1]);
        assert_eq!(check.pending_versions, [2]);
        let added: bool =
            sqlx::query_scalar("SELECT to_regclass('node_traffic_limit') IS NOT NULL")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(!added, "checking compatibility must not run migrations");
    }
    db.store.migrate_database_schema().await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible && check.pending_versions.is_empty());
    assert_eq!(check.installed_versions, [1, 2]);
    db.store.migrate_database_schema().await.unwrap();
    let preserved: (String, Vec<u8>) = sqlx::query_as(
        "SELECT site_name, checksum FROM control_state CROSS JOIN _sqlx_migrations WHERE version=1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(preserved, ("keep existing configuration".into(), receipt));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn upgrade_rejects_unknown_and_newer_histories_without_rewriting_receipts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_v031_schema(&db).await;
    let original: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let unknown = vec![7_u8; 48];
    sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=1")
        .bind(&unknown)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(!db.store.check_database_upgrade().await.unwrap().compatible);
    let actual: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(actual, unknown);
    sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=1")
        .bind(&original)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(9999,'future',true,$1,0)")
        .bind(&unknown).execute(db.pool()).await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(!check.compatible);
    assert_eq!(check.installed_versions, [1, 9999]);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn rejected_upgrade_releases_its_session_lock_before_a_repaired_retry() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_v031_schema(&db).await;
    let original: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query(
        "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=1",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db.store.migrate_database_schema().await.is_err());

    // Inspect from another session so reacquiring the migrator's pooled connection cannot hide
    // its session-level advisory lock through PostgreSQL's reentrant locking semantics.
    let mut observer = <sqlx::PgConnection as sqlx::Connection>::connect(&db.url)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=1")
        .bind(original)
        .execute(&mut observer)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        db.store.migrate_database_schema(),
    )
    .await
    .expect("repaired migration must not wait for a pooled connection to expire")
    .unwrap();
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND granted
          AND database=(SELECT oid FROM pg_database WHERE datname=current_database())",
    )
    .fetch_one(&mut observer)
    .await
    .unwrap();
    assert_eq!(
        held, 0,
        "a rejected migration retained its database session lock"
    );
    assert_eq!(
        db.store
            .check_database_upgrade()
            .await
            .unwrap()
            .installed_versions,
        [1, 2]
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cancelled_upgrade_does_not_leave_a_session_lock_for_the_next_attempt() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    install_v031_schema(&db).await;
    let mut blocker = <sqlx::PgConnection as sqlx::Connection>::connect(&db.url)
        .await
        .unwrap();
    sqlx::migrate::Migrate::lock(&mut blocker).await.unwrap();
    let store = db.store.clone();
    let attempt = tokio::spawn(async move { store.migrate_database_schema().await });
    wait_for_advisory_waiters(db.pool(), 1).await;
    attempt.abort();
    assert!(attempt.await.unwrap_err().is_cancelled());
    sqlx::migrate::Migrate::unlock(&mut blocker).await.unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        db.store.migrate_database_schema(),
    )
    .await
    .expect("a cancelled migrator must not retain the lock after its blocker exits")
    .unwrap();
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype='advisory' AND granted
          AND database=(SELECT oid FROM pg_database WHERE datname=current_database())",
    )
    .fetch_one(&mut blocker)
    .await
    .unwrap();
    assert_eq!(held, 0, "a cancelled migration retained a session lock");
}
