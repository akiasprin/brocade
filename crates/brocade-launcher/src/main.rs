mod access;
mod database;
mod download;
mod entry;
mod paths;
mod platform;
mod upgrade;

use access::AccessArgs;
use anyhow::{bail, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use clap::{Args, Parser, Subcommand};
use database::{DatabaseChoice, ManagedPostgres};
use paths::{atomic_write_private, LauncherPaths};
use platform::Platform;
use serde::Deserialize;
use std::{
    env, fs,
    future::pending,
    io::IsTerminal,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};

#[derive(Debug, Parser)]
#[command(
    name = "brocade",
    version,
    about = "启动 Brocade 控制面及其可选本机运行时"
)]
struct Cli {
    #[command(subcommand)]
    command: LauncherCommand,
}

#[derive(Debug, Subcommand)]
enum LauncherCommand {
    /// 启动 Console；没有 DATABASE_URL 时使用受管 PostgreSQL
    Up(UpArgs),
    /// 显示当前控制台入口；交互终端会自动携带一次性初始化凭据
    Url(entry::UrlArgs),
    /// 显示 HTTP IP 入口，供安装器确认地址；不启动服务
    Address(AccessArgs),
    /// 检查、备份并前向迁移既有数据库；供安装器 --upgrade 调用
    UpgradeDatabase(upgrade::UpgradeDatabaseArgs),
}

#[derive(Debug, Args)]
struct UpArgs {
    /// 强制使用受管 PostgreSQL；不能同时设置 DATABASE_URL
    #[arg(long, conflicts_with = "external_db")]
    managed_db: bool,

    /// 强制使用外部 PostgreSQL；缺少 DATABASE_URL 时立即失败
    #[arg(long, conflicts_with = "managed_db")]
    external_db: bool,

    #[command(flatten)]
    access: AccessArgs,

    /// Brocade 持久数据根目录
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 下载及可再生缓存根目录
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// brocade-console 可执行文件
    #[arg(long, value_name = "PATH")]
    console_bin: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct AuthState {
    initialized: bool,
}

struct BootstrapToken {
    secret: String,
    private_file: Option<PathBuf>,
}

struct InitializationAccess<'a> {
    url: &'a str,
    bootstrap_token: &'a str,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("brocade: {error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        LauncherCommand::Up(args) => up(args).await,
        LauncherCommand::Url(args) => entry::print_current(args).await,
        LauncherCommand::Address(args) => {
            println!("{}", args.resolve()?.public_url);
            Ok(())
        }
        LauncherCommand::UpgradeDatabase(args) => upgrade::run(args).await,
    }
}

