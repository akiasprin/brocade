//! The work list for end-to-end probing: which chains a machine, as their head,
//! must dial itself once on behalf of.
//!
//! This layer and `user.rs` are two directions of one thing. `user.rs` projects how
//! a person connects in; this projects how the chain head itself connects in — both
//! read the same fields (ingress address, port, REALITY's five parameters), because
//! a probe is only meaningful when it travels byte for byte the same path as a real
//! user. One differing fingerprint is enough to measure the health of a different
//! path.
//!
//! Why the head must originate it: this chain's ingress exists only on that machine,
//! and nowhere else can dial the port a user sees. The control plane least of all —
//! it has no path to the data plane whatsoever.
//!
//! ## Expected exits are a set, not a single value
//!
//! A chain can fork: the rule table sends different traffic to different downstreams,
//! yielding several egress nodes. A probe sends one request and leaves through only
//! one of them, so the test is membership in that set rather than equality with one
//! of them. Given a single value, a forking chain would be reliably misreported as
//! exiting from the wrong place.

use std::collections::BTreeSet;

use crate::{
    ir::routing::{AppIr, AppNode},
    model::{probe_uuid, Action, Hysteria2},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbePlan {
    /// Which chains this machine probes for. Empty means it heads none of them.
    pub targets: Vec<ProbeChainTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeChainTarget {
    pub app_id: Option<String>,
    pub chain_id: String,
    pub chain_name: String,
    pub ingress_id: String,
    /// Which address to dial. The local ingress, so loopback or whatever address it
    /// explicitly bound — not a public IP: going over the public internet also
    /// measures our own inbound routing, which is not a property of this chain.
    pub dial_host: String,
    pub port: u16,
    pub uuid: String,
    pub security: ProbeSecurity,
    /// The HTTP layer, if this ingress has one. A probe dialing plain TCP at an XHTTP ingress is
    /// refused at the path check and reports the chain down while it is carrying traffic.
    pub xhttp: Option<crate::model::Xhttp>,
    /// Which machines this chain may exit from. A forking chain has several; see the
    /// module header.
    pub exit_nodes: Vec<String>,
    /// Those machines' public addresses, both families included. The IP the endpoint
    /// saw must fall in this set to agree. Empty means the check cannot be made — an
    /// exit is behind NAT on either family, or has no public address — an outcome that
    /// must say so explicitly rather than count as a pass.
    pub expected_exit_ips: Vec<String>,
}

/// What the probe has to speak to be let in.
///
/// A probe is only meaningful when it travels byte for byte the path a real user's client does,
/// so this mirrors what the subscription hands out rather than describing it separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeSecurity {
    Reality(ProbeRealityParams),
    Tls(ProbeTlsParams),
    Hysteria2(ProbeHysteria2Params),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeHysteria2Params {
    pub server_name: String,
    pub settings: Hysteria2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTlsParams {
    /// The name on the machine's own certificate. Unlike REALITY's borrowed name this one has to
    /// be right, because the client verifies it against a chain a public CA signed.
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRealityParams {
    pub public_key: String,
    pub short_id: String,
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

/// A machine with no certificate still gets a probe target here, naming a certificate it does
/// not have. Refusing to emit one would report the chain as *absent* rather than as failing,
/// which is the wrong answer to "is this ingress carrying traffic" — and the compiler already
/// refuses that model outright (`ingress.tls-no-certificate`), so it cannot reach a machine.
/// # Which half a two-wire ingress is probed on
///
/// The TCP one, when it exists. One target per ingress is what the whole chain of reporting
/// downstream is keyed on (`ingress_id`), and splitting that key is a change to the deployment
/// protocol, the store and the console — not something to smuggle in here.
///
/// The cost is stated rather than hidden: on an ingress serving both, a QUIC half that stops
/// working is not what this probe measures, and the chain still reports green. Until the probe
/// grows a second target, UDP reachability has to be watched some other way.
fn probe_security(ingress: &crate::ir::routing::Ingress) -> ProbeSecurity {
    if !ingress.wires.has_tcp() {
        if let Some(settings) = ingress.wires.hysteria2() {
            return ProbeSecurity::Hysteria2(ProbeHysteria2Params {
                server_name: ingress.certificate_name.clone().unwrap_or_default(),
                settings: settings.clone(),
            });
        }
    }
    match ingress.wires.reality() {
        Some(reality) => ProbeSecurity::Reality(ProbeRealityParams {
            public_key: ingress.identity.public_key.clone(),
            short_id: ingress
                .identity
                .short_ids
                .first()
                .cloned()
                .unwrap_or_default(),
            server_name: reality.server_name(ingress.certificate_name.as_deref()),
            fingerprint: reality.fingerprint.clone(),
            flow: reality.flow.clone(),
        }),
        None => ProbeSecurity::Tls(ProbeTlsParams {
            server_name: ingress.certificate_name.clone().unwrap_or_default(),
            fingerprint: ingress.wires.fingerprint().to_owned(),
            flow: ingress.wires.flow().map(str::to_owned),
        }),
    }
}

pub fn project_probe(apps: &[AppIr], node_id: &str) -> ProbePlan {
    let mut targets = Vec::new();

    for app in sorted_apps(apps) {
        for ingress in app
            .ingresses
            .iter()
            .filter(|ingress| ingress.node == node_id)
        {
            let chain = app.chains.iter().find(|chain| chain.id == ingress.chain);
            let exits = exit_nodes(app, &ingress.chain);

            targets.push(ProbeChainTarget {
                app_id: app.app_id.clone(),
                chain_id: ingress.chain.clone(),
                chain_name: chain
                    .map(|chain| chain.name.clone())
                    .unwrap_or_else(|| ingress.chain.clone()),
                ingress_id: ingress.id.clone(),
                dial_host: dial_host(&ingress.bind),
                port: ingress.port,
                uuid: probe_uuid(&ingress.identity.private_key, &ingress.id),
                security: probe_security(ingress),
                xhttp: ingress.wires.xhttp().cloned(),
                expected_exit_ips: exits
                    .iter()
                    .filter_map(|id| app.nodes.iter().find(|node| node.id == *id))
                    .flat_map(public_addresses)
                    .collect(),
                exit_nodes: exits.into_iter().collect(),
            });
        }
    }

    targets.sort_by(|a, b| {
        a.app_id
            .cmp(&b.app_id)
            .then_with(|| a.chain_id.cmp(&b.chain_id))
            .then_with(|| a.ingress_id.cmp(&b.ingress_id))
    });
    ProbePlan { targets }
}

/// Which address to dial the local ingress on.
///
/// Bound to `0.0.0.0` / `::`, use loopback — that binding accepts any address,
/// loopback is the shortest of them, and it does not depend on this machine's public
/// reachability. Bound to a specific address, only that one will do: substituting
/// loopback would fail to connect, and that failure would be the probe's own, not the
/// chain's.
fn dial_host(bind: &std::net::IpAddr) -> String {
    match bind {
        std::net::IpAddr::V4(v4) if v4.is_unspecified() => "127.0.0.1".to_owned(),
        std::net::IpAddr::V6(v6) if v6.is_unspecified() => "::1".to_owned(),
        other => other.to_string(),
    }
}

/// Which machines on this chain egress.
///
/// The test is an actual `Egress` in the rule table, not `node.egress_allowed` — the
/// latter says the machine is permitted to exit, which differs from this chain
/// exiting there: a relay cleared for egress may still only forward.
fn exit_nodes(app: &AppIr, chain_id: &str) -> BTreeSet<String> {
    app.steps
        .iter()
        .filter(|step| step.chain == chain_id)
        .filter(|step| {
            step.rules
                .iter()
                .any(|rule| matches!(rule.action, Action::Egress { .. } | Action::Proxy { .. }))
        })
        .map(|step| step.node.clone())
        .collect()
}

/// Addresses behind NAT do not count: that is not what the endpoint sees. Whether the exit goes
/// v4 or v6 is decided by the endpoint and by routing, and the probe does not guess — which is
/// why NAT on *either* family disqualifies the machine entirely, its other family included.
///
/// Dropping only the NATed family used to look like the more precise choice, and it produced a
/// false accusation: with v4 behind NAT and v6 native, the expectation held the v6 address alone,
/// traffic leaving over v4 was seen by the endpoint as the NATed address, and matching it against
/// a v6-only expectation yielded `Mismatch` — the verdict meaning "the traffic never reached this
/// chain's exit", raised against a chain that is working. A check that cannot run has to say so,
/// and an empty expectation is how `E2eExitVerdict::Unknown` is reached.
fn public_addresses(node: &AppNode) -> Vec<String> {
    // Tied to an address being present: the two flags default to false and carry no meaning for a
    // family the machine does not have, so `public_ipv6_nat` left true beside an absent v6 address
    // must not silently disable the check for a perfectly ordinary v4-only machine.
    let behind_nat = (node.public_ipv4.is_some() && node.public_ipv4_nat)
        || (node.public_ipv6.is_some() && node.public_ipv6_nat);
    if behind_nat {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(address) = node.public_ipv4.as_ref() {
        out.push(address.clone());
    }
    if let Some(address) = node.public_ipv6.as_ref() {
        out.push(address.clone());
    }
    out
}

fn sorted_apps(apps: &[AppIr]) -> Vec<&AppIr> {
    let mut apps = apps.iter().collect::<Vec<_>>();
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dns, DomainStrategy, NodeConnection};

    fn node(ipv4: Option<&str>, ipv4_nat: bool, ipv6: Option<&str>, ipv6_nat: bool) -> AppNode {
        AppNode {
            id: "sg-01".to_owned(),
            tenant: "platform.acme".to_owned(),
            name: "sg-01".to_owned(),
            public_ipv4: ipv4.map(str::to_owned),
            public_ipv6: ipv6.map(str::to_owned),
            public_ipv4_nat: ipv4_nat,
            public_ipv6_nat: ipv6_nat,
            egress_allowed: true,
            api_port: None,
            dns: Dns::System,
            domain_strategy: DomainStrategy::default(),
            connection: NodeConnection::default(),
        }
    }

    #[test]
    fn a_machine_with_no_nat_offers_every_address_it_has() {
        assert_eq!(
            public_addresses(&node(
                Some("203.0.113.9"),
                false,
                Some("2001:db8::9"),
                false
            )),
            vec!["203.0.113.9".to_owned(), "2001:db8::9".to_owned()]
        );
    }

    /// The whole point of the coarse rule. Keeping the native family and dropping only the NATed
    /// one looks more precise and produces a false accusation: the expectation would hold the v6
    /// address alone, traffic leaving over v4 is seen by the endpoint as the NATed v4 address, and
    /// comparing the two yields `Mismatch` — the verdict that says the traffic never reached this
    /// chain's exit, raised against a chain that works.
    #[test]
    fn nat_on_either_family_withdraws_the_other_family_too() {
        assert!(
            public_addresses(&node(Some("100.64.0.9"), true, Some("2001:db8::9"), false))
                .is_empty(),
            "v4 在 NAT 后，v6 也不能拿去核对"
        );
        assert!(
            public_addresses(&node(Some("203.0.113.9"), false, Some("2001:db8::9"), true))
                .is_empty(),
            "v6 在 NAT 后，v4 也不能拿去核对"
        );
    }

    /// The two flags default to false and mean nothing for a family the machine does not have, but
    /// nothing in the schema stops one being left true beside an absent address. Read without that
    /// guard, an ordinary v4-only machine would silently stop being checkable.
    #[test]
    fn a_nat_flag_beside_an_absent_address_disqualifies_nothing() {
        assert_eq!(
            public_addresses(&node(Some("203.0.113.9"), false, None, true)),
            vec!["203.0.113.9".to_owned()]
        );
        assert_eq!(
            public_addresses(&node(None, true, Some("2001:db8::9"), false)),
            vec!["2001:db8::9".to_owned()]
        );
    }
}
