use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) bind: SocketAddr,
    pub(crate) console_url: String,
    pub(crate) agent_url: String,
    pub(crate) preview_public_url: String,
    pub(crate) docker: String,
    pub(crate) network: String,
    pub(crate) subnet: String,
    pub(crate) ip_prefix: String,
    pub(crate) subnet_ipv6: String,
    pub(crate) ip6_prefix: String,
    pub(crate) node_image: String,
    pub(crate) node_image_context: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) agent_bin_path: PathBuf,
    pub(crate) agent_bin_url: Option<String>,
    pub(crate) agent_bin_sha256: Option<String>,
    pub(crate) xray_bin_url: Option<String>,
    pub(crate) xray_bin_sha256: Option<String>,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, String> {
        let bind = env::var("BROCADE_PREVIEW_BIND")
            .unwrap_or_else(|_| "0.0.0.0:8090".to_owned())
            .parse()
            .map_err(|error| format!("invalid BROCADE_PREVIEW_BIND: {error}"))?;
        let console_url = trim_url(
            env::var("BROCADE_PREVIEW_CONSOLE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned()),
        );
        let agent_url = trim_url(
            env::var("BROCADE_PREVIEW_AGENT_URL")
                .unwrap_or_else(|_| "http://host.docker.internal:8081".to_owned()),
        );
        let preview_public_url = trim_url(
            env::var("BROCADE_PREVIEW_PUBLIC_URL")
                .unwrap_or_else(|_| "http://host.docker.internal:8090".to_owned()),
        );
        let subnet =
            env::var("BROCADE_PREVIEW_SUBNET").unwrap_or_else(|_| "172.31.90.0/24".to_owned());
        let ip_prefix =
            env::var("BROCADE_PREVIEW_IP_PREFIX").unwrap_or_else(|_| "172.31.90".to_owned());
        let subnet_ipv6 = env::var("BROCADE_PREVIEW_IPV6_SUBNET")
            .unwrap_or_else(|_| "fd42:31:90::/64".to_owned());
        let ip6_prefix =
            env::var("BROCADE_PREVIEW_IP6_PREFIX").unwrap_or_else(|_| "fd42:31:90::".to_owned());
        let workspace = env::var("BROCADE_PREVIEW_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let agent_bin_path = env::var("BROCADE_PREVIEW_AGENT_BIN_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| workspace.join("target/debug/brocade-agent"));
        Ok(Self {
            bind,
            console_url,
            agent_url,
            preview_public_url,
            docker: env::var("BROCADE_PREVIEW_DOCKER").unwrap_or_else(|_| "docker".to_owned()),
            network: env::var("BROCADE_PREVIEW_NETWORK")
                .unwrap_or_else(|_| "brocade-preview".to_owned()),
            subnet,
            ip_prefix,
            subnet_ipv6,
            ip6_prefix,
            node_image: env::var("BROCADE_PREVIEW_NODE_IMAGE")
                .unwrap_or_else(|_| "brocade-lab-node:latest".to_owned()),
            node_image_context: env::var("BROCADE_PREVIEW_NODE_IMAGE_CONTEXT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| workspace.join("crates/brocade-preview/node-image")),
            workspace,
            agent_bin_path,
            agent_bin_url: optional_env("BROCADE_PREVIEW_AGENT_BIN_URL")
                .or_else(|| optional_env("BROCADE_AGENT_BIN_URL")),
            agent_bin_sha256: optional_env("BROCADE_PREVIEW_AGENT_BIN_SHA256")
                .or_else(|| optional_env("BROCADE_AGENT_BIN_SHA256")),
            xray_bin_url: optional_env("BROCADE_PREVIEW_XRAY_BIN_URL")
                .or_else(|| optional_env("BROCADE_XRAY_BIN_URL")),
            xray_bin_sha256: optional_env("BROCADE_PREVIEW_XRAY_BIN_SHA256")
                .or_else(|| optional_env("BROCADE_XRAY_BIN_SHA256")),
        })
    }

    /// A default configuration independent of the environment, for unit tests only.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            bind: "127.0.0.1:0".parse().unwrap(),
            console_url: "http://127.0.0.1:8080".to_owned(),
            agent_url: "http://host.docker.internal:8081".to_owned(),
            preview_public_url: "http://host.docker.internal:8090".to_owned(),
            docker: "docker".to_owned(),
            network: "brocade-preview-test".to_owned(),
            subnet: "172.31.90.0/24".to_owned(),
            ip_prefix: "172.31.90".to_owned(),
            subnet_ipv6: "fd42:31:90::/64".to_owned(),
            ip6_prefix: "fd42:31:90::".to_owned(),
            node_image: "brocade-lab-node:latest".to_owned(),
            node_image_context: PathBuf::from("."),
            workspace: PathBuf::from("."),
            agent_bin_path: PathBuf::from("target/debug/brocade-agent"),
            agent_bin_url: None,
            agent_bin_sha256: None,
            xray_bin_url: None,
            xray_bin_sha256: None,
        }
    }
}

fn trim_url(value: String) -> String {
    value.trim().trim_end_matches('/').to_owned()
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
