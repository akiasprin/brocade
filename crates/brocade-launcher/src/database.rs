use crate::{
    download,
    paths::{atomic_write_private, ensure_private_dir},
    platform::Platform,
};
use anyhow::{bail, Context, Result};
use postgresql_embedded::{PostgreSQL, Settings, VersionReq};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const POSTGRES_VERSION: &str = "17.11.0";
const POSTGRES_MAJOR: &str = "17";
const POSTGRES_VERSION_NUM: u32 = 170_011;
const ZONKY_RELEASES: &str = "https://github.com/zonkyio/embedded-postgres-binaries";
const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2/io/zonky/test/postgres";

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum DatabaseChoice {
    External(String),
    Managed,
}

impl DatabaseChoice {
    pub(crate) fn select(
        force_managed: bool,
        force_external: bool,
        database_url: Option<String>,
    ) -> Result<Self> {
        if force_managed && force_external {
            bail!("--managed-db 与 --external-db 不能同时使用");
        }
        if database_url
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("DATABASE_URL 已设置但为空；为避免误启空数据库，不会回退到受管 PostgreSQL");
        }
        match (force_managed, force_external, database_url) {
            (true, _, Some(_)) => {
                bail!("--managed-db 与 DATABASE_URL 不能同时使用；请明确移除其中一个")
            }
            (true, _, None) => Ok(Self::Managed),
            (_, true, Some(url)) => Ok(Self::External(url)),
            (_, true, None) => bail!("--external-db 要求设置 DATABASE_URL"),
            (_, _, Some(url)) => Ok(Self::External(url)),
            (_, _, None) => Ok(Self::Managed),
        }
    }
}

struct PostgresAsset {
    artifact: &'static str,
    sha256: &'static str,
}

fn postgres_asset(platform: Platform) -> PostgresAsset {
    let artifact = platform.postgres_artifact();
    let sha256 = match artifact {
        "embedded-postgres-binaries-linux-amd64" => {
            "0dd7b72b6f335b8ecfb355fa24c5781e8a93edd09880bb77eb52ebbf29b3e96d"
        }
        "embedded-postgres-binaries-linux-amd64-alpine" => {
            "ace9a6f95b1c98ce84c9841267cd04ebee5d7de5dc7b8fea148ead44f145afbd"
        }
        "embedded-postgres-binaries-linux-arm64v8" => {
            "8b042e0ea418b1927d95399207950da0734d27fe01e29503ade3bdc464ad4c2b"
        }
        "embedded-postgres-binaries-linux-arm64v8-alpine" => {
            "4ab1a5ced1200b2740186d537fd79628bc1c83f07d2bab7b18b6103390d48c54"
        }
        _ => unreachable!("Platform validates the supported matrix"),
    };
    PostgresAsset { artifact, sha256 }
}

pub(crate) struct ManagedPostgres {
    // Field order matters: stop PostgreSQL before releasing the ownership lock.
    server: PostgreSQL,
    _lock: File,
}

