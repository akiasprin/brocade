use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ir::routing::{AppIr, AppNode, Ingress},
    model::{
        AnyTls, ExternalOutboundProtocol, ExternalOutboundSecurity, FrontStrategy, Hysteria2,
        IpFamily, Network, ProjectionDownloadEndpoint, ProjectionEndpoint, Xhttp,
    },
};

/// Which client protocol a subscription view retains. This is a projection filter rather than
/// model state: one ingress can publish both wires, while a particular URL can expose either one
/// or the complete pair without changing what is deployed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionProtocol {
    Vless,
    AnyTls,
    Hysteria2,
    MtProto,
}

/// Optional dimensions applied to a generated subscription. Keeping these together makes every
/// output path apply the same intersection when both a network family and protocol are selected.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionFilter {
    pub family: Option<IpFamily>,
    pub protocol: Option<SubscriptionProtocol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPlan {
    pub tenant: String,
    pub user: String,
    pub uuid: String,
    pub entries: Vec<UserSubscriptionEntryPlan>,
    pub external_proxies: Vec<UserExternalProxyPlan>,
    pub front_groups: Vec<UserFrontGroupPlan>,
}

impl UserPlan {
    pub fn retain_filter(&mut self, filter: SubscriptionFilter) {
        if let Some(family) = filter.family {
            self.retain_family(family);
        }
        if let Some(protocol) = filter.protocol {
            self.retain_protocol(protocol);
        }
    }

    /// Drop the entries a client restricted to `family` cannot dial.
    ///
    /// The whole subscription is compiled first and narrowed here, rather than compiled per
    /// family: what a person receives has to stay a subset of what the fleet actually serves,
    /// and a second projection path would be a second place for that to drift.
    ///
    /// Entries with no family (`server` is the `?` placeholder, meaning neither family
    /// contributed an address) survive every filter. They mark a model the compiler refuses to
    /// publish; removing them here would answer with an empty subscription instead, which reads
    /// as "this person has no grants".
    ///
    /// Front groups are narrowed alongside: their members are entry names, and mihomo refuses a
    /// group naming a proxy the list does not contain. A group left with no members is dropped,
    /// and so are the entries that name it — an entry whose front group is empty has nothing to
    /// dial through, so keeping it produces a subscription that imports and fails on every
    /// connection. Dropping those entries can empty a further group where fronts are nested,
    /// hence the loop; it ends because each round removes at least one group.
    pub fn retain_family(&mut self, family: IpFamily) {
        self.entries
            .retain(|entry| entry.family.is_none_or(|entry| entry == family));
        self.prune_empty_fronts();
    }

    /// Keep one wire protocol while preserving every front-group invariant maintained by the
    /// address-family filter. Reality and TLS are both VLESS security variants; Hysteria 2 has a
    /// distinct account and URI/YAML shape.
    pub fn retain_protocol(&mut self, protocol: SubscriptionProtocol) {
        self.entries.retain(|entry| {
            matches!(
                (protocol, &entry.security),
                (
                    SubscriptionProtocol::Vless,
                    UserSecurityPlan::Reality(_)
                        | UserSecurityPlan::Tls(_)
                        | UserSecurityPlan::VlessEncryption { .. }
                ) | (SubscriptionProtocol::AnyTls, UserSecurityPlan::AnyTls(_))
                    | (
                        SubscriptionProtocol::Hysteria2,
                        UserSecurityPlan::Hysteria2(_)
                    )
                    | (
                        SubscriptionProtocol::MtProto,
                        UserSecurityPlan::MtProto { .. }
                    )
            )
        });
        self.prune_empty_fronts();
    }

