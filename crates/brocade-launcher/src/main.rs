mod cloudflare;
mod database;
mod download;
mod paths;
mod platform;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::{Args, Parser, Subcommand};
use cloudflare::{CloudflaredBinary, QuickTunnel};
use database::{DatabaseChoice, ManagedPostgres};
use paths::{atomic_write_private, LauncherPaths};
use platform::Platform;
use serde::Deserialize;
use std::{
    env, fs,
    future::pending,
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
}

#[derive(Debug, Args)]
struct UpArgs {
    /// 强制使用受管 PostgreSQL；不能同时设置 DATABASE_URL
    #[arg(long, conflicts_with = "external_db")]
    managed_db: bool,

    /// 强制使用外部 PostgreSQL；缺少 DATABASE_URL 时立即失败
    #[arg(long, conflicts_with = "managed_db")]
    external_db: bool,

    /// 建立无需域名的 Cloudflare Quick Tunnel
    #[arg(long)]
    tunnel: bool,

    /// Brocade 持久数据根目录
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// 下载及可再生缓存根目录
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// brocade-console 可执行文件
    #[arg(long, value_name = "PATH")]
    console_bin: Option<PathBuf>,

    /// Console 监听地址；默认 127.0.0.1:8080
    #[arg(long, value_name = "ADDR")]
    bind: Option<SocketAddr>,

    /// 使用指定 cloudflared，不下载受管版本
    #[arg(
        long,
        value_name = "PATH",
        requires = "tunnel",
        conflicts_with = "cloudflared_version"
    )]
    cloudflared_bin: Option<PathBuf>,

    /// 指定 cloudflared 版本；默认查询官方 latest
    #[arg(long, value_name = "VERSION", requires = "tunnel")]
    cloudflared_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthState {
    initialized: bool,
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
    }
}

async fn up(args: UpArgs) -> Result<()> {
    let platform = Platform::current()?;
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

    let admin_bind = match args.bind {
        Some(bind) => bind,
        None => optional_env("BROCADE_ADMIN_BIND")?
            .map(|value| {
                value
                    .parse()
                    .context("BROCADE_ADMIN_BIND 不是合法的 IP:PORT")
            })
            .transpose()?
            .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))),
    };
    if args.tunnel
        && env::var("BROCADE_AGENT_BIND")
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    {
        bail!("--tunnel 要求 Console 与 Agent 共用一个监听端口；请移除 BROCADE_AGENT_BIND");
    }
    let local_addr = reachable_loopback(admin_bind);
    let local_url = format!("http://{local_addr}");
    let public_url_file = paths.runtime.join("public-url");
    if args.tunnel {
        remove_stale_runtime_file(&public_url_file)?;
    }

    let secret = if managed.is_some() {
        Some(managed_secret(&paths.data)?)
    } else {
        None
    };
    let console_bin = console_binary(args.console_bin.clone())?;
    let mut console_command = Command::new(&console_bin);
    console_command
        .env("DATABASE_URL", &database_url)
        .env("BROCADE_ADMIN_BIND", admin_bind.to_string())
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
    if args.tunnel {
        console_command.env("BROCADE_RUNTIME_PUBLIC_URL_FILE", &public_url_file);
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
            let mut no_tunnel = None;
            cleanup(&mut no_tunnel, &mut console, managed).await;
            return Err(error);
        }
        None => {
            let mut no_tunnel = None;
            cleanup(&mut no_tunnel, &mut console, managed).await;
            return Ok(());
        }
    }
    println!("Local: {local_url}");

    if args.tunnel {
        match wait_until_initialized(&client, &local_url, &mut console, &mut shutdown).await {
            Ok(true) => {}
            Ok(false) => {
                let mut no_tunnel = None;
                cleanup(&mut no_tunnel, &mut console, managed).await;
                return Ok(());
            }
            Err(error) => {
                let _ = remove_stale_runtime_file(&public_url_file);
                let mut no_tunnel = None;
                cleanup(&mut no_tunnel, &mut console, managed).await;
                return Err(error);
            }
        }
    }

    let mut tunnel = if args.tunnel {
        let started = tokio::select! {
            _ = shutdown.as_mut() => None,
            result = start_tunnel(
                &args,
                &paths,
                platform,
                &client,
                &local_url,
                &public_url_file,
            ) => Some(result),
        };
        match started {
            Some(Ok(tunnel)) => Some(tunnel),
            Some(Err(error)) => {
                let _ = remove_stale_runtime_file(&public_url_file);
                let mut no_tunnel = None;
                cleanup(&mut no_tunnel, &mut console, managed).await;
                return Err(error);
            }
            None => {
                let _ = remove_stale_runtime_file(&public_url_file);
                let mut no_tunnel = None;
                cleanup(&mut no_tunnel, &mut console, managed).await;
                return Ok(());
            }
        }
    } else {
        None
    };

    let event = tokio::select! {
        _ = &mut shutdown => RuntimeExit::Signal,
        status = console.wait() => RuntimeExit::Console(status?),
        status = wait_for_tunnel(&mut tunnel) => RuntimeExit::Tunnel(status?),
        result = wait_for_postgres(managed.as_ref()) => RuntimeExit::Postgres(result),
    };

    if args.tunnel {
        if let Err(error) = remove_stale_runtime_file(&public_url_file) {
            eprintln!("清理临时公网地址失败：{error:#}");
        }
    }
    cleanup(&mut tunnel, &mut console, managed).await;
    match event {
        RuntimeExit::Signal => Ok(()),
        RuntimeExit::Console(status) if status.success() => Ok(()),
        RuntimeExit::Console(status) => bail!("brocade-console 意外退出：{status}"),
        RuntimeExit::Tunnel(status) => bail!("cloudflared 意外退出：{status}"),
        RuntimeExit::Postgres(result) => result,
    }
}

