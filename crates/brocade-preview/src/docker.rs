use std::{net::IpAddr, process::Stdio};

use tokio::process::Command;

use crate::{config::Config, error::PreviewError};

/// The probe traffic's originating end, where a one-shot xray client runs during subscription
/// verification.
pub(crate) const CLIENT_CONTAINER: &str = "brocade-preview-client";

/// The probe traffic's endpoint, answering with JSON carrying the source IP.
pub(crate) const ECHO_CONTAINER: &str = "brocade-preview-echo";

// The labels stamped on containers. The writing side is provision::start_node_container and
// probe::ensure_probe_containers, the reading side addr::allocate_address and this file's
// preview_container_by_ip, and both must use one set of keys.
pub(crate) const LABEL_PREVIEW: &str = "brocade.preview";
pub(crate) const LABEL_NODE_ID: &str = "brocade.node_id";
pub(crate) const LABEL_ROLE: &str = "brocade.preview.role";
pub(crate) const LABEL_IPV6: &str = "brocade.preview.ipv6";
pub(crate) const LABEL_IPV4: &str = "brocade.preview.ipv4";

/// `LABEL_IPV4`'s old name, still stamped on containers and read as a fallback.
pub(crate) const LABEL_IP_LEGACY: &str = "brocade.preview.ip";

/// A node container's name.
///
/// Note that it shares a namespace with `CLIENT_CONTAINER` and `ECHO_CONTAINER`: a node_id of
/// `client` or `echo` produces a name colliding with the helper containers.
pub(crate) fn container_name(node_id: &str) -> String {
    format!("brocade-preview-{node_id}")
}

/// The Go template fragment for reading one label in `docker ps --format`.
pub(crate) fn label_template(key: &str) -> String {
    format!("{{{{.Label \"{key}\"}}}}")
}

pub(crate) async fn docker_ok(config: &Config, args: &[&str]) -> bool {
    Command::new(&config.docker)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

pub(crate) async fn docker_output(config: &Config, args: &[&str]) -> Result<String, PreviewError> {
    docker_output_owned(
        config,
        args.iter().map(|value| (*value).to_owned()).collect(),
    )
    .await
}

pub(crate) async fn docker_output_owned(
    config: &Config,
    args: Vec<String>,
) -> Result<String, PreviewError> {
    let output = Command::new(&config.docker)
        .args(&args)
        .output()
        .await
        .map_err(|error| PreviewError::internal(format!("run {}: {error}", config.docker)))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Err(PreviewError::internal(format!(
            "{} {} failed: {}{}{}",
            config.docker,
            args.join(" "),
            stderr,
            if stderr.is_empty() || stdout.is_empty() {
                ""
            } else {
                "\n"
            },
            stdout
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) async fn preview_container_by_ip(
    config: &Config,
    ip: IpAddr,
) -> Result<Option<String>, PreviewError> {
    let label = match ip {
        IpAddr::V4(ip) => format!("label={LABEL_IPV4}={ip}"),
        IpAddr::V6(ip) => format!("label={LABEL_IPV6}={ip}"),
    };
    let found = preview_container_by_label(config, label).await?;
    if found.is_some() {
        return Ok(found);
    }
    if let IpAddr::V4(ip) = ip {
        return preview_container_by_label(config, format!("label={LABEL_IP_LEGACY}={ip}")).await;
    }
    Ok(None)
}

async fn preview_container_by_label(
    config: &Config,
    label_filter: String,
) -> Result<Option<String>, PreviewError> {
    let output = docker_output_owned(
        config,
        vec![
            "ps".to_owned(),
            "--filter".to_owned(),
            label_filter,
            "--format".to_owned(),
            "{{.Names}}".to_owned(),
        ],
    )
    .await?;
    Ok(output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned))
}
