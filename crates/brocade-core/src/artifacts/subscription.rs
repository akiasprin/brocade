use crate::{
    model::{FrontStrategy, Hysteria2},
    physical::user::{UserPlan, UserRealityPlan, UserSecurityPlan},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub tenant: String,
    pub user: String,
    pub entries: Vec<SubscriptionEntry>,
    pub front_groups: Vec<SubscriptionFrontGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionEntry {
    pub name: String,
    pub server: String,
    pub port: u16,
    pub uuid: String,
    pub security: SubscriptionSecurity,
    /// What the stream is carried inside. A client that does not know this cannot connect at all:
    /// the server matches the request path and refuses a mismatch outright
    /// (`failed to validate path`), so an XHTTP ingress whose subscription still says `tcp`
    /// produces a configuration that looks complete and never works.
    pub stream: SubscriptionStream,
    pub front_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SubscriptionStream {
    #[default]
    Tcp,
    Xhttp {
        path: String,
        host: Option<String>,
        download: Option<SubscriptionDownload>,
        /// Carried into the client's configuration because multiplexing has to be agreed on both
        /// ends; a server that multiplexes and a client that does not simply opens one connection
        /// per stream, which is the cost this exists to avoid.
        mux: Option<u16>,
        /// Carried for a blunter reason than `mux`: a server given an explicit mode refuses every
        /// client that disagrees, so a subscription that omits it hands out a configuration the
        /// server will turn away. `None` is the operator having chosen nothing, and then the two
        /// sides agree by both leaving it alone.
        mode: Option<&'static str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionDownload {
    pub server: String,
    pub port: u16,
    pub server_name: String,
    pub fingerprint: String,
    pub http_host: Option<String>,
    pub mux: Option<u16>,
}

/// The security layer a client must speak, in the two forms a subscription can name.
///
/// A client told the wrong one does not degrade — it fails outright, and in the TLS case it fails
/// on the client's own certificate check before a byte reaches us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionSecurity {
    Reality(SubscriptionReality),
    Tls(SubscriptionTls),
    Hysteria2(SubscriptionHysteria2),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionHysteria2 {
    pub server_name: String,
    pub settings: Hysteria2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionTls {
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionReality {
    pub public_key: String,
    pub short_id: String,
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionFrontGroup {
    pub name: String,
    pub strategy: FrontStrategy,
    pub members: Vec<String>,
}

pub fn build(plan: &UserPlan) -> Subscription {
    Subscription {
        tenant: plan.tenant.clone(),
        user: plan.user.clone(),
        entries: plan
            .entries
            .iter()
            .map(|entry| SubscriptionEntry {
                name: entry.name.clone(),
                server: entry.server.clone(),
                port: entry.port,
                uuid: entry.uuid.clone(),
                security: security(&entry.security),
                stream: match &entry.xhttp {
                    None => SubscriptionStream::Tcp,
                    Some(xhttp) => SubscriptionStream::Xhttp {
                        path: xhttp.path.clone(),
                        host: xhttp.host.clone(),
                        download: entry
                            .download
                            .as_ref()
                            .map(|download| SubscriptionDownload {
                                server: download.server.clone(),
                                port: download.port,
                                server_name: download.server_name.clone(),
                                fingerprint: download.fingerprint.clone(),
                                http_host: download.http_host.clone(),
                                mux: download.mux,
                            }),
                        mux: xhttp.mux,
                        mode: xhttp.mode.as_str(),
                    },
                },
                front_name: entry.front_name.clone(),
            })
            .collect(),
        front_groups: plan
            .front_groups
            .iter()
            .map(|front| SubscriptionFrontGroup {
                name: front.name.clone(),
                strategy: front.strategy,
                members: front.members.clone(),
            })
            .collect(),
    }
}

fn security(security: &UserSecurityPlan) -> SubscriptionSecurity {
    match security {
        UserSecurityPlan::Reality(reality) => {
            SubscriptionSecurity::Reality(reality_params(reality))
        }
        UserSecurityPlan::Tls(tls) => SubscriptionSecurity::Tls(SubscriptionTls {
            server_name: tls.server_name.clone(),
            fingerprint: tls.fingerprint.clone(),
            flow: tls.flow.clone(),
        }),
        UserSecurityPlan::Hysteria2(hysteria) => {
            SubscriptionSecurity::Hysteria2(SubscriptionHysteria2 {
                server_name: hysteria.server_name.clone(),
                settings: hysteria.settings.clone(),
            })
        }
    }
}

fn reality_params(reality: &UserRealityPlan) -> SubscriptionReality {
    SubscriptionReality {
        public_key: reality.public_key.clone(),
        short_id: reality.short_id.clone(),
        server_name: reality.server_name.clone(),
        fingerprint: reality.fingerprint.clone(),
        flow: reality.flow.clone(),
    }
}
