//! Independently committed client-facing subscription configuration.
//!
//! A [`ModelSnapshot`] mixes three kinds of state: machine topology, permissions, and fields
//! consumed only while rendering subscriptions.  Machines must never be told they run a newer
//! topology merely because a chain was renamed, so the store persists this small typed projection
//! under its own checkpoint and composes it over the last converged topology at read time.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    hash::sha256_hex,
    model::{
        AnyTlsSecurity, ExternalOutbound, FrontStrategy, Hysteria2, HysteriaBandwidth,
        HysteriaCongestion, HysteriaObfs, Ingress, ModelSnapshot, Node, Projection,
        ProjectionDownloadEndpoint, ProjectionEndpoint, Transport, XhttpMode, XhttpTuning,
        XhttpXmux,
    },
};

pub const SUBSCRIPTION_CLIENT_CONFIG_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionClientConfig {
    pub schema: u32,
    pub app_order: Vec<String>,
    pub chain_order: BTreeMap<String, Vec<String>>,
    pub chains: BTreeMap<String, ClientChain>,
    pub fronts: BTreeMap<String, ClientFront>,
    pub ingresses: BTreeMap<String, ClientIngress>,
    /// Stored as a sorted vector so the existing credential sealing machinery can transform the
    /// same `external_outbounds[*].protocol.v.credential` shape as model snapshots.
    pub external_outbounds: Vec<ExternalOutbound>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientChain {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_country: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFront {
    pub name: String,
    pub strategy: FrontStrategy,
    pub external_via: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIngress {
    /// Contract computed from the latest desired revision in which this ingress exists. Older
    /// candidates remain below solely for serving rollback.
    pub desired_contract: String,
    /// One client-facing candidate per topology shape. Old shapes are retained so a topology
    /// rollback selects the matching public endpoint without rewinding this checkpoint.
    pub projections: BTreeMap<String, ClientProjection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v4: Option<ClientProjectionEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v6: Option<ClientProjectionEndpoint>,
    /// Outer `None` is a checkpoint written before client transport controls moved here.
    /// `Some(None)` explicitly omits HTTP Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp_host: Option<Option<String>>,
    /// Outer `None` preserves an old checkpoint's topology-era value. New checkpoints always
    /// write `Some`, including `Some(None)` when XMUX is intentionally left to Xray.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp_xmux: Option<Option<XhttpXmux>>,
    /// Client-side independent download for IPv4. The outer `None` preserves checkpoints from
    /// before download settings moved out of the address projection; `Some(None)` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp_download_v4: Option<Option<ClientProjectionDownloadEndpoint>>,
    /// Client-side independent download for IPv6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhttp_download_v6: Option<Option<ClientProjectionDownloadEndpoint>>,
    /// ClientHello preset used by subscribers and probes. The REALITY listener never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality_fingerprint: Option<String>,
    /// Outer `None` preserves a checkpoint written before AnyTLS session controls moved here;
    /// `Some(None)` explicitly follows the consuming client's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anytls_idle_session_check_interval_secs: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anytls_idle_session_timeout_secs: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anytls_min_idle_session: Option<Option<u32>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientProjectionEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<ClientProjectionDownloadEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientProjectionDownloadEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<u16>,
}

#[derive(Debug, Serialize)]
struct SubscriptionTopologyContract<'a> {
    schema: u32,
    app_id: &'a str,
    chain_id: &'a str,
    ingress_id: &'a str,
    node_id: &'a str,
    ingress_port: u16,
    vless: Option<VlessTopologyContract<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anytls: Option<AnyTlsTopologyContract<'a>>,
    hysteria2: Option<Hysteria2TopologyContract<'a>>,
    split_download_ports: [Option<u16>; 2],
    split_download_certificate_name: Option<&'a str>,
}

/// Client-visible VLESS capabilities owned by the serving topology.
///
/// This is deliberately not a serialized [`Transport`]. REALITY fallback routing and throttling
/// are server-only, while client fingerprint, HTTP Host and XMUX live on the candidate rather than
/// in this contract. Flow is topology-owned: changing it must hold the client candidate until the
/// matching server release succeeds.
#[derive(Debug, Serialize)]
#[serde(tag = "security", rename_all = "kebab-case")]
enum VlessTopologyContract<'a> {
    Reality {
        public_key: &'a str,
        short_id: Option<&'a str>,
        server_name: String,
        flow: Option<&'a str>,
        xhttp: Option<XhttpTopologyContract<'a>>,
    },
    Tls {
        certificate_name: Option<&'a str>,
        flow: Option<&'a str>,
        xhttp: Option<XhttpTopologyContract<'a>>,
    },
}

#[derive(Debug, Serialize)]
struct XhttpTopologyContract<'a> {
    path: &'a str,
    tuning: Option<&'a XhttpTuning>,
    mode: XhttpMode,
}

