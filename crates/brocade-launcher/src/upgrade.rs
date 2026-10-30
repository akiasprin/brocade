//! Offline upgrade operations, executed as the existing service account.
use crate::{
    console_binary,
    database::{DatabaseChoice, ManagedPostgres},
    download, paths,
    platform::Platform,
};
use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::Value;
use std::{
    env, fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Debug, Args)]
pub(crate) struct UpgradeDatabaseArgs {
    #[arg(long, conflicts_with = "external_db")]
    managed_db: bool,
    #[arg(long, conflicts_with = "managed_db")]
    external_db: bool,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    #[arg(long)]
    console_bin: Option<PathBuf>,
    #[arg(long)]
    previous_launcher: PathBuf,
    #[arg(long)]
    backup_dir: PathBuf,
    /// 安装器在停服后创建的受管数据完整冷备份；外部数据库使用 pg_dump
    #[arg(long)]
    cold_backup: Option<PathBuf>,
    /// 只读检查运行中的旧库，不启动 PostgreSQL、备份或执行迁移
    #[arg(long)]
    check_only: bool,
}

fn release_version(text: &str) -> Result<(u64, u64, u64)> {
    let version = text
        .trim()
        .strip_prefix("brocade ")
        .context("无法识别旧 launcher 版本")?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("升级只接受 v0.3.1 起的正式版本");
    }
    Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?))
}

async fn check_release(path: &Path) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    if !output.status.success() {
        bail!("无法运行旧 launcher；升级尚未开始");
    }
    let old = release_version(std::str::from_utf8(&output.stdout)?)?;
    let target = release_version(concat!("brocade ", env!("CARGO_PKG_VERSION")))?;
    if old < (0, 3, 1) || old > target {
        bail!("仅支持从 v0.3.1 起前向升级，不接受更早版本或降级");
    }
    Ok(())
}

async fn console_command(program: &Path, url: &str, operation: &str) -> Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(180),
        Command::new(program)
            .arg(operation)
            .env("DATABASE_URL", url)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("数据库维护命令超时")?
    .context("无法运行新 Console 的数据库维护命令")?;
    let check: Value =
        serde_json::from_slice(&output.stdout).context("新 Console 未返回数据库兼容检查结果")?;
    if !output.status.success() || check["compatible"] != true {
        bail!(
            "数据库升级检查失败：{}",
            check["reason"]
                .as_str()
                .unwrap_or("维护命令失败，未确认迁移完成")
        );
    }
    Ok(check)
}

async fn backup(program: &Path, url: &str, directory: &Path) -> Result<()> {
    let (connection, password) = dump_connection(url)?;
    let path = directory.join("database.dump");
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let mut command = Command::new(program);
    command.args([
        "--format=custom",
        "--compress=6",
        "--no-password",
        "--dbname",
        &connection,
    ]);
    command.env_remove("PGPASSWORD");
    if let Some(password) = password {
        command.env("PGPASSWORD", password);
    }
    let status = tokio::time::timeout(
        Duration::from_secs(900),
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("pg_dump 备份超时")?
    .context("无法运行 pg_dump；外部数据库需安装兼容的 PostgreSQL 客户端")?;
    if !status.success() {
        bail!("pg_dump 失败；请检查客户端版本、连接及备份权限，未执行迁移");
    }
    let mut file = fs::File::open(&path)?;
    let mut magic = [0; 5];
    file.read_exact(&mut magic)?;
    if &magic != b"PGDMP" {
        bail!("数据库备份格式无效，未执行迁移");
    }
    file.sync_all()?;
    let restore = program.with_file_name("pg_restore");
    let status = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new(restore)
            .arg("--list")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    if !status.success() {
        bail!("pg_restore 无法读取备份，未执行迁移");
    }
    Ok(())
}

/// libpq does not expand a URI supplied through PGDATABASE. Pass the connection explicitly,
/// with its password removed from argv and supplied only through the child environment.
fn dump_connection(text: &str) -> Result<(String, Option<String>)> {
    let mut url = reqwest::Url::parse(text).map_err(|_| anyhow::anyhow!("数据库地址格式无效"))?;
    let mut password = url
        .password()
        .map(|value| {
            percent_encoding::percent_decode_str(value)
                .decode_utf8()
                .map(|v| v.into_owned())
        })
        .transpose()
        .map_err(|_| anyhow::anyhow!("数据库密码编码无效"))?;
    url.set_password(None)
        .map_err(|_| anyhow::anyhow!("数据库地址格式无效"))?;
    let mut public = Vec::new();
    for (key, value) in url.query_pairs() {
        if key == "password" {
            password = Some(value.into_owned());
        } else if key == "sslpassword" {
            bail!(
                "自动备份不把 SSL 私钥口令放入命令行；请使用无需交互口令的客户端证书配置或手动升级"
            );
        } else {
            public.push((key.into_owned(), value.into_owned()));
        }
    }
    url.set_query(None);
    if !public.is_empty() {
        url.query_pairs_mut().extend_pairs(public);
    }
    Ok((url.to_string(), password))
}

enum DatabaseBackup {
    Logical(PathBuf),
    VerifiedColdArchive(PathBuf),
}

async fn verify_cold_archive(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).context("缺少停服后的完整数据冷备份")?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("受管数据库冷备份不是非空普通文件");
    }
    let status = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("tar")
            .arg("-tzf")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    if !status.success() {
        bail!("受管数据库冷备份无法读取，不执行迁移");
    }
    Ok(())
}