impl ManagedPostgres {
    pub(crate) async fn start(
        data_root: &Path,
        cache_root: &Path,
        platform: Platform,
        client: &reqwest::Client,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        bail!("受管 PostgreSQL 当前只支持 Unix");

        #[cfg(unix)]
        {
            if unsafe { libc::geteuid() } == 0 {
                bail!("initdb 拒绝 root；请用专用普通用户运行 brocade up");
            }
        }

        ensure_private_dir(data_root)?;
        let data_root = fs::canonicalize(data_root)?;
        ensure_safe_socket_root(&data_root)?;
        let socket = data_root.join("run");
        ensure_private_dir(&socket)?;

        let lock = private_file(&data_root.join("owner.lock"), false)?;
        lock.try_lock().with_context(|| {
            format!("另一个 brocade up 已经占用 {}", data_root.to_string_lossy())
        })?;

        let data = data_root.join("data");
        let password_file = data_root.join("password");
        let state_file = data_root.join("cluster-state");
        let previous_state = read_optional_private_text(&state_file)?;
        validate_state(previous_state.as_deref())?;
        if (previous_state.is_some() || password_file.exists())
            && !data.join("PG_VERSION").is_file()
        {
            bail!("已有 PostgreSQL 状态但 PGDATA 缺失或不完整；请恢复备份，不会创建空库替代");
        }
        validate_data_directory(&data)?;
        // Fetch the reproducible engine before writing any cluster state. A first-run network
        // failure must leave a virgin data directory retryable rather than looking like a
        // partially initialized database on the next `brocade up`.
        let engine = prepare_engine(cache_root, platform, client).await?;
        if previous_state.is_none() {
            atomic_write_private(&state_file, b"initializing\n", 0o600)?;
        }
        let password = load_or_create_password(&password_file, data.join("PG_VERSION").exists())?;

        let settings = Settings {
            releases_url: ZONKY_RELEASES.to_owned(),
            version: VersionReq::parse(&format!("={POSTGRES_VERSION}"))?,
            installation_dir: engine,
            password_file,
            data_dir: data,
            host: "localhost".to_owned(),
            // TCP is disabled. The port only names the socket file inside our private directory.
            port: 5432,
            username: "postgres".to_owned(),
            password,
            temporary: false,
            timeout: Some(Duration::from_secs(60)),
            configuration: HashMap::from([
                ("listen_addresses".to_owned(), "''".to_owned()),
                ("unix_socket_permissions".to_owned(), "0700".to_owned()),
                ("fsync".to_owned(), "on".to_owned()),
                ("synchronous_commit".to_owned(), "on".to_owned()),
                ("full_page_writes".to_owned(), "on".to_owned()),
                ("password_encryption".to_owned(), "scram-sha-256".to_owned()),
                ("timezone".to_owned(), "UTC".to_owned()),
            ]),
            trust_installation_dir: true,
            socket_dir: Some(socket),
        };

        let running = existing_postmaster(&settings).await?;
        let mut managed = Self {
            server: PostgreSQL::new(settings),
            _lock: lock,
        };
        if running {
            managed.server.stop().await?;
        }
        managed.server.setup().await?;
        atomic_write_private(
            &managed.server.settings().data_dir.join("pg_hba.conf"),
            b"local all all scram-sha-256\n",
            0o600,
        )?;
        managed.server.start().await.map_err(|error| {
            anyhow::anyhow!(
                "{error}; PostgreSQL 日志：{}",
                managed
                    .server
                    .settings()
                    .data_dir
                    .join("start.log")
                    .display()
            )
        })?;

        let pool = sqlx::PgPool::connect(&managed.server.settings().url("postgres")).await?;
        let verified = verify_server(&pool).await;
        let identity: String =
            sqlx::query_scalar("SELECT system_identifier::text FROM pg_control_system()")
                .fetch_one(&pool)
                .await?;
        pool.close().await;
        verified?;

        if let Some(expected) = previous_state
            .as_deref()
            .and_then(|state| state.strip_prefix("ready "))
        {
            if expected.trim() != identity {
                bail!("PostgreSQL 集群身份发生变化；请恢复与状态文件匹配的备份");
            }
        }
        if !managed.server.database_exists("brocade").await? {
            if previous_state
                .as_deref()
                .is_some_and(|state| state.starts_with("ready "))
            {
                bail!("已初始化的 PostgreSQL 缺少 brocade 数据库；请恢复备份");
            }
            managed.server.create_database("brocade").await?;
        }
        atomic_write_private(&state_file, format!("ready {identity}\n").as_bytes(), 0o600)?;
        Ok(managed)
    }

    /// Contains a generated credential. Callers must pass it only through the child environment.
    pub(crate) fn database_url(&self) -> String {
        self.server.settings().url("brocade")
    }