/// AnyTLS values which must match the listener before a new public endpoint can be advertised.
/// Padding, masquerade and client session pooling are deliberately absent.
#[derive(Debug, Serialize)]
#[serde(tag = "security", rename_all = "kebab-case")]
enum AnyTlsTopologyContract<'a> {
    Tls {
        port: u16,
        certificate_name: Option<&'a str>,
    },
    Reality {
        port: u16,
        public_key: Option<&'a str>,
        short_id: Option<&'a str>,
        server_name: Option<&'a str>,
        fingerprint: Option<&'a str>,
    },
}

/// Hysteria 2 values which are either required to reach the listener or emitted by a supported
/// subscription renderer.
///
/// Masquerade, BBR profile and the server-only QUIC controls are intentionally absent. They may
/// change how the node serves traffic, but do not change the configuration handed to a subscriber
/// and therefore do not make a public endpoint candidate incompatible with the serving topology.
#[derive(Debug, Serialize)]
struct Hysteria2TopologyContract<'a> {
    port: u16,
    hop: Option<(u16, u16)>,
    certificate_name: Option<&'a str>,
    obfs: Option<&'a HysteriaObfs>,
    client_bandwidth: Option<&'a HysteriaBandwidth>,
    client_quic: Hysteria2ClientQuicContract,
}

/// The receive-side QUIC controls currently written into Clash subscriptions. Other QUIC fields
/// configure only the Xray listener and must not delay a client-only projection update.
#[derive(Debug, Serialize)]
struct Hysteria2ClientQuicContract {
    init_stream_receive_window: Option<u64>,
    max_stream_receive_window: Option<u64>,
    init_connection_receive_window: Option<u64>,
    max_connection_receive_window: Option<u64>,
}

impl SubscriptionClientConfig {
    /// Build the initial manifest. Subsequent revisions should use [`Self::advance`] so deleting a
    /// resource from desired state cannot make a still-serving topology fall back to stale names.
    pub fn from_snapshot(snapshot: &ModelSnapshot) -> Self {
        Self::advance(None, snapshot)
    }

    /// Carry the previous manifest forward and replace only resources present in `desired`.
    /// Structural deletion remains topology-owned; newly created resources are harmless because
    /// [`Self::apply`] only traverses resources found in the serving topology.
    pub fn advance(previous: Option<&Self>, desired: &ModelSnapshot) -> Self {
        let mut next = previous.cloned().unwrap_or_else(Self::empty);
        next.schema = SUBSCRIPTION_CLIENT_CONFIG_SCHEMA;
        next.app_order = merge_order(
            &next.app_order,
            desired.apps.iter().map(|app| app.id.as_str()),
        );

        for app in &desired.apps {
            let old_order = next.chain_order.get(&app.id).cloned().unwrap_or_default();
            next.chain_order.insert(
                app.id.clone(),
                merge_order(&old_order, app.chains.iter().map(|chain| chain.id.as_str())),
            );
            for chain in &app.chains {
                next.chains.insert(
                    chain.id.clone(),
                    ClientChain {
                        name: chain.name.clone(),
                        subscription_country: chain.subscription_country.clone(),
                    },
                );
            }
            for front in &app.fronts {
                next.fronts.insert(
                    front.id.clone(),
                    ClientFront {
                        name: front.name.clone(),
                        strategy: front.strategy,
                        external_via: front.external_via.clone(),
                    },
                );
            }
            for ingress in &app.ingresses {
                let contract = topology_contract_hash(&desired.nodes, &app.id, ingress);
                let client = next.ingresses.entry(ingress.id.clone()).or_default();
                client.desired_contract.clone_from(&contract);
                client
                    .projections
                    .insert(contract, ClientProjection::from(ingress));
            }
        }

        let mut external = next
            .external_outbounds
            .into_iter()
            .map(|outbound| (outbound.id.clone(), outbound))
            .collect::<BTreeMap<_, _>>();
        for outbound in &desired.external_outbounds {
            let mut outbound = outbound.clone();
            // Bindings are machine/provider state. They are never rendered as client proxies and
            // copying their private keys into this checkpoint would create a needless secret copy.
            outbound.bindings.clear();
            external.insert(outbound.id.clone(), outbound);
        }
        next.external_outbounds = external.into_values().collect();
        next
    }