async fn up(args: UpArgs) -> Result<()> {
    let access = args.access.resolve()?;
    let paths = LauncherPaths::resolve(args.data_dir.clone(), args.cache_dir.clone())?;
    let _instance = paths::InstanceLock::acquire(&paths.runtime)?;
    let client = download::client()?;
    // Poll the signal handler throughout startup, not just after every child is ready. Otherwise
    // Ctrl-C during initdb or a runtime download could bypass our orderly child cleanup.
    let mut shutdown = Box::pin(shutdown_signal());

    let database_url = env::var("DATABASE_URL")
        .map(Some)
        .or_else(|error| match error {
            env::VarError::NotPresent => Ok(None),
            other => Err(other),
        })?;
    let choice = DatabaseChoice::select(args.managed_db, args.external_db, database_url)?;
    let mut managed = None;
    let database_url = match choice {
        DatabaseChoice::External(url) => {
            eprintln!("Database: external ({})", redact_database_url(&url));
            url
        }
        DatabaseChoice::Managed => {
            let platform = Platform::current().await?;
            let root = paths.data.join("postgresql");
            eprintln!(
                "Database: managed PostgreSQL {}\nData: {}",
                database::POSTGRES_VERSION,
                root.display()
            );
            let postgres = tokio::select! {
                _ = shutdown.as_mut() => return Ok(()),
                result = ManagedPostgres::start(&root, &paths.cache, platform, &client) => result?,
            };
            let url = postgres.database_url();
            managed = Some(postgres);
            url
        }
    };

    let local_addr = reachable_loopback(access.bind);
    let local_url = format!("http://{local_addr}");

    let secret = if managed.is_some() {
        Some(managed_secret(&paths.data)?)
    } else {
        None
    };
    let bootstrap_token = bootstrap_token(&paths.data, optional_env("BROCADE_BOOTSTRAP_TOKEN")?)?;
    let console_bin = console_binary(args.console_bin.clone())?;
    let mut console_command = Command::new(&console_bin);
    console_command
        .env("DATABASE_URL", &database_url)
        .env("BROCADE_ADMIN_BIND", access.bind.to_string())
        .env("BROCADE_AGENT_PUBLIC_URL", &access.public_url)
        .env("BROCADE_BOOTSTRAP_TOKEN", &bootstrap_token.secret)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if env::var_os("BROCADE_CACHE_DIR").is_none() {
        console_command.env("BROCADE_CACHE_DIR", &paths.cache);
    }
    if env::var_os("BROCADE_PROBE_RUNTIME_DIR").is_none() {
        console_command.env("BROCADE_PROBE_RUNTIME_DIR", paths.runtime.join("probes"));
    }
    if let Some(secret) = &secret {
        console_command.env("BROCADE_SECRET_KEY", secret);
    }
    let mut console = console_command
        .spawn()
        .with_context(|| format!("无法启动 {}", console_bin.display()))?;

    let console_ready = tokio::select! {
        _ = shutdown.as_mut() => None,
        result = wait_for_console(&client, &local_url, &mut console) => Some(result),
    };
    match console_ready {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            cleanup(&mut console, managed).await;
            return Err(error);
        }
        None => {
            cleanup(&mut console, managed).await;
            return Ok(());
        }
    }
    println!("Local: {local_url}");

    println!("控制台地址：{}", access.public_url);

    match wait_until_initialized(
        &client,
        &local_url,
        InitializationAccess {
            url: &access.public_url,
            bootstrap_token: &bootstrap_token.secret,
        },
        &mut console,
        &mut shutdown,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            cleanup(&mut console, managed).await;
            return Ok(());
        }
        Err(error) => {
            cleanup(&mut console, managed).await;
            return Err(error);
        }
    }
    if let Some(path) = &bootstrap_token.private_file {
        if let Err(error) = remove_stale_runtime_file(path) {
            eprintln!("初始化已完成，但无法删除一次性凭据文件：{error:#}");
        }
    }

    let event = tokio::select! {
        _ = &mut shutdown => RuntimeExit::Signal,
        status = console.wait() => RuntimeExit::Console(status?),
        result = wait_for_postgres(managed.as_ref()) => RuntimeExit::Postgres(result),
    };

    cleanup(&mut console, managed).await;
    match event {
        RuntimeExit::Signal => Ok(()),
        RuntimeExit::Console(status) if status.success() => Ok(()),
        RuntimeExit::Console(status) => bail!("brocade-console 意外退出：{status}"),
        RuntimeExit::Postgres(result) => result,
    }
}

enum RuntimeExit {
    Signal,
    Console(ExitStatus),
    Postgres(Result<()>),
}

