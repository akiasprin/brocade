use crate::{
    download::{self, sha256_file},
    paths::{atomic_write_private, ensure_private_dir},
    platform::Platform,
};
use anyhow::{bail, Context, Result};
use semver::Version;
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

const RELEASE_API: &str = "https://api.github.com/repos/cloudflare/cloudflared/releases";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CloudflaredBinary {
    pub(crate) path: PathBuf,
    pub(crate) version: Option<Version>,
}

impl CloudflaredBinary {
    pub(crate) async fn prepare(
        explicit: Option<PathBuf>,
        requested_version: Option<&str>,
        cache_root: &Path,
        platform: Platform,
        client: &reqwest::Client,
    ) -> Result<Self> {
        if let Some(path) = explicit {
            let path = fs::canonicalize(&path)
                .with_context(|| format!("无法读取 cloudflared：{}", path.display()))?;
            verify_executable(&path, None).await?;
            return Ok(Self {
                path,
                version: None,
            });
        }

        let release_result = fetch_release(client, requested_version).await;
        match release_result {
            Ok(release) => install_release(cache_root, platform, client, release).await,
            Err(error) => {
                if requested_version.is_some() {
                    return Err(error);
                }
                if let Some(cached) = newest_verified_cache(cache_root).await? {
                    eprintln!(
                        "无法查询 cloudflared 最新版本，继续使用已验证缓存 {}：{error:#}",
                        cached
                            .version
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "unknown".to_owned())
                    );
                    Ok(cached)
                } else {
                    Err(error)
                }
            }
        }
    }
}

async fn fetch_release(
    client: &reqwest::Client,
    requested_version: Option<&str>,
) -> Result<GitHubRelease> {
    let endpoint = match requested_version {
        None | Some("latest") => format!("{RELEASE_API}/latest"),
        Some(requested) => {
            let version = Version::parse(requested.trim_start_matches('v'))
                .with_context(|| format!("cloudflared 版本不是合法 semver：{requested}"))?;
            format!("{RELEASE_API}/tags/{version}")
        }
    };
    client
        .get(endpoint)
        .send()
        .await
        .context("无法查询 cloudflared Release")?
        .error_for_status()
        .context("Cloudflare Release API 返回错误")?
        .json()
        .await
        .context("无法解析 cloudflared Release")
}

async fn install_release(
    cache_root: &Path,
    platform: Platform,
    client: &reqwest::Client,
    release: GitHubRelease,
) -> Result<CloudflaredBinary> {
    let version = Version::parse(release.tag_name.trim_start_matches('v'))
        .context("Cloudflare Release tag 不是合法 semver")?;
    let asset_name = platform.cloudflared_asset();
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .with_context(|| format!("cloudflared {version} 缺少 {asset_name}"))?;
    let digest = release_asset_digest(&release, asset)?;
    let expected_prefix = format!(
        "https://github.com/cloudflare/cloudflared/releases/download/{}/",
        release.tag_name
    );
    if !asset.browser_download_url.starts_with(&expected_prefix) {
        bail!("cloudflared Release 返回了非官方资产地址");
    }

    let directory = cache_root.join("cloudflared").join(version.to_string());
    ensure_private_dir(&directory)?;
    let binary = directory.join("cloudflared");
    let digest_file = directory.join("sha256");
    if binary.is_file()
        && fs::read_to_string(&digest_file)
            .ok()
            .is_some_and(|value| value.trim() == digest)
        && sha256_file(&binary)? == digest
    {
        verify_executable(&binary, Some(&version)).await?;
        return Ok(CloudflaredBinary {
            path: binary,
            version: Some(version),
        });
    }

    eprintln!("cloudflared {version} 首次下载中…");
    let bytes = download::verified_bytes(client, &asset.browser_download_url, &digest).await?;
    atomic_write_private(&binary, &bytes, 0o500)?;
    atomic_write_private(&digest_file, format!("{digest}\n").as_bytes(), 0o600)?;
    verify_executable(&binary, Some(&version)).await?;
    Ok(CloudflaredBinary {
        path: binary,
        version: Some(version),
    })
}

fn release_asset_digest(release: &GitHubRelease, asset: &GitHubAsset) -> Result<String> {
    let digest = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_owned)
        .or_else(|| {
            checksum_from_release_body(release.body.as_deref().unwrap_or_default(), &asset.name)
        })
        .context("cloudflared Release 没有可验证的 SHA-256")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("cloudflared Release 的 SHA-256 格式无效");
    }
    Ok(digest.to_ascii_lowercase())
}