    /// Overlay client-owned values on an immutable serving topology.
    pub fn apply(&self, mut topology: ModelSnapshot) -> Result<ModelSnapshot, String> {
        if self.schema != SUBSCRIPTION_CLIENT_CONFIG_SCHEMA {
            return Err(format!(
                "unsupported subscription client config schema {}",
                self.schema
            ));
        }

        let nodes = &topology.nodes;
        reorder_by_id(&mut topology.apps, &self.app_order, |app| &app.id);

        for app in &mut topology.apps {
            if let Some(order) = self.chain_order.get(&app.id) {
                reorder_by_id(&mut app.chains, order, |chain| &chain.id);
            }
            for chain in &mut app.chains {
                if let Some(client) = self.chains.get(&chain.id) {
                    chain.name.clone_from(&client.name);
                    chain
                        .subscription_country
                        .clone_from(&client.subscription_country);
                }
            }
            for front in &mut app.fronts {
                if let Some(client) = self.fronts.get(&front.id) {
                    front.name.clone_from(&client.name);
                    front.strategy = client.strategy;
                    front.external_via.clone_from(&client.external_via);
                }
            }
            for ingress in &mut app.ingresses {
                let contract = topology_contract_hash(nodes, &app.id, ingress);
                let Some(client) = self
                    .ingresses
                    .get(&ingress.id)
                    .and_then(|item| item.projections.get(&contract))
                else {
                    continue;
                };
                ingress.projection = client.to_model()?;
                if let (Some(reality), Some(fingerprint)) = (
                    ingress.wires.reality_mut(),
                    client.reality_fingerprint.as_ref(),
                ) {
                    reality.fingerprint.clone_from(fingerprint);
                }
                let reality_split =
                    matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)));
                if let Some(xhttp) = ingress.wires.xhttp_mut() {
                    if let Some(host) = &client.xhttp_host {
                        xhttp.host.clone_from(host);
                    }
                    if let Some(xmux) = &client.xhttp_xmux {
                        xhttp.xmux.clone_from(xmux);
                    }
                    client.apply_xhttp_download(xhttp, reality_split)?;
                }
                if let Some(anytls) = ingress.wires.anytls_mut() {
                    if let Some(value) = client.anytls_idle_session_check_interval_secs {
                        anytls.idle_session_check_interval_secs = value;
                    }
                    if let Some(value) = client.anytls_idle_session_timeout_secs {
                        anytls.idle_session_timeout_secs = value;
                    }
                    if let Some(value) = client.anytls_min_idle_session {
                        anytls.min_idle_session = value;
                    }
                }
            }
        }

        // These are consumed only by user projection on this composed snapshot. Machine artifact
        // planning never calls this method and continues to use the topology revision's copy.
        //
        // WARP is the one logical outbound whose publishability also depends on machine-owned
        // state. Its per-node identities are deliberately absent from the client checkpoint, so
        // restore them from the immutable serving topology before the composed model passes
        // through the ordinary publish gate. They still never become subscription proxies — the
        // user projector rejects WARP — and are never persisted in this checkpoint.
        let mut external_outbounds = self.external_outbounds.clone();
        for outbound in &mut external_outbounds {
            if !matches!(
                outbound.protocol,
                crate::model::ExternalOutboundProtocol::Warp { .. }
            ) {
                continue;
            }
            let Some(serving) = topology.external_outbounds.iter().find(|serving| {
                serving.id == outbound.id
                    && matches!(
                        serving.protocol,
                        crate::model::ExternalOutboundProtocol::Warp { .. }
                    )
            }) else {
                continue;
            };
            outbound.bindings.clone_from(&serving.bindings);
        }
        topology.external_outbounds = external_outbounds;
        Ok(topology)
    }

    /// Stable resource labels for client candidates which do not match the current topology.
    pub fn pending_topology(&self, topology: &ModelSnapshot) -> Vec<String> {
        let nodes = &topology.nodes;
        let serving = topology
            .apps
            .iter()
            .flat_map(|app| {
                app.ingresses.iter().map(move |ingress| {
                    (
                        ingress.id.as_str(),
                        topology_contract_hash(nodes, &app.id, ingress),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        serving
            .into_iter()
            .filter_map(|(id, contract)| {
                self.ingresses
                    .get(id)
                    .is_some_and(|item| item.desired_contract != contract)
                    .then(|| format!("ingress:{id}:projection"))
            })
            .collect()
    }

    fn empty() -> Self {
        Self {
            schema: SUBSCRIPTION_CLIENT_CONFIG_SCHEMA,
            app_order: Vec::new(),
            chain_order: BTreeMap::new(),
            chains: BTreeMap::new(),
            fronts: BTreeMap::new(),
            ingresses: BTreeMap::new(),
            external_outbounds: Vec::new(),
        }
    }
}

impl From<&Ingress> for ClientProjection {
    fn from(value: &Ingress) -> Self {
        let xhttp = value.wires.xhttp();
        let anytls = value.wires.anytls();
        Self {
            v4: value
                .projection
                .v4
                .as_ref()
                .map(ClientProjectionEndpoint::from),
            v6: value
                .projection
                .v6
                .as_ref()
                .map(ClientProjectionEndpoint::from),
            xhttp_host: xhttp.map(|xhttp| xhttp.host.clone()),
            xhttp_xmux: xhttp.map(|xhttp| xhttp.xmux.clone()),
            xhttp_download_v4: xhttp.map(|xhttp| {
                xhttp
                    .download
                    .as_ref()
                    .and_then(|download| download.v4.as_ref())
                    .map(ClientProjectionDownloadEndpoint::from)
            }),
            xhttp_download_v6: xhttp.map(|xhttp| {
                xhttp
                    .download
                    .as_ref()
                    .and_then(|download| download.v6.as_ref())
                    .map(ClientProjectionDownloadEndpoint::from)
            }),
            reality_fingerprint: value
                .wires
                .reality()
                .map(|reality| reality.fingerprint.clone()),
            anytls_idle_session_check_interval_secs: anytls
                .map(|settings| settings.idle_session_check_interval_secs),
            anytls_idle_session_timeout_secs: anytls
                .map(|settings| settings.idle_session_timeout_secs),
            anytls_min_idle_session: anytls.map(|settings| settings.min_idle_session),
        }
    }
}

impl From<&ProjectionEndpoint> for ClientProjectionEndpoint {
    fn from(value: &ProjectionEndpoint) -> Self {
        Self {
            host: value.host.clone(),
            port: value.port,
            // Historical checkpoints may still contain this nested value and are handled as a
            // fallback during apply. New checkpoints keep download settings on ClientProjection.
            download: None,
        }
    }
}

impl From<&ProjectionDownloadEndpoint> for ClientProjectionDownloadEndpoint {
    fn from(value: &ProjectionDownloadEndpoint) -> Self {
        Self {
            host: value.host.clone(),
            port: value.port,
            http_host: value.http_host.clone(),
            mux: value.mux,
        }
    }
}

impl ClientProjection {
    fn to_model(&self) -> Result<Projection, String> {
        Ok(Projection {
            v4: self
                .v4
                .as_ref()
                .map(ClientProjectionEndpoint::to_model)
                .transpose()?,
            v6: self
                .v6
                .as_ref()
                .map(ClientProjectionEndpoint::to_model)
                .transpose()?,
        })
    }

    fn apply_xhttp_download(
        &self,
        xhttp: &mut crate::model::Xhttp,
        reality_split: bool,
    ) -> Result<(), String> {
        let v4 = self.download_for(
            self.xhttp_download_v4.as_ref(),
            self.v4
                .as_ref()
                .and_then(|endpoint| endpoint.download.as_ref()),
            xhttp
                .download
                .as_ref()
                .and_then(|download| download.v4.as_ref()),
            reality_split,
        )?;
        let v6 = self.download_for(
            self.xhttp_download_v6.as_ref(),
            self.v6
                .as_ref()
                .and_then(|endpoint| endpoint.download.as_ref()),
            xhttp
                .download
                .as_ref()
                .and_then(|download| download.v6.as_ref()),
            reality_split,
        )?;
        xhttp.download =
            (v4.is_some() || v6.is_some()).then_some(crate::model::XhttpDownload { v4, v6 });
        Ok(())
    }

    fn download_for(
        &self,
        configured: Option<&Option<ClientProjectionDownloadEndpoint>>,
        legacy: Option<&ClientProjectionDownloadEndpoint>,
        serving: Option<&ProjectionDownloadEndpoint>,
        reality_split: bool,
    ) -> Result<Option<ProjectionDownloadEndpoint>, String> {
        let Some(configured) = configured else {
            return Ok(serving.cloned().or_else(|| {
                legacy.map(|legacy| ProjectionDownloadEndpoint {
                    host: legacy.host.clone(),
                    port: legacy.port,
                    origin_port: None,
                    http_host: legacy.http_host.clone(),
                    mux: legacy.mux,
                })
            }));
        };
        let Some(configured) = configured else {
            return Ok(None);
        };
        let origin_port = serving.and_then(|download| download.origin_port);
        if reality_split && origin_port.is_none() {
            return Err(
                "matching REALITY+XHTTP topology has no split download listener".to_owned(),
            );
        }
        Ok(Some(ProjectionDownloadEndpoint {
            host: configured.host.clone(),
            port: configured.port,
            origin_port: reality_split.then_some(origin_port).flatten(),
            http_host: configured.http_host.clone(),
            mux: configured.mux,
        }))
    }
}

impl ClientProjectionEndpoint {
    fn to_model(&self) -> Result<ProjectionEndpoint, String> {
        Ok(ProjectionEndpoint {
            host: self.host.clone(),
            port: self.port,
            // Legacy nested downloads are read by ClientProjection::apply_xhttp_download.
            download: None,
        })
    }
}

pub fn topology_contract_hash(nodes: &[Node], app_id: &str, ingress: &Ingress) -> String {
    let certificate_name = nodes
        .iter()
        .find(|node| node.id == ingress.node)
        .and_then(|node| node.certificate_name.as_deref());
    let vless = vless_topology_contract(ingress, certificate_name);
    let anytls = ingress.wires.anytls().map(|wire| match wire.security {
        AnyTlsSecurity::Tls => AnyTlsTopologyContract::Tls {
            port: wire.port,
            certificate_name,
        },
        AnyTlsSecurity::Reality => AnyTlsTopologyContract::Reality {
            port: wire.port,
            public_key: ingress
                .anytls_identity
                .as_ref()
                .map(|identity| identity.public_key.as_str()),
            short_id: ingress
                .anytls_identity
                .as_ref()
                .and_then(|identity| identity.short_ids.first())
                .map(String::as_str),
            server_name: wire
                .reality
                .as_ref()
                .and_then(|reality| reality.server_names.first())
                .map(String::as_str),
            fingerprint: wire
                .reality
                .as_ref()
                .map(|reality| reality.fingerprint.as_str()),
        },
    });
    let hysteria2 = ingress
        .wires
        .hysteria2()
        .map(|wire| hysteria2_topology_contract(wire, certificate_name));
    let reality_split = matches!(ingress.wires.vless(), Some(Transport::VlessRealityXhttp(_)));
    let split_download_ports = if reality_split {
        [
            ingress
                .wires
                .xhttp()
                .and_then(|xhttp| xhttp.download.as_ref())
                .and_then(|download| download.v4.as_ref())
                .map(ProjectionDownloadEndpoint::node_port),
            ingress
                .wires
                .xhttp()
                .and_then(|xhttp| xhttp.download.as_ref())
                .and_then(|download| download.v6.as_ref())
                .map(ProjectionDownloadEndpoint::node_port),
        ]
    } else {
        [None, None]
    };
    let split_download_certificate_name = split_download_ports
        .iter()
        .any(Option::is_some)
        .then_some(certificate_name)
        .flatten();
    let contract = SubscriptionTopologyContract {
        schema: 2,
        app_id,
        chain_id: &ingress.chain,
        ingress_id: &ingress.id,
        node_id: &ingress.node,
        ingress_port: ingress.port,
        vless,
        anytls,
        hysteria2,
        split_download_ports,
        split_download_certificate_name,
    };
    sha256_hex(
        &serde_json::to_vec(&contract)
            .expect("SubscriptionTopologyContract contains only serializable model values"),
    )
}

fn vless_topology_contract<'a>(
    ingress: &'a Ingress,
    certificate_name: Option<&'a str>,
) -> Option<VlessTopologyContract<'a>> {
    let transport = ingress.wires.vless()?;
    let xhttp = transport.xhttp().map(|xhttp| XhttpTopologyContract {
        path: &xhttp.path,
        tuning: xhttp.tuning.as_ref(),
        mode: xhttp.mode,
    });
    Some(match transport.reality() {
        Some(reality) => VlessTopologyContract::Reality {
            public_key: &ingress.identity.public_key,
            // Subscription rendering deliberately selects the first accepted identity.
            short_id: ingress.identity.short_ids.first().map(String::as_str),
            server_name: reality.server_name(certificate_name),
            flow: transport.flow(),
            xhttp,
        },
        None => VlessTopologyContract::Tls {
            certificate_name,
            flow: transport.flow(),
            xhttp,
        },
    })
}

fn hysteria2_topology_contract<'a>(
    wire: &'a Hysteria2,
    certificate_name: Option<&'a str>,
) -> Hysteria2TopologyContract<'a> {
    // Keep this normalization aligned with `format::yaml::push_hysteria2_proxy`: BBR estimates
    // its own rate, while the other supported client shapes receive the configured limits.
    let client_bandwidth = (wire.congestion != HysteriaCongestion::Bbr).then_some(&wire.bandwidth);
    Hysteria2TopologyContract {
        port: wire.port,
        hop: wire.hop.as_ref().map(|hop| (hop.start, hop.end)),
        certificate_name,
        obfs: wire.obfs.as_ref(),
        client_bandwidth,
        client_quic: Hysteria2ClientQuicContract {
            init_stream_receive_window: wire.quic.init_stream_receive_window,
            max_stream_receive_window: wire.quic.max_stream_receive_window,
            init_connection_receive_window: wire.quic.init_connection_receive_window,
            max_connection_receive_window: wire.quic.max_connection_receive_window,
        },
    }
}