async fn start_tunnel(
    args: &UpArgs,
    paths: &LauncherPaths,
    platform: Platform,
    client: &reqwest::Client,
    local_url: &str,
    public_url_file: &Path,
) -> Result<QuickTunnel> {
    let environment_version = optional_env("BROCADE_CLOUDFLARED_VERSION")?;
    let requested_version = args
        .cloudflared_version
        .as_deref()
        .or(environment_version.as_deref());
    let explicit_binary = args.cloudflared_bin.clone().or_else(|| {
        env::var_os("BROCADE_CLOUDFLARED_BIN")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    if explicit_binary.is_some() && requested_version.is_some() {
        bail!("指定 cloudflared 路径时不能同时指定 BROCADE_CLOUDFLARED_VERSION");
    }
    let cloudflared = CloudflaredBinary::prepare(
        explicit_binary,
        requested_version,
        &paths.cache,
        platform,
        client,
    )
    .await?;
    if let Some(version) = &cloudflared.version {
        eprintln!("Tunnel: cloudflared {version}");
    }
    let tunnel = QuickTunnel::start(&cloudflared, local_url, &paths.runtime, client).await?;
    atomic_write_private(
        public_url_file,
        format!("{}\n", tunnel.url).as_bytes(),
        0o600,
    )?;
    println!("Public: {}", tunnel.url);
    Ok(tunnel)
}

enum RuntimeExit {
    Signal,
    Console(ExitStatus),
    Tunnel(ExitStatus),
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
    console: &mut Child,
    shutdown: &mut std::pin::Pin<Box<impl std::future::Future<Output = ()>>>,
) -> Result<bool> {
    if auth_state(client, local_url).await?.initialized {
        return Ok(true);
    }
    println!(
        "首次启动尚未创建管理员。请先打开 {local_url} 完成初始化；完成前不会开放公网 Tunnel。"
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

async fn wait_for_tunnel(tunnel: &mut Option<QuickTunnel>) -> Result<ExitStatus> {
    match tunnel {
        Some(tunnel) => Ok(tunnel.child.wait().await?),
        None => pending().await,
    }
}

async fn wait_for_postgres(postgres: Option<&ManagedPostgres>) -> Result<()> {
    match postgres {
        Some(postgres) => postgres.wait_for_exit().await,
        None => pending().await,
    }
}

async fn cleanup(
    tunnel: &mut Option<QuickTunnel>,
    console: &mut Child,
    postgres: Option<ManagedPostgres>,
) {
    if let Some(tunnel) = tunnel {
        stop_child("cloudflared", &mut tunnel.child, Duration::from_secs(20)).await;
    }
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
}