async fn wait_for_console(
    client: &reqwest::Client,
    local_url: &str,
    child: &mut Child,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("brocade-console 在就绪前退出：{status}");
        }
        if client
            .get(format!("{local_url}/healthz"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("brocade-console 在 90 秒内没有就绪");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_until_initialized(
    client: &reqwest::Client,
    local_url: &str,
    access: InitializationAccess<'_>,
    console: &mut Child,
    shutdown: &mut std::pin::Pin<Box<impl std::future::Future<Output = ()>>>,
) -> Result<bool> {
    if auth_state(client, local_url).await?.initialized {
        return Ok(true);
    }
    println!(
        "{}",
        initialization_instructions(
            access.url,
            access.bootstrap_token,
            std::io::stdout().is_terminal(),
        )?
    );
    loop {
        tokio::select! {
            _ = shutdown.as_mut() => return Ok(false),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if let Some(status) = console.try_wait()? {
            bail!("brocade-console 在等待管理员初始化时退出：{status}");
        }
        if auth_state(client, local_url).await?.initialized {
            return Ok(true);
        }
    }
}

fn initialization_instructions(
    initialization_url: &str,
    bootstrap_token: &str,
    terminal: bool,
) -> Result<String> {
    let initialization_url = entry::entry_url(initialization_url, Some(bootstrap_token), terminal)?;
    let credential = if terminal {
        "打开完整初始化链接后，设置管理员用户名和密码即可。"
    } else {
        "请在交互终端运行 sudo brocade url 获取一次性初始化链接。"
    };
    Ok(format!(
        "首次启动尚未创建管理员。\n初始化地址：{initialization_url}\n{credential}"
    ))
}

async fn auth_state(client: &reqwest::Client, local_url: &str) -> Result<AuthState> {
    client
        .get(format!("{local_url}/auth/state"))
        .timeout(Duration::from_secs(2))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("无法读取管理员初始化状态")
}

async fn wait_for_postgres(postgres: Option<&ManagedPostgres>) -> Result<()> {
    match postgres {
        Some(postgres) => postgres.wait_for_exit().await,
        None => pending().await,
    }
}

async fn cleanup(console: &mut Child, postgres: Option<ManagedPostgres>) {
    stop_child("brocade-console", console, Duration::from_secs(90)).await;
    if let Some(postgres) = postgres {
        if let Err(error) = postgres.stop().await {
            eprintln!("停止受管 PostgreSQL 失败：{error:#}");
        }
    }
}

async fn stop_child(name: &str, child: &mut Child, timeout: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(timeout, child.wait()).await.is_err() {
        eprintln!("{name} 未及时停止，发送强制终止");
        let _ = child.kill().await;
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn reachable_loopback(bind: SocketAddr) -> SocketAddr {
    match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bind.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), bind.port())
        }
        _ => bind,
    }
}

fn console_binary(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit.or_else(|| {
        env::var_os("BROCADE_CONSOLE_BIN")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }) {
        return Ok(path);
    }
    let sibling = env::current_exe()?
        .parent()
        .context("launcher 可执行文件没有父目录")?
        .join("brocade-console");
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Ok(PathBuf::from("brocade-console"))
    }
}

fn bootstrap_token(data_root: &Path, configured: Option<String>) -> Result<BootstrapToken> {
    if let Some(secret) = configured {
        validate_bootstrap_token(&secret)?;
        return Ok(BootstrapToken {
            secret,
            private_file: None,
        });
    }

    let path = data_root.join("bootstrap-token");
    if path.exists() {
        let secret = paths::read_private_text(&path)?;
        let secret = secret.trim().to_owned();
        validate_bootstrap_token(&secret)?;
        return Ok(BootstrapToken {
            secret,
            private_file: Some(path),
        });
    }

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let secret = URL_SAFE_NO_PAD.encode(bytes);
    atomic_write_private(&path, format!("{secret}\n").as_bytes(), 0o600)?;
    Ok(BootstrapToken {
        secret,
        private_file: Some(path),
    })
}

fn validate_bootstrap_token(secret: &str) -> Result<()> {
    if secret.trim() != secret {
        bail!("BROCADE_BOOTSTRAP_TOKEN 不能包含首尾空白");
    }
    if !(32..=512).contains(&secret.len()) {
        bail!("BROCADE_BOOTSTRAP_TOKEN 必须为 32 至 512 字节");
    }
    Ok(())
}

fn managed_secret(data_root: &Path) -> Result<String> {
    if let Some(value) = optional_env("BROCADE_SECRET_KEY")? {
        validate_secret(value.trim())?;
        return Ok(value);
    }
    let path = data_root.join("secret-key");
    if path.exists() {
        let value = paths::read_private_text(&path)?;
        validate_secret(value.trim())?;
        return Ok(value.trim().to_owned());
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let value = STANDARD.encode(bytes);
    atomic_write_private(&path, format!("{value}\n").as_bytes(), 0o600)?;
    Ok(value)
}

fn optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => bail!("{name} 已设置但为空"),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} 不是有效 UTF-8"),
    }
}

fn validate_secret(value: &str) -> Result<()> {
    let decoded = STANDARD
        .decode(value)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value))
        .context("BROCADE_SECRET_KEY 不是有效 base64")?;
    if decoded.len() != 32 {
        bail!("BROCADE_SECRET_KEY 解码后必须正好是 32 字节");
    }
    Ok(())
}

fn remove_stale_runtime_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("无法清理 {}", path.display())),
    }
}