    pub(crate) async fn wait_for_exit(&self) -> Result<()> {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            let mut command =
                tokio::process::Command::new(self.server.settings().binary_dir().join("pg_ctl"));
            command
                .arg("-D")
                .arg(&self.server.settings().data_dir)
                .arg("status")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let status = tokio::time::timeout(Duration::from_secs(5), command.status()).await??;
            if !status.success() {
                bail!("受管 PostgreSQL 意外停止");
            }
        }
    }

    pub(crate) async fn stop(self) -> Result<()> {
        self.server.stop().await?;
        Ok(())
    }
}

async fn prepare_engine(
    cache_root: &Path,
    platform: Platform,
    client: &reqwest::Client,
) -> Result<PathBuf> {
    let asset = postgres_asset(platform);
    let parent = cache_root
        .join("postgresql")
        .join(POSTGRES_VERSION)
        .join(asset.artifact);
    ensure_private_dir(&parent)?;
    let engine = parent.join("engine");
    let marker = engine.join(".brocade-archive-sha256");
    let postgres = engine.join("bin/postgres");

    if postgres.is_file()
        && fs::read_to_string(&marker)
            .ok()
            .is_some_and(|value| value.trim() == asset.sha256)
    {
        return Ok(engine);
    }
    if engine.exists() {
        // Engine is a verified, reproducible cache. PGDATA lives under data_root and is untouched.
        fs::remove_dir_all(&engine).context("无法移除未完成或校验失败的 PostgreSQL 引擎缓存")?;
    }

    let filename = format!("{}-{POSTGRES_VERSION}.jar", asset.artifact);
    let url = format!(
        "{MAVEN_CENTRAL}/{}/{POSTGRES_VERSION}/{filename}",
        asset.artifact
    );
    eprintln!(
        "PostgreSQL {POSTGRES_VERSION} ({}) 首次下载中…",
        asset.artifact
    );
    let bytes = download::verified_bytes(client, &url, asset.sha256).await?;
    postgresql_archive::extract(ZONKY_RELEASES, &bytes, &engine).await?;
    if !postgres.is_file() {
        bail!("PostgreSQL 归档解压后缺少 bin/postgres");
    }
    atomic_write_private(&marker, format!("{}\n", asset.sha256).as_bytes(), 0o600)?;
    Ok(engine)
}

async fn existing_postmaster(settings: &Settings) -> Result<bool> {
    if !settings.data_dir.join("postmaster.pid").exists() {
        return Ok(false);
    }
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(settings.binary_dir().join("pg_ctl"))
            .arg("-D")
            .arg(&settings.data_dir)
            .arg("status")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    match status.code() {
        Some(3) => Ok(false),
        Some(0) => {
            let contents = fs::read_to_string(settings.data_dir.join("postmaster.pid"))?;
            let mut lines = contents.lines();
            let pid: u32 = lines.next().context("postmaster.pid 为空")?.parse()?;
            let executable = fs::read_link(format!("/proc/{pid}/exe"))?;
            let command = fs::read(format!("/proc/{pid}/cmdline"))?;
            let arguments: Vec<_> = command.split(|byte| *byte == 0).collect();
            if executable != fs::canonicalize(settings.binary_dir().join("postgres"))?
                || lines.next() != settings.data_dir.to_str()
                || !arguments.windows(2).any(|argument| {
                    argument[0] == b"-D"
                        && argument[1] == settings.data_dir.as_os_str().as_encoded_bytes()
                })
            {
                bail!("postmaster.pid 指向无法识别的存活进程，拒绝向它发送信号");
            }
            Ok(true)
        }
        _ => bail!("无法判断既有 PostgreSQL 进程状态：{status}"),
    }
}

async fn verify_server(pool: &sqlx::PgPool) -> Result<()> {
    let settings: (String, String, String, String, String) = sqlx::query_as(
        "SELECT current_setting('fsync'), current_setting('synchronous_commit'),
                current_setting('full_page_writes'), current_setting('listen_addresses'),
                current_setting('server_version_num')",
    )
    .fetch_one(pool)
    .await?;
    if settings.0 != "on"
        || settings.1 != "on"
        || settings.2 != "on"
        || !settings.3.is_empty()
        || settings.4.parse::<u32>()? != POSTGRES_VERSION_NUM
    {
        bail!("受管 PostgreSQL 的版本、持久性或监听配置与 launcher 不一致");
    }
    Ok(())
}