async fn apply(program: &Path, url: &str, source: &DatabaseBackup, directory: &Path) -> Result<()> {
    let check = console_command(program, url, "database-check").await?;
    paths::atomic_write_private(
        &directory.join("database-check.json"),
        &serde_json::to_vec_pretty(&check)?,
        0o600,
    )?;
    match source {
        DatabaseBackup::Logical(dump) => backup(dump, url, directory).await?,
        DatabaseBackup::VerifiedColdArchive(path) => paths::atomic_write_private(
            &directory.join("cold-backup.txt"),
            path.as_os_str().as_encoded_bytes(),
            0o600,
        )?,
    }
    if !check["pending_versions"]
        .as_array()
        .context("缺少待迁移版本")?
        .is_empty()
    {
        // The installer must not restart an old binary after this durable uncertainty boundary.
        paths::atomic_write_private(
            &directory.join("migration-started"),
            b"schema migration may have started\n",
            0o600,
        )?;
        console_command(program, url, "database-migrate").await?;
    }
    let after = console_command(program, url, "database-check").await?;
    if !after["pending_versions"]
        .as_array()
        .context("缺少迁移结果")?
        .is_empty()
    {
        bail!("数据库仍有待执行迁移，请保留备份后检查");
    }
    paths::atomic_write_private(
        &directory.join("database-complete"),
        b"schema ready\n",
        0o600,
    )?;
    Ok(())
}

