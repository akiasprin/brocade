use std::{collections::BTreeMap, net::IpAddr};

use sha2::{Digest, Sha256};

use crate::{
    hash::hex_lower,
    ir::{hops::HopDialWire, routing::DestMatch},
    model::{
        AnyTls, Dns, DomainStrategy, EgressDnsAddressStrategy, EgressDnsTransport,
        ExternalOutboundProtocol, ExternalOutboundSecurity, GeodataSettings, HopPool, HopWire,
        Hysteria2, Network, RealityClientPolicy, XhttpTuning,
    },
    physical::node::{
        IngressProtocol, IngressSecurity, NodePlan, XrayClientPlan, XrayEgressDnsPlan,
        XrayEgressOutboundPlan, XrayExternalOutboundPlan, XrayFallbackLimitsPlan,
        XrayForwardOutboundPlan, XrayHopInboundPlan, XrayIngressPlan, XrayRuleSelector,
    },
    text::normalize_host_port,
};

/// Where the agent puts the machine's certificate.
///
/// Absolute, and coupled to `templates/install.sh`, which defaults `BROCADE_AGENT_STATE_DIR` to
/// `/var/lib/brocade-agent`. A relative path was tried first and cannot work: xray resolves a
/// relative `certificateFile` against **its own executable's** directory, not the working
/// directory, so `tls/cert.pem` looked for the file next to the xray binary no matter where the
/// process was started. Measured on 26.4.25 by running it.
///
/// The consequence of the coupling: a machine whose state directory was overridden at install
/// time holds its certificate elsewhere, and a TLS ingress on it fails at `xray -test` naming the
/// path it could not open. That failure appears in the agent's report rather than leaving the
/// ingress serving nothing.
pub const NODE_CERTIFICATE_FILE: &str = "/var/lib/brocade-agent/tls/cert.pem";
pub const NODE_CERTIFICATE_KEY_FILE: &str = "/var/lib/brocade-agent/tls/key.pem";

pub const API_TAG: &str = "api";
pub const DNS_TAG: &str = "dns-out";
pub const BLOCK_OUTBOUND_TAG: &str = "out:block";
pub const REALITY_COVER_OUTBOUND_TAG: &str = "out:reality-cover";
/// Where a fallback requesting a name this ingress does not impersonate is routed.
///
/// Its own blackhole rather than `BLOCK_OUTBOUND_TAG`, which is emitted only where a rule blocks
/// something. On a machine whose rules never block, the guard's deny rule would otherwise name an
/// outbound that was never built, and xray rejects that config.
///
/// It closes the connection and writes no HTTP response: what arrives here is raw TLS from a
/// client addressing the impersonated site, and injecting a 403 into that stream would reveal
/// that something other than the site is listening. `REALITY_COVER_OUTBOUND_TAG` may write a
/// response because its traffic has already been decrypted by a local listener.
pub const REALITY_GUARD_OUTBOUND_TAG: &str = "out:reality-guard";
/// The control plane's own direct outbound. Every machine has one, regardless of
/// `egress_allowed`.
///
/// It exists for one reason: a geodata update has to name an outbound, and a relay
/// machine has no `out:egress`, because that outbound is built only where a rule's
/// action is `Egress` and a relay's actions are all `Forward`. Routing the download
/// through a user chain would attribute that traffic to the chain; a dedicated
/// outbound keeps the attribution correct.
///
/// No routing rule references it, so user traffic cannot reach it; only the `geodata`
/// block selects it by tag.
///
/// Its name must contain no `>`: liveness recognizes forwarding outbounds by the shape
/// `out:{app}/{chain}>{to}` (the agent's `hop_of_tag`), so a `>` would make it look
/// like a hop; billing reads only the `user>>>` family and never touches it anyway.
/// Sidestepping both, it enters no ledger at all.
pub const INTERNAL_OUTBOUND_TAG: &str = "out:internal";
pub const NEVER_MATCH_DOMAIN: &str = "full:invalid.brocade.never-match";
/// The liveness balancer's tag. Purely a read handle, taking no part in real routing
/// (`XrayRouting::balancers`).
pub const HOP_HEALTH_BALANCER_TAG: &str = "hop-health";
// Probe address and interval. The 10-second interval trades probe traffic for
// sensitivity. One round trip is roughly 850 bytes, so every 10 seconds is about 7 MB
// per hop per day, which is recorded in the backbone counters as operator cost rather
// than user usage.
pub const HOP_HEALTH_PROBE_URL: &str = "http://www.gstatic.com/generate_204";
pub const HOP_HEALTH_PROBE_INTERVAL_SECS: u16 = 10;

