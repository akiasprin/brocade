use crate::artifacts::wireguard::{WireGuardArtifact, WireGuardConfig};

pub fn wireguard(artifact: &WireGuardArtifact) -> String {
    match artifact {
        WireGuardArtifact::Disabled { node_id } => {
            format!("# {node_id} 不在 overlay 里，没有 WireGuard 配置。\n")
        }
        WireGuardArtifact::Config(config) => wireguard_config(config),
    }
}

fn wireguard_config(config: &WireGuardConfig) -> String {
    let mut lines = Vec::new();

    lines.push("[Interface]".to_owned());
    lines.push(format!("PrivateKey = {}", config.interface.private_key));
    lines.push(format!("Address    = {}/32", config.interface.address));
    if let Some(listen_port) = config.interface.listen_port {
        lines.push(format!("ListenPort = {listen_port}"));
    }
    if let Some(mtu) = config.interface.mtu {
        lines.push(format!("MTU        = {mtu}"));
    }

    for peer in &config.peers {
        lines.push(String::new());
        lines.push("[Peer]".to_owned());
        lines.push(format!("# {}", peer.node_id));
        lines.push(format!("PublicKey  = {}", peer.public_key));
        lines.push(format!("AllowedIPs = {}/32", peer.allowed_ip));
        if let Some(endpoint) = &peer.endpoint {
            lines.push(format!("Endpoint   = {endpoint}"));
        }
        if let Some(keepalive) = peer.persistent_keepalive {
            lines.push(format!("PersistentKeepalive = {keepalive}"));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}