fn redact_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return "configured".to_owned();
    };
    let scheme = &url[..scheme_end];
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let suffix = &rest[authority_end..];
    format!("{scheme}://{host}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_url_log_never_contains_credentials() {
        assert_eq!(
            redact_database_url(
                "postgres://brocade:secret@db.example:5432/brocade?sslmode=require"
            ),
            "postgres://db.example:5432/brocade?sslmode=require"
        );
        assert_eq!(redact_database_url("opaque"), "configured");
    }

    #[test]
    fn unspecified_binds_are_reached_through_loopback() {
        assert_eq!(
            reachable_loopback("0.0.0.0:8080".parse().unwrap()),
            "127.0.0.1:8080".parse().unwrap()
        );
        assert_eq!(
            reachable_loopback("[::]:8080".parse().unwrap()),
            "[::1]:8080".parse().unwrap()
        );
    }

    #[test]
    fn direct_ip_bootstrap_prints_a_complete_initialization_link() {
        let token = "bootstrap-test-token-with-at-least-32-bytes";
        let message = initialization_instructions("http://192.0.2.10:8080", token, true).unwrap();
        assert!(message.contains(&format!(
            "初始化地址：http://192.0.2.10:8080/#/initialize/{token}"
        )));
        let logged = initialization_instructions("http://192.0.2.10:8080", token, false).unwrap();
        assert!(!logged.contains(token));
        assert!(logged.contains("sudo brocade url"));
    }

    #[test]
    fn release_installer_records_direct_access_and_database_choices() {
        const INSTALLER: &str = include_str!("../../../install.sh");
        assert!(INSTALLER.contains("--public-ip"));
        assert!(INSTALLER.contains("--port"));
        assert!(INSTALLER.contains("BROCADE_AGENT_PUBLIC_URL"));
        assert!(INSTALLER.contains("Brocade 托管 PostgreSQL"));
        assert!(INSTALLER.contains("--managed-db"));
        assert!(INSTALLER.contains("--external-db"));
        assert!(INSTALLER.contains("--no-start"));
        assert!(INSTALLER.contains("\"$command_path\" url"));
        assert!(!INSTALLER.contains("cloudflared"));
        assert!(!INSTALLER.contains("--tunnel"));
        let verify = INSTALLER.find("sha256sum -c SHA256SUMS").unwrap();
        let service = INSTALLER.find("systemctl restart brocade.service").unwrap();
        assert!(
            verify < service,
            "integrity checks must precede service changes"
        );
    }

    #[test]
    fn cli_accepts_ip_port_and_rejects_removed_tunnel_options() {
        let cli = Cli::try_parse_from(["brocade", "address", "--public-ip", "192.0.2.10"]).unwrap();
        let LauncherCommand::Address(args) = cli.command else {
            panic!("address command")
        };
        let access = args.resolve().unwrap();
        assert_eq!(access.bind, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(access.public_url, "http://192.0.2.10:8080");
        let cli = Cli::try_parse_from([
            "brocade",
            "address",
            "--public-ip",
            "2001:db8::10",
            "--port",
            "9000",
        ])
        .unwrap();
        let LauncherCommand::Address(args) = cli.command else {
            panic!("address command")
        };
        let access = args.resolve().unwrap();
        assert_eq!(access.bind, "[::]:9000".parse().unwrap());
        assert_eq!(access.public_url, "http://[2001:db8::10]:9000");
        for command in ["up", "url", "address"] {
            for flag in ["--tunnel", "--cloudflared-bin", "--cloudflared-version"] {
                assert!(Cli::try_parse_from(["brocade", command, flag]).is_err());
            }
            for port in ["0", "65536", "invalid"] {
                assert!(Cli::try_parse_from(["brocade", command, "--port", port]).is_err());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn launcher_bootstrap_token_is_private_and_reused_until_initialization() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let first = bootstrap_token(root.path(), None).unwrap();
        let path = first.private_file.as_ref().unwrap();
        assert_eq!(path, &root.path().join("bootstrap-token"));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let first_digest = Sha256::digest(first.secret.as_bytes());

        let second = bootstrap_token(root.path(), None).unwrap();
        assert_eq!(Sha256::digest(second.secret.as_bytes()), first_digest);
        assert_eq!(second.private_file.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn configured_bootstrap_token_must_have_minimum_entropy_budget() {
        let root = tempfile::tempdir().unwrap();
        let error = bootstrap_token(root.path(), Some("too-short".to_owned()))
            .err()
            .unwrap();
        assert!(error.to_string().contains("32 至 512 字节"));
        assert!(!root.path().join("bootstrap-token").exists());
    }
}