fn ensure_safe_socket_root(path: &Path) -> Result<()> {
    if path.as_os_str().as_encoded_bytes().len() > 76 {
        bail!("受管 PostgreSQL 目录过长，无法安全创建 Unix socket");
    }
    if !path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(byte))
    {
        bail!("受管 PostgreSQL 目录只能包含 ASCII 字母、数字以及 / . _ -");
    }
    Ok(())
}

fn validate_state(state: Option<&str>) -> Result<()> {
    if state.is_some_and(|state| {
        state != "initializing\n"
            && state
                .strip_prefix("ready ")
                .is_none_or(|identity| identity.trim().parse::<u64>().is_err())
    }) {
        bail!("cluster-state 无效；请恢复已验证的备份");
    }
    Ok(())
}

fn validate_data_directory(data: &Path) -> Result<()> {
    if !data.exists() {
        return Ok(());
    }
    ensure_private_dir(data)?;
    match fs::read_to_string(data.join("PG_VERSION")) {
        Ok(version) if version.trim() == POSTGRES_MAJOR => {
            if !data.join("postgresql.conf").is_file() {
                bail!("PostgreSQL 数据目录不完整；请恢复已验证的备份");
            }
            Ok(())
        }
        Ok(_) => bail!("PostgreSQL 大版本不同；必须显式执行 pg_upgrade 或导出恢复"),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && fs::read_dir(data)?.next().is_none() =>
        {
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            bail!("非空数据目录缺少 PG_VERSION，拒绝在其中初始化")
        }
        Err(error) => Err(error.into()),
    }
}

fn private_file(path: &Path, create_new: bool) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(!create_new)
            .create_new(create_new)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("{} 必须是当前用户所有的 0600 普通文件", path.display());
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    bail!("brocade up 当前只支持 Unix 权限模型")
}

fn read_optional_private_text(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut value = String::new();
    private_file(path, false)?.read_to_string(&mut value)?;
    Ok(Some(value))
}

fn load_or_create_password(path: &Path, initialized: bool) -> Result<String> {
    if let Some(value) = read_optional_private_text(path)? {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("受管 PostgreSQL 密码文件无效；请恢复原密码文件");
        }
        return Ok(value);
    }
    if initialized {
        bail!("受管 PostgreSQL 密码文件缺失；请随数据库备份恢复它");
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let value: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    atomic_write_private(path, value.as_bytes(), 0o600)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Libc;

    #[test]
    fn database_selection_never_falls_back_from_explicit_external_configuration() {
        assert_eq!(
            DatabaseChoice::select(false, false, None).unwrap(),
            DatabaseChoice::Managed
        );
        assert_eq!(
            DatabaseChoice::select(false, false, Some("postgres://db/brocade".into())).unwrap(),
            DatabaseChoice::External("postgres://db/brocade".into())
        );
        assert!(DatabaseChoice::select(false, false, Some(String::new())).is_err());
        assert!(DatabaseChoice::select(false, true, None).is_err());
        assert!(DatabaseChoice::select(true, false, Some("postgres://db/brocade".into())).is_err());
    }

    #[test]
    fn every_supported_postgres_asset_has_a_pinned_digest() {
        for (arch, libc) in [
            ("x86_64", Libc::Gnu),
            ("x86_64", Libc::Musl),
            ("aarch64", Libc::Gnu),
            ("aarch64", Libc::Musl),
        ] {
            let asset = postgres_asset(Platform { arch, libc });
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn managed_state_rejects_ambiguous_recovery_inputs() {
        assert!(validate_state(None).is_ok());
        assert!(validate_state(Some("initializing\n")).is_ok());
        assert!(validate_state(Some("ready 123\n")).is_ok());
        assert!(validate_state(Some("ready nope\n")).is_err());
        assert!(validate_state(Some("unknown\n")).is_err());
    }
}