// Config is not boxed: the two variants differ by 440 bytes, and a snapshot holds at
// most a few dozen machines, so boxing would save tens of kilobytes. The cost would be
// that every later reader has to work out why the indirection is there.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayArtifact {
    Disabled { node_id: String },
    Config(XrayConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayConfig {
    pub log_level: String,
    pub api: XrayApi,
    pub policy: XrayPolicy,
    pub dns: XrayDns,
    pub inbounds: Vec<XrayInbound>,
    pub outbounds: Vec<XrayOutbound>,
    pub routing: XrayRouting,
    /// Emitted only where this machine has forwarding outbounds (`XrayHopHealth`).
    pub hop_health: Option<XrayHopHealth>,
    /// Automatic `.dat` updates. Not an `Option`, because every machine running xray
    /// emits this block.
    pub geodata: XrayGeodata,
}

/// The `geodata` block.
///
/// An older xray does not recognize this key and ignores it without reporting anything,
/// verified on v26.3.27 where `xray -test` still returns `Configuration OK`, so shipping
/// it breaks nothing. The cost is that a config specifying daily updates while none has
/// ever run produces no symptom, and only the version distinguishes the two. The feature
/// reached main on 2026-04-25 (XTLS/Xray-core#5992) and is not in v26.3.27.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayGeodata {
    pub cron: String,
    /// Which outbound to download through, always `INTERNAL_OUTBOUND_TAG`.
    pub outbound: String,
    pub assets: Vec<XrayGeodataAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayGeodataAsset {
    pub url: String,
    /// The filename on disk, fixed at `geoip.dat` / `geosite.dat`. xray rewrites the
    /// `geosite:` prefix into `ext:geosite.dat:`, so the name is not configurable.
    pub file: String,
}

// The reverse tunnel has no block of its own any more. xray removed the top-level
// `reverse` config (portals/bridges over Mux.cool, steered by an agreed fake domain)
// and replaced it with a VLESS sub-protocol: the reverse role now rides on the
// credential. `XrayClient::reverse_tag` carries the portal end and
// `XrayOutbound::Vless::reverse_tag` the bridge end.
//
// Both tags keep their previous meaning: the portal's names a virtual outbound that
// rules send traffic to, and the bridge's names a virtual inbound that tunnelled
// traffic arrives on. The routing table below therefore needed no rewrite; only the
// two setup rules were removed, because there is no longer a control connection to
// recognize.

/// Relay-hop liveness.
///
/// The test is the outbound's counters rather than the observatory's verdict. The probe
/// traffic travels that outbound, so the hop's state appears directly in
/// `outbound>>>out:{app}/{chain}>{to}>>>traffic>>>downlink`, where growth means the hop
/// works. This requires no new read channel: `xray api` has no observatory verb,
/// `ObservatoryService.GetOutboundStatus` is gRPC only, and the agent writes its own
/// HTTP. `xray api bi` was tested and emits only Selects, with no health data.
///
/// It also removes the ambiguity between idle and dead: the observatory produces
/// traffic for as long as the hop is up, so a zero delta means the hop is down and no
/// separate active probe is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayHopHealth {
    /// The outbounds to observe, each by full name.
    ///
    /// The `out:` prefix must not be written: `subjectSelector` matches by prefix and
    /// would also select `out:egress` and `out:block`. The first is always up, being a
    /// direct freedom outbound that measures no relay liveness, and the second is
    /// always down, being a blackhole, so both would distort the result.
    pub subjects: Vec<String>,
    pub probe_url: String,
    pub probe_interval_secs: u16,
    /// What the routing rule whose only job is to instantiate the balancer
    /// matches.
    pub balancer_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayApi {
    pub tag: String,
    pub services: Vec<String>,
}

/// The `policy` block, already resolved to the numbers this machine writes.
///
/// `stats_user_online` sits next to the timeouts because xray puts them in one block, not
/// because they are one kind of setting: the timeouts are this machine's capacity and the
/// counter is a fleet-wide decision. The model keeps them apart and they meet here, at the
/// last moment before rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrayPolicy {
    pub conn_idle_secs: u32,
    pub handshake_secs: u32,
    pub uplink_only_secs: u32,
    pub downlink_only_secs: u32,
    /// `None` writes no key, leaving xray to size the buffer by CPU architecture.
    pub buffer_size_kb: Option<u32>,
    pub stats_user_online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayDns {
    pub tag: Option<String>,
    pub servers: Vec<XrayDnsServer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayDnsServer {
    Address(String),
    Scoped {
        address: String,
        port: u16,
        domains: Vec<String>,
        query_strategy: String,
        tag: String,
        final_query: bool,
    },
}

/// Which certificate an ingress presents — xray's `streamSettings.security`.
///
/// The REALITY branch carries the client-version policy because that is where xray places it.
/// TLS has no equivalent, so the fleet-wide setting does not reach a TLS inbound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayIngressSecurity {
    None,
    Reality {
        dest: String,
        server_names: Vec<String>,
        private_key: String,
        short_ids: Vec<String>,
        min_client_ver: Option<String>,
        max_client_ver: Option<String>,
        max_time_diff_ms: Option<u64>,
        fallback_limits: Option<XrayFallbackLimitsPlan>,
    },
    /// This machine's own certificate, named by path rather than by content.
    ///
    /// The paths are relative, and xray is started with the state directory as its working
    /// directory so that they resolve. Absolute ones cannot be written here at all: where a
    /// machine keeps its state is that machine's business, set at install time, and a compiler
    /// that had to know it would stop being a function of the model alone.
    Tls {
        certificate_file: String,
        key_file: String,
        /// `None` keeps xray's normal negotiation. The local HTTP-403 cover pins HTTP/1.1 because
        /// blackhole's built-in response is an HTTP/1.1 byte string, not an HTTP/2 frame.
        alpn: Option<Vec<String>>,
    },
}

/// What an ingress's stream is carried inside, which is xray's `streamSettings.network`.
///
/// Its own type rather than a flag, because the two carry different settings objects and one of
/// them has fields. `Tcp` writes exactly what was written before this existed, so every ingress
/// that has not asked for anything else produces byte-identical artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayStream {
    Tcp,
    Xhttp {
        path: String,
        /// No `xmux` here, deliberately. XMUX is a dialer's connection pool and only a dialer
        /// reads it: `XmuxManager` is built in `getHTTPClient` on the client path
        /// (`transport/internet/splithttp/mux.go`), and the listener side (`hub.go`) does not
        /// reference it. An `xmux` object on an inbound loads without error and has no effect,
        /// which is worse than omitting it, because the operator's concurrency then appears in
        /// the machine's config while taking effect only if it also reached the client. The
        /// ingress carries the value, and it travels in the subscription rather than here.
        ///
        /// The literal value xray expects, or `None` to write no `mode` key. Absent leaves the
        /// server accepting every upload shape; a value makes it a filter, which is why the
        /// default resolves to nothing here rather than to a named mode.
        mode: Option<&'static str>,
        /// Listener-side request/response padding.
        tuning: Option<XhttpTuning>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayInbound {
    Api {
        tag: String,
        listen: String,
        port: u16,
        address: String,
    },
    Vless {
        tag: String,
        listen: String,
        port: u16,
        security: XrayIngressSecurity,
        sniff: bool,
        stream: XrayStream,
    },
    Hysteria2 {
        tag: String,
        listen: String,
        port: u16,
        security: XrayIngressSecurity,
        sniff: bool,
        settings: Hysteria2,
    },
    AnyTls {
        tag: String,
        listen: String,
        port: u16,
        security: XrayIngressSecurity,
        sniff: bool,
        settings: AnyTls,
    },
    /// A security-only public front. dokodemo-door preserves the decrypted byte stream and sends
    /// it to the loopback XHTTP inbound where VLESS identity, routing and accounting live.
    Dokodemo {
        tag: String,
        listen: String,
        port: u16,
        target_port: u16,
        security: XrayIngressSecurity,
    },
    /// REALITY's fallback target, placed in front of the impersonated site.
    ///
    /// Its own variant rather than a `Dokodemo` with a different address, because the two differ
    /// in every significant field: this one terminates nothing, since the bytes crossing it are
    /// the client's own TLS and still encrypted; its target is an external machine rather than
    /// loopback; and it is the only inbound here that sniffs, because reading the requested name
    /// is its purpose.
    ///
    /// `route_only` on that sniffing is what keeps the target at `address:port` while routing
    /// still sees the name; without it the sniffed name would replace the target and the guard
    /// would faithfully deliver each stranger to whatever they asked for.
    RealityGuard {
        tag: String,
        listen: String,
        port: u16,
        /// The impersonated site, split from `dest`. The listener dials the same address
        /// REALITY previously dialed.
        target_address: String,
        target_port: u16,
    },
    Backbone {
        tag: String,
        listen: String,
        port: u16,
        security: XrayHopInboundWire,
        clients: Vec<XrayClient>,
    },
    /// A relay inbound speaking Shadowsocks 2022. Its own variant rather than a wire format
    /// inside `Backbone`, because none of the VLESS shape applies: the settings object is a
    /// method and a key, and there is no client list, because the key is the credential.
    BackboneShadowsocks {
        tag: String,
        listen: String,
        port: u16,
        method: &'static str,
        /// The port-wide key.
        password: String,
        /// The accounts under it. Each is one relay credential, and carrying them is what
        /// gives arriving traffic an identity: without it the routing rules select nothing
        /// and the usage counters have no name to hang off.
        users: Vec<XrayShadowsocksUser>,
    },
}

/// What the relay-port side needs for rendering. The strings are assembled here, and
/// `format/json.rs` only places them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayHopInboundWire {
    /// `decryption: "none"` plus `security: "none"`. Correct only where the hop runs
    /// over the overlay.
    None,
    /// `decryption` carries the key while `streamSettings.security` stays `none`,
    /// because VLESS Encryption is applied at the protocol layer rather than the
    /// transport layer.
    Encryption { decryption: String },
    /// The same `security: "reality"` as an ingress.
    ///
    /// Without `RealityClientPolicy`, meaning the minClientVer settings. Those exist to
    /// reject old clients and replays on the user side, whereas both ends of this hop
    /// run the same xray version this system distributes, so applying them would break
    /// relaying whenever upgrades are out of step.
    Reality {
        dest: String,
        server_names: Vec<String>,
        private_key: String,
        short_ids: Vec<String>,
    },
}

/// The dialer's side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayHopOutboundWire {
    None,
    Encryption {
        encryption: String,
    },
    Reality {
        server_name: String,
        public_key: String,
        short_id: String,
        fingerprint: String,
    },
}