fn merge_order<'a>(previous: &[String], desired: impl Iterator<Item = &'a str>) -> Vec<String> {
    let desired = desired.map(str::to_owned).collect::<Vec<_>>();
    let desired_set = desired.iter().cloned().collect::<BTreeSet<_>>();
    let previous_set = previous.iter().cloned().collect::<BTreeSet<_>>();
    let mut reordered_existing = desired
        .iter()
        .filter(|id| previous_set.contains(id.as_str()))
        .cloned();
    let mut merged = previous
        .iter()
        .map(|id| {
            if desired_set.contains(id.as_str()) {
                reordered_existing
                    .next()
                    .expect("every desired previous id occupies one previous slot")
            } else {
                id.clone()
            }
        })
        .collect::<Vec<_>>();

    // A resource missing from desired may still exist in serving topology. Leave it in its old
    // slot; otherwise merely deleting B from desired [A, B, C] would prematurely reorder the old
    // serving topology to [A, C, B]. New resources are inserted before their next surviving
    // desired neighbour (or appended), and remain invisible until topology contains them.
    for (index, id) in desired.iter().enumerate() {
        if previous_set.contains(id.as_str()) {
            continue;
        }
        let before = desired[index + 1..]
            .iter()
            .find(|next| previous_set.contains(next.as_str()))
            .and_then(|next| merged.iter().position(|existing| existing == next));
        match before {
            Some(position) => merged.insert(position, id.clone()),
            None => merged.push(id.clone()),
        }
    }
    merged
}

fn reorder_by_id<T>(items: &mut [T], order: &[String], id: impl Fn(&T) -> &String) {
    let position = order
        .iter()
        .enumerate()
        .map(|(position, id)| (id.as_str(), position))
        .collect::<BTreeMap<_, _>>();
    items.sort_by_key(|item| {
        position
            .get(id(item).as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
}