fn checksum_from_release_body(body: &str, asset_name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let line = line.trim().trim_matches('`').trim();
        let rest = line.strip_prefix(asset_name)?.strip_prefix(':')?.trim();
        let digest = rest.split_whitespace().next()?;
        (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| digest.to_ascii_lowercase())
    })
}

async fn newest_verified_cache(cache_root: &Path) -> Result<Option<CloudflaredBinary>> {
    let root = cache_root.join("cloudflared");
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(None);
    };
    let mut versions = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            Version::parse(&entry.file_name().to_string_lossy())
                .ok()
                .map(|version| (version, entry.path()))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    for (version, directory) in versions {
        let binary = directory.join("cloudflared");
        let expected = match fs::read_to_string(directory.join("sha256")) {
            Ok(value) => value.trim().to_owned(),
            Err(_) => continue,
        };
        if expected.len() != 64 || sha256_file(&binary).ok().as_deref() != Some(&expected) {
            continue;
        }
        if verify_executable(&binary, Some(&version)).await.is_ok() {
            return Ok(Some(CloudflaredBinary {
                path: binary,
                version: Some(version),
            }));
        }
    }
    Ok(None)
}

async fn verify_executable(path: &Path, expected_version: Option<&Version>) -> Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(path)
            .arg("version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("cloudflared version 检查超时")??;
    if !output.status.success() {
        bail!("cloudflared 无法执行：{}", path.display());
    }
    if let Some(expected) = expected_version {
        let banner = String::from_utf8_lossy(&output.stdout);
        let expected = expected.to_string();
        if !banner.split_whitespace().any(|part| part == expected) {
            bail!("cloudflared 二进制版本与 Release {expected} 不一致");
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct QuickTunnelInfo {
    hostname: String,
}

pub(crate) struct QuickTunnel {
    pub(crate) child: Child,
    pub(crate) url: String,
}

impl QuickTunnel {
    pub(crate) async fn start(
        cloudflared: &CloudflaredBinary,
        origin: &str,
        runtime_dir: &Path,
        client: &reqwest::Client,
    ) -> Result<Self> {
        let metrics_listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let metrics_port = metrics_listener.local_addr()?.port();
        drop(metrics_listener);
        let metrics = format!("127.0.0.1:{metrics_port}");
        let empty_config = runtime_dir.join("cloudflared-empty.yml");
        // An explicit empty mapping prevents a user's ~/.cloudflared/config.yml from turning this
        // into a named tunnel. A comment-only file is semantically empty too, but cloudflared logs
        // it as an error before continuing.
        atomic_write_private(&empty_config, b"{}\n", 0o600)?;

        let mut child = Command::new(&cloudflared.path)
            .arg("tunnel")
            .arg("--config")
            .arg(empty_config)
            .arg("--no-autoupdate")
            .arg("--metrics")
            .arg(&metrics)
            .arg("--url")
            .arg(origin)
            .arg("--loglevel")
            .arg("info")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("无法启动 cloudflared")?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let quick_url = loop {
            if let Some(status) = child.try_wait()? {
                bail!("cloudflared 在建立 Tunnel 前退出：{status}");
            }
            let quick = client
                .get(format!("http://{metrics}/quicktunnel"))
                .send()
                .await
                .ok()
                .and_then(|response| response.error_for_status().ok());
            if let Some(response) = quick {
                if let Ok(info) = response.json::<QuickTunnelInfo>().await {
                    if !info.hostname.is_empty() {
                        let ready = client
                            .get(format!("http://{metrics}/ready"))
                            .send()
                            .await
                            .is_ok_and(|response| response.status().is_success());
                        if ready {
                            break format!("https://{}", info.hostname);
                        }
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                bail!("cloudflared 在 60 秒内没有建立 Quick Tunnel");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        Ok(Self {
            child,
            url: quick_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checksum_from_cloudflare_release_body() {
        let digest = "f29324fe934d1e100617484c78deef803c4dc2cd351d645bbde42e96b4fccc5e";
        let body = format!("### SHA256 Checksums:\n`cloudflared-linux-amd64: {digest}\nother: 00`");
        assert_eq!(
            checksum_from_release_body(&body, "cloudflared-linux-amd64").as_deref(),
            Some(digest)
        );
        assert!(checksum_from_release_body(&body, "cloudflared-linux-arm64").is_none());
    }

    #[test]
    fn api_digest_takes_precedence_and_is_validated() {
        let release = GitHubRelease {
            tag_name: "2026.8.3".into(),
            body: Some(String::new()),
            assets: Vec::new(),
        };
        let asset = GitHubAsset {
            name: "cloudflared-linux-amd64".into(),
            browser_download_url: String::new(),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        };
        assert_eq!(
            release_asset_digest(&release, &asset).unwrap(),
            "a".repeat(64)
        );
    }
}
