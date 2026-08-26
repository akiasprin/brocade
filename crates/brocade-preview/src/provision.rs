use std::{net::Ipv4Addr, net::Ipv6Addr, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::{
    addr::{validate_slot_prefixes, PreviewAddress},
    config::Config,
    docker::{
        container_name, docker_ok, docker_output, docker_output_owned, LABEL_IPV4, LABEL_IPV6,
        LABEL_IP_LEGACY, LABEL_NODE_ID, LABEL_PREVIEW,
    },
    error::PreviewError,
};

/// Where the install script's output lands inside a container, read by the node log endpoint.
pub(crate) const INSTALL_LOG_PATH: &str = "/var/log/brocade-preview-install.log";

#[derive(Debug, Serialize)]
pub(crate) struct PreviewRuntime {
    container_name: String,
    /// Backward-compatible alias for existing UI code.
    ip: Ipv4Addr,
    ipv4: Ipv4Addr,
    ipv6: Ipv6Addr,
    network: String,
    image: String,
    install_started: bool,
    install_log_path: &'static str,
}

/// Start a node container, then run the production install script inside it in the background.
pub(crate) async fn start_node_container(
    config: &Config,
    node_id: &str,
    address: PreviewAddress,
    enrollment_token: &str,
) -> Result<PreviewRuntime, PreviewError> {
    ensure_network(config).await?;
    ensure_node_image(config).await?;
    ensure_agent_binary(config).await?;
    let container = container_name(node_id);
    if docker_ok(config, &["container", "inspect", &container]).await {
        return Err(PreviewError::bad_request(format!(
            "preview container already exists: {container}"
        )));
    }
    docker_output_owned(
        config,
        vec![
            "run".to_owned(),
            "-d".to_owned(),
            "--name".to_owned(),
            container.clone(),
            "--hostname".to_owned(),
            node_id.to_owned(),
            "--network".to_owned(),
            config.network.clone(),
            "--ip".to_owned(),
            address.ipv4.to_string(),
            "--ip6".to_owned(),
            address.ipv6.to_string(),
            "--cap-add".to_owned(),
            "NET_ADMIN".to_owned(),
            // phantun opens a TUN device, and a container has no /dev/net/tun node by default.
            // NET_ADMIN grants the ability to configure networking, not the presence of that
            // character device — without it phantun panics on startup (opening /dev/net/tun yields
            // ENOENT), while the agent only checks whether the process started and cannot see the
            // reason, so that machine's release still reports success. wg0 is unaffected: it goes
            // through `ip link add type wireguard`, a kernel module rather than a TUN.
            "--device".to_owned(),
            "/dev/net/tun".to_owned(),
            "--sysctl".to_owned(),
            "net.ipv4.ip_forward=1".to_owned(),
            "--sysctl".to_owned(),
            "net.ipv6.conf.all.forwarding=1".to_owned(),
            "--add-host".to_owned(),
            "host.docker.internal:host-gateway".to_owned(),
            "--label".to_owned(),
            format!("{LABEL_PREVIEW}=1"),
            "--label".to_owned(),
            format!("{LABEL_NODE_ID}={node_id}"),
            "--label".to_owned(),
            format!("{LABEL_IP_LEGACY}={}", address.ipv4),
            "--label".to_owned(),
            format!("{LABEL_IPV4}={}", address.ipv4),
            "--label".to_owned(),
            format!("{LABEL_IPV6}={}", address.ipv6),
            "--entrypoint".to_owned(),
            "sleep".to_owned(),
            config.node_image.clone(),
            "infinity".to_owned(),
        ],
    )
    .await?;
    let install = install_command(config);
    docker_output_owned(
        config,
        vec![
            "exec".to_owned(),
            "-d".to_owned(),
            "-e".to_owned(),
            format!("BROCADE_ENROLL_TOKEN={enrollment_token}"),
            container.clone(),
            "sh".to_owned(),
            "-lc".to_owned(),
            format!("{install} >{INSTALL_LOG_PATH} 2>&1"),
        ],
    )
    .await?;
    Ok(PreviewRuntime {
        container_name: container,
        ip: address.ipv4,
        ipv4: address.ipv4,
        ipv6: address.ipv6,
        network: config.network.clone(),
        image: config.node_image.clone(),
        install_started: true,
        install_log_path: INSTALL_LOG_PATH,
    })
}

/// Assemble the install command to run inside the container: fetch the production install.sh and
/// run it with the production arguments.
fn install_command(config: &Config) -> String {
    let agent_bin_url = config
        .agent_bin_url
        .clone()
        .unwrap_or_else(|| format!("{}/preview/dist/brocade-agent", config.preview_public_url));
    let agent_sha = config
        .agent_bin_sha256
        .clone()
        .or_else(|| sha256_file(&config.agent_bin_path).ok());
    let fetch = format!(
        "curl -fsSL {}/enroll/install.sh -o /tmp/brocade-install.sh",
        shell_quote(&config.agent_url)
    );
    let mut install = format!(
        "sh /tmp/brocade-install.sh --server {} --apply linux --service-mode foreground",
        shell_quote(&config.agent_url)
    );
    install.push_str(" --agent-bin-url ");
    install.push_str(&shell_quote(&agent_bin_url));
    if let Some(sha256) = &agent_sha {
        install.push_str(" --agent-bin-sha256 ");
        install.push_str(&shell_quote(sha256));
    }
    if let Some(url) = &config.xray_bin_url {
        install.push_str(" --xray-bin-url ");
        install.push_str(&shell_quote(url));
    }
    if let Some(sha256) = &config.xray_bin_sha256 {
        install.push_str(" --xray-bin-sha256 ");
        install.push_str(&shell_quote(sha256));
    }
    format!("{fetch} && {install}")
}

/// With no external agent download URL, build one locally and serve it to the container.
pub(crate) async fn ensure_agent_binary(config: &Config) -> Result<(), PreviewError> {
    if config.agent_bin_url.is_some() {
        return Ok(());
    }
    if config.agent_bin_path.is_file() {
        return Ok(());
    }
    let output = Command::new("cargo")
        .args(["build", "-p", "brocade-agent"])
        .current_dir(&config.workspace)
        .output()
        .await
        .map_err(|error| {
            PreviewError::internal(format!("run cargo build -p brocade-agent: {error}"))
        })?;
    if !output.status.success() {
        return Err(PreviewError::internal(format!(
            "cargo build -p brocade-agent failed:\n{}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )));
    }
    if !config.agent_bin_path.is_file() {
        return Err(PreviewError::internal(format!(
            "cargo build finished but {} does not exist",
            config.agent_bin_path.display()
        )));
    }
    Ok(())
}

pub(crate) async fn ensure_node_image(config: &Config) -> Result<(), PreviewError> {
    if docker_ok(config, &["image", "inspect", &config.node_image]).await {
        return Ok(());
    }
    if !config.node_image_context.is_dir() {
        return Err(PreviewError::internal(format!(
            "preview node image {} is missing and build context {} does not exist",
            config.node_image,
            config.node_image_context.display()
        )));
    }
    docker_output_owned(
        config,
        vec![
            "build".to_owned(),
            "-t".to_owned(),
            config.node_image.clone(),
            config.node_image_context.display().to_string(),
        ],
    )
    .await?;
    Ok(())
}

/// Create the dual-stack preview network. Where one exists without IPv6, the operator decides what
/// to do about it.
pub(crate) async fn ensure_network(config: &Config) -> Result<(), PreviewError> {
    if docker_ok(config, &["network", "inspect", &config.network]).await {
        let enabled = docker_output_owned(
            config,
            vec![
                "network".to_owned(),
                "inspect".to_owned(),
                "-f".to_owned(),
                "{{.EnableIPv6}}".to_owned(),
                config.network.clone(),
            ],
        )
        .await?
        .trim()
        .eq_ignore_ascii_case("true");
        if enabled {
            return Ok(());
        }
        return Err(PreviewError::bad_request(format!(
            "preview network {} exists without IPv6; remove preview containers and the network, or set BROCADE_PREVIEW_NETWORK to a new name",
            config.network
        )));
    }
    validate_slot_prefixes(config)?;
    docker_output(
        config,
        &[
            "network",
            "create",
            "--ipv6",
            "--subnet",
            &config.subnet,
            "--subnet",
            &config.subnet_ipv6,
            &config.network,
        ],
    )
    .await?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, PreviewError> {
    let bytes = std::fs::read(path)
        .map_err(|error| PreviewError::internal(format!("read {}: {error}", path.display())))?;
    let digest = Sha256::digest(&bytes);
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a b; rm -rf /"), "'a b; rm -rf /'");
    }

    #[test]
    fn hex_lower_pads_every_byte_to_two_digits() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn install_command_defaults_the_agent_url_to_the_preview_endpoint() {
        let config = Config::for_test();

        let command = install_command(&config);

        assert!(command.contains("curl -fsSL 'http://host.docker.internal:8081'/enroll/install.sh"));
        assert!(command.contains("--service-mode foreground"));
        assert!(command.contains(
            "--agent-bin-url 'http://host.docker.internal:8090/preview/dist/brocade-agent'"
        ));
    }

    #[test]
    fn install_command_passes_through_configured_binaries_and_digests() {
        let mut config = Config::for_test();
        config.agent_bin_url = Some("https://example.test/agent".to_owned());
        config.agent_bin_sha256 = Some("abc123".to_owned());
        config.xray_bin_url = Some("https://example.test/xray".to_owned());
        config.xray_bin_sha256 = Some("def456".to_owned());

        let command = install_command(&config);

        assert!(command.contains("--agent-bin-url 'https://example.test/agent'"));
        assert!(command.contains("--agent-bin-sha256 'abc123'"));
        assert!(command.contains("--xray-bin-url 'https://example.test/xray'"));
        assert!(command.contains("--xray-bin-sha256 'def456'"));
    }

    #[test]
    fn install_command_omits_the_digest_when_the_binary_is_unreadable() {
        let mut config = Config::for_test();
        config.agent_bin_path = "/nonexistent/brocade-agent".into();

        let command = install_command(&config);

        assert!(!command.contains("--agent-bin-sha256"));
    }
}