/// The one Shadowsocks 2022 method offered.
///
/// blake3-aes-128-gcm rather than a configurable set. The 256-bit variant provides nothing a
/// relay hop needs, and chacha20, which is faster without AES hardware support, is the one
/// method xray's multi-user server rejects ("only blake3-aes-*-gcm methods are supported",
/// `infra/conf/shadowsocks.go`), so offering it would store a value that a later move to
/// per-dialer keys could not carry.
///
/// The name is xray's configuration syntax and stays here rather than in the model, the same
/// split `HopEncryption` makes for its handshake prefix.
const SS2022_METHOD: &str = "2022-blake3-aes-128-gcm";

// VLESS Encryption handshake parameters. `mlkem768x25519plus` is the suite name,
// `native` the mode that adds no further camouflage, `600s` the server ticket lifetime,
// and `0rtt` the client's permission to reuse 0-RTT.
//
// This combination is exactly what `xray vlessenc` emits by default, and the only one
// verified end to end (two real xrays passing traffic, plus a wrong public key that
// must fail). Verify the same way before changing it; do not change it from the docs.
const VLESS_ENCRYPTION_SUITE: &str = "mlkem768x25519plus.native";
const VLESS_ENCRYPTION_TICKET: &str = "600s";
const VLESS_ENCRYPTION_CLIENT_MODE: &str = "0rtt";

/// `dest` taken apart for a dokodemo-door, which wants the two halves in separate fields.
///
/// The 443 is unreachable in practice, because validation rejects a `reality.dest` that does not
/// parse as host:port, and it exists so an artifact built from an unvalidated snapshot still
/// names a port rather than panicking. An IPv6 literal loses its brackets, which exist to
/// separate the port from the address, and this field holds no port.
fn split_host_port(value: &str) -> (String, u16) {
    let value = value.trim();
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => match port.trim().parse::<u16>() {
            Ok(port) => (host.trim(), port),
            Err(_) => (value, 443),
        },
        None => (value, 443),
    };
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    (host.to_owned(), port)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayClient {
    pub id: String,
    pub email: String,
    pub level: u8,
    /// Set on the downstream's credential where this machine is the portal end of a
    /// reverse hop. xray registers a virtual outbound under this tag the moment that
    /// credential dials in, and business rules route to it by name.
    ///
    /// It is attached to the credential rather than to a config block because that is
    /// where xray reads it (`vless.Account.Reverse`), and it makes the previous
    /// domain-plus-user pairing unnecessary: holding the credential *is* the
    /// authorization, so no other client can take over the tunnel.
    pub reverse_tag: Option<String>,
}

// The variants differ widely in size (Vless/External carry a full outbound config, Blackhole
// just a bool), but this is a one-shot artifact built at compile time — never cloned on a hot
// path or packed into a large Vec, so its layout footprint is immaterial. Splitting the
// named-field variants into their own boxed structs to close the gap would only make this
// intermediate representation harder to read.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrayOutbound {
    Vless {
        tag: String,
        address: String,
        port: u16,
        uuid: String,
        security: XrayHopOutboundWire,
        /// Set where this outbound dials the upstream portal of a reverse hop. xray
        /// then treats the connection as inverted and exposes a virtual inbound under
        /// this tag, which is what tunnelled traffic arrives on.
        ///
        /// Its presence also changes how the outbound is rendered: xray accepts
        /// `reverse` only in the flat settings form and rejects it under `vnext`
        /// ("please use simplified outbound's config style to use reverse"), so
        /// `format/json.rs` selects the shape from this field. Verified against
        /// `infra/conf/vless.go` in v26.4.25.
        reverse_tag: Option<String>,
        mux: Option<XrayMux>,
    },
    /// A relay hop speaking Shadowsocks 2022. Its own variant rather than a field inside
    /// `Vless`, because none of VLESS's shape applies: no uuid, no encryption string, and no
    /// `reverse`. The last of those is why `ir/validate.rs` rejects the pairing rather than
    /// leaving it to appear as a tunnel that never establishes.
    Shadowsocks {
        tag: String,
        address: String,
        port: u16,
        method: &'static str,
        password: String,
        /// Carried here too: `mux` sits on xray's outbound object rather than inside the
        /// protocol settings, so which proxy speaks over the connection has no bearing on
        /// whether connections are pooled.
        mux: Option<XrayMux>,
    },
    External {
        tag: String,
        address: String,
        port: u16,
        protocol: ExternalOutboundProtocol,
        security: ExternalOutboundSecurity,
        wireguard_workers: u16,
    },
    Freedom {
        tag: String,
        send_through: Option<IpAddr>,
        /// xray's own form of `model::DomainStrategy`, already converted. The same split
        /// `XrayRouting::domain_strategy` uses: casing is xray's configuration syntax, so
        /// it is resolved here rather than in `format/json.rs`.
        domain_strategy: String,
    },
    Blackhole {
        tag: String,
        /// Ask xray to send its small built-in HTTP 403 before closing the connection.
        http_response: bool,
    },
}

/// Connection pooling on one outbound, already reduced to what xray takes.
///
/// `enabled` is not a field: an absent `XrayMux` writes no `mux` block, which is equivalent and
/// leaves one representation instead of two. `xudpConcurrency` and `xudpProxyUDP443` are absent
/// for a different reason: they govern UDP over TCP, and a relay hop carries none, because the
/// ingress runs XTLS Vision, which intercepts UDP/443 and moves browsers onto standard HTTPS, so
/// QUIC is gone before the first hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrayMux {
    /// Streams per connection. 1 pools without sharing; 2 and up share.
    pub concurrency: u16,
}

