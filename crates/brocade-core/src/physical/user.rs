use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ir::routing::{AppIr, AppNode, Ingress},
    model::{
        FrontStrategy, Hysteria2, IpFamily, ProjectionDownloadEndpoint, ProjectionEndpoint, Xhttp,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPlan {
    pub tenant: String,
    pub user: String,
    pub uuid: String,
    pub entries: Vec<UserSubscriptionEntryPlan>,
    pub front_groups: Vec<UserFrontGroupPlan>,
}

impl UserPlan {
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
        loop {
            let kept = self
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
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
    pub fingerprint: String,
    pub http_host: Option<String>,
    pub mux: Option<u16>,
}

/// What the client has to present, which is the same question the probe answers in `probe.rs`
/// and has to answer identically: a probe measures the path a subscriber takes only while both
/// are handed the same parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserSecurityPlan {
    Reality(UserRealityPlan),
    Tls(UserTlsPlan),
    Hysteria2(UserHysteria2Plan),
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
    pub fingerprint: String,
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

/// The security halves this ingress hands a subscriber, in the order they appear.
///
/// One entry per wire: a client cannot speak both at once, so an ingress serving TCP and QUIC
/// gives a person two lines to choose between rather than one line describing two things.
///
/// The suffix disambiguates the names, and only when there is something to disambiguate — two
/// entries under one chain name would collide, and mihomo refuses a proxy list with duplicate
/// names outright. A single-wire ingress keeps the bare chain name it always had.
fn securities(ingress: &Ingress) -> Vec<(UserSecurityPlan, &'static str)> {
    let mut wires = Vec::new();
    let both = ingress.wires.has_tcp() && ingress.wires.has_udp();

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
                fingerprint: ingress.wires.fingerprint().to_owned(),
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
            if both { "（QUIC）" } else { "" },
        ));
    }

    wires
}

pub fn project_user(apps: &[AppIr], tenant: &str, user: &str) -> UserPlan {
    let uuid = sorted_apps(apps)
        .into_iter()
        .flat_map(|app| app.users.iter())
        .find(|candidate| candidate.tenant == tenant && candidate.id == user)
        .map(|user| user.uuid.clone())
        .unwrap_or_else(|| "?".to_owned());

    let mut entries = Vec::new();
    for app in sorted_apps(apps) {
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
                .map(|chain| chain.name.clone())
                .unwrap_or_else(|| ingress.id.clone());
            let front = ingress
                .front
                .as_ref()
                .and_then(|front_id| app.fronts.iter().find(|front| front.id == *front_id));
            let wires = securities(ingress);
            for server in subscription_servers(node, ingress) {
                for (security, wire_suffix) in &wires {
                    let quic = matches!(security, UserSecurityPlan::Hysteria2(_));
                    entries.push(UserSubscriptionEntryPlan {
                        grant_id: grant.id.clone(),
                        ingress_id: ingress.id.clone(),
                        name: format!("{}{}{}", chain_name, server.name_suffix, wire_suffix),
                        server: server.address.clone(),
                        family: server.family,
                        // The QUIC wire announces its own port even where a projection set one.
                        // A projection is one address and one number, and there are now two
                        // wires wanting different numbers behind it — so the address it
                        // supplies is honoured and the port comes from the wire that will
                        // actually answer. An external line still has to carry that UDP port;
                        // announcing the projection's instead would name a port nothing
                        // listens on at either end.
                        port: match security {
                            UserSecurityPlan::Hysteria2(plan) => plan.settings.port,
                            _ => server.port,
                        },
                        // The independent download belongs to the XHTTP half and to nothing else;
                        // QUIC carries its own streams and has no second connection to project.
                        download: server.download.clone().filter(|_| !quic).map(|download| {
                            UserDownloadPlan {
                                server: download.host,
                                port: download.port,
                                // A split REALITY ingress terminates the downlink with this
                                // machine's certificate. Validation prevents the empty case from
                                // being published.
                                server_name: ingress.certificate_name.clone().unwrap_or_default(),
                                fingerprint: ingress.wires.fingerprint().to_owned(),
                                http_host: download.http_host,
                                mux: download.mux,
                            }
                        }),
                        uuid: uuid.clone(),
                        security: security.clone(),
                        xhttp: if quic {
                            None
                        } else {
                            ingress.wires.xhttp().cloned()
                        },
                        front_id: ingress.front.clone(),
                        front_name: front.map(|front| front.name.clone()),
                    });
                }
            }
        }
    }
    entries.sort_by(|a, b| {
        a.grant_id
            .cmp(&b.grant_id)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.server.cmp(&b.server))
    });

    let front_groups = front_groups(apps, &entries);

    UserPlan {
        tenant: tenant.to_owned(),
        user: user.to_owned(),
        uuid,
        entries,
        front_groups,
    }
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
fn subscription_servers(node: &AppNode, ingress: &Ingress) -> Vec<SubscriptionServer> {
    let mut servers = Vec::new();
    servers.extend(family_server(
        ingress.projection.v4.as_ref(),
        node.public_ipv4.as_deref(),
        node.public_ipv4_nat,
        ingress.port,
        IpFamily::V4,
        "",
    ));
    servers.extend(family_server(
        ingress.projection.v6.as_ref(),
        node.public_ipv6.as_deref(),
        node.public_ipv6_nat,
        ingress.port,
        IpFamily::V6,
        "（IPv6）",
    ));
    if servers.is_empty() {
        servers.push(SubscriptionServer {
            address: "?".to_owned(),
            family: None,
            port: ingress.port,
            download: None,
            name_suffix: "",
        });
    }
    servers
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
            download: projected.download.clone(),
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
            download: None,
            name_suffix,
        })
}

fn front_groups(apps: &[AppIr], entries: &[UserSubscriptionEntryPlan]) -> Vec<UserFrontGroupPlan> {
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

    for app in sorted_apps(apps) {
        for front in app
            .fronts
            .iter()
            .filter(|front| used_fronts.contains(front.id.as_str()))
        {
            let members = front
                .via
                .iter()
                .filter_map(|ingress_id| entries_by_ingress.get(ingress_id.as_str()))
                .flat_map(|entries| entries.iter().map(|entry| entry.name.clone()))
                .collect::<Vec<_>>();
            groups.push(UserFrontGroupPlan {
                id: front.id.clone(),
                name: front.name.clone(),
                strategy: front.strategy,
                members,
            });
        }
    }

    groups.sort_by(|a, b| a.id.cmp(&b.id));
    groups
}

fn sorted_apps(apps: &[AppIr]) -> Vec<&AppIr> {
    let mut apps = apps.iter().collect::<Vec<_>>();
    apps.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    apps
}
