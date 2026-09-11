use anyhow::{bail, Context, Result};
use reqwest::Client;
use sha2::{Digest, Sha256};

pub(crate) const MAX_RUNTIME_ASSET_BYTES: u64 = 128 * 1024 * 1024;

pub(crate) fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("brocade-launcher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("无法建立运行时下载客户端")
}

pub(crate) async fn verified_bytes(
    client: &Client,
    url: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("下载失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("下载地址返回错误：{url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RUNTIME_ASSET_BYTES)
    {
        bail!("拒绝下载超过 128 MiB 的运行时文件：{url}");
    }
    let bytes = response.bytes().await?.to_vec();
    if bytes.len() as u64 > MAX_RUNTIME_ASSET_BYTES {
        bail!("下载的运行时文件超过 128 MiB：{url}");
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected_sha256 {
        bail!("运行时文件 SHA-256 不匹配：期望 {expected_sha256}，实际 {actual}");
    }
    Ok(bytes)
}

pub(crate) fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
