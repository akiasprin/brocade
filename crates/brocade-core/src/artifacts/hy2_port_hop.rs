use crate::physical::node::{Hy2PortHopPlan, NodePlan};

/// Hysteria 2's port hopping, server side: which UDP ranges this machine folds onto the single
/// port its listener binds. The fifth node artifact.
///
/// Not a chain's relay hop, which this codebase also calls a hop: that one is a TCP inbound
/// carrying traffic to the next machine in a chain, allocated upward from
/// `PortSettings::hop_base`, and has nothing to do with any of this. Hysteria 2's own hopping
/// then has two halves, and this is the second: `Hysteria2::hop` on the model is the range an
/// ingress declares, and here is what a machine must do about it. Per-machine and per-ingress,
/// UDP only, never per-chain.
///
/// Hopping is a client-side rotation, and the server side of it is not a server feature at all —
/// the process binds exactly one port, and every other port in the range reaches it because the
/// machine redirects them. So the range appears in two places and neither of them is the xray
/// config: in the subscriptions clients rotate through, and here.
///
/// Its own artifact rather than a corner of the phantun one, whose rules live in `inet brocade`
/// and are torn down and rebuilt wholesale on every phantun convergence. Sharing that table would
/// make each feature's convergence silently delete the other's rules, and the symptom — "hopping
/// worked until someone touched fake TCP" — points nowhere near the cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hy2PortHopArtifact {
    Disabled { node_id: String },
    Config(Hy2PortHopConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hy2PortHopConfig {
    pub node_id: String,
    /// Non-empty: a machine with no ranges takes `Disabled` instead, so that the agent is told to
    /// tear its table down rather than handed an empty list to interpret.
    pub redirects: Vec<Hy2PortHopRedirect>,
}

/// One range and the port it folds onto. Clients rotate over `start..=end`; the server binds
/// `to` and nothing else, and `to` lies inside the range — a range excluding its own listener
/// would hand clients a set of ports where the one that actually works is missing, which the
/// compiler refuses (`ingress.hy2-hop-listener`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hy2PortHopRedirect {
    pub start: u16,
    pub end: u16,
    pub to: u16,
}

pub fn build(plan: &NodePlan) -> Hy2PortHopArtifact {
    if plan.hy2_port_hops.is_empty() {
        return Hy2PortHopArtifact::Disabled {
            node_id: plan.node_id.clone(),
        };
    }
    Hy2PortHopArtifact::Config(Hy2PortHopConfig {
        node_id: plan.node_id.clone(),
        redirects: plan
            .hy2_port_hops
            .iter()
            .map(|hop: &Hy2PortHopPlan| Hy2PortHopRedirect {
                start: hop.start,
                end: hop.end,
                to: hop.to,
            })
            .collect(),
    })
}