/// One account on a shadowsocks relay port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayShadowsocksUser {
    /// xray names its statistics after this, which is also what `usage.rs` matches relay
    /// traffic against. The same string a VLESS client carries as its email.
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayRouting {
    pub domain_strategy: String,
    pub rules: Vec<XrayRoutingRule>,
    /// The liveness balancer. It takes no part in routing, but some rule has to
    /// reference it, or xray does not instantiate it and the observatory does not
    /// start. The rule below therefore matches a domain that never appears.
    pub balancers: Vec<XrayBalancer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayBalancer {
    pub tag: String,
    pub selector: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrayRoutingRule {
    /// What names this rule inside the running xray, so it can be removed by name
    /// (`xray api rmrules`) without restarting the process.
    ///
    /// Assigned by `stamp_rule_tags`, never written by the code that builds a rule —
    /// the value depends on the whole table, which no single construction site knows.
    pub rule_tag: String,
    pub inbound_tags: Vec<String>,
    pub users: Vec<String>,
    pub condition: XrayMatchCondition,
    pub outbound_tag: String,
    /// Points at a balancer rather than a concrete outbound. Only the liveness rule
    /// uses it, and its purpose is to instantiate the balancer; see
    /// `XrayRouting::balancers`.
    pub balancer_tag: Option<String>,
}

/// Names every rule in the table, as `r:<generation>:<index>`.
///
/// # Why the generation, and why it covers the whole table
///
/// The names exist so a rule table can be swapped inside a running xray. The only swap
/// that leaves no gap is to append the entire new table, which the old rules still
/// precede so they continue to match, and then remove the entire old table by name, at
/// which point the new rules take effect. Measured against xray 26.4.25, that sequence
/// drops no connections, while `adrules` without `-append` replaces the table and
/// removes the balancers with it permanently, since a balancer cannot be recreated or
/// reused once its tag is gone.
///
/// Appending the new table on top of the old means both are present at once, and xray
/// rejects a duplicate `ruleTag` (`app/router: duplicate ruleTag`), so the two
/// generations must not share a name. A per-rule identity cannot guarantee that: a rule
/// that survives a compilation unchanged would keep its name and collide with itself.
/// Hashing the whole table does guarantee it, because a changed table changes every
/// name.
///
/// The same reasoning forbids the opposite convention. A name carrying a revision
/// counter would differ on every compilation whether or not anything changed, and the
/// artifact's sha256 is what decides that a machine needs a deployment at all: every
/// compilation would ship. Derived from the table's own content, an unchanged table
/// yields identical names, identical bytes, and no deployment.
///
/// # Why the index is in the name
///
/// It is not needed for uniqueness, because the generation already separates tables and
/// the digest covers the whole table. It exists so a table read back from a machine with
/// `xray api lsrules` can be aligned with the compiled one: that command returns names
/// and outbound tags but no match conditions, so without the position the listing cannot
/// be compared.
fn stamp_rule_tags(rules: &mut [XrayRoutingRule]) {
    // Lengths are hashed ahead of the elements they precede. Without them `["a", "bc"]`
    // and `["ab", "c"]` feed the hasher the same bytes, so two different tables would
    // produce the same generation, which is the collision this scheme has to avoid.
    let mut hasher = Sha256::new();
    hasher.update(b"brocade/xray-rule-generation/v1");
    hasher.update((rules.len() as u64).to_le_bytes());
    let field = |hasher: &mut Sha256, bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    for rule in rules.iter() {
        hasher.update((rule.inbound_tags.len() as u64).to_le_bytes());
        for tag in &rule.inbound_tags {
            field(&mut hasher, tag.as_bytes());
        }
        hasher.update((rule.users.len() as u64).to_le_bytes());
        for user in &rule.users {
            field(&mut hasher, user.as_bytes());
        }
        hasher.update((rule.condition.domain.len() as u64).to_le_bytes());
        for domain in &rule.condition.domain {
            field(&mut hasher, domain.as_bytes());
        }
        hasher.update((rule.condition.ip.len() as u64).to_le_bytes());
        for ip in &rule.condition.ip {
            field(&mut hasher, ip.as_bytes());
        }
        field(
            &mut hasher,
            rule.condition.port.as_deref().unwrap_or("").as_bytes(),
        );
        field(
            &mut hasher,
            rule.condition.network.as_deref().unwrap_or("").as_bytes(),
        );
        field(&mut hasher, rule.outbound_tag.as_bytes());
        field(
            &mut hasher,
            rule.balancer_tag.as_deref().unwrap_or("").as_bytes(),
        );
    }
    let digest = hasher.finalize();
    let generation = hex_lower(&digest[..4]);

    for (index, rule) in rules.iter_mut().enumerate() {
        rule.rule_tag = format!("r:{generation}:{index:03}");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XrayMatchCondition {
    pub domain: Vec<String>,
    pub ip: Vec<String>,
    pub port: Option<String>,
    pub network: Option<String>,
    /// What the sniffer decided the connection speaks. Empty on every rule that does not ask.
    pub protocol: Vec<String>,
}

pub fn build(plan: &NodePlan) -> XrayArtifact {
    let Some(xray) = &plan.xray else {
        return XrayArtifact::Disabled {
            node_id: plan.node_id.clone(),
        };
    };

    let mut inbounds = Vec::new();
    if let Some(api_port) = xray.api_port {
        inbounds.push(XrayInbound::Api {
            tag: API_TAG.to_owned(),
            listen: "127.0.0.1".to_owned(),
            port: api_port,
            address: "127.0.0.1".to_owned(),
        });
    }
    inbounds.extend(
        xray.inbounds
            .iter()
            .flat_map(|ingress| ingress_inbounds(ingress, &xray.reality_client)),
    );
    // Which downstream credential on which relay port is a portal end. The reverse tag
    // lives on the credential now, so it has to be joined onto the inbound's client
    // list rather than written as a block of its own.
    let portal_tags = xray
        .reverse_portals
        .iter()
        .map(|portal| {
            (
                (portal.inbound_tag.as_str(), portal.peer_label.as_str()),
                portal.tag.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    inbounds.extend(
        xray.hop_inbounds
            .iter()
            .map(|hop| hop_inbound(hop, &portal_tags)),
    );

    let mut outbounds = Vec::new();
    // The mirror of the above for the bridge end: which dialling outbound carries the
    // tunnel, and under which tag its virtual inbound appears.
    let bridge_tags = xray
        .reverse_bridges
        .iter()
        .map(|bridge| (bridge.dial_tag.as_str(), bridge.tag.as_str()))
        .collect::<BTreeMap<_, _>>();
    outbounds.extend(
        xray.forward_outbounds
            .iter()
            .map(|outbound| forward_outbound(outbound, &bridge_tags)),
    );
    outbounds.extend(
        xray.egress_outbounds
            .iter()
            .map(|outbound| egress_outbound(outbound, xray.domain_strategy)),
    );
    outbounds.extend(xray.external_outbounds.iter().map(external_outbound));
    if xray.block_outbound {
        outbounds.push(XrayOutbound::Blackhole {
            tag: BLOCK_OUTBOUND_TAG.to_owned(),
            http_response: false,
        });
    }
    let cover_tags = xray
        .inbounds
        .iter()
        .filter_map(|ingress| ingress.cover_port.map(|_| format!("{}:cover", ingress.tag)))
        .collect::<Vec<_>>();
    if !cover_tags.is_empty() {
        outbounds.push(XrayOutbound::Blackhole {
            tag: REALITY_COVER_OUTBOUND_TAG.to_owned(),
            http_response: true,
        });
    }
    // Each guarded ingress with the names it impersonates. Paired here rather than reduced to
    // tags, because the allow rule is per ingress: two ingresses on one machine may impersonate
    // different sites, and a single merged domain list would let either one's fallback reach the
    // other's site.
    let guards = xray
        .inbounds
        .iter()
        .filter(|ingress| ingress.guard_port.is_some())
        .filter_map(|ingress| match &ingress.security {
            IngressSecurity::Reality { params, .. } => Some((
                format!("{}:guard", ingress.tag),
                params.server_names.clone(),
            )),
            IngressSecurity::Tls => None,
        })
        .collect::<Vec<_>>();
    if !guards.is_empty() {
        outbounds.push(XrayOutbound::Blackhole {
            tag: REALITY_GUARD_OUTBOUND_TAG.to_owned(),
            http_response: false,
        });
    }
    // The internal direct outbound is emitted unconditionally. Without it, a relay
    // machine, whose `egress_outbounds` is empty, has no outbound for geodata to use,
    // and relay machines are where `geosite:` rules are most numerous.
    outbounds.push(XrayOutbound::Freedom {
        tag: INTERNAL_OUTBOUND_TAG.to_owned(),
        send_through: None,
        // Takes the machine's strategy like every other freedom outbound here. This one
        // carries no user traffic, but resolution is a property of the machine's egress
        // and this outbound is egress. A fixed value would make one machine resolve two
        // ways, which is an unrequested difference that would not be looked for.
        domain_strategy: domain_strategy_name(xray.domain_strategy).to_owned(),
    });

    let mut rules = Vec::new();
    if xray.api_port.is_some() {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: vec![API_TAG.to_owned()],
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: API_TAG.to_owned(),
            balancer_tag: None,
        });
    }
    if let Some(dns_route) = &xray.dns_route {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: vec![DNS_TAG.to_owned()],
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: dns_route.clone(),
            balancer_tag: None,
        });
    }
    for dns in &xray.egress_dns {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: vec![dns.tag.clone()],
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: dns.outbound_tag.clone(),
            balancer_tag: None,
        });
    }
    if !cover_tags.is_empty() {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: cover_tags,
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: REALITY_COVER_OUTBOUND_TAG.to_owned(),
            balancer_tag: None,
        });
    }
    // One pass letting each guarded ingress reach the names it impersonates, then one catching
    // everything else those listeners saw. The order is the mechanism: reversed, every fallback
    // is dropped, including the handshakes REALITY needs the impersonated site to answer.
    //
    // `full:` rather than the bare name the upstream example uses. Bare is xray's subdomain
    // match, which would also admit `anything.impersonated.example`, and on shared
    // infrastructure those neighbours are what this rule excludes.
    for (tag, server_names) in &guards {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: vec![tag.clone()],
            users: Vec::new(),
            condition: XrayMatchCondition {
                domain: server_names
                    .iter()
                    .map(|name| format!("full:{name}"))
                    .collect(),
                ..XrayMatchCondition::default()
            },
            outbound_tag: INTERNAL_OUTBOUND_TAG.to_owned(),
            balancer_tag: None,
        });
    }
    if !guards.is_empty() {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: guards.iter().map(|(tag, _)| tag.clone()).collect(),
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: REALITY_GUARD_OUTBOUND_TAG.to_owned(),
            balancer_tag: None,
        });
    }
    let split_fronts = xray
        .inbounds
        .iter()
        .filter_map(|ingress| ingress.split.as_ref().map(|split| (ingress, split)))
        .flat_map(|(ingress, split)| {
            std::iter::once(format!("{}:upload", ingress.tag)).chain(
                split
                    .download_ports
                    .iter()
                    .map(|port| format!("{}:download:{port}", ingress.tag)),
            )
        })
        .collect::<Vec<_>>();
    if !split_fronts.is_empty() {
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: split_fronts,
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: INTERNAL_OUTBOUND_TAG.to_owned(),
            balancer_tag: None,
        });
    }
    // The reverse tunnel previously required two setup rules ahead of everything else,
    // to recognize the control connection between bridge and portal by an agreed
    // synthetic domain. It requires none now: the tunnel is negotiated inside VLESS, so
    // there is no control connection travelling as ordinary traffic for the table to
    // match. That also removes the ordering hazard, where an `any → Egress` fallback
    // placed above would consume the setup request and leave both sides appearing
    // correct while no tunnel was established.

    rules.extend(xray.routing_rules.iter().map(|rule| {
        let (inbound_tags, users) = match &rule.selector {
            XrayRuleSelector::InboundTags(tags) => (tags.clone(), Vec::new()),
            XrayRuleSelector::Users(users) => (Vec::new(), users.clone()),
        };
        XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags,
            users,
            condition: match_condition(&rule.dest_match),
            outbound_tag: rule.outbound_tag.clone(),
            balancer_tag: None,
        }
    }));

    // Liveness applies only to hops that exist. A machine with no forwarding outbound,
    // such as a pure exit or a pure ingress, emits no observatory, because a synthetic
    // probe target would measure nothing.
    let hop_health = (!xray.forward_outbounds.is_empty()).then(|| XrayHopHealth {
        subjects: xray
            .forward_outbounds
            .iter()
            .map(|outbound| outbound.tag.clone())
            .collect(),
        probe_url: HOP_HEALTH_PROBE_URL.to_owned(),
        probe_interval_secs: HOP_HEALTH_PROBE_INTERVAL_SECS,
        balancer_tag: HOP_HEALTH_BALANCER_TAG.to_owned(),
    });
    if hop_health.is_some() {
        // Its only effect is that the balancer gets referenced and therefore
        // instantiated. It matches a domain that never appears, so it has no effect
        // whatever on real traffic.
        rules.push(XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: Vec::new(),
            users: Vec::new(),
            condition: XrayMatchCondition {
                domain: vec![NEVER_MATCH_DOMAIN.to_owned()],
                ..XrayMatchCondition::default()
            },
            outbound_tag: String::new(),
            balancer_tag: Some(HOP_HEALTH_BALANCER_TAG.to_owned()),
        });
    }

    // After the table is final and in its final order: the name carries the position,
    // and the generation is a digest of every rule in it.
    stamp_rule_tags(&mut rules);

    XrayArtifact::Config(XrayConfig {
        log_level: "warning".to_owned(),
        api: XrayApi {
            tag: API_TAG.to_owned(),
            services: vec![
                "HandlerService".to_owned(),
                "StatsService".to_owned(),
                // Reaching the rule table of a running xray. Enabled on every machine
                // rather than only where it is currently used: the service is what
                // allows a routing change to be applied without dropping every
                // connection, and a machine that enrolled before the caller existed
                // would otherwise need one restart to become able to avoid restarts.
                "RoutingService".to_owned(),
            ],
        },
        policy: XrayPolicy {
            conn_idle_secs: xray.connection.conn_idle_secs,
            handshake_secs: xray.connection.handshake_secs,
            uplink_only_secs: xray.connection.uplink_only_secs,
            downlink_only_secs: xray.connection.downlink_only_secs,
            buffer_size_kb: xray.connection.buffer_size_kb,
            stats_user_online: xray.connection.stats_user_online,
        },
        dns: dns_config(&xray.dns, xray.dns_route.as_deref(), &xray.egress_dns),
        inbounds,
        outbounds,
        routing: XrayRouting {
            domain_strategy: "AsIs".to_owned(),
            balancers: hop_health
                .as_ref()
                .map(|health| {
                    vec![XrayBalancer {
                        tag: health.balancer_tag.clone(),
                        selector: health.subjects.clone(),
                    }]
                })
                .unwrap_or_default(),
            rules,
        },
        hop_health,
        geodata: geodata_config(&xray.geodata),
    })
}

