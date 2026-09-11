//! The work list for end-to-end probing: which chains a machine, as their head,
//! must dial itself once on behalf of.
//!
//! This layer and `user.rs` are two directions of one thing. `user.rs` projects how
//! a person connects in; this projects how the chain head itself connects in — both
//! read the same fields (ingress address, port, REALITY's five parameters), because
//! a probe is only meaningful when it travels byte for byte the same path as a real
//! user. One differing transport parameter is enough to measure the health of a different path.
//!
//! Why the head must originate it: this chain's ingress exists only on that machine,
//! and nowhere else can dial the port a user sees. The control plane least of all —
//! it has no path to the data plane whatsoever.
//!
use crate::{
    ir::routing::AppIr,
    model::{probe_uuid, AnyTls, Hysteria2},
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
}

/// What the probe has to speak to be let in.
///
/// A probe is only meaningful when it travels byte for byte the path a real user's client does,
/// so this mirrors what the subscription hands out rather than describing it separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeSecurity {
    VlessEncryption {
        port: u16,
        public_key: String,
        options: crate::model::VlessEncryptionOptions,
    },
    Reality(ProbeRealityParams),
    Tls(ProbeTlsParams),
    AnyTls(ProbeAnyTlsParams),
    Hysteria2(ProbeHysteria2Params),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAnyTlsParams {
    pub server_name: String,
    pub settings: AnyTls,
    pub reality: Option<ProbeRealityParams>,
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
    // The probe work list has historically been one target per ingress. For an AnyTLS-only
    // ingress there is no VLESS half to prefer, so select AnyTLS and its independent port. A
    // mixed VLESS+AnyTLS ingress keeps the established VLESS target until the result model grows
    // a protocol dimension of its own.
    if ingress.wires.vless().is_none() {
        if let Some(settings) = ingress.wires.vless_encryption() {
            return ProbeSecurity::VlessEncryption {
                port: settings.port,
                public_key: settings.public_key.clone(),
                options: settings.options.clone(),
            };
        }
        if let Some(settings) = ingress.wires.anytls() {
            let reality = settings.reality().map(|reality| ProbeRealityParams {
                public_key: ingress
                    .anytls_identity
                    .as_ref()
                    .map(|identity| identity.public_key.clone())
                    .unwrap_or_default(),
                short_id: ingress
                    .anytls_identity
                    .as_ref()
                    .and_then(|identity| identity.short_ids.first())
                    .cloned()
                    .unwrap_or_default(),
                server_name: reality.server_name(ingress.certificate_name.as_deref()),
                fingerprint: reality.fingerprint.clone(),
                flow: None,
            });
            return ProbeSecurity::AnyTls(ProbeAnyTlsParams {
                server_name: reality
                    .as_ref()
                    .map(|reality| reality.server_name.clone())
                    .unwrap_or_else(|| ingress.certificate_name.clone().unwrap_or_default()),
                settings: settings.clone(),
                reality,
            });
        }
    }
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
            let security = probe_security(ingress);
            let port = match &security {
                ProbeSecurity::AnyTls(anytls) => anytls.settings.port,
                ProbeSecurity::VlessEncryption { port, .. } => *port,
                _ => ingress.port,
            };

            targets.push(ProbeChainTarget {
                app_id: app.app_id.clone(),
                chain_id: ingress.chain.clone(),
                chain_name: chain
                    .map(|chain| chain.name.clone())
                    .unwrap_or_else(|| ingress.chain.clone()),
                ingress_id: ingress.id.clone(),
                dial_host: dial_host(&ingress.bind),
                port,
                uuid: probe_uuid(&ingress.identity.private_key, &ingress.id),
                security,
                xhttp: ingress.wires.xhttp().cloned(),
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

fn sorted_apps(apps: &[AppIr]) -> Vec<&AppIr> {
    let mut apps = apps.iter().collect::<Vec<_>>();
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    apps
}
