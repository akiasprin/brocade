use std::net::Ipv4Addr;

use crate::physical::node::NodePlan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardArtifact {
    Disabled { node_id: String },
    Config(WireGuardConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardConfig {
    pub interface: WireGuardInterface,
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardInterface {
    pub private_key: String,
    pub address: Ipv4Addr,
    pub listen_port: Option<u16>,
    pub mtu: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardPeer {
    pub node_id: String,
    pub public_key: String,
    pub allowed_ip: Ipv4Addr,
    pub endpoint: Option<String>,
    pub persistent_keepalive: Option<u16>,
}

pub fn build(plan: &NodePlan) -> WireGuardArtifact {
    let Some(wireguard) = &plan.wireguard else {
        return WireGuardArtifact::Disabled {
            node_id: plan.node_id.clone(),
        };
    };

    WireGuardArtifact::Config(WireGuardConfig {
        interface: WireGuardInterface {
            private_key: wireguard.private_key.clone(),
            address: wireguard.address,
            listen_port: wireguard.listen_port,
            mtu: wireguard.mtu,
        },
        peers: wireguard
            .peers
            .iter()
            .map(|peer| WireGuardPeer {
                node_id: peer.node_id.clone(),
                public_key: peer.public_key.clone(),
                allowed_ip: peer.allowed_ip,
                endpoint: peer.endpoint.clone(),
                persistent_keepalive: peer.persistent_keepalive,
            })
            .collect(),
    })
}