/// The two assets are emitted with geoip first. Artifacts have to be byte-for-byte
/// stable, and with no natural sort key, a fixed order is the only option.
fn geodata_config(settings: &GeodataSettings) -> XrayGeodata {
    XrayGeodata {
        cron: settings.cron.clone(),
        outbound: INTERNAL_OUTBOUND_TAG.to_owned(),
        assets: vec![
            XrayGeodataAsset {
                url: settings.geoip_url.clone(),
                file: "geoip.dat".to_owned(),
            },
            XrayGeodataAsset {
                url: settings.geosite_url.clone(),
                file: "geosite.dat".to_owned(),
            },
        ],
    }
}

fn ingress_security(
    ingress: &XrayIngressPlan,
    policy: &RealityClientPolicy,
) -> XrayIngressSecurity {
    match &ingress.security {
        IngressSecurity::Reality {
            params,
            private_key,
            short_ids,
        } => XrayIngressSecurity::Reality {
            // Both loopback ports are the same substitution from different ends: the cover
            // replaces the impersonated site, and the guard is placed in front of it. They are
            // mutually exclusive by construction (`RealitySettings::guards_fallback`), so their
            // order here has no effect.
            dest: ingress
                .cover_port
                .or(ingress.guard_port)
                .map(|port| format!("127.0.0.1:{port}"))
                .unwrap_or_else(|| normalize_host_port(&params.dest)),
            server_names: if ingress.cover_port.is_some() {
                vec![ingress.certificate_name.clone().unwrap_or_default()]
            } else {
                params.server_names.clone()
            },
            private_key: private_key.clone(),
            short_ids: short_ids.clone(),
            min_client_ver: policy.min_client_ver.clone(),
            max_client_ver: policy.max_client_ver.clone(),
            max_time_diff_ms: policy.max_time_diff_ms,
            fallback_limits: crate::physical::node::reality_fallback_limits(
                &params.fallback_limits,
                &ingress.tag,
            ),
        },
        IngressSecurity::Tls => XrayIngressSecurity::Tls {
            certificate_file: NODE_CERTIFICATE_FILE.to_owned(),
            key_file: NODE_CERTIFICATE_KEY_FILE.to_owned(),
            // v26.4.25 does not add h3 on the server side. Without this the client offers only
            // h3 while the server offers h2/http1 and every handshake fails with no server log.
            alpn: matches!(ingress.protocol, IngressProtocol::Hysteria2(_))
                .then(|| vec!["h3".to_owned()]),
        },
    }
}