    fn prune_empty_fronts(&mut self) {
        loop {
            let kept = self
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .chain(
                    self.external_proxies
                        .iter()
                        .map(|proxy| proxy.name.as_str()),
                )
                .collect::<BTreeSet<_>>();
            for group in &mut self.front_groups {
                group
                    .members
                    .retain(|member| kept.contains(member.as_str()));
            }
            let emptied = self
                .front_groups
                .iter()
                .filter(|group| group.members.is_empty())
                .map(|group| group.id.clone())
                .collect::<BTreeSet<_>>();
            if emptied.is_empty() {
                return;
            }
            self.front_groups
                .retain(|group| !emptied.contains(&group.id));
            self.entries.retain(|entry| {
                entry
                    .front_id
                    .as_ref()
                    .is_none_or(|front| !emptied.contains(front))
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSubscriptionEntryPlan {
    pub grant_id: String,
    pub ingress_id: String,
    pub name: String,
    pub server: String,
    pub port: u16,
    /// Which family's address `server` is, taken from the projection slot it came from rather
    /// than parsed back out of the string: a projected entry carries a hostname, and a hostname
    /// does not say which family it resolves to.
    ///
    /// `None` only for the `?` placeholder — see [`UserPlan::retain_family`].
    pub family: Option<IpFamily>,
    pub download: Option<UserDownloadPlan>,
    pub uuid: String,
    pub security: UserSecurityPlan,
    /// Copied from the ingress. A subscription that omits it produces a client configuration that
    /// imports cleanly and cannot connect — the server refuses a path it did not expect.
    pub xhttp: Option<Xhttp>,
    pub front_id: Option<String>,
    pub front_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDownloadPlan {
    pub server: String,
    pub port: u16,
    pub server_name: String,
    pub http_host: Option<String>,
    pub mux: Option<u16>,
}

/// What the client has to present, which is the same question the probe answers in `probe.rs`
/// and has to answer identically: a probe measures the path a subscriber takes only while both
/// are handed the same parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserSecurityPlan {
    VlessEncryption {
        port: u16,
        public_key: String,
        options: crate::model::VlessEncryptionOptions,
    },
    Reality(UserRealityPlan),
    Tls(UserTlsPlan),
    AnyTls(UserAnyTlsPlan),
    Hysteria2(UserHysteria2Plan),
    MtProto {
        port: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAnyTlsPlan {
    pub server_name: String,
    pub settings: AnyTls,
    pub reality: Option<UserRealityPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserHysteria2Plan {
    pub server_name: String,
    pub settings: Hysteria2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTlsPlan {
    /// The name on the machine's certificate, which the client both sends as SNI and verifies the
    /// certificate against. Wrong, and the client refuses the connection itself — unlike
    /// REALITY's borrowed name, which is only ever a label.
    pub server_name: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRealityPlan {
    pub public_key: String,
    pub short_id: String,
    pub server_name: String,
    pub fingerprint: String,
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFrontGroupPlan {
    pub id: String,
    pub name: String,
    pub strategy: FrontStrategy,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserExternalProxyPlan {
    /// Stable tenant resource id. Tunnel ids are globally unique so one proxy can be shared by
    /// fronts in several projects without duplicating it in a subscription.
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol: ExternalOutboundProtocol,
    pub security: ExternalOutboundSecurity,
}

/// The security halves this ingress hands a subscriber, in the order they appear.
///
/// One entry per wire: a client cannot speak both at once, so an ingress serving TCP and QUIC
/// gives a person two lines to choose between rather than one line describing two things.
///
/// VLESS is the default wire and keeps the bare chain name. The Hysteria2 wire is identified by
/// its QUIC transport, which makes mixed-wire entries distinct without repeating the full protocol
/// name in every subscription.
fn securities(ingress: &Ingress) -> Vec<(UserSecurityPlan, &'static str)> {
    let mut wires = Vec::new();
    if let Some(settings) = ingress.wires.vless_encryption() {
        wires.push((
            UserSecurityPlan::VlessEncryption {
                port: settings.port,
                public_key: settings.public_key.clone(),
                options: settings.options.clone(),
            },
            " | VLESS Encryption",
        ));
    }

    if ingress.wires.vless().is_some() {
        let vless = match ingress.wires.reality() {
            Some(reality) => UserSecurityPlan::Reality(UserRealityPlan {
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
            None => UserSecurityPlan::Tls(UserTlsPlan {
                // Empty only where the machine holds no certificate, which the compiler refuses
                // outright (`ingress.tls-no-certificate`) rather than letting reach a subscription.
                server_name: ingress.certificate_name.clone().unwrap_or_default(),
                flow: ingress.wires.flow().map(str::to_owned),
            }),
        };
        wires.push((vless, ""));
    }

    if let Some(settings) = ingress.wires.hysteria2() {
        wires.push((
            UserSecurityPlan::Hysteria2(UserHysteria2Plan {
                server_name: ingress.certificate_name.clone().unwrap_or_default(),
                settings: settings.clone(),
            }),
            " | QUIC",
        ));
    }

    if let Some(settings) = ingress.wires.anytls() {
        let reality = settings.reality().map(|reality| UserRealityPlan {
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
            // Vision is a VLESS flow and does not apply to AnyTLS.
            flow: None,
        });
        wires.push((
            UserSecurityPlan::AnyTls(UserAnyTlsPlan {
                server_name: reality
                    .as_ref()
                    .map(|reality| reality.server_name.clone())
                    .unwrap_or_else(|| ingress.certificate_name.clone().unwrap_or_default()),
                settings: settings.clone(),
                reality,
            }),
            " | AnyTLS",
        ));
    }

    if let Some(settings) = ingress.wires.mtproto() {
        wires.push((
            UserSecurityPlan::MtProto {
                port: settings.port,
            },
            " | MTProxy",
        ));
    }

    wires
}

pub fn project_user(apps: &[AppIr], tenant: &str, user: &str) -> UserPlan {
    let uuid = apps
        .iter()
        .flat_map(|app| app.users.iter())
        .find(|candidate| candidate.tenant == tenant && candidate.id == user)
        .map(|user| user.uuid.clone())
        .unwrap_or_else(|| "?".to_owned());

    let mut entries = Vec::new();
    for app in apps {
        let app_start = entries.len();
        let chain_rank = app
            .chains
            .iter()
            .enumerate()
            .map(|(position, chain)| (chain.id.as_str(), position))
            .collect::<BTreeMap<_, _>>();
        let ingress_rank = app
            .ingresses
            .iter()
            .map(|ingress| {
                (
                    ingress.id.as_str(),
                    chain_rank
                        .get(ingress.chain.as_str())
                        .copied()
                        .unwrap_or(usize::MAX),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for grant in app
            .grants
            .iter()
            .filter(|grant| grant.tenant == tenant && grant.user == user)
        {
            let Some(ingress) = app
                .ingresses
                .iter()
                .find(|ingress| ingress.id == grant.ingress)
            else {
                continue;
            };
            let Some(node) = app.nodes.iter().find(|node| node.id == ingress.node) else {
                continue;
            };
            let chain_name = app
                .chains
                .iter()
                .find(|chain| chain.id == ingress.chain)
                .map(|chain| {
                    subscription_chain_name(&chain.name, chain.subscription_country.as_deref())
                })
                .unwrap_or_else(|| ingress.id.clone());
            let front = ingress
                .front
                .as_ref()
                .and_then(|front_id| app.fronts.iter().find(|front| front.id == *front_id))
                // An ancestor ingress may be shared by several tenant branches. Its Front
                // attachment is client-only and belongs to one of those branches; users outside
                // that branch keep their ordinary direct projection and must not receive the
                // group's external tunnel credentials.
                .filter(|front| tenant_within(tenant, &front.tenant));
            let wires = securities(ingress);
            for (security, wire_suffix) in &wires {
                for server in subscription_servers(node, ingress, security) {
                    let independent_transport = !matches!(
                        security,
                        UserSecurityPlan::Reality(_) | UserSecurityPlan::Tls(_)
                    );
                    entries.push(UserSubscriptionEntryPlan {
                        grant_id: grant.id.clone(),
                        ingress_id: ingress.id.clone(),
                        // Protocol precedes address family so the optional v6 marker is always
                        // the final segment: `name | QUIC | v6`.
                        name: format!("{}{}{}", chain_name, wire_suffix, server.name_suffix),
                        server: server.address.clone(),
                        family: server.family,
                        port: server.port,
                        // The independent download belongs to the XHTTP half and to nothing else;
                        // QUIC carries its own streams and has no second connection to project.
                        download: server
                            .download
                            .clone()
                            .filter(|_| !independent_transport)
                            .map(|download| {
                                UserDownloadPlan {
                                    server: download.host,
                                    port: download.port,
                                    // A split REALITY ingress terminates the downlink with this
                                    // machine's certificate. Validation prevents the empty case from
                                    // being published.
                                    server_name: ingress
                                        .certificate_name
                                        .clone()
                                        .unwrap_or_default(),
                                    http_host: download.http_host,
                                    mux: download.mux,
                                }
                            }),
                        uuid: uuid.clone(),
                        security: security.clone(),
                        xhttp: if independent_transport {
                            None
                        } else {
                            ingress.wires.xhttp().cloned()
                        },
                        front_id: front.map(|front| front.id.clone()),
                        front_name: front.map(|front| front.name.clone()),
                    });
                }
            }
        }
        // Both levels are semantic: apps arrive in apps.position order and chains in
        // chains.position order. Within one grant, keep each protocol's v4/v6 pair together;
        // sorting by the rendered name would move the bare VLESS v6 entry behind every named
        // protocol suffix (`AnyTLS`, `QUIC`, ...).
        entries[app_start..].sort_by(|a, b| {
            ingress_rank
                .get(a.ingress_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
                .cmp(
                    &ingress_rank
                        .get(b.ingress_id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX),
                )
                .then_with(|| a.grant_id.cmp(&b.grant_id))
                .then_with(|| {
                    subscription_protocol_rank(&a.security)
                        .cmp(&subscription_protocol_rank(&b.security))
                })
                .then_with(|| {
                    subscription_family_rank(a.family).cmp(&subscription_family_rank(b.family))
                })
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.server.cmp(&b.server))
        });
    }

    let external_proxies = external_proxies(apps, &entries);
    let front_groups = front_groups(apps, &entries, &external_proxies);

    let mut plan = UserPlan {
        tenant: tenant.to_owned(),
        user: user.to_owned(),
        uuid,
        entries,
        external_proxies,
        front_groups,
    };
    // A client-only checkpoint can name members which are waiting for a machine topology release.
    // Keep that state fail-closed: until at least one member is both served and granted to this
    // user, omit the empty group and every target that would otherwise fall back to a direct dial.
    plan.prune_empty_fronts();
    plan
}

/// Keep the exact per-project inputs consumed by [`project_user`].  Empty project shells remain
/// in source order because chain ordering is part of the subscription contract; unrelated users,
/// grants and chains no longer make this user's projection cache miss.
pub fn scope_user_apps(apps: &[AppIr], tenant: &str, user: &str) -> Vec<AppIr> {
    apps.iter()
        .map(|app| {
            let grants = app
                .grants
                .iter()
                .filter(|grant| grant.tenant == tenant && grant.user == user)
                .cloned()
                .collect::<Vec<_>>();
            let ingress_ids = grants
                .iter()
                .map(|grant| grant.ingress.as_str())
                .collect::<BTreeSet<_>>();
            let ingresses = app
                .ingresses
                .iter()
                .filter(|ingress| ingress_ids.contains(ingress.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let chain_ids = ingresses
                .iter()
                .map(|ingress| ingress.chain.as_str())
                .collect::<BTreeSet<_>>();
            let front_ids = ingresses
                .iter()
                .filter_map(|ingress| ingress.front.as_deref())
                .collect::<BTreeSet<_>>();
            let fronts = app
                .fronts
                .iter()
                .filter(|front| front_ids.contains(front.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let outbound_ids = fronts
                .iter()
                .flat_map(|front| front.external_via.iter().map(String::as_str))
                .collect::<BTreeSet<_>>();
            let external_outbounds = app
                .external_outbounds
                .iter()
                .filter(|outbound| outbound_ids.contains(outbound.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let node_ids = ingresses
                .iter()
                .map(|ingress| ingress.node.as_str())
                .collect::<BTreeSet<_>>();
            let nodes = app
                .nodes
                .iter()
                .filter(|node| node_ids.contains(node.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let users = app
                .users
                .iter()
                .filter(|candidate| candidate.tenant == tenant && candidate.id == user)
                .cloned()
                .collect::<Vec<_>>();
            let chains = app
                .chains
                .iter()
                .filter(|chain| chain_ids.contains(chain.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let tenants = nodes
                .iter()
                .map(|node| node.tenant.clone())
                .chain(users.iter().map(|user| user.tenant.clone()))
                .chain(chains.iter().map(|chain| chain.tenant.clone()))
                .chain(ingresses.iter().map(|ingress| ingress.tenant.clone()))
                .chain(fronts.iter().map(|front| front.tenant.clone()))
                .chain(grants.iter().map(|grant| grant.tenant.clone()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            AppIr {
                revision: 0,
                app_id: app.app_id.clone(),
                tenants,
                nodes,
                users,
                external_outbounds,
                chains,
                ingresses,
                fronts,
                steps: Vec::new(),
                listener_roots: BTreeSet::new(),
                grants,
                hops: Vec::new(),
            }
        })
        .collect()
}

/// Subscription protocols have a product order independent of their rendered suffixes.
///
/// VLESS keeps the bare chain name, AnyTLS follows it, and Hysteria2/QUIC comes last. Keeping
/// this rank separate from `name` also means changing translated labels cannot silently reorder
/// a user's subscription.
fn subscription_protocol_rank(security: &UserSecurityPlan) -> u8 {
    match security {
        UserSecurityPlan::Reality(_)
        | UserSecurityPlan::Tls(_)
        | UserSecurityPlan::VlessEncryption { .. } => 0,
        UserSecurityPlan::AnyTls(_) => 1,
        UserSecurityPlan::Hysteria2(_) => 2,
        UserSecurityPlan::MtProto { .. } => 3,
    }
}

fn tenant_within(tenant: &str, scope: &str) -> bool {
    tenant == scope || tenant.starts_with(&format!("{scope}."))
}

/// The direct address precedes its v6 peer inside one protocol pair. `None` is the single
/// placeholder emitted when neither family has a publishable address, so putting it first is
/// deterministic without affecting a real dual-stack pair.
fn subscription_family_rank(family: Option<IpFamily>) -> u8 {
    match family {
        None | Some(IpFamily::V4) => 0,
        Some(IpFamily::V6) => 1,
    }
}

/// The only portable icon a URI or YAML subscription can carry is text. Regional-indicator
/// Unicode characters become a country flag in clients with an emoji font, while the explicit
/// model field keeps the generated name stable across probe failures and historical revisions.
fn subscription_chain_name(name: &str, country: Option<&str>) -> String {
    let Some(code) = country
        .map(str::trim)
        .filter(|code| code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_uppercase()))
    else {
        return name.to_owned();
    };
    let mut flag = String::new();
    for byte in code.bytes() {
        // The shape check above constrains this to the 26 regional-indicator symbols.
        flag.push(
            char::from_u32(0x1f1e6 + u32::from(byte - b'A')).expect("valid regional indicator"),
        );
    }
    format!("{flag}{name}")
}

struct SubscriptionServer {
    address: String,
    family: Option<IpFamily>,
    /// The port follows the address: a projected entry takes the projection's port,
    /// an unprojected one the ingress's listening port. There used to be no port
    /// here — writing `ingress.port` for every entry sufficed, because the address
    /// could only be this machine's own and that is where it listens. Projection
    /// broke that premise.
    port: u16,
    download: Option<ProjectionDownloadEndpoint>,
    name_suffix: &'static str,
}

/// Which addresses this ingress contributes to a subscription.
///
/// The two families are computed independently and do not affect each other: v4 may
/// be projected alone, both may be, or neither.
fn subscription_servers(
    node: &AppNode,
    ingress: &Ingress,
    security: &UserSecurityPlan,
) -> Vec<SubscriptionServer> {
    let vless = matches!(
        security,
        UserSecurityPlan::Reality(_) | UserSecurityPlan::Tls(_)
    );
    let xhttp_download = vless
        .then(|| {
            ingress
                .wires
                .xhttp()
                .and_then(|xhttp| xhttp.download.as_ref())
        })
        .flatten();
    let v4 = projected_endpoint(ingress, security, IpFamily::V4);
    let v6 = projected_endpoint(ingress, security, IpFamily::V6);
    let listen_port = security_port(ingress, security);
    let mut servers = Vec::new();
    servers.extend(family_server(
        v4.as_ref(),
        node.public_ipv4.as_deref(),
        node.public_ipv4_nat,
        listen_port,
        IpFamily::V4,
        "",
        xhttp_download.and_then(|download| download.v4.as_ref()),
    ));
    servers.extend(family_server(
        v6.as_ref(),
        node.public_ipv6.as_deref(),
        node.public_ipv6_nat,
        listen_port,
        IpFamily::V6,
        " | v6",
        xhttp_download.and_then(|download| download.v6.as_ref()),
    ));
    if servers.is_empty() {
        servers.push(SubscriptionServer {
            address: "?".to_owned(),
            family: None,
            port: listen_port,
            download: None,
            name_suffix: "",
        });
    }
    servers
}

/// Resolve one protocol's mapping. `None` on a per-protocol pair is the historical shared
/// representation: keep the VLESS host but substitute the protocol's real listening port.
/// `Some(empty)` is an explicit direct mapping and therefore must not inherit anything.
fn projected_endpoint(
    ingress: &Ingress,
    security: &UserSecurityPlan,
    family: IpFamily,
) -> Option<ProjectionEndpoint> {
    let legacy = || match family {
        IpFamily::V4 => ingress.projection.v4.as_ref(),
        IpFamily::V6 => ingress.projection.v6.as_ref(),
    };
    let specific = match security {
        UserSecurityPlan::Reality(_) | UserSecurityPlan::Tls(_) => return legacy().cloned(),
        UserSecurityPlan::VlessEncryption { .. } => ingress.projection.vless_encryption.as_ref(),
        UserSecurityPlan::AnyTls(_) => ingress.projection.anytls.as_ref(),
        UserSecurityPlan::Hysteria2(_) => ingress.projection.hysteria2.as_ref(),
        UserSecurityPlan::MtProto { .. } => ingress.projection.mtproto.as_ref(),
    };
    if let Some(specific) = specific {
        return match family {
            IpFamily::V4 => specific.v4.clone(),
            IpFamily::V6 => specific.v6.clone(),
        };
    }
    legacy().map(|endpoint| ProjectionEndpoint {
        host: endpoint.host.clone(),
        port: security_port(ingress, security),
    })
}

fn security_port(ingress: &Ingress, security: &UserSecurityPlan) -> u16 {
    match security {
        UserSecurityPlan::Reality(_) | UserSecurityPlan::Tls(_) => ingress.port,
        UserSecurityPlan::VlessEncryption { port, .. } => *port,
        UserSecurityPlan::AnyTls(plan) => plan.settings.port,
        UserSecurityPlan::Hysteria2(plan) => plan.settings.port,
        UserSecurityPlan::MtProto { port } => *port,
    }
}

/// One socket which a target ingress actually publishes to subscribers.
///
/// Port and transport are part of reachability: a member chain can allow TCP/443 and block
/// UDP/8443 while both sockets share one hostname. Keeping this derivation beside subscription
/// generation prevents the static Front analysis from checking a route different from the one a
/// client will dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionEndpoint {
    pub host: String,
    pub port: u16,
    pub network: Network,
}

impl SubscriptionEndpoint {
    pub fn label(&self) -> String {
        let host = if self.host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let network = match self.network {
            Network::Tcp => "tcp",
            Network::Udp => "udp",
        };
        format!("{host}:{}/{network}", self.port)
    }
}

pub(crate) fn subscription_endpoints(
    node: &AppNode,
    ingress: &Ingress,
) -> Vec<SubscriptionEndpoint> {
    let mut endpoints = Vec::new();
    for (security, _) in securities(ingress) {
        let network = if matches!(security, UserSecurityPlan::Hysteria2(_)) {
            Network::Udp
        } else {
            Network::Tcp
        };
        for server in subscription_servers(node, ingress, &security)
            .into_iter()
            .filter(|server| server.family.is_some())
        {
            endpoints.push(SubscriptionEndpoint {
                host: server.address.clone(),
                port: server.port,
                network,
            });
            if let Some(download) = server.download {
                endpoints.push(SubscriptionEndpoint {
                    host: download.host,
                    port: download.port,
                    network: Network::Tcp,
                });
            }
        }
    }
    endpoints.sort_by(|left, right| {
        let network_rank = |network| match network {
            Network::Tcp => 0,
            Network::Udp => 1,
        };
        left.host
            .cmp(&right.host)
            .then_with(|| left.port.cmp(&right.port))
            .then_with(|| network_rank(left.network).cmp(&network_rank(right.network)))
    });
    endpoints.dedup();
    endpoints
}

/// Whether a family contributes an entry, and with which address.
///
/// Projection wins, and it neither considers NAT nor requires the node to have a
/// public address in that family — a projection is an external line's endpoint and
/// has nothing to do with this machine's own position on the network. So a machine
/// behind NAT with no dialable public v4 can still offer v4 ingress through a
/// projection.
fn family_server(
    projected: Option<&ProjectionEndpoint>,
    public: Option<&str>,
    nat: bool,
    listen_port: u16,
    family: IpFamily,
    name_suffix: &'static str,
    xhttp_download: Option<&ProjectionDownloadEndpoint>,
) -> Option<SubscriptionServer> {
    if let Some(projected) = projected {
        // An empty host means "projection is on but unfilled", not "no projection"
        // — `ingress.projection-blank` blocks the release. Should validation ever be
        // bypassed, this family contributes nothing rather than quietly falling back
        // to the direct address: an operator sets a projection precisely to route
        // traffic over that line, and falling back sends it down the path they did
        // not want.
        let host = projected.host.trim();
        if host.is_empty() {
            return None;
        }
        return Some(SubscriptionServer {
            address: host.to_owned(),
            family: Some(family),
            port: projected.port,
            download: xhttp_download.cloned(),
            name_suffix,
        });
    }
    public
        .filter(|_| !nat)
        .filter(|address| !address.is_empty())
        .map(|address| SubscriptionServer {
            address: address.to_owned(),
            family: Some(family),
            port: listen_port,
            download: xhttp_download.cloned(),
            name_suffix,
        })
}

fn external_proxies(
    apps: &[AppIr],
    entries: &[UserSubscriptionEntryPlan],
) -> Vec<UserExternalProxyPlan> {
    let used_fronts = entries
        .iter()
        .filter_map(|entry| entry.front_id.as_deref())
        .collect::<BTreeSet<_>>();
    let mut candidates = Vec::new();
    for app in apps {
        for front in app
            .fronts
            .iter()
            .filter(|front| used_fronts.contains(front.id.as_str()))
        {
            for outbound_id in &front.external_via {
                let Some(outbound) = app
                    .external_outbounds
                    .iter()
                    .find(|outbound| outbound.id == *outbound_id)
                else {
                    continue;
                };
                // Validation rejects this combination. Keeping it out of the subscription as
                // well makes a bypassed validator fail closed instead of publishing a logical
                // WARP resource with no user identity.
                if matches!(
                    outbound.protocol,
                    ExternalOutboundProtocol::Warp { .. }
                        | ExternalOutboundProtocol::Vpngate { .. }
                ) {
                    continue;
                }
                let id = outbound.id.clone();
                if candidates
                    .iter()
                    .any(|candidate: &UserExternalProxyPlan| candidate.id == id)
                {
                    continue;
                }
                candidates.push(UserExternalProxyPlan {
                    id,
                    name: outbound.name.clone(),
                    address: outbound.address.clone(),
                    port: outbound.port,
                    protocol: outbound.protocol.clone(),
                    security: outbound.security.clone(),
                });
            }
        }
    }
    let counts = candidates
        .iter()
        .fold(BTreeMap::new(), |mut counts, proxy| {
            *counts.entry(proxy.name.clone()).or_insert(0_usize) += 1;
            counts
        });
    for proxy in &mut candidates {
        if counts.get(&proxy.name).copied().unwrap_or_default() > 1 {
            proxy.name = format!("{} · {}", proxy.name, proxy.id);
        }
    }
    candidates
}

fn front_groups(
    apps: &[AppIr],
    entries: &[UserSubscriptionEntryPlan],
    external_proxies: &[UserExternalProxyPlan],
) -> Vec<UserFrontGroupPlan> {
    let entries_by_ingress = entries.iter().fold(
        BTreeMap::<&str, Vec<&UserSubscriptionEntryPlan>>::new(),
        |mut by_ingress, entry| {
            by_ingress
                .entry(entry.ingress_id.as_str())
                .or_default()
                .push(entry);
            by_ingress
        },
    );
    let used_fronts = entries
        .iter()
        .filter_map(|entry| entry.front_id.as_deref())
        .collect::<BTreeSet<_>>();
    let mut groups = Vec::new();

    for app in apps {
        for front in app
            .fronts
            .iter()
            .filter(|front| used_fronts.contains(front.id.as_str()))
        {
            let mut members = front
                .via
                .iter()
                .filter_map(|ingress_id| entries_by_ingress.get(ingress_id.as_str()))
                .flat_map(|entries| entries.iter().map(|entry| entry.name.clone()))
                .collect::<Vec<_>>();
            members.extend(front.external_via.iter().filter_map(|outbound_id| {
                external_proxies
                    .iter()
                    .find(|proxy| proxy.id == *outbound_id)
                    .map(|proxy| proxy.name.clone())
            }));
            groups.push(UserFrontGroupPlan {
                id: front.id.clone(),
                name: front.name.clone(),
                strategy: front.strategy,
                members,
            });
        }
    }

    groups
}
