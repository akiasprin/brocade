//! Exercise the actual binary's feature identity and offline database boundary.
use serde_json::{json, Value};
use std::process::Command;
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;

fn console() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_brocade-console"));
    command.env_remove("DATABASE_URL");
    command
}

#[test]
fn first_release_product_identity_needs_no_database() {
    let output = console().arg("product-info").output().unwrap();
    assert!(output.status.success());
    let info: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        info,
        json!({"flavor": if cfg!(feature="cvm") {"cvm"} else {"base"}, "features":{"cvm":cfg!(feature="cvm")}})
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn offline_upgrade_rejects_empty_databases_and_checks_initialized_schema() {
    assert_eq!(std::env::var("BROCADE_RUN_PG_TESTS").as_deref(), Ok("1"));
    let container = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgresql://postgres:postgres@127.0.0.1:{port}/postgres");
    for operation in ["database-check", "database-migrate"] {
        let output = console()
            .arg(operation)
            .env("DATABASE_URL", &url)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["compatible"], false);
    }
    // First installation initializes through the normal startup path. Offline upgrade
    // commands must never turn a typo in an existing database URL into a new installation.
    let store = brocade_store::PgStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    for command in [
        "database-check",
        "database-migrate",
        "database-check",
        "database-migrate",
    ] {
        let output = console()
            .arg(command)
            .env("DATABASE_URL", &url)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["compatible"], true);
    }
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'cvm_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 33);
    let identities: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        identities
            .iter()
            .map(|(version, _)| *version)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    pool.close().await;
}