fn ingress_inbounds(ingress: &XrayIngressPlan, policy: &RealityClientPolicy) -> Vec<XrayInbound> {
    let mut inbounds =
        match &ingress.split {
            None => vec![ingress_inbound(ingress, policy)],
            Some(split) => {
                let xhttp = ingress
                    .xhttp
                    .as_ref()
                    .expect("分离接入面必须有 XHTTP")
                    .clone();
                let mut split_inbounds = vec![
                    XrayInbound::Dokodemo {
                        tag: format!("{}:upload", ingress.tag),
                        listen: ingress.listen.to_string(),
                        port: ingress.port,
                        target_port: split.core_port,
                        security: ingress_security(ingress, policy),
                    },
                    XrayInbound::Vless {
                        tag: ingress.tag.clone(),
                        listen: "127.0.0.1".to_owned(),
                        port: split.core_port,
                        security: XrayIngressSecurity::None,
                        sniff: ingress.sniff,
                        stream: XrayStream::Xhttp {
                            path: xhttp.path,
                            mode: xhttp.mode.as_str(),
                            tuning: xhttp.tuning,
                        },
                    },
                ];
                split_inbounds.extend(split.download_ports.iter().map(|port| {
                    XrayInbound::Dokodemo {
                        tag: format!("{}:download:{port}", ingress.tag),
                        listen: ingress.listen.to_string(),
                        port: *port,
                        target_port: split.core_port,
                        security: XrayIngressSecurity::Tls {
                            certificate_file: NODE_CERTIFICATE_FILE.to_owned(),
                            key_file: NODE_CERTIFICATE_KEY_FILE.to_owned(),
                            alpn: None,
                        },
                    }
                }));
                split_inbounds
            }
        };
    if let Some(port) = ingress.guard_port {
        // Only reachable from the REALITY security block above, whose dest was pointed here.
        let dest = match &ingress.security {
            IngressSecurity::Reality { params, .. } => params.dest.as_str(),
            IngressSecurity::Tls => "",
        };
        let (target_address, target_port) = split_host_port(dest);
        inbounds.push(XrayInbound::RealityGuard {
            tag: format!("{}:guard", ingress.tag),
            listen: "127.0.0.1".to_owned(),
            port,
            target_address,
            target_port,
        });
    }
    if let Some(port) = ingress.cover_port {
        inbounds.push(XrayInbound::Dokodemo {
            tag: format!("{}:cover", ingress.tag),
            listen: "127.0.0.1".to_owned(),
            port,
            // The dedicated routing rule consumes this inbound before freedom can use the
            // placeholder target. No external address is present anywhere in the cover path.
            target_port: 1,
            security: XrayIngressSecurity::Tls {
                certificate_file: NODE_CERTIFICATE_FILE.to_owned(),
                key_file: NODE_CERTIFICATE_KEY_FILE.to_owned(),
                alpn: Some(vec!["http/1.1".to_owned()]),
            },
        });
    }
    inbounds
}

fn ingress_inbound(ingress: &XrayIngressPlan, policy: &RealityClientPolicy) -> XrayInbound {
    match &ingress.protocol {
        IngressProtocol::Vless => XrayInbound::Vless {
            tag: ingress.tag.clone(),
            listen: ingress.listen.to_string(),
            port: ingress.port,
            security: ingress_security(ingress, policy),
            sniff: ingress.sniff,
            stream: match &ingress.xhttp {
                None => XrayStream::Tcp,
                Some(xhttp) => XrayStream::Xhttp {
                    path: xhttp.path.clone(),
                    mode: xhttp.mode.as_str(),
                    tuning: xhttp.tuning.clone(),
                },
            },
        },
        IngressProtocol::Hysteria2(settings) => XrayInbound::Hysteria2 {
            tag: ingress.tag.clone(),
            listen: ingress.listen.to_string(),
            port: ingress.port,
            security: ingress_security(ingress, policy),
            sniff: ingress.sniff,
            settings: settings.clone(),
        },
        IngressProtocol::AnyTls(settings) => XrayInbound::AnyTls {
            tag: ingress.tag.clone(),
            listen: ingress.listen.to_string(),
            port: ingress.port,
            security: ingress_security(ingress, policy),
            sniff: ingress.sniff,
            settings: settings.clone(),
        },
    }
}

fn hop_inbound(
    hop: &XrayHopInboundPlan,
    portal_tags: &BTreeMap<(&str, &str), &str>,
) -> XrayInbound {
    // One match producing a whole inbound, rather than a helper mapping the wire format and a
    // branch above it selecting the protocol. Shadowsocks has no VLESS security layer to return,
    // so a helper would have to return a not-applicable value for it and the caller would have
    // to rely on the case it already handled being unreachable here.
    let clients = || {
        hop.clients
            .iter()
            .map(|plan| {
                client(
                    plan,
                    portal_tags
                        .get(&(hop.tag.as_str(), plan.label.as_str()))
                        .map(|tag| (*tag).to_owned()),
                )
            })
            .collect()
    };
    let backbone = |security| XrayInbound::Backbone {
        tag: hop.tag.clone(),
        listen: hop.listen.to_string(),
        port: hop.port,
        security,
        clients: clients(),
    };

    match &hop.security {
        HopWire::None => backbone(XrayHopInboundWire::None),
        HopWire::Encryption(encryption) => backbone(XrayHopInboundWire::Encryption {
            decryption: format!(
                "{VLESS_ENCRYPTION_SUITE}.{VLESS_ENCRYPTION_TICKET}.{}",
                encryption.private_key
            ),
        }),
        HopWire::Reality(reality) => backbone(XrayHopInboundWire::Reality {
            dest: normalize_host_port(&reality.dest),
            server_names: reality.server_names.clone(),
            private_key: reality.private_key.clone(),
            short_ids: reality.short_ids.clone(),
        }),
        // The key is the credential here, so the client list has nothing to carry. A forward
        // relay port's clients all presented one shared credential to begin with; a reverse
        // hop's extra entries are the case `ir/validate.rs` refuses.
        HopWire::Shadowsocks2022 {
            server_psk,
            user_psk,
        } => XrayInbound::BackboneShadowsocks {
            tag: hop.tag.clone(),
            listen: hop.listen.to_string(),
            port: hop.port,
            method: SS2022_METHOD,
            password: server_psk.clone(),
            // One entry, because only a reverse hop would add a second and those are refused
            // for this wire format. A second entry would share this key, which removes the
            // purpose of having accounts, so the refusal in `ir/validate.rs` is required here
            // rather than a convenience.
            users: hop
                .clients
                .iter()
                .map(|client| XrayShadowsocksUser {
                    email: client.label.clone(),
                    password: user_psk.clone(),
                })
                .collect(),
        },
    }
}

fn client(client: &XrayClientPlan, reverse_tag: Option<String>) -> XrayClient {
    XrayClient {
        id: client.uuid.clone(),
        email: client.label.clone(),
        level: 0,
        reverse_tag,
    }
}

