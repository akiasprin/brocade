use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn migration_chain_reuses_receipts_and_rejects_changed_history() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("UPDATE control_state SET site_name = 'migration preservation fixture'")
        .execute(db.pool())
        .await
        .unwrap();
    let before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(!before.is_empty());

    db.store.migrate().await.unwrap();
    let after: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before, "restarting must not reapply migrations");
    let site_name: String = sqlx::query_scalar("SELECT site_name FROM control_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(site_name, "migration preservation fixture");

    let (version, mut checksum) = before[0].clone();
    assert!(!checksum.is_empty());
    checksum[0] ^= 1;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
        .bind(&checksum)
        .bind(version)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.store.migrate().await.unwrap_err(),
        StoreError::Migrate(sqlx::migrate::MigrateError::VersionMismatch(actual)) if actual == version
    ));
    let recorded: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = $1")
            .bind(version)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(recorded, checksum, "migration must not rewrite history");
    let site_name: String = sqlx::query_scalar("SELECT site_name FROM control_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(site_name, "migration preservation fixture");
}
