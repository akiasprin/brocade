use std::net::Ipv4Addr;

use crate::physical::node::{NodePlan, PhantunClientPlan, PhantunServerPlan, PhantunTun};

/// The fourth node artifact: the phantun instances to run on this machine.
///
/// It exists because WireGuard is UDP only. When an upstream seals inbound UDP, the
/// only way out is to wrap the UDP in something else at both ends.
///
/// It must be an artifact rather than something configured by hand. Both ends are
/// derived from the same `SystemIr`, so "a server configured on one side and the
/// client forgotten on the other", or mismatched ports, cannot occur — which is the
/// whole point of replacing hand configuration with the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhantunArtifact {
    Disabled { node_id: String },
    Config(PhantunConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunConfig {
    pub node_id: String,
    /// The servers to stand up here. One public machine may serve as the endpoint
    /// for several peers behind NAT, hence the plural — see
    /// `ir::system::LinkWrap`.
    pub servers: Vec<PhantunServer>,
    pub clients: Vec<PhantunClient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunServer {
    pub tcp_port: u16,
    pub forward_to_udp_port: u16,
    /// The peers arriving through this port. The agent uses it to establish that
    /// these peers' Endpoints can only be loopback.
    pub peers: Vec<String>,
    pub tun: Tun,
}

/// The TUN device an instance owns, and the addresses at both its ends. The agent
/// starts processes from it and writes NAT rules from it — phantun sends and
/// receives through the TUN, the client needs MASQUERADE and the server needs DNAT,
/// and without them packets get neither out nor in (`phantun --help` says as
/// much).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tun {
    pub name: String,
    pub local: Ipv4Addr,
    pub peer: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhantunClient {
    pub peer_node_id: String,
    pub listen_udp_port: u16,
    pub remote_tcp_endpoint: String,
    pub tun: Tun,
}

pub fn build(plan: &NodePlan) -> PhantunArtifact {
    let Some(phantun) = &plan.phantun else {
        return PhantunArtifact::Disabled {
            node_id: plan.node_id.clone(),
        };
    };
    PhantunArtifact::Config(PhantunConfig {
        node_id: phantun.node_id.clone(),
        servers: phantun.servers.iter().map(server).collect(),
        clients: phantun.clients.iter().map(client).collect(),
    })
}

fn server(plan: &PhantunServerPlan) -> PhantunServer {
    PhantunServer {
        tcp_port: plan.tcp_port,
        forward_to_udp_port: plan.forward_to_udp_port,
        peers: plan.peers.clone(),
        tun: tun(&plan.tun),
    }
}

fn tun(plan: &PhantunTun) -> Tun {
    Tun {
        name: plan.name.clone(),
        local: plan.local,
        peer: plan.peer,
    }
}

fn client(plan: &PhantunClientPlan) -> PhantunClient {
    PhantunClient {
        peer_node_id: plan.peer_node_id.clone(),
        listen_udp_port: plan.listen_udp_port,
        remote_tcp_endpoint: plan.remote_tcp_endpoint.clone(),
        tun: tun(&plan.tun),
    }
}