fn forward_outbound(
    outbound: &XrayForwardOutboundPlan,
    bridge_tags: &BTreeMap<&str, &str>,
) -> XrayOutbound {
    let mux = mux_of(outbound.pool);
    // Same shape as `hop_inbound`, for the same reason.
    let vless = |security| XrayOutbound::Vless {
        tag: outbound.tag.clone(),
        address: outbound.address.clone(),
        port: outbound.port,
        uuid: outbound.uuid.clone(),
        security,
        reverse_tag: bridge_tags
            .get(outbound.tag.as_str())
            .map(|tag| (*tag).to_owned()),
        mux,
    };

    match &outbound.security {
        HopDialWire::None => vless(XrayHopOutboundWire::None),
        HopDialWire::Encryption { public_key } => vless(XrayHopOutboundWire::Encryption {
            encryption: format!(
                "{VLESS_ENCRYPTION_SUITE}.{VLESS_ENCRYPTION_CLIENT_MODE}.{public_key}"
            ),
        }),
        HopDialWire::Reality {
            public_key,
            server_name,
            short_id,
            fingerprint,
        } => vless(XrayHopOutboundWire::Reality {
            server_name: server_name.clone(),
            public_key: public_key.clone(),
            short_id: short_id.clone(),
            fingerprint: fingerprint.clone(),
        }),
        // No uuid and no `reverse`: the dialling end of a shadowsocks hop is the key and the
        // address, and a hop that needed a reverse tunnel could not have been this variant.
        HopDialWire::Shadowsocks2022 {
            server_psk,
            user_psk,
        } => XrayOutbound::Shadowsocks {
            tag: outbound.tag.clone(),
            address: outbound.address.clone(),
            port: outbound.port,
            method: SS2022_METHOD,
            // Colon-joined, which is the protocol's own way of saying "this account under
            // that port". Verified against sing-shadowsocks, which splits on it and decodes
            // each half as its own key.
            password: format!("{server_psk}:{user_psk}"),
            mux,
        },
    }
}

/// `HopPool` reduced to what the artifact carries.
///
/// `Pool` is `concurrency: 1` rather than a separate mechanism: xray reuses an idle worker
/// before creating one, and at a limit of one stream per worker a finished stream leaves its
/// connection available for the next. Measured on 26.4.25: 20 sequential streams used one
/// connection, and 8 concurrent streams used eight. The worker is not probed before reuse, so
/// this mapping is retained for authored-model compatibility but is no longer the console's
/// default; silently changing an existing `Pool` to no mux or concurrency 2 would change its
/// requested semantics.
fn mux_of(pool: HopPool) -> Option<XrayMux> {
    match pool {
        HopPool::None => None,
        HopPool::Pool => Some(XrayMux { concurrency: 1 }),
        HopPool::Merge(concurrency) => Some(XrayMux { concurrency }),
    }
}

fn egress_outbound(outbound: &XrayEgressOutboundPlan, strategy: DomainStrategy) -> XrayOutbound {
    XrayOutbound::Freedom {
        tag: outbound.tag.clone(),
        send_through: outbound.send_through,
        domain_strategy: domain_strategy_name(outbound.domain_strategy.unwrap_or(strategy))
            .to_owned(),
    }
}

fn external_outbound(outbound: &XrayExternalOutboundPlan) -> XrayOutbound {
    XrayOutbound::External {
        tag: outbound.tag.clone(),
        address: outbound.address.clone(),
        port: outbound.port,
        protocol: outbound.protocol.clone(),
        security: outbound.security.clone(),
        wireguard_workers: outbound.wireguard_workers,
    }
}

/// xray's form of the strategy.
///
/// `infra/conf/freedom.go` lowercases before matching, so the casing has no effect; it is
/// written the way xray's own documentation writes it, so that anyone holding the two side
/// by side reads the same token. Verified against v26.4.25.
fn domain_strategy_name(strategy: DomainStrategy) -> &'static str {
    match strategy {
        DomainStrategy::UseIp => "UseIP",
        DomainStrategy::UseIpv4 => "UseIPv4",
        DomainStrategy::UseIpv6 => "UseIPv6",
        DomainStrategy::UseIpv4v6 => "UseIPv4v6",
        DomainStrategy::UseIpv6v4 => "UseIPv6v4",
        DomainStrategy::AsIs => "AsIs",
    }
}

fn dns_config(dns: &Dns, dns_route: Option<&str>, scoped: &[XrayEgressDnsPlan]) -> XrayDns {
    let mut servers = scoped
        .iter()
        .map(|server| XrayDnsServer::Scoped {
            address: dns_server_address(server),
            port: server.port,
            domains: server.domains.clone(),
            query_strategy: dns_query_strategy_name(server.address_strategy).to_owned(),
            tag: server.tag.clone(),
            final_query: matches!(server.fallback, crate::model::EgressDnsFallback::Stop),
        })
        .collect::<Vec<_>>();
    servers.extend(match dns {
        Dns::System => vec![XrayDnsServer::Address("localhost".to_owned())],
        Dns::Servers(servers) => servers
            .iter()
            .cloned()
            .map(XrayDnsServer::Address)
            .collect(),
    });
    XrayDns {
        tag: dns_route.map(|_| DNS_TAG.to_owned()),
        servers,
    }
}

fn dns_server_address(server: &XrayEgressDnsPlan) -> String {
    let address = server
        .address
        .trim()
        .parse::<IpAddr>()
        .map(|address| match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => address.to_string(),
        })
        .unwrap_or_else(|_| server.address.trim().to_owned());
    match server.transport {
        EgressDnsTransport::Udp => address,
        EgressDnsTransport::Tcp => match address.parse::<IpAddr>() {
            Ok(IpAddr::V6(address)) => format!("tcp://[{address}]"),
            _ => format!("tcp://{address}"),
        },
    }
}

fn dns_query_strategy_name(strategy: EgressDnsAddressStrategy) -> &'static str {
    match strategy {
        EgressDnsAddressStrategy::UseIp
        | EgressDnsAddressStrategy::UseIpv4v6
        | EgressDnsAddressStrategy::UseIpv6v4 => "UseIP",
        EgressDnsAddressStrategy::UseIpv4 => "UseIPv4",
        EgressDnsAddressStrategy::UseIpv6 => "UseIPv6",
    }
}

fn match_condition(dest_match: &DestMatch) -> XrayMatchCondition {
    let mut condition = XrayMatchCondition::default();
    if !is_representable_match(dest_match) {
        put_never(&mut condition);
        return condition;
    }
    put_match(dest_match, &mut condition);
    condition
}