pub(crate) async fn run(args: UpgradeDatabaseArgs) -> Result<()> {
    check_release(&args.previous_launcher).await?;
    let data = args
        .data_dir
        .or_else(|| env::var_os("BROCADE_DATA_DIR").map(PathBuf::from))
        .context("升级必须明确指定现有 BROCADE_DATA_DIR 或 --data-dir")?;
    let cache = args
        .cache_dir
        .or_else(|| env::var_os("BROCADE_CACHE_DIR").map(PathBuf::from))
        .context("升级必须明确指定现有 BROCADE_CACHE_DIR 或 --cache-dir")?;
    let program = console_binary(args.console_bin)?;
    if !args.check_only {
        paths::ensure_private_dir(&args.backup_dir)?;
    }
    let choice = DatabaseChoice::select(
        args.managed_db,
        args.external_db,
        env::var("DATABASE_URL").ok(),
    )?;
    let mut managed = None;
    let (url, backup) = match choice {
        DatabaseChoice::External(url) => (url, DatabaseBackup::Logical(PathBuf::from("pg_dump"))),
        DatabaseChoice::Managed => {
            let existing = ManagedPostgres::existing_database_url(&data.join("postgresql"))?;
            if args.check_only {
                (existing, DatabaseBackup::Logical(PathBuf::from("pg_dump")))
            } else {
                let cold = args
                    .cold_backup
                    .context("受管数据库升级要求 --cold-backup 指定停服后的完整数据冷备份")?;
                verify_cold_archive(&cold).await?;
                let client = download::client()?;
                let postgres = ManagedPostgres::start(
                    &data.join("postgresql"),
                    &cache,
                    Platform::current().await?,
                    &client,
                )
                .await?;
                let result = (
                    postgres.database_url(),
                    DatabaseBackup::VerifiedColdArchive(cold),
                );
                managed = Some(postgres);
                result
            }
        }
    };
    let result = if args.check_only {
        console_command(&program, &url, "database-check")
            .await
            .map(|check| println!("{check}"))
    } else {
        apply(&program, &url, &backup, &args.backup_dir).await
    };
    let stopped = if let Some(postgres) = managed {
        postgres.stop().await
    } else {
        Ok(())
    };
    result?;
    stopped?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_versions_are_exact_and_order_numerically() {
        assert_eq!(release_version("brocade 0.3.1\n").unwrap(), (0, 3, 1));
        assert!(
            release_version("brocade 0.3.12").unwrap() > release_version("brocade 0.3.2").unwrap()
        );
        for text in [
            "brocade 0.3.1-dev",
            "other 0.3.1",
            "brocade 0.3",
            "brocade 0.3.1 extra",
        ] {
            assert!(release_version(text).is_err());
        }
    }

    #[test]
    fn dump_arguments_preserve_connection_options_without_exposing_passwords() {
        let (url, password) =
            dump_connection("postgresql://user:p%40ss+word@localhost:5433/db?sslmode=require")
                .unwrap();
        assert_eq!(password.as_deref(), Some("p@ss+word"));
        assert_eq!(url, "postgresql://user@localhost:5433/db?sslmode=require");
        let (url, password) =
            dump_connection("postgresql://user@localhost/db?password=private&sslmode=require")
                .unwrap();
        assert_eq!(password.as_deref(), Some("private"));
        assert!(!url.contains("private"));
        assert!(dump_connection("postgresql://localhost/db?sslpassword=private").is_err());
    }

    fn program(path: &Path, body: &str) {
        // Keep executable bytes immutable: another test's fork can briefly inherit an
        // open writer and make execve return ETXTBSY even after this thread closes it.
        let mut body_path = path.as_os_str().to_owned();
        body_path.push(".body");
        fs::write(PathBuf::from(body_path), format!("set -eu\n{body}\n")).unwrap();
        if !path.exists() {
            let runner =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-program.sh");
            std::os::unix::fs::symlink(runner, path).unwrap();
        }
    }

    #[tokio::test]
    async fn failure_boundaries_distinguish_backup_failure_from_possible_schema_change() {
        let root = tempfile::tempdir().unwrap();
        let console = root.path().join("console");
        let dump = root.path().join("pg_dump");
        let restore = root.path().join("pg_restore");
        program(
            &console,
            "printf '%s\\n' '{\"compatible\":true,\"pending_versions\":[2]}'",
        );
        program(&dump, "exit 1");
        program(&restore, "exit 0");
        let before = root.path().join("before");
        paths::ensure_private_dir(&before).unwrap();
        assert!(apply(
            &console,
            "postgresql://fixture",
            &DatabaseBackup::Logical(dump.clone()),
            &before
        )
        .await
        .is_err());
        assert!(!before.join("migration-started").exists());
        assert!(!before.join("database-complete").exists());

        program(&dump, "printf 'PGDMPfixture'");
        program(&console, "printf '%s\\n' '{\"compatible\":true,\"pending_versions\":[2]}'\n[ \"$1\" != database-migrate ]");
        let during = root.path().join("during");
        paths::ensure_private_dir(&during).unwrap();
        assert!(apply(
            &console,
            "postgresql://fixture",
            &DatabaseBackup::Logical(dump),
            &during
        )
        .await
        .is_err());
        assert!(during.join("migration-started").exists());
        assert!(during.join("database.dump").exists());
        assert!(!during.join("database-complete").exists());
    }

    #[tokio::test]
    async fn compatible_up_to_date_database_is_backed_up_without_reapplying_schema() {
        let root = tempfile::tempdir().unwrap();
        let console = root.path().join("console");
        let dump = root.path().join("pg_dump");
        program(&console, "[ \"$1\" = database-check ]\nprintf '%s\\n' '{\"compatible\":true,\"pending_versions\":[]}'");
        program(&dump, "printf 'PGDMPfixture'");
        program(&root.path().join("pg_restore"), "exit 0");
        let backup = root.path().join("backup");
        paths::ensure_private_dir(&backup).unwrap();
        apply(
            &console,
            "postgresql://fixture",
            &DatabaseBackup::Logical(dump),
            &backup,
        )
        .await
        .unwrap();
        assert!(backup.join("database.dump").exists());
        assert!(backup.join("database-complete").exists());
        assert!(!backup.join("migration-started").exists());
    }

    #[tokio::test]
    #[ignore = "requires pinned PostgreSQL download and BROCADE_UPGRADE_CONSOLE_BIN candidate"]
    async fn managed_upgrade_checks_running_database_and_migrates_the_same_existing_cluster() {
        let console = PathBuf::from(
            env::var_os("BROCADE_UPGRADE_CONSOLE_BIN").expect("explicit candidate Console"),
        );
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("brocade");
        let cache = root.path().join("cache");
        paths::ensure_private_dir(&data).unwrap();
        paths::ensure_private_dir(&cache).unwrap();
        let previous = root.path().join("previous-launcher");
        program(&previous, "printf 'brocade 0.3.1\\n'");
        let client = download::client().unwrap();
        let platform = Platform::current().await.unwrap();
        let server = ManagedPostgres::start(&data.join("postgresql"), &cache, platform, &client)
            .await
            .unwrap();
        let pool = sqlx::PgPool::connect(&server.database_url()).await.unwrap();
        sqlx::raw_sql(include_str!("../../brocade-store/migrations/0001_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE _sqlx_migrations (
          version BIGINT PRIMARY KEY, description TEXT NOT NULL,
          installed_on TIMESTAMPTZ DEFAULT now() NOT NULL, success BOOLEAN NOT NULL,
          checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL);
          INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
          VALUES(1,'init',true,decode('794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947','hex'),0);
          UPDATE control_state SET site_name='managed upgrade fixture';")
            .execute(&pool).await.unwrap();
        pool.close().await;
        let make_args = |check_only| UpgradeDatabaseArgs {
            managed_db: true,
            external_db: false,
            data_dir: Some(data.clone()),
            cache_dir: Some(cache.clone()),
            console_bin: Some(console.clone()),
            previous_launcher: previous.clone(),
            backup_dir: root.path().join("backup"),
            cold_backup: Some(root.path().join("cold.tar.gz")),
            check_only,
        };
        run(make_args(true)).await.unwrap();
        assert!(
            !root.path().join("backup").exists(),
            "read-only preflight creates no backup/state"
        );
        server.stop().await.unwrap();
        let status = Command::new("tar")
            .arg("-czf")
            .arg(root.path().join("cold.tar.gz"))
            .arg("-C")
            .arg(root.path())
            .arg("brocade")
            .status()
            .await
            .unwrap();
        assert!(status.success());
        run(make_args(false)).await.unwrap();
        assert!(root.path().join("backup/database-complete").is_file());
        let server = ManagedPostgres::start(&data.join("postgresql"), &cache, platform, &client)
            .await
            .unwrap();
        let pool = sqlx::PgPool::connect(&server.database_url()).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../brocade-store/migrations");
        let mut expected_versions: Vec<i64> = fs::read_dir(migrations)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_str().unwrap();
                name.split_once('_').unwrap().0.parse().unwrap()
            })
            .collect();
        expected_versions.sort_unstable();
        assert!(expected_versions.len() >= 2);
        assert_eq!(versions, expected_versions);
        let name: String = sqlx::query_scalar("SELECT site_name FROM control_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "managed upgrade fixture");
        pool.close().await;
        server.stop().await.unwrap();
        let restored = root.path().join("restored");
        fs::create_dir(&restored).unwrap();
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(root.path().join("cold.tar.gz"))
            .arg("-C")
            .arg(&restored)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let server = ManagedPostgres::start(
            &restored.join("brocade/postgresql"),
            &cache,
            platform,
            &client,
        )
        .await
        .unwrap();
        let pool = sqlx::PgPool::connect(&server.database_url()).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            versions,
            [1],
            "cold archive restores the pre-upgrade database"
        );
        let name: String = sqlx::query_scalar("SELECT site_name FROM control_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "managed upgrade fixture");
        pool.close().await;
        server.stop().await.unwrap();
    }
}