fn put_match(dest_match: &DestMatch, condition: &mut XrayMatchCondition) {
    match dest_match {
        DestMatch::Any => {}
        DestMatch::DomainSuffix(values) => put_domains(
            values.iter().map(|value| format!("domain:{value}")),
            condition,
        ),
        DestMatch::DomainKeyword(values) => put_domains(values.iter().cloned(), condition),
        DestMatch::DomainRegex(value) => put_domains([format!("regexp:{value}")], condition),
        DestMatch::Geosite(values) => put_domains(
            values.iter().map(|value| format!("geosite:{value}")),
            condition,
        ),
        DestMatch::IpCidr(values) => put_ips(values.iter().cloned(), condition),
        DestMatch::Geoip(values) => put_ips(
            values.iter().map(|value| format!("geoip:{value}")),
            condition,
        ),
        DestMatch::Port(values) => condition.port = Some(values.join(",")),
        // The complement, written out. xray has no negation in `port`, and two ranges around the
        // kept number is the only form it accepts. 1-0 and 65536-65535 are both empty ranges,
        // which is what an empty side has to compile to when the kept port sits at an end.
        DestMatch::PortExcept(values) => {
            let mut kept = values.clone();
            kept.sort_unstable();
            kept.dedup();
            let mut spans = Vec::new();
            let mut low: u32 = 1;
            for port in kept {
                let port = u32::from(port);
                if port > low {
                    spans.push(format!("{}-{}", low, port - 1));
                }
                low = port + 1;
            }
            if low <= 65535 {
                spans.push(format!("{low}-65535"));
            }
            if spans.is_empty() {
                put_never(condition);
            } else {
                condition.port = Some(spans.join(","));
            }
        }
        DestMatch::Protocol(values) => {
            let before = condition.protocol.len();
            condition
                .protocol
                .extend(values.iter().filter(|v| !v.trim().is_empty()).cloned());
            if condition.protocol.len() == before {
                put_never(condition);
            }
        }
        DestMatch::Network(Network::Tcp) => condition.network = Some("tcp".to_owned()),
        DestMatch::Network(Network::Udp) => condition.network = Some("udp".to_owned()),
        DestMatch::All(values) => {
            if values.is_empty() {
                put_never(condition);
            }
            for value in values {
                put_match(value, condition);
            }
        }
        DestMatch::FrontDownstream => put_never(condition),
    }
}

fn is_representable_match(dest_match: &DestMatch) -> bool {
    let DestMatch::All(values) = dest_match else {
        return true;
    };
    if values.is_empty() {
        return false;
    }

    let mut slots = MatchSlots::default();
    values
        .iter()
        .all(|value| collect_match_slots(value, &mut slots))
}

fn collect_match_slots(dest_match: &DestMatch, slots: &mut MatchSlots) -> bool {
    match dest_match {
        DestMatch::Any => true,
        DestMatch::DomainSuffix(_)
        | DestMatch::DomainKeyword(_)
        | DestMatch::DomainRegex(_)
        | DestMatch::Geosite(_)
        | DestMatch::FrontDownstream => slots.put(MatchSlot::Domain),
        DestMatch::IpCidr(_) | DestMatch::Geoip(_) => slots.put(MatchSlot::Ip),
        DestMatch::Port(_) | DestMatch::PortExcept(_) => slots.put(MatchSlot::Port),
        DestMatch::Network(_) => slots.put(MatchSlot::Network),
        DestMatch::Protocol(_) => slots.put(MatchSlot::Protocol),
        DestMatch::All(values) => {
            is_representable_match(dest_match)
                && values.iter().all(|value| collect_match_slots(value, slots))
        }
    }
}

#[derive(Debug, Default)]
struct MatchSlots {
    domain: bool,
    ip: bool,
    port: bool,
    network: bool,
    protocol: bool,
}

impl MatchSlots {
    fn put(&mut self, slot: MatchSlot) -> bool {
        let occupied = match slot {
            MatchSlot::Domain => &mut self.domain,
            MatchSlot::Ip => &mut self.ip,
            MatchSlot::Port => &mut self.port,
            MatchSlot::Network => &mut self.network,
            MatchSlot::Protocol => &mut self.protocol,
        };
        if *occupied {
            return false;
        }
        *occupied = true;
        true
    }
}

#[derive(Debug, Clone, Copy)]
enum MatchSlot {
    Domain,
    Ip,
    Port,
    Network,
    Protocol,
}

fn put_domains<I>(values: I, condition: &mut XrayMatchCondition)
where
    I: IntoIterator<Item = String>,
{
    let before = condition.domain.len();
    condition
        .domain
        .extend(values.into_iter().filter(|value| !value.trim().is_empty()));
    if condition.domain.len() == before {
        put_never(condition);
    }
}

fn put_ips<I>(values: I, condition: &mut XrayMatchCondition)
where
    I: IntoIterator<Item = String>,
{
    let before = condition.ip.len();
    condition
        .ip
        .extend(values.into_iter().filter(|value| !value.trim().is_empty()));
    if condition.ip.len() == before {
        put_never(condition);
    }
}

fn put_never(condition: &mut XrayMatchCondition) {
    condition.domain.push(NEVER_MATCH_DOMAIN.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(inbound: &str, outbound: &str) -> XrayRoutingRule {
        XrayRoutingRule {
            rule_tag: String::new(),
            inbound_tags: vec![inbound.to_owned()],
            users: Vec::new(),
            condition: XrayMatchCondition::default(),
            outbound_tag: outbound.to_owned(),
            balancer_tag: None,
        }
    }

    fn tags(mut rules: Vec<XrayRoutingRule>) -> Vec<String> {
        stamp_rule_tags(&mut rules);
        rules.into_iter().map(|rule| rule.rule_tag).collect()
    }

    /// The property the artifact's sha256 rests on. A table that did not change must
    /// name itself identically, or every compilation would produce different bytes and
    /// every machine would be handed a deployment for a config it already runs.
    #[test]
    fn an_unchanged_table_names_itself_identically() {
        let table = || vec![rule("api", "api"), rule("in:a", "out:egress")];
        assert_eq!(tags(table()), tags(table()));
        // The digest itself is not pinned: which bytes go into it is an implementation
        // detail, and changing it is caught by the golden artifacts. What must hold here
        // is that two runs agree.
    }

    /// The property the swap depends on. Appending the new table while the old one is
    /// still installed puts both in the running xray at once, and xray rejects a
    /// duplicate `ruleTag`, so one name shared between the generations fails the whole
    /// append, which is atomic, and the machine keeps the old routing with no error.
    #[test]
    fn a_changed_table_shares_no_name_with_the_old_one() {
        let before = tags(vec![rule("api", "api"), rule("in:a", "out:egress")]);
        // Only the second rule moved, and only in its outbound.
        let after = tags(vec![rule("api", "api"), rule("in:a", "out:block")]);
        for name in &after {
            assert!(
                !before.contains(name),
                "{name} appears in both generations; appending the new table would be \
                 rejected as a duplicate ruleTag and the swap would leave the old rules in place",
            );
        }
    }

    /// Adding or removing a rule also has to change the generation: a table one rule
    /// longer is a different table even when every retained rule is unchanged.
    #[test]
    fn the_generation_covers_the_table_length() {
        let short = tags(vec![rule("api", "api")]);
        let long = tags(vec![rule("api", "api"), rule("in:a", "out:egress")]);
        assert_ne!(short[0], long[0]);
    }

    /// Why the digest carries a length ahead of every string. Without them these two
    /// tables feed the hasher the same bytes, produce the same generation, and collide
    /// on every name while routing differently.
    #[test]
    fn adjacent_strings_do_not_run_together_in_the_digest() {
        let mut left = rule("a", "out:egress");
        left.inbound_tags = vec!["a".to_owned(), "bc".to_owned()];
        let mut right = rule("a", "out:egress");
        right.inbound_tags = vec!["ab".to_owned(), "c".to_owned()];
        assert_ne!(tags(vec![left]), tags(vec![right]));
    }

    /// The index is the rule's position, which is what makes a table read back with
    /// `xray api lsrules`, returning names and outbound tags but no match conditions,
    /// comparable with the compiled one.
    #[test]
    fn the_name_carries_the_position() {
        let names = tags(vec![
            rule("api", "api"),
            rule("in:a", "out:egress"),
            rule("in:b", "out:block"),
        ]);
        let suffixes = names
            .iter()
            .map(|name| name.rsplit(':').next().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(suffixes, vec!["000", "001", "002"]);
        // One generation across the whole table rather than one per rule.
        let generations = names
            .iter()
            .map(|name| name.split(':').nth(1).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(generations.len(), 1);
    }
}
