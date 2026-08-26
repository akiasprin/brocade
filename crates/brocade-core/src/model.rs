//! The domain model an operator edits. Fields may be left blank and only the exceptions
//! need to be written; defaulting and validation happen at the IR layer.
//!
//! These types carry serde directly, and `ModelSnapshot`'s JSON form is the model's only
//! wire format. What store materializes from the database, what the console sends back
//! after an edit, and what wasm compiles in the browser are the same representation. The
//! browser defines no mirror DTO of its own, because two independent definitions of one
//! structure diverge over time.
//!
//! The notation follows the model documentation (`{t, v}`): an enum carries a `t` tag
//! with its value in `v`, and a rule's two halves are `m` and `a`.
//!
//! `deny_unknown_fields` is deliberate. A mistyped field name has to be rejected at parse
//! time rather than silently take a default: `egress` spelled `egres` means "this machine
//! may not exit", the artifacts are produced as usual, and nothing in the output
//! indicates the mistake.

use std::net::{IpAddr, Ipv4Addr};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSnapshot {
    pub revision: u64,
    pub overlay_cidr: Ipv4Net,
    #[serde(default)]
    pub settings: ModelSettings,
    pub nodes: Vec<Node>,
    pub users: Vec<User>,
    /// Project-scoped proxy servers which a rule may select as its terminal outbound.
    ///
    /// They live beside nodes and users rather than inside `AppView`: both are resources used by
    /// a project, while the app view itself remains the routing document. `app` supplies the join
    /// without copying the same credential into every rule which uses it.
    #[serde(default)]
    pub external_outbounds: Vec<ExternalOutbound>,
    pub apps: Vec<AppView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    #[serde(default)]
    pub reality_client: RealityClientPolicy,
    /// The external site REALITY impersonates. The operator sets it once in the global
    /// settings, and an ingress that specifies none inherits it. Inheritance is resolved
    /// while the snapshot is generated, so by the IR every ingress carries a concrete
    /// site.
    #[serde(default)]
    pub reality_site: RealitySite,
    #[serde(default)]
    pub overlay: OverlaySettings,
    #[serde(default)]
    pub ports: PortSettings,
    #[serde(default)]
    pub probe: ProbeSettings,
    #[serde(default)]
    pub geodata: GeodataSettings,
    /// Defaults for connection lifetime and per-connection memory. These are defaults
    /// only: the value that reaches a machine is `Node.connection` falling back to this,
    /// the same arrangement as `overlay.mtu` and `Node.mtu`.
    #[serde(default)]
    pub connection: ConnectionSettings,
    /// Count, per account, how many distinct source addresses are using it at a given
    /// time.
    ///
    /// Global rather than per machine. The split from `connection` above is deliberate
    /// even though xray writes both into the same `policy` block, because the two
    /// settings answer different questions. Per-connection memory is a capacity decision
    /// that depends on the machine, so each machine sets it independently. Whether shared
    /// accounts are tracked is a fleet-wide decision with a single value.
    ///
    /// This setting only counts. xray does not refuse a connection at any number, so
    /// acting on the count is the control plane's responsibility and outside the scope of
    /// this field.
    #[serde(default)]
    pub stats_user_online: bool,
}

/// Connection lifetime and per-connection memory.
///
/// Every field except `handshake_secs` is a per-machine default: memory pressure depends
/// on the machine, and a fleet containing both 2-core relays and larger exits has no
/// single correct value. `handshake_secs` is fleet-wide; see its own note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSettings {
    /// Seconds without data before a connection is reclaimed. xray's own default.
    ///
    /// This setting determines a relay's memory usage: idle connections accumulate, and
    /// each one holds a buffer of `buffer_size_kb`.
    pub conn_idle_secs: u32,
    /// Seconds a half-closed connection waits before release. Platform defaults are 2 and 5.
    pub uplink_only_secs: u32,
    pub downlink_only_secs: u32,
    /// Per-connection buffer. `None` omits the key from the artifact, which is not
    /// equivalent to writing a number.
    ///
    /// With the key omitted, xray selects by CPU architecture: 512 KB on x86_64, 4 KB on
    /// arm64 (its comment reads "4k cache for low-end devices"). The 128-fold difference
    /// is intended, because the machines that receive 4 KB are the ones without the
    /// memory for 512. A single number written here applies to both architectures, so the
    /// default stays `None` and any value is an explicit operator choice.
    pub buffer_size_kb: Option<u32>,
    /// Seconds to complete a handshake.
    ///
    /// Fleet-wide, and not changed without a specific reason. xray sets 60 to match
    /// nginx's `client_header_timeout`, with the comment "So that this value will not
    /// indicate server identity": the number is chosen to be unremarkable rather than
    /// tuned. Any other value is a measurable difference from the server being
    /// impersonated, which is why the field is not per-machine — varying it across the
    /// fleet would make each machine separately distinguishable.
    pub handshake_secs: u32,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            conn_idle_secs: 300,
            uplink_only_secs: 2,
            downlink_only_secs: 5,
            buffer_size_kb: None,
            handshake_secs: 60,
        }
    }
}

/// One machine's overrides. Every field absent means "use the global default".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConnection {
    #[serde(default)]
    pub conn_idle_secs: Option<u32>,
    #[serde(default)]
    pub uplink_only_secs: Option<u32>,
    #[serde(default)]
    pub downlink_only_secs: Option<u32>,
    /// Absent here falls back to `ConnectionSettings::buffer_size_kb`, which may itself
    /// be absent; in that case no key is written and xray selects by architecture. Both
    /// levels of "not set" mean the same thing: write nothing.
    #[serde(default)]
    pub buffer_size_kb: Option<u32>,
}

/// Automatic `geoip.dat` / `geosite.dat` updates.
///
/// Shipped to every machine with no way to disable it. The `geosite:` / `geoip:` matches
/// in rule tables depend entirely on these two files, and the files go stale as
/// categories are renamed and domains enter and leave the lists. Staleness produces no
/// error: the rules stop matching, traffic takes the fallback, and nothing reports it. As
/// long as a machine runs xray, its .dat files have to be maintained.
///
/// Only the cron and the two URLs are exposed, not `file`. xray rewrites the `geosite:`
/// prefix into `ext:geosite.dat:` (`common/geodata/rule_parser.go`), so the filename is
/// fixed, and exposing it would only permit configuring a file that can never be read.
///
/// The download verifies neither signature nor checksum (`app/geodata/download.go` checks
/// only the HTTP status and a non-empty body). The URLs therefore have to be
/// configurable: whatever they point at is trusted, and an operator has to be able to
/// point them at their own mirror. The two defaults are the copies Xray's own release
/// workflow pulls, the same source `install.sh` installs, so enabling auto-update does
/// not change routing semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeodataSettings {
    /// When to update. A five-field cron, optionally with a `CRON_TZ=` / `TZ=`
    /// prefix.
    ///
    /// The prefix is a correctness requirement, not optional formatting. The
    /// `robfig/cron` xray uses takes the process's local timezone when `cron.New()`
    /// receives no `WithLocation`, and each node's `/etc/localtime` differs. Without
    /// pinning the zone, one expression fires 9 hours apart on a Tokyo machine and a UTC
    /// machine, and that difference produces no symptoms.
    ///
    /// The default `Asia/Shanghai 6:30` is `22:30 UTC`, half an hour after upstream
    /// publishes: Loyalsoldier's workflow runs `0 22 * * *` (GitHub Actions crons are
    /// always UTC). The half hour is margin for the release upload and the CDN.
    pub cron: String,
    /// Where to fetch `geoip.dat` from.
    pub geoip_url: String,
    /// Where to fetch `geosite.dat` from.
    pub geosite_url: String,
}

impl Default for GeodataSettings {
    fn default() -> Self {
        Self {
            cron: "CRON_TZ=Asia/Shanghai 30 6 * * *".to_owned(),
            geoip_url:
                "https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geoip.dat"
                    .to_owned(),
            geosite_url:
                "https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/geosite.dat"
                    .to_owned(),
        }
    }
}

/// End-to-end probing parameters.
///
/// These parameters affect no artifact. Like `PortSettings`, they change only the
/// agent's behavior, not the configuration running on machines, so editing this section
/// triggers no release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSettings {
    /// Where to send the request once connected.
    ///
    /// This endpoint rather than `generate_204`, because `generate_204` cannot return
    /// the exit IP, and a chain that connects but exits at the wrong location is the
    /// condition a probe most needs to detect. Cloudflare's trace returns reachability,
    /// time to first byte, `ip=`, and `loc=` in one request.
    ///
    /// Plain HTTP is deliberate: the measurement covers the chain itself, and the
    /// endpoint's own TLS handshake would otherwise be included in the timing.
    pub endpoint_url: String,
    /// No first byte within this time counts as down.
    pub timeout_secs: u16,
    /// How often to probe. Configurable because the cost depends on the fleet: each
    /// round starts one short-lived xray process per chain, which is significant on a
    /// machine carrying many chains.
    pub interval_secs: u32,
}

impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            endpoint_url: "http://cp.cloudflare.com/cdn-cgi/trace".to_owned(),
            timeout_secs: 10,
            interval_secs: 60,
        }
    }
}

/// Where automatic port selection starts.
///
/// These bases affect only the defaults for newly created objects, never values already
/// stored. Once a port is recorded in the model it has to stay stable: changing it
/// alters the xray config, restarts the process, and drops every connection on that
/// machine (see `HopIn::port`). Editing these two numbers therefore modifies no existing
/// chain; the next chain searches upward from the new base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSettings {
    /// Ingresses search upward from this port.
    ///
    /// 8443 rather than 443 by default, because port 443 usually carries the operator's
    /// own site or reverse proxy. A collision leaves two processes binding one port, and
    /// the only symptom is a failed xray start. Setting this number to 443 selects 443;
    /// that is an operator decision rather than a default.
    pub ingress_base: u16,
    /// Relay ports search upward from this port. A high range keeps them clear of
    /// ingresses and system services.
    pub hop_base: u16,
    /// Hysteria 2 ingresses search upward from this port, on UDP.
    ///
    /// A separate base rather than a share of `ingress_base`, for two reasons. Occupancy is
    /// keyed by protocol as well as by number, so a QUIC listener contends only with other UDP
    /// listeners and is not displaced upward by TCP allocations. A hopping ingress also needs
    /// a contiguous run of free ports above the one it binds, which requires a range no other
    /// allocator consumes one port at a time.
    pub hy2_base: u16,
}

impl Default for PortSettings {
    fn default() -> Self {
        Self {
            ingress_base: 8443,
            hop_base: 20000,
            hy2_base: HYSTERIA2_PORT_BASE,
        }
    }
}

/// The backbone's global parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlaySettings {
    /// Written into the wg config only for one-way dialing (`Dial::Both` needs no
    /// keepalive). Environments with aggressive NAT aging need a lower value; 25 seconds
    /// is WireGuard's usual default.
    ///
    /// Keepalive is a global setting: it describes the side that keeps the path open
    /// because the far side cannot dial it, and links are computed from the full mesh,
    /// so there is no per-link object to attach it to.
    pub keepalive_secs: u16,
    /// The default for machines with no `Node.mtu`, not the authoritative value.
    ///
    /// MTU is a property of the wg interface, and a machine with one `wg0` has one MTU;
    /// `[Peer]` has no such key (`format/ini.rs`). The value is therefore node-level, and
    /// `Node.mtu` is authoritative.
    pub mtu: u16,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            keepalive_secs: 25,
            mtu: 1420,
        }
    }
}

/// The globally defaulted REALITY site. Blank means never set, in which case an
/// ingress must write its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealitySite {
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub server_names: Vec<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// XTLS flow control. `None` turns it off, leaving plain VLESS over TLS.
    /// A fresh database defaults to [`DEFAULT_REALITY_FLOW`], which enables Vision.
    #[serde(default)]
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealityClientPolicy {
    #[serde(default)]
    pub min_client_ver: Option<String>,
    #[serde(default)]
    pub max_client_ver: Option<String>,
    #[serde(default)]
    pub max_time_diff_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub tenant: String,
    pub name: String,
    #[serde(default)]
    pub public_ipv4: Option<String>,
    #[serde(default)]
    pub public_ipv6: Option<String>,
    /// Whether this public IPv4 is behind NAT, which prevents other nodes from dialing
    /// it directly.
    ///
    /// NAT is a property of an address, not of a node: IPv4 may be NAT'd while IPv6
    /// remains directly dialable. Compilation must not treat a public IP marked NAT as
    /// a dialable endpoint for a WireGuard or Xray relay.
    #[serde(default)]
    pub public_ipv4_nat: bool,
    #[serde(default)]
    pub public_ipv6_nat: bool,
    pub overlay_addr: Ipv4Addr,
    /// The name on this machine's own TLS certificate, once it has one.
    ///
    /// # Why a certificate reaches the compiler at all
    ///
    /// Certificates are deliberately not model state: the control plane issues them on its own
    /// schedule and the agent fetches them over a separate channel, so a renewal is not a
    /// revision and restarts nothing. An ingress presenting its own certificate still has to
    /// write the file path into its configuration and the name into every subscription, and
    /// neither is derivable from another field. The *name* is therefore an input here, on the
    /// same footing as `public_ipv4`: observed outside the model, read by the compiler, owned
    /// by neither.
    ///
    /// `None` marks a machine with nothing issued yet. A TLS ingress on such a machine is
    /// unreachable, so `ingress.tls-no-certificate` rejects it rather than compiling a server
    /// that presents no certificate.
    #[serde(default)]
    pub certificate_name: Option<String>,
    pub wireguard: WireGuardKeys,
    #[serde(default)]
    pub api_port: Option<u16>,
    pub overlay: bool,
    pub egress_allowed: bool,
    pub dns: Dns,
    /// See `DomainStrategy`. Defaulted so that a snapshot written before this field existed
    /// still deserializes, landing on the value the artifact layer used to hard-code.
    #[serde(default)]
    pub domain_strategy: DomainStrategy,
    /// Decommissioned. A decommissioned machine stays in the snapshot because it still
    /// has to receive a desired state that disables all three artifacts, which the agent
    /// applies to shut wg0 and xray down. Removed from the snapshot it would receive no
    /// desired state, and local reconcile would keep it running from the old artifacts
    /// indefinitely, leaving an unattended relay holding valid credentials.
    #[serde(default)]
    pub retired: bool,
    /// This machine's `wg0` MTU. Blank takes `settings.overlay.mtu`.
    ///
    /// MTU is a property of the interface, and the interface belongs to the node. One
    /// machine has one `wg0` and one MTU; `MTU =` appears only in `[Interface]`, and
    /// `[Peer]` has no such key. WireGuard therefore has no per-link MTU: giving
    /// different peers different MTUs requires one interface per peer.
    ///
    /// The value is derived rather than estimated: the agent probes path MTU per pair,
    /// and this machine's suggested value is the smallest across all its paths minus 60.
    #[serde(default)]
    pub mtu: Option<u16>,
    /// This machine's connection policy, each field falling back to
    /// `settings.connection`.
    #[serde(default)]
    pub connection: NodeConnection,
}

/// A relay port's wire format: which protocol it uses and, where the protocol allows a
/// choice, how it is secured.
///
/// The two are one field rather than two because they are not independent. Every variant
/// that names a security layer is VLESS underneath, and a protocol carrying its own
/// encryption admits no security layer. As separate fields, a configuration pairing the two
/// would be expressible and would need a rule forbidding it; as one field it is
/// unrepresentable.
///
/// The field belongs to the chain, not the node. One inbound cannot accept both cleartext
/// and encrypted traffic, but what follows is that each chain has its own inbound on the
/// machine, not that the machine has one wire format. A single machine-wide format would
/// leave a relay serving two chains unable to distinguish them: one chain requiring REALITY
/// to pass censorship equipment and another requiring throughput on a datacenter network
/// would not be expressible. The field therefore sits on `Step.hop_in` alongside the port.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum HopWire {
    /// Unencrypted. Correct only where the hop runs over the overlay.
    #[default]
    None,
    /// VLESS Encryption: encryption happens at the protocol layer while
    /// `streamSettings.security` stays `none`. It provides forward secrecy and appears as
    /// random bytes on the wire, but imitates no other protocol.
    Encryption(HopEncryption),
    /// The same REALITY as an ingress: in addition to encryption, the connection is
    /// presented as a visit to the real site at `dest`. Used where the hop has to pass
    /// censorship equipment.
    Reality(Reality),
    /// Shadowsocks 2022, which is not VLESS. The protocol carries its own AEAD, so there is
    /// no separate security layer to choose and nothing wraps it. It costs less than the
    /// alternatives on a machine without AES acceleration and imitates no other protocol.
    ///
    /// Two symmetric keys rather than an asymmetric pair. Shadowsocks 2022 identifies the
    /// connecting account by layering a per-account key under a port-wide one, and the dialer
    /// presents them joined with a colon; the artifact layer performs the joining.
    ///
    /// The account is required. Relay traffic is attributed by the credential that carried
    /// it: the routing rules select on it, and the usage counters are keyed by it. A port
    /// holding only the port-wide key therefore admits traffic with no attribution, which
    /// leaves by whichever rule matches next and is never counted. This shipped once and was
    /// found when a chain in the preview cluster took an unrelated relay's exit.
    ///
    /// Both keys are secret in full. Unlike VLESS Encryption or REALITY, whose private half
    /// stays on the listener, these are written into the dialing machine's artifacts as well,
    /// so compromising either end of the hop exposes both.
    ///
    /// A reverse hop cannot use it. xray attaches a reverse tunnel to a VLESS account
    /// specifically, and a shadowsocks account is not one, so the pairing is rejected at
    /// compile time rather than producing a machine that appears configured and never
    /// connects.
    Shadowsocks2022 {
        /// The port-wide key that every account under it is layered on.
        server_psk: String,
        /// This relay port's one account. One rather than a list, because only a reverse hop
        /// would add a second, and reverse hops are refused.
        user_psk: String,
    },
}

/// VLESS Encryption's X25519 key pair, base64url without padding. The curve and the
/// encoding match REALITY's, so store reuses one generator for both.
///
/// Only the keys themselves are stored, not the `mlkem768x25519plus.native.600s.`
/// prefix: that prefix is xray's configuration syntax and belongs to the artifact layer
/// (`artifacts/xray.rs`). Storing the whole string in the model would turn any future
/// handshake-parameter change into a data migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HopEncryption {
    pub private_key: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGuardKeys {
    pub private_key: String,
    pub public_key: String,
    pub listen_port: u16,
    /// How others dial this wg port. See `WgTransport`.
    #[serde(default)]
    pub transport: WgTransport,
}

/// WireGuard is UDP only and has no TCP mode. Where an upstream blocks inbound UDP, the
/// remaining option is to wrap the UDP at both ends; phantun does that here. It only adds
/// a synthetic TCP header to pass filters and performs neither retransmission nor
/// congestion control, so it avoids the TCP-over-TCP degradation in which the two layers
/// amplify each other under loss. A real TCP tunnel such as wstunnel has that degradation.
///
/// This describes how other nodes dial this machine, not how it dials them. Unlike a relay
/// port, the setting belongs to the node: one machine has one `wg0` shared by every peer,
/// the TCP wrapping is a property of that interface, and every dialer has to match it.
/// Relay ports can split into several inbounds per chain; a wg interface cannot, because
/// giving different peers different transports requires one interface per peer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum WgTransport {
    /// The default: others dial `public_ipv4:listen_port` or
    /// `[public_ipv6]:listen_port` directly.
    #[default]
    Udp,
    /// Fake TCP: others dial this TCP port, and phantun at both ends wraps wg's UDP
    /// inside it.
    FakeTcp { port: u16 },
}

/// The default fake-TCP port.
///
/// A high number claimed by no common service. Not 443, because an ingress on this machine
/// has most likely already taken it, and a non-TLS listener on 443 is identified by the
/// first active probe. Not near 51820, because that is WireGuard's default port and
/// identifies the traffic.
///
/// This is a starting point only. Operators should change it per environment, because one
/// default reused across every network is itself a signature.
pub const DEFAULT_FAKE_TCP_PORT: u16 = 39743;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum Dns {
    System,
    Servers(Vec<String>),
}

impl Dns {
    /// Whether resolving through this setting needs a route off the machine.
    ///
    /// `localhost`, `fakedns` and the `+local://` forms are answered on the machine itself, so a
    /// node configured with only those needs no DNS route. Any other value names a server the
    /// machine has to reach, and the plan then has to carry a route to it.
    ///
    /// Validation and planning both call this, and the two have to agree: a model that validation
    /// accepts but planning cannot route is a configuration accepted at the console and broken on
    /// the node.
    pub fn needs_route(&self) -> bool {
        let Self::Servers(servers) = self else {
            return false;
        };
        servers.iter().any(|server| !is_local_dns(server))
    }
}

fn is_local_dns(server: &str) -> bool {
    let value = server.trim();
    value == "localhost" || value == "fakedns" || value.contains("+local://")
}

/// How this machine resolves a domain to an address on egress.
///
/// The field sits on the node beside `dns`, not on the egress rule. Resolver selection and
/// family selection are one axis, and xray applies this on the outbound rather than on a
/// routing rule: a rule references an outbound by tag and carries no resolution setting of
/// its own. Placing it per rule would force the egress outbound map (`physical/node.rs`) to
/// key on `(send_through, strategy)`, and two disagreeing rules on a machine with external
/// DNS servers would then trigger `dns.route-ambiguous`, because the internal DNS's own
/// queries need exactly one egress and the compiler cannot select among several. Per node,
/// every outbound on the machine takes the same value and that key is unchanged.
///
/// The variants are xray's, written in the model's naming convention; the artifact layer
/// (`artifacts/xray.rs`) maps them to xray's casing, the same separation `HopEncryption`
/// applies to the handshake prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainStrategy {
    /// The default, and the value the artifact layer previously hard-coded. Both families are
    /// queried and the address is selected at random from the merged list, per dial, so one
    /// domain may leave over v4 on one connection and v6 on the next. This is not a preference
    /// order: the probability follows the record count rather than the family, so four A
    /// records and one AAAA give a one-in-five chance of leaving over v6.
    #[default]
    UseIp,
    /// A only, no fallback. A domain that publishes AAAA alone is unreachable from this machine.
    UseIpv4,
    /// AAAA only, no fallback.
    UseIpv6,
    /// A first; a second query for AAAA is sent only if the first returned nothing. This costs
    /// two round trips for a v6-only domain rather than one query sorted afterwards. The
    /// fallback is skipped when the outbound binds a source address (`send_through`), which
    /// reduces this to `UseIpv4` without reporting anything.
    UseIpv4v6,
    /// AAAA first, then A. Same shape as `UseIpv4v6`, including the `send_through` caveat.
    UseIpv6v4,
    /// Do not resolve: the domain reaches the dialer unchanged and the machine's own resolver
    /// handles it. `dns` on this node is then unused, because xray's DNS is never queried, so
    /// an external server configured there has no effect; `ir/validate.rs` reports that
    /// combination.
    AsIs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct User {
    pub id: String,
    pub tenant: String,
    pub uuid: String,
}

/// A proxy server outside the Brocade fleet.
///
/// Unlike `Node`, this is not a deployment target and never becomes a chain member. A rule
/// references it through [`Action::Proxy`], and only the Brocade node holding that rule receives
/// the resulting Xray outbound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalOutbound {
    pub app: String,
    pub id: String,
    pub tenant: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol: ExternalOutboundProtocol,
    pub security: ExternalOutboundSecurity,
}

/// The first externally managed protocol set.
///
/// `credential` has one name across variants so the store can seal it through one path and the
/// console redactor can mask it without a deny-list of protocol-specific field names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum ExternalOutboundProtocol {
    Vless {
        credential: String,
        #[serde(default = "external_vless_encryption_none")]
        encryption: String,
        #[serde(default)]
        flow: Option<String>,
        /// The wire carrying VLESS. Missing on rows and historical snapshots written before
        /// external XHTTP existed, where RAW/TCP was the only possible value.
        #[serde(default)]
        transport: ExternalVlessTransport,
    },
    Shadowsocks2022 {
        credential: String,
        method: String,
    },
    Socks5 {
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        credential: String,
    },
    HttpConnect {
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        credential: String,
    },
    Wireguard {
        /// The local WireGuard private key.
        credential: String,
        peer_public_key: String,
        local_addresses: Vec<String>,
        #[serde(default = "external_wireguard_mtu")]
        mtu: u16,
        #[serde(default)]
        reserved: Vec<u8>,
        #[serde(default)]
        keep_alive: u16,
        #[serde(default = "external_wireguard_allowed_ips")]
        allowed_ips: Vec<String>,
        #[serde(default)]
        no_kernel_tun: bool,
        #[serde(default = "external_wireguard_domain_strategy")]
        domain_strategy: String,
    },
}

/// Network layer used by an externally managed VLESS server.
///
/// This belongs to the VLESS protocol variant rather than to [`ExternalOutbound`]: the other
/// supported proxy protocols keep their own fixed transport shape, and making this a global axis
/// would allow combinations Xray cannot use (for example WireGuard over XHTTP).
// Raw carries no data while Xhttp carries a full XHTTP transport config, so the two variants
// differ widely in size. This is per-external-outbound configuration, off any hot path, so the
// layout footprint is immaterial and Xhttp is not worth boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum ExternalVlessTransport {
    /// Xray still spells the renamed RAW transport `tcp` in the broadly compatible JSON form.
    #[default]
    Raw,
    Xhttp(ExternalVlessXhttp),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalVlessXhttp {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<u16>,
    #[serde(default)]
    pub mode: XhttpMode,
    /// Optional independent downlink. Xray treats it as another complete stream dial, so its
    /// endpoint, security and XHTTP settings must travel together rather than as loose overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<ExternalVlessXhttpDownload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalVlessXhttpDownload {
    pub address: String,
    pub port: u16,
    pub security: ExternalOutboundSecurity,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<u16>,
    #[serde(default)]
    pub mode: XhttpMode,
}

fn external_vless_encryption_none() -> String {
    "none".to_owned()
}

fn external_wireguard_mtu() -> u16 {
    1420
}

fn external_wireguard_allowed_ips() -> Vec<String> {
    vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()]
}

fn external_wireguard_domain_strategy() -> String {
    "ForceIP".to_owned()
}

impl ExternalOutboundProtocol {
    pub fn credential(&self) -> &str {
        match self {
            Self::Vless { credential, .. }
            | Self::Shadowsocks2022 { credential, .. }
            | Self::Socks5 { credential, .. }
            | Self::HttpConnect { credential, .. }
            | Self::Wireguard { credential, .. } => credential,
        }
    }

    pub fn set_credential(&mut self, value: String) {
        match self {
            Self::Vless { credential, .. }
            | Self::Shadowsocks2022 { credential, .. }
            | Self::Socks5 { credential, .. }
            | Self::HttpConnect { credential, .. }
            | Self::Wireguard { credential, .. } => *credential = value,
        }
    }

    pub fn allows_empty_credential(&self) -> bool {
        matches!(self, Self::Socks5 { .. } | Self::HttpConnect { .. })
    }
}

/// Transport security used to reach an external proxy.
///
/// The current vertical slice is RAW transport only. Keeping transport out of this enum is
/// deliberate: TLS/REALITY and RAW/XHTTP are separate axes in Xray, and the latter can be added
/// without changing these stored security values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum ExternalOutboundSecurity {
    None,
    Tls {
        server_name: String,
        #[serde(default = "external_fingerprint_chrome")]
        fingerprint: String,
    },
    Reality {
        server_name: String,
        public_key: String,
        short_id: String,
        #[serde(default = "external_fingerprint_chrome")]
        fingerprint: String,
    },
}

fn external_fingerprint_chrome() -> String {
    "chrome".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppView {
    pub id: String,
    pub label: String,
    pub chains: Vec<Chain>,
    pub ingresses: Vec<Ingress>,
    pub fronts: Vec<Front>,
    pub steps: Vec<Step>,
    pub grants: Vec<Grant>,
}

/// A chain is a container of rules and declares no trunk. The head is the machine hosting
/// the ingress (`Ingress.node`), and the members are the nodes reachable by breadth-first
/// expansion from the head along each node's `Forward` rules. The trunk is the path those
/// `any → Forward` edges describe, derived rather than stored. Decommissioning any node on
/// the chain disables the whole chain (`compile_app` does not compile it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub id: String,
    pub tenant: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ingress {
    pub id: String,
    pub chain: String,
    pub node: String,
    pub bind: IpAddr,
    pub port: u16,
    #[serde(default)]
    pub front: Option<String>,
    /// Stable credentials owned by the ingress, independent of how its traffic is carried.
    pub identity: IngressIdentity,
    pub wires: IngressWires,
    /// This ingress's outward address. With neither family projected, the artifacts are
    /// byte for byte what they would be without this field.
    #[serde(default)]
    pub projection: Projection,
    /// What this ingress refuses to carry, applied before the chain's rules.
    #[serde(default)]
    pub guard: IngressGuard,
}

/// Traffic classes an ingress refuses to carry.
///
/// # Why here and not on the chain
///
/// A chain's rules determine where traffic goes; these determine whether it is admitted, which
/// is a property of the ingress rather than of the chain behind it. Two ingresses onto one chain
/// can serve different populations, such as trial and paying users, and a chain-level setting
/// would apply the stricter value to both.
///
/// # Why booleans and not a rule list
///
/// Hand-written rules are error-prone: a match one step too broad disables the whole ingress, and
/// one too narrow appears to protect while blocking nothing. Each flag here compiles to a fixed
/// match reviewed once. The cost is that a sixth class of block cannot be defined here; that
/// belongs in the chain's own rules, which already support arbitrary matching.
///
/// # What is on by default, and why
///
/// Four of the five. Those four cover traffic no subscriber requests deliberately: reaching the
/// fleet's own overlay from outside it, seeding torrents from a shared address, sending mail from
/// an address that will be blacklisted for it, and reflecting amplification traffic off open UDP
/// services. The fifth is off because it breaks ordinary use: games and voice both require UDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressGuard {
    /// The fleet's own overlay and every private range. This is isolation rather than abuse
    /// protection: without it a subscriber reaches this machine's `10.66.0.0/16` neighbours and
    /// anything else on the datacenter's internal network.
    #[serde(default = "yes")]
    pub no_private: bool,
    /// BitTorrent, by protocol rather than by port.
    ///
    /// Depends on sniffing: the match reads the sniffer's result, so an ingress that does not
    /// sniff cannot apply it. Rejected at validation (`ingress.guard-needs-sniffing`) rather
    /// than compiled into a rule that matches nothing.
    #[serde(default = "yes")]
    pub no_bittorrent: bool,
    /// Outbound SMTP. Abuse of this port is the most common cause of the machine's address being
    /// blacklisted, and ordinary clients do not send mail through a proxy.
    #[serde(default = "yes")]
    pub no_mail: bool,
    /// The UDP services used for reflection attacks: chargen, DNS, NTP, SNMP, CLDAP, SSDP,
    /// memcached. A subscriber reaches these deliberately only when running their own.
    #[serde(default = "yes")]
    pub no_udp_amplification: bool,
    /// Everything UDP except 443. Off by default, because it also blocks games, voice, and every
    /// self-hosted UDP service. QUIC continues to work, since it runs on 443.
    #[serde(default)]
    pub tcp_and_quic_only: bool,
}

fn yes() -> bool {
    true
}

impl Default for IngressGuard {
    fn default() -> Self {
        Self {
            no_private: true,
            no_bittorrent: true,
            no_mail: true,
            no_udp_amplification: true,
            tcp_and_quic_only: false,
        }
    }
}

impl IngressGuard {
    /// Nothing refused. The behavior of an ingress created before this field existed, and the
    /// value the tests asserting on the old artifacts require.
    pub const OPEN: Self = Self {
        no_private: false,
        no_bittorrent: false,
        no_mail: false,
        no_udp_amplification: false,
        tcp_and_quic_only: false,
    };

    pub fn blocks_nothing(&self) -> bool {
        *self == Self::OPEN
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressIdentity {
    pub private_key: String,
    pub public_key: String,
    pub short_ids: Vec<String>,
}

/// Ingress projection: the address written into subscriptions, decoupled from where the
/// machine listens.
///
/// Some machines sit behind an optimized line, such as a datacenter relay or third-party
/// acceleration, and clients have to dial that line's address rather than the machine's own
/// public IP. How the line delivers the packets is outside brocade; this field only supplies
/// the address for the subscription.
///
/// **This field affects only the subscription artifacts issued to users**: the `@host:port`
/// of a VLESS URI and Clash's `server`/`port`. The xray config, node-to-node dialing,
/// WireGuard, probing, billing, and reachability derivation all ignore it. In particular,
/// `FrontDownstream` continues to match the node's declared public addresses rather than a
/// projection, because the relay behind a projection is outside brocade and the compiler can
/// neither infer nor validate how traffic reaches it. Changing a projection therefore
/// restarts no process, and the remaining risk is a wrong address leaving users unable to
/// connect.
///
/// Two families rather than one address, because a line usually carries only v4 or only v6
/// and each is projected separately. That separation is also why a machine behind NAT with
/// no dialable public v4 can still serve ingress through a v4 projection: a projection is an
/// external line's endpoint and is independent of this machine's own position on the network
/// (`physical/user.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    #[serde(default)]
    pub v4: Option<ProjectionEndpoint>,
    #[serde(default)]
    pub v6: Option<ProjectionEndpoint>,
}

/// One family's projected endpoint.
///
/// `None` means no projection. An empty `host` means enabled but unfilled, which
/// `ingress.projection-blank` rejects. The two have to stay distinguishable: once an empty
/// string is stored, whether the operator meant to disable the projection or left it
/// half-filled can no longer be determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionEndpoint {
    /// An IP or a name. IPv6 literals need no brackets of their own; `uri_host` in
    /// `format/uri.rs` adds them.
    pub host: String,
    pub port: u16,
    /// Optional client downlink. It reaches the same XHTTP core and inherits the ingress's path,
    /// TLS name and fingerprint. A split REALITY ingress may additionally name the node-side TLS
    /// listener separately from the public dial port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<ProjectionDownloadEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionDownloadEndpoint {
    pub host: String,
    /// The public port written into client artifacts.
    pub port: u16,
    /// The node-side TLS listener used by a split REALITY ingress. `None` preserves the original
    /// behavior where the public port was also the origin port. TLS + XHTTP projections do not
    /// create a listener and leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_port: Option<u16>,
    /// HTTP Host sent on the independent download, or TLS SNI/address fallback when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_host: Option<String>,
    /// Download-side XMUX concurrency, independent from the upload connection pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux: Option<u16>,
}

impl ProjectionDownloadEndpoint {
    /// Port actually occupied on the node by the TLS download front.
    pub fn node_port(&self) -> u16 {
        self.origin_port.unwrap_or(self.port)
    }
}

/// An ingress's complete wire format: protocol, security layer and network layer named
/// together as one verified combination.
///
/// # Why one enum rather than two fields
///
/// xray's configuration treats these as separate axes (`protocol`, `security`, `network`), and
/// this model mirrored that for a time, with the network as its own field on the ingress. The
/// axes are not independent: among the pairs that model could express, Vision over XHTTP builds
/// cleanly, passes `xray -test`, and then refuses every connection at runtime. Orthogonal fields
/// make an unbuildable configuration expressible and leave a validator to reject it afterwards.
///
/// Naming whole combinations limits this enum to combinations that have been run, and adding one
/// is explicit and carries a place to record what was verified. The cross product given up would
/// be a cost if it were large; it has four members.
///
/// # The two security layers, and when each is right
///
/// REALITY presents a real site's certificate: the machine holds no certificate of its own, and
/// an observer sees a handshake carrying another domain's name. The cost is that the server dials
/// that site on *every* handshake; measured on one busy ingress, 27% of the machine's traffic was
/// spent fetching certificates for that domain.
///
/// TLS presents this machine's own certificate for its own name. It costs no additional dial, and
/// it is the only shape a CDN can front, because a CDN terminates TLS and there is no external
/// certificate to present. It provides no disguise: the handshake identifies itself, and over TCP
/// it exposes the TLS-in-TLS signature REALITY exists to suppress. Over HTTP that signature is
/// absent, which is why [`Transport::VlessTlsXhttp`] is the useful member of the pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Transport {
    /// VLESS over REALITY, one TCP connection per client connection. The shape every ingress
    /// used before XHTTP existed, and the one Vision flow control requires.
    VlessReality(RealitySettings),
    /// VLESS over REALITY, carried inside HTTP. Flow control is unavailable here; see the
    /// type's own note for why.
    VlessRealityXhttp(RealityXhttp),
    /// VLESS over this machine's own TLS certificate, one TCP connection per client connection.
    ///
    /// The simplest shape, and the most recognizable: nested TLS has a distinct wire signature
    /// that no configuration removes. It exists because Vision requires direct TLS or REALITY, so
    /// it is the only way to run flow control without presenting another site's certificate.
    VlessTls(Tls),
    /// VLESS over this machine's own TLS certificate, carried inside HTTP.
    ///
    /// The shape a CDN can front: the edge sees ordinary HTTPS to a name this machine owns and
    /// forwards it by path like any other origin. Flow control is unavailable, as it is over
    /// REALITY.
    VlessTlsXhttp(TlsXhttp),
}

impl Transport {
    /// The REALITY parameters, or `None` for a shape that presents its own certificate.
    ///
    /// This returned a plain reference while every shape carried REALITY parameters. Having the
    /// compiler flag each caller once that stopped being true is the purpose of naming whole
    /// shapes: a call site that needs a public key has to state what it does for a machine
    /// without one.
    pub fn reality(&self) -> Option<&RealitySettings> {
        match self {
            Self::VlessReality(reality) => Some(reality),
            Self::VlessRealityXhttp(shape) => Some(&shape.reality),
            Self::VlessTls(_) | Self::VlessTlsXhttp(_) => None,
        }
    }

    pub fn reality_mut(&mut self) -> Option<&mut RealitySettings> {
        match self {
            Self::VlessReality(reality) => Some(reality),
            Self::VlessRealityXhttp(shape) => Some(&mut shape.reality),
            Self::VlessTls(_) | Self::VlessTlsXhttp(_) => None,
        }
    }

    /// Flow control. Every shape carries the field, and two of them reject a non-empty value.
    ///
    /// `None` means nothing is set on this ingress and the fleet value applies; `Some("")` means
    /// this ingress has it disabled. The two have to stay distinguishable: conflating them once
    /// made a single ingress without Vision inexpressible without changing every client's
    /// configuration.
    pub fn flow(&self) -> Option<&str> {
        match self {
            Self::VlessReality(reality) => reality.flow.as_deref(),
            Self::VlessRealityXhttp(shape) => shape.reality.flow.as_deref(),
            Self::VlessTls(tls) => tls.flow.as_deref(),
            Self::VlessTlsXhttp(shape) => shape.tls.flow.as_deref(),
        }
    }

    pub fn set_flow(&mut self, flow: Option<String>) {
        match self {
            Self::VlessReality(reality) => reality.flow = flow,
            Self::VlessRealityXhttp(shape) => shape.reality.flow = flow,
            Self::VlessTls(tls) => tls.flow = flow,
            Self::VlessTlsXhttp(shape) => shape.tls.flow = flow,
        }
    }

    /// The uTLS fingerprint a client imitates. Not a REALITY setting despite sitting beside them:
    /// it names the TLS ClientHello to synthesize, and a client dialing plain TLS synthesizes one
    /// for the same reason.
    pub fn fingerprint(&self) -> &str {
        match self {
            Self::VlessReality(reality) => &reality.fingerprint,
            Self::VlessRealityXhttp(shape) => &shape.reality.fingerprint,
            Self::VlessTls(tls) => &tls.fingerprint,
            Self::VlessTlsXhttp(shape) => &shape.tls.fingerprint,
        }
    }

    /// The HTTP layer's settings, or `None` for a shape that has no HTTP layer.
    pub fn xhttp(&self) -> Option<&Xhttp> {
        match self {
            Self::VlessReality(_) | Self::VlessTls(_) => None,
            Self::VlessRealityXhttp(shape) => Some(&shape.xhttp),
            Self::VlessTlsXhttp(shape) => Some(&shape.xhttp),
        }
    }

    /// Whether this shape presents a certificate belonging to this machine, which is equivalent
    /// to asking whether the ingress requires the node to hold one. The compiler resolves this
    /// before writing either the artifact or the subscription.
    pub fn needs_node_certificate(&self) -> bool {
        matches!(self, Self::VlessTls(_) | Self::VlessTlsXhttp(_))
            || self
                .reality()
                .is_some_and(RealitySettings::uses_node_certificate_fallback)
    }

    /// How the shape is named in storage and on the wire between console and browser.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::VlessReality(_) => "vless-reality",
            Self::VlessRealityXhttp(_) => "vless-reality-xhttp",
            Self::VlessTls(_) => "vless-tls",
            Self::VlessTlsXhttp(_) => "vless-tls-xhttp",
        }
    }
}

/// Hysteria 2's complete operator-controlled shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hysteria2 {
    /// The UDP port this wire listens on. It belongs to the wire, not to the ingress.
    ///
    /// A port belongs to a wire because the ingress identifies which machine receives and the
    /// wire identifies where on that machine. The two wires previously shared `Ingress::port`,
    /// on the grounds that TCP and UDP are separate spaces and one number collides with nothing.
    /// That holds until port hopping, which claims a whole UDP range; a range containing the TCP
    /// wire's number is one omitted `-p udp` away from capturing it. Separate numbers make that
    /// class of error unrepresentable.
    ///
    /// Defaulted rather than required on the wire so that a payload written before this field
    /// existed still parses. The default is the allocator's base, which is wrong for every
    /// ingress after the first, and detectably so: two of them on one machine produce a
    /// `node.port-clash` that blocks the release.
    #[serde(default = "default_hysteria2_port")]
    pub port: u16,
    /// The UDP range clients rotate through. `None` keeps every client on `port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop: Option<HysteriaPortHop>,
    /// Both directions are present or absent together. Xray accepts strings such as
    /// `200 mbps` and interprets them as bits per second.
    #[serde(default)]
    pub bandwidth: HysteriaBandwidth,
    #[serde(default)]
    pub congestion: HysteriaCongestion,
    #[serde(default)]
    pub bbr_profile: HysteriaBbrProfile,
    /// QUIC windows, timeouts and concurrent streams. Every field is optional, and an absent
    /// field is omitted from the artifact.
    #[serde(default, skip_serializing_if = "HysteriaQuic::is_default")]
    pub quic: HysteriaQuic,
    /// `None` means standard QUIC packets; the only mask offered is Salamander.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs: Option<HysteriaObfs>,
    #[serde(default)]
    pub masquerade: HysteriaMasquerade,
}

impl Default for Hysteria2 {
    fn default() -> Self {
        Self {
            port: DEFAULT_HYSTERIA2_PORT,
            hop: None,
            bandwidth: HysteriaBandwidth::default(),
            congestion: HysteriaCongestion::default(),
            bbr_profile: HysteriaBbrProfile::default(),
            quic: HysteriaQuic::default(),
            obfs: None,
            masquerade: HysteriaMasquerade::default(),
        }
    }
}

/// The factory value behind [`PortSettings::hy2_base`], and the port a [`Hysteria2`] wire takes
/// when it is built or deserialized without one.
///
/// The console's allocator reads the setting rather than this constant, so an operator who moves
/// the base gets the next ingress at the new value. This constant supplies the number a fresh
/// database starts from and the value for paths that have no settings to consult.
///
/// The number carries no protocol meaning. It only has to name a range where a run of free UDP
/// ports can be found, because a hop range grows upward from whatever the allocator returns. It
/// sits below the relay range (20000) rather than above it at no cost: occupancy is keyed by
/// protocol as well as by number (`Proto` in `validate_ports`), so UDP ports allocated here and
/// TCP relay inbounds there never contend, even where the two ranges overlap.
pub const HYSTERIA2_PORT_BASE: u16 = 18_000;
const DEFAULT_HYSTERIA2_PORT: u16 = HYSTERIA2_PORT_BASE;

fn default_hysteria2_port() -> u16 {
    DEFAULT_HYSTERIA2_PORT
}
/// How many ports a newly enabled hop covers. Ten is wide enough that blocking the range costs
/// more than blocking one port, and narrow enough that a machine can still find a free run.
pub const DEFAULT_HYSTERIA2_HOP_SPAN: u16 = 10;

/// The UDP range a hopping client rotates through.
///
/// Inclusive on both ends, and `start..=end` has to contain [`Hysteria2::port`], because the
/// server binds that one port only. Every other port in the range reaches it through the
/// machine's redirect, so a range excluding the listener leaves the only working port outside
/// the set clients are told to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HysteriaPortHop {
    pub start: u16,
    pub end: u16,
}

impl HysteriaPortHop {
    /// How many ports it covers. Saturating rather than panicking on a reversed pair; validation
    /// rejects that pair, and artifact generation is not the place to detect it.
    pub fn span(&self) -> u16 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }

    pub fn contains(&self, port: u16) -> bool {
        self.start <= port && port <= self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HysteriaBandwidth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HysteriaCongestion {
    /// BBR when bandwidth is absent; Brutal when both directions are present.
    #[default]
    Brutal,
    Bbr,
    /// New Reno. Xray's fourth value, and the only one that never reads the bandwidth pair.
    Reno,
    ForceBrutal,
}

impl HysteriaCongestion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brutal => "brutal",
            Self::Bbr => "bbr",
            Self::Reno => "reno",
            Self::ForceBrutal => "force-brutal",
        }
    }
}

/// BBR's aggressiveness. Read only when the connection runs BBR, which is `bbr` directly or
/// `brutal` with no bandwidth pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HysteriaBbrProfile {
    #[default]
    Standard,
    Conservative,
    Aggressive,
}

impl HysteriaBbrProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Conservative => "conservative",
            Self::Aggressive => "aggressive",
        }
    }
}

/// The QUIC parameters under `finalmask.quicParams` other than congestion control and bandwidth.
///
/// `None` on any field means the key is not written, which is also how Xray reads a zero: it
/// applies its own default. Writing an explicit default here would freeze the current upstream
/// value into every artifact, and the artifact would no longer track the version it is deployed
/// against.
///
/// The bounds in the doc comments are Xray's. They are enforced in three places: a CHECK on the
/// column, which is the durable one; a compile diagnostic, so a bad value is caught before publish
/// rather than by the agent; and the input's `min`/`max` in the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HysteriaQuic {
    /// Bytes, at least 16384. Upstream advises keeping stream:connection near 2:5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_stream_receive_window: Option<u64>,
    /// Bytes, at least 16384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_stream_receive_window: Option<u64>,
    /// Bytes, at least 16384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_connection_receive_window: Option<u64>,
    /// Bytes, at least 16384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connection_receive_window: Option<u64>,
    /// Seconds, 4 to 120.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_idle_timeout_secs: Option<u32>,
    /// Seconds, 2 to 60. Absent means Xray sends no keep-alives at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive_period_secs: Option<u32>,
    /// At least 8. Server side only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_incoming_streams: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disable_path_mtu_discovery: bool,
}

/// The bounds Xray checks, kept next to the struct so the validator, the CHECK constraints and the
/// console inputs read one list rather than three copies.
impl HysteriaQuic {
    pub const MIN_RECEIVE_WINDOW: u64 = 16384;
    pub const MIN_IDLE_TIMEOUT_SECS: u32 = 4;
    pub const MAX_IDLE_TIMEOUT_SECS: u32 = 120;
    pub const MIN_KEEP_ALIVE_SECS: u32 = 2;
    pub const MAX_KEEP_ALIVE_SECS: u32 = 60;
    pub const MIN_INCOMING_STREAMS: u32 = 8;

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HysteriaObfs {
    Salamander { password: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HysteriaMasquerade {
    #[default]
    NotFound,
    Proxy {
        url: String,
    },
}

/// What an ingress accepts on the wire: one shape, the other, or both.
///
/// # Why both at once is the point
///
/// The two stacks fail under different conditions. A network that drops all UDP blocks Hysteria 2
/// and leaves the TCP shape working; a network that fingerprints TLS-in-TLS does the reverse.
/// Serving both from one ingress gives one credential, one grant, one ledger entry, and a client
/// that can use whichever connects, rather than requiring the operator to predict which network a
/// user is on.
///
/// # Why an enum rather than two `Option`s
///
/// Because an ingress cannot have neither. Two optional fields would make that state expressible
/// and leave a validator to reject it afterwards, which is the same argument [`Transport`] makes
/// about orthogonal fields one layer down. Here the type carries the invariant.
///
/// The cross product stays out of the variant list: `Both` carries a [`Transport`] rather than
/// adding four more names. [`Transport`] enumerates combinations that are *unbuildable* when mixed
/// wrong; a TCP shape alongside a QUIC one is neither unbuildable nor a combination, because they
/// are two listeners sharing only a credential.
// Not boxed, for the same reason as `xray::Config`: the two variants differ by about 200 bytes,
// and one snapshot holds at most a few dozen ingresses, so boxing would save single-digit
// kilobytes. The cost would be that every later reader of this type has to work out why the
// indirection is there.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IngressWiresWire", into = "IngressWiresWire")]
pub enum IngressWires {
    Vless(Transport),
    Hysteria2(Hysteria2),
    Both {
        vless: Transport,
        hysteria2: Hysteria2,
    },
}

/// The wire form: two optional halves, which matches both the JSON payload and the existing
/// database columns. The type above is the form the rest of the program uses. The conversion
/// enforces the at-least-one rule, so no other layer has to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressWiresWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vless: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hysteria2: Option<Hysteria2>,
}

impl TryFrom<IngressWiresWire> for IngressWires {
    type Error = &'static str;

    fn try_from(wire: IngressWiresWire) -> Result<Self, Self::Error> {
        match (wire.vless, wire.hysteria2) {
            (Some(vless), Some(hysteria2)) => Ok(Self::Both { vless, hysteria2 }),
            (Some(vless), None) => Ok(Self::Vless(vless)),
            (None, Some(hysteria2)) => Ok(Self::Hysteria2(hysteria2)),
            (None, None) => Err("接入面至少要有一条线：vless 和 hysteria2 不能都空着"),
        }
    }
}

impl From<IngressWires> for IngressWiresWire {
    fn from(wires: IngressWires) -> Self {
        match wires {
            IngressWires::Vless(vless) => Self {
                vless: Some(vless),
                hysteria2: None,
            },
            IngressWires::Hysteria2(hysteria2) => Self {
                vless: None,
                hysteria2: Some(hysteria2),
            },
            IngressWires::Both { vless, hysteria2 } => Self {
                vless: Some(vless),
                hysteria2: Some(hysteria2),
            },
        }
    }
}

impl IngressWires {
    /// The TCP half, or `None` where this ingress is QUIC only.
    pub fn vless(&self) -> Option<&Transport> {
        match self {
            Self::Vless(vless) | Self::Both { vless, .. } => Some(vless),
            Self::Hysteria2(_) => None,
        }
    }

    pub fn vless_mut(&mut self) -> Option<&mut Transport> {
        match self {
            Self::Vless(vless) | Self::Both { vless, .. } => Some(vless),
            Self::Hysteria2(_) => None,
        }
    }

    /// The UDP half, or `None` where this ingress is TCP only.
    pub fn hysteria2(&self) -> Option<&Hysteria2> {
        match self {
            Self::Hysteria2(hysteria2) | Self::Both { hysteria2, .. } => Some(hysteria2),
            Self::Vless(_) => None,
        }
    }

    pub fn hysteria2_mut(&mut self) -> Option<&mut Hysteria2> {
        match self {
            Self::Hysteria2(hysteria2) | Self::Both { hysteria2, .. } => Some(hysteria2),
            Self::Vless(_) => None,
        }
    }

    /// Whether a TCP listener exists. `false` for the QUIC-only shape, which is why the
    /// port-occupancy checks call this rather than assuming (`ir/validate.rs`).
    pub fn has_tcp(&self) -> bool {
        self.vless().is_some()
    }

    /// Whether a UDP listener exists.
    pub fn has_udp(&self) -> bool {
        self.hysteria2().is_some()
    }

    pub fn reality(&self) -> Option<&RealitySettings> {
        self.vless().and_then(Transport::reality)
    }

    pub fn reality_mut(&mut self) -> Option<&mut RealitySettings> {
        self.vless_mut().and_then(Transport::reality_mut)
    }

    pub fn flow(&self) -> Option<&str> {
        self.vless().and_then(Transport::flow)
    }

    pub fn set_flow(&mut self, flow: Option<String>) {
        if let Some(vless) = self.vless_mut() {
            vless.set_flow(flow);
        }
    }

    /// The uTLS fingerprint the TCP half instructs a client to imitate. Empty where there is no
    /// TCP half, because Hysteria 2 clients expose no such setting.
    pub fn fingerprint(&self) -> &str {
        self.vless().map(Transport::fingerprint).unwrap_or("")
    }

    pub fn xhttp(&self) -> Option<&Xhttp> {
        self.vless().and_then(Transport::xhttp)
    }

    /// True when *either* half presents this machine's own certificate. Hysteria 2 always does;
    /// the TCP half does in two of its four shapes.
    pub fn needs_node_certificate(&self) -> bool {
        self.has_udp() || self.vless().is_some_and(Transport::needs_node_certificate)
    }

    /// Whether clients are told to skip verifying the machine certificate.
    ///
    /// How the TCP half is named in storage, or `None` where there is no TCP half.
    pub fn vless_kind(&self) -> Option<&'static str> {
        self.vless().map(Transport::kind)
    }
}

/// This machine's own TLS.
///
/// The struct is small by design rather than by omission: the certificate is not selected per
/// ingress. Each machine holds one, issued for a name of its own, and every TLS ingress on it
/// presents that certificate. The remaining settings are the ones REALITY never covered: whether
/// to run flow control and which ClientHello the client imitates.
///
/// Skipping certificate verification is deliberately absent, on this wire and on Hysteria 2
/// alike: xray removed `allowInsecure` in v26.2.6 and rejects the whole config from v26.6.1 on,
/// and a subscription that still carried it would hand out a configuration the fleet's own core
/// refuses to load. A certificate that does not verify is now a fault to fix, not a switch to
/// turn off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    /// Same three-state rule as [`Reality::flow`]: unset follows the fleet, empty is off.
    #[serde(default)]
    pub flow: Option<String>,
    pub fingerprint: String,
}

/// VLESS over this machine's own TLS, carried inside HTTP.
///
/// # Why this shape exists at all
///
/// It is the only shape a CDN can front. REALITY cannot be proxied, because the edge would have
/// to complete the handshake it is supposed to forward, so a machine behind a CDN has to present
/// a certificate for a name it owns. That is also the benefit: clients no longer dial the
/// origin's address, and traffic arrives from the CDN's ranges rather than from client addresses.
///
/// # Why flow control is absent here too
///
/// The same limit as the REALITY shape: xray refuses XTLS over anything but direct TLS or
/// REALITY, and refuses it only at runtime. [`Tls::flow`] may hold only the empty value.
///
/// The settings are flattened for the same reason [`RealityXhttp`] flattens its own: every shape
/// has to place its fields at the same depth, or a reader that takes `transport` and looks up a
/// key finds it in one shape and not in another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsXhttp {
    #[serde(flatten)]
    pub tls: Tls,
    pub xhttp: Xhttp,
}

/// VLESS over REALITY over HTTP.
///
/// # Why XHTTP is here at all
///
/// Plain TCP opens one connection per client connection, and under REALITY each one costs the
/// server a dial to the impersonated site to fetch a genuine handshake. Measured on one busy
/// ingress: 6.8 new connections per second, and 27% of the machine's traffic spent fetching
/// certificates. XHTTP carries many streams inside one HTTP/2 connection, which reduces the
/// handshake count and that cost.
///
/// # What it costs
///
/// Multiplexing over one connection means one lost packet stalls every stream sharing it, which
/// is head-of-line blocking. The worse the link, the lower the concurrency should be, and on a
/// sufficiently lossy link it should be one. This is the same trade-off as a relay hop's
/// [`HopPool`], one layer up.
///
/// # Why this shape has no flow control
///
/// [`Reality::flow`] is carried here as elsewhere, but the only value it may hold is the empty
/// one. xray refuses Vision over XHTTP at runtime with `XTLS only supports TLS and REALITY
/// directly for now`, and refuses it *only* at runtime: `xray -test` reports `Configuration OK`
/// for the pair. Measured on 26.4.25 by running it.
///
/// # Why the REALITY parameters are flattened
///
/// Both shapes have to serialize identically down to the key names, because everything downstream
/// reads them by name. Left nested, this shape writes `{"kind":…,"reality":{…}}` while the plain
/// shape writes its REALITY fields at the top level, so every reader of `transport.server_names`,
/// starting with the console, receives `undefined` for the ingresses using this shape. The
/// mismatch is not visible in the type definitions: two variants of one enum do not serialize
/// alike unless they are made to.
///
/// `deny_unknown_fields` is deliberately absent rather than forgotten: serde cannot combine it
/// with `flatten`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealityXhttp {
    #[serde(flatten)]
    pub reality: RealitySettings,
    pub xhttp: Xhttp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Xhttp {
    /// The URL path the tunnel's requests carry, `/` first.
    ///
    /// It is a routing key, not a secret: under REALITY it travels encrypted, and anyone able to
    /// read it has already passed REALITY's authentication. It allows one host to serve a real
    /// site and a tunnel together, and it gives a CDN in front something to route on, which is
    /// the arrangement this shape exists for.
    ///
    /// Generated once and stored rather than derived from the ingress id, because it is copied
    /// into every client configuration; deriving it would make renaming an ingress disconnect
    /// every client using it.
    pub path: String,
    /// Client HTTP Host. The server deliberately leaves its Host filter unset so independently
    /// routed upload and download requests can share the same core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// How many streams may share one underlying connection. Absent does **not** disable
    /// sharing.
    ///
    /// The three settings, read from `XmuxManager::GetXmuxClient`
    /// (`transport/internet/splithttp/mux.go`) rather than from the documentation:
    ///
    /// - `None`: xray builds a zero-value `XmuxConfig`, and a `maxConcurrency` of 0 skips the
    ///   eligibility filter (`else { xmuxClients = m.xmuxClients }`), so every stream uses the
    ///   connection already open. This is the most aggressive reuse, not the absence of reuse.
    ///   mihomo interprets the same absence in the opposite way: `NewReuseManager` returns a nil
    ///   manager for a nil config and nothing is reused. A subscription that leaves this unset
    ///   therefore behaves differently on the two clients, and an operator who needs a specific
    ///   behavior has to set a number.
    /// - `1`: a connection carries one stream at a time and is reused by the next stream once it
    ///   goes idle. This is a connection pool, and the only pooling available to a client:
    ///   Mux.cool is xray's other pooling mechanism, and Vision rejects every non-XUDP Mux
    ///   session (`isMuxAndNotXUDP`, `proxy/vless/inbound/inbound.go`), which is what an ingress
    ///   runs.
    /// - `2..=128`: that many streams share a connection, which saves handshakes and costs stream
    ///   isolation, because one lost packet stalls every stream on that connection.
    ///
    /// One of XMUX's five settings is exposed. The other four control when a connection is
    /// rotated, and their defaults are *ranges* xray samples at random, because a constant value
    /// is a fingerprint. Exposing them as fields would lead to fixed numbers being entered, which
    /// removes the property they provide. `maxConnections` is excluded for a second reason:
    /// mihomo treats it as a hard ceiling and fails the dial once the pool is full (`manager: no
    /// available connection`), while xray opens another connection, so one field would carry two
    /// behaviors and neither is a ceiling an operator could reason about.
    #[serde(default)]
    pub mux: Option<u16>,
    /// How the client sends its upload half.
    #[serde(default)]
    pub mode: XhttpMode,
}

/// XHTTP's upload shape.
///
/// # What the setting does on each side
///
/// On the client the setting selects a behavior; on the server it acts as an admission filter.
/// Measured on 26.4.25 by running every pair (server row, client column, "does traffic arrive"):
///
/// |            | auto | packet-up | stream-up | stream-one |
/// |------------|------|-----------|-----------|------------|
/// | auto       | yes  | yes       | yes       | yes        |
/// | packet-up  | no   | yes       | no        | no         |
/// | stream-up  | yes  | no        | yes       | yes        |
/// | stream-one | yes  | no        | no        | yes        |
///
/// A server left at [`XhttpMode::Auto`] therefore accepts every client, and any other value makes
/// the setting a filter. [`XhttpMode::PacketUp`] is the value to apply with care: it is the only
/// one that rejects a client not configured for it, and that includes every client holding a
/// subscription issued before the change.
///
/// # Why the default emits nothing
///
/// Because no single value represents it. `auto` is resolved by the client at dial time and the
/// result depends on the security layer beneath it: measured on 26.4.25 with the same binary and
/// an otherwise identical configuration, REALITY resolves to `stream-one` and this machine's own
/// TLS resolves to `packet-up`. Writing either observed value would pin an unchosen value on both
/// sides and be wrong for the other shape.
///
/// Absent also has to stay distinguishable from present, for the same reason as `xmux`: an unset
/// value has to read back as unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum XhttpMode {
    /// Each side resolves the mode itself, and the result depends on the layer beneath; see the
    /// type's note. Written into no artifact.
    #[default]
    Auto,
    /// Upload split across many small POSTs. This is the form a caching CDN accepts, and the
    /// value `auto` resolves to under this machine's own TLS, which is the shape a CDN fronts.
    PacketUp,
    /// Upload as one streaming POST, download as a separate GET.
    StreamUp,
    /// Upload and download inside a single request. The value `auto` resolves to under REALITY.
    StreamOne,
}

impl XhttpMode {
    /// The value xray expects, or `None` for the default, which is written nowhere.
    ///
    /// xray rejects an unknown value at build time (`unsupported mode: …`), so these strings are
    /// checked by the acceptance test that runs the real binary rather than by a validator here.
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::PacketUp => Some("packet-up"),
            Self::StreamUp => Some("stream-up"),
            Self::StreamOne => Some("stream-one"),
        }
    }
}

impl Xhttp {
    /// One is its own setting, the connection pool, rather than the lower end of a multiplexing
    /// range, so the floor is 1 rather than 2. Zero is rejected because xray reads it as no
    /// limit, which is what omitting the field already means, and two representations of one
    /// meaning leave the setting ambiguous. Rejected rather than clamped, for the same reason as
    /// [`HopPool`]: a value changed without notice reads back as something the operator did not
    /// choose.
    pub const MUX_MIN: u16 = 1;
    /// xray documents no upper bound. This one is local, and it guards against a typo rather than
    /// being a tuned maximum: above it, head-of-line blocking outweighs the saved handshakes.
    pub const MUX_MAX: u16 = 128;
}

/// The factory default for flow control: a fresh database has Vision enabled. An operator can
/// disable it in the global settings, where disabled is the empty string. Disabled leaves a
/// plain TLS proxy that exposes the TLS-in-TLS signature, which is why it is an option rather
/// than the default.
///
/// Xray-core's server-side account accepts only this value or the empty string
/// (`vless.XRV`; anything else is `unknown request flow`).
///
/// The `xtls-rprx-vision-udp443` often taken for a third value is a client-side variant:
/// the outbound recognizes it, sets allowUDP443, and truncates the string to its first 16
/// bytes before sending, so the server never sees that suffix.
///
/// Constraints (`proxy/vless/inbound/inbound.go`): Vision supports only direct TLS/REALITY
/// transports and does not support UDP. An ingress is always `network: tcp` with
/// `security: reality`, which satisfies both.
pub const DEFAULT_REALITY_FLOW: &str = "xtls-rprx-vision";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealitySettings {
    pub dest: String,
    pub server_names: Vec<String>,
    pub fingerprint: String,
    #[serde(default)]
    pub flow: Option<String>,
    /// What an unauthenticated REALITY connection is allowed to reach.
    ///
    /// The two site modes both use `dest`; their distinction is retained so the console can
    /// keep following the global setting without guessing from equal values. The node-certificate
    /// mode replaces `dest` at artifact time with a compiler-owned loopback TLS listener.
    #[serde(default)]
    pub fallback_mode: RealityFallbackMode,
    /// Per-connection fallback throttling. Presets are expanded with stable per-ingress jitter by
    /// the physical compiler, so repeated builds stay byte-identical without making every node
    /// advertise the same values.
    #[serde(default)]
    pub fallback_limits: RealityFallbackLimits,
    /// Whether the fallback is confined to the impersonated site's own name.
    ///
    /// REALITY dials `dest` before reading any byte of the ClientHello, and a client that fails
    /// the check is joined to that connection regardless of the name it requested
    /// (`XTLS/REALITY` `tls.go`: `!config.ServerNames[hs.clientHello.serverName]` exits the loop
    /// and the connection is copied through). Where `dest` is a name on a CDN, its address also
    /// answers for every other site on that CDN, so anyone who finds the port can use this
    /// machine's bandwidth to reach any of them.
    ///
    /// When set, the compiler places a loopback listener in front of `dest` that sniffs the
    /// requested name and admits only `server_names`. It complements throttling:
    /// `fallback_limits` bounds the rate an unauthenticated client gets, and this bounds which
    /// destinations it reaches.
    ///
    /// It has no effect under [`NodeCertificate`](RealityFallbackMode::NodeCertificate), whose
    /// fallback is a local listener answering 403 and never leaves the machine.
    #[serde(default = "default_fallback_guard")]
    pub fallback_guard: bool,
}

fn default_fallback_guard() -> bool {
    true
}

impl RealitySettings {
    pub fn uses_node_certificate_fallback(&self) -> bool {
        self.fallback_mode == RealityFallbackMode::NodeCertificate
    }

    /// Whether this ingress compiles a fallback listener. Off under the local cover, which has no
    /// external site to confine the fallback to.
    ///
    /// Also off when there are no names to admit. Validation rejects that combination
    /// (`reality.no-sni`), so it cannot be published, but artifacts are also built on paths that
    /// do not run the publish gate. A listener built from an empty name list is the case this
    /// rules out: the allow rule would carry no `domain` condition, would match every connection
    /// it saw, and would forward every unauthenticated client to the external site while
    /// appearing, in both the artifact and the console, identical to a guarded ingress.
    pub fn guards_fallback(&self) -> bool {
        self.fallback_guard
            && !self.uses_node_certificate_fallback()
            && !self.server_names.is_empty()
    }

    /// SNI sent by subscribers and probes. A local cover has to name the certificate it presents;
    /// external fallbacks keep the configured external site name.
    pub fn server_name(&self, certificate_name: Option<&str>) -> String {
        if self.uses_node_certificate_fallback() {
            certificate_name.unwrap_or_default().to_owned()
        } else {
            self.server_names.first().cloned().unwrap_or_default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealityFallbackMode {
    /// Follow `settings.reality_site`; materialization has already filled `dest` and names.
    #[default]
    GlobalSite,
    /// Use the certificate already managed for the ingress node and answer fallback HTTP locally.
    NodeCertificate,
    /// Use this ingress's own external `dest` and names.
    CustomSite,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum RealityFallbackLimits {
    #[default]
    Off,
    Balanced,
    Strict,
    Custom {
        upload: RealityFallbackRateLimit,
        download: RealityFallbackRateLimit,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealityFallbackRateLimit {
    pub after_bytes: u64,
    pub bytes_per_sec: u64,
    pub burst_bytes_per_sec: u64,
}

/// Complete REALITY material used by relay hops, whose identity belongs to the wire rather than
/// to an ingress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reality {
    pub private_key: String,
    pub public_key: String,
    pub short_ids: Vec<String>,
    pub dest: String,
    pub server_names: Vec<String>,
    pub fingerprint: String,
    #[serde(default)]
    pub flow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Front {
    pub id: String,
    pub tenant: String,
    pub name: String,
    pub via: Vec<String>,
    pub strategy: FrontStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrontStrategy {
    UrlTest,
    Select,
    Fallback,
}

impl FrontStrategy {
    /// The single representation shared by the database column, the compiled artifacts and the
    /// subscription formats.
    ///
    /// Three copies of this match existed, one per layer. They are not three formats that happen
    /// to coincide: a strategy written by the console has to read back as itself and compile to
    /// itself, so a diverging copy changes a front group's behavior in transit.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UrlTest => "url-test",
            Self::Select => "select",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub chain: String,
    pub node: String,
    #[serde(default)]
    pub accept: Option<Accept>,
    /// This chain's relay inbound on this machine: the port it listens on and its wire
    /// format.
    ///
    /// One inbound per chain, so a relay serving two chains has two inbounds, two ports and
    /// separate keys. A chain that accepts no relay here, such as one where this machine is
    /// the ingress, leaves it `None`.
    ///
    /// Separate from `accept`: `accept` holds the credential that admits a peer, and this
    /// holds the listening port and the wire format. Both belong to the chain, but one
    /// describes an identity and the other a socket.
    #[serde(default)]
    pub hop_in: Option<HopIn>,
    pub rules: Vec<Rule>,
}

/// One chain's relay inbound on one machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HopIn {
    /// The listening port. Chains on one machine must not overlap, or two inbounds bind the
    /// same port; `validate` rejects that (`node.hop-port-conflict`) at compile time rather
    /// than leaving it to surface as a failed bind at xray startup.
    ///
    /// Where the inbound is dialed only over the overlay, the number does not matter
    /// operationally: it binds this machine's overlay address (the listen decision in
    /// `physical/node.rs`) and nothing outside wg reaches it. It still belongs in the model,
    /// because a changed port alters the xray config, restarts the process and drops every
    /// connection on that machine, so the value has to be stable and the model is where that
    /// stability is recorded. If the compiler recomputed it on each build, adding a chain
    /// could move it and trigger a restart with no visible cause. The UI selects a free port
    /// when a chain is created.
    pub port: u16,
    /// This inbound's wire format. `None` is sufficient for a hop over WireGuard, which
    /// already encrypts it, so another layer consumes CPU with no benefit. A hop dialed
    /// directly from another network has to select one, or the uuid and target address
    /// travel in the clear.
    #[serde(default)]
    pub security: HopWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accept {
    pub uuid: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(rename = "m")]
    pub dest_match: DestMatch,
    #[serde(rename = "a")]
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum DestMatch {
    Any,
    DomainSuffix(Vec<String>),
    DomainKeyword(Vec<String>),
    DomainRegex(String),
    Geosite(Vec<String>),
    IpCidr(Vec<String>),
    Geoip(Vec<String>),
    Port(Vec<String>),
    /// Ports *except* these. xray's `port` syntax has no negation, so this compiles to the
    /// complement as two ranges, which is why it accepts single numbers rather than ranges.
    PortExcept(Vec<u16>),
    Network(Network),
    /// The protocol the sniffer identified on this connection. On an inbound that does not sniff
    /// it matches nothing, which fails open without reporting anything while appearing to be in
    /// effect.
    Protocol(Vec<String>),
    All(Vec<DestMatch>),
    FrontDownstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Tcp,
    Udp,
}

/// Which of the peer's addresses this hop dials.
///
/// The address is written on the chain rather than derived from the node. One machine may
/// have several addresses that reach it, such as a public one and a datacenter-internal
/// one, and the choice belongs to the chain: for one relay, a neighbour inside the
/// datacenter should use the internal network and every other peer the public one. The
/// compiler cannot determine which pairs have direct internal reachability, because that is
/// a topological fact absent from the model, and an incorrect guess produces a hop taking a
/// different path while the artifacts appear correct.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum HopDial {
    /// Dial the peer's overlay address, wrapped in WireGuard. The port comes from that
    /// peer's `hop_in.port`.
    ///
    /// This variant is symbolic and stores no address: overlay addresses are assigned by
    /// the system, a transcribed copy is error-prone, and changing the range would break
    /// the chain.
    #[default]
    Overlay,
    /// Dial this concrete `host:port`, bypassing WireGuard.
    ///
    /// The port is the peer's external one, and a custom address may carry a port different
    /// from the peer's `hop_in.port`; with forwarding in between the two differ by design,
    /// on the same reasoning as wg's `endpoint` versus `listen_port`. Where the host is a
    /// public IPv4 or IPv6 address the target node declared, this is the automatic public
    /// variant, and the compiler derives the port from the target's `hop_in.port` on this
    /// chain so that a stale port on the chain cannot miscompile the artifacts.
    Addr(String),
    /// Do not dial the peer: the peer connects to this machine and traffic travels back
    /// along that connection (xray's reverse).
    ///
    /// The edge's direction is unchanged: traffic still flows `from → to` and the `Hop`
    /// is still that edge. Only the initiator of the TCP connection is inverted, so this
    /// variant describes the same property as the two above, with the peer as initiator.
    ///
    /// This variant exists so a machine can relay without joining the backbone. A machine
    /// behind NAT can always reach the backbone over wg, which the overlay variant
    /// covers, but once in the backbone it is an overlay member and can dial every other
    /// overlay address. An exit machine hosted in a private residence should not have
    /// that reach. The reverse tunnel provides one connection to a designated upstream
    /// and no visibility of the other machines in the backbone.
    ///
    /// Symbolic like `Overlay`, storing no address. The peer dials this machine, and this
    /// machine's address is a node property (`public_ipv4/6`) rather than chain state;
    /// storing it here would duplicate the same fact, and after an IP change the copy on
    /// the chain would be stale while the artifacts appeared correct. The port likewise
    /// comes from this machine's `hop_in.port` on this chain, which is the port the peer
    /// connects to.
    ///
    /// The address family has to be selected explicitly, with no fallback to v4.
    /// Reachability differs per family: the downstream may have only a v6 egress and the
    /// upstream only a v4 ingress. If the compiler selected whichever worked, the chain
    /// would carry an unchosen value, and the hop would change family on its own once the
    /// upstream gained a v6 address, with no change to the model. An unreachable
    /// selection has to be reported as an error rather than replaced by the other family.
    ///
    /// No reverse-over-overlay combination is offered: a machine that can use the overlay
    /// is already in the backbone, where `Overlay` is simpler, and the combination would
    /// only permit configuring an equivalent with more steps.
    Reverse(IpFamily),
}

/// An address family. Where one has to be selected, it is selected explicitly; see the
/// note on `HopDial::Reverse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpFamily {
    V4,
    V6,
}

impl IpFamily {
    /// The human-facing name, matching the wording of the `hop.nat-public`
    /// diagnostic.
    pub fn label(self) -> &'static str {
        match self {
            IpFamily::V4 => "公网 IPv4",
            IpFamily::V6 => "公网 IPv6",
        }
    }
}

/// How this hop manages the TCP connections it opens to the peer.
///
/// One axis covering two properties: how many streams share a connection, and whether a
/// connection outlives the stream that opened it. Without pooling, each stream dials its own
/// connection and closes it on completion, so every new stream pays the handshake of every
/// hop down the chain in sequence. The cost falls on connection setup rather than on
/// throughput.
///
/// The three variants are named rather than exposing xray's `concurrency` as a bare number,
/// because 1 is a different arrangement rather than the low end of a range: at 1 each stream
/// still gets its own connection and only reuses an idle one, so no stream stalls another.
/// From 2 upward, streams share a live connection and a loss on one delays the rest. A flat
/// 1–128 field would place that boundary in the middle of a range.
///
/// Applicable only where this machine dials. Under `HopDial::Reverse` the peer opens the
/// connection and traffic travels back along it, so there is no outbound to pool; see the
/// check in `validate`.
///
/// Mux.cool is the only mechanism available here rather than the preferred one. xray's other
/// multiplexer, `xmux`, exposes what this one lacks: `maxConnections` for a pool ceiling, and
/// `hMaxReusableSecs` and `hKeepAlivePeriod` for connection retention. It sits under
/// `xhttpSettings` and works only over HTTP/2 and HTTP/3, while a relay hop is plain TCP
/// inside wg, so reaching it would mean wrapping the hop in HTTP for a path that does not
/// require it. The consequence is that Mux.cool's retention is a constant in its own
/// `monitor()` with no configuration key, so the short retention window cannot be changed
/// from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum HopPool {
    /// One connection per stream, closed with it. The behavior of every hop before this
    /// field existed, and still the default: pooling justifies a disruptive release only
    /// where it has been measured to help.
    #[default]
    None,
    /// Idle connections are retained and reused by the next stream, one stream at a time.
    ///
    /// Measured against xray 26.4.25: 20 sequential streams used 1 connection, and 8
    /// concurrent streams used 8. Retention is short, roughly 20 to 40 seconds of idle
    /// rather than the 300 of `connIdle`, so a relay idle for a minute pays the handshakes
    /// again.
    Pool,
    /// Up to `n` streams share one connection.
    ///
    /// This saves the most handshakes and costs stream isolation: the shared connection is
    /// one TCP connection, so a loss affecting one stream delays every other stream on it.
    /// On a lossy cross-border link the result can be worse than `None`.
    ///
    /// `n` is 2..=128. One is `Pool` and has its own variant. Above 128 xray clamps without
    /// reporting, which `validate` rejects rather than reproduces: a number that reads one
    /// way in the console and runs another on the machine is the failure the golden
    /// artifacts exist to prevent.
    Merge(u16),
}

impl HopPool {
    /// The floor of `Merge`. The value below it belongs to `Pool`.
    pub const MERGE_MIN: u16 = 2;
    /// The ceiling xray enforces on `concurrency`.
    pub const MERGE_MAX: u16 = 128;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Action {
    Forward {
        to: String,
        #[serde(default)]
        dial: HopDial,
        /// Absent on every rule written before this field existed, which is why it
        /// defaults rather than being required. The default is the previous behavior, so
        /// an old revision recompiles byte for byte.
        #[serde(default)]
        pool: HopPool,
    },
    Egress {
        #[serde(default)]
        send_through: Option<IpAddr>,
    },
    /// Send through a project-scoped external proxy. This is terminal like `Egress`, not an edge
    /// in the Brocade node graph; the referenced id is resolved within the current project.
    Proxy {
        outbound: String,
    },
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub tenant: String,
    pub user: String,
    pub ingress: String,
}

/// A slug's maximum length. The cap sits on the slug rather than on the assembled label,
/// because a label appears in the grant-sync JSON, in every access-log line, and in the
/// stats API's full response all at once.
pub const SLUG_MAX_LEN: usize = 32;

/// Whether a slug satisfies `[a-z0-9._-]{1,32}`.
///
/// The character set is defined as an allow list rather than a deny list, because a deny
/// list omits characters. Only one case is permitted: two labels differing only in case are
/// indistinguishable in the console and in the logs but are two counters in xray, so usage
/// splits into two series that cannot be recombined.
///
/// Compile-time validation (`ir::validate`) and the write path (`brocade-store`) share this
/// one test, so the two cannot diverge and allow objects that can be created but not
/// compiled.
pub fn is_valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= SLUG_MAX_LEN
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// A user credential's label: `{user}@{tenant}#{ingress}`.
///
/// The label serves three roles: an xray client's email, the key of a statistics counter
/// (`user>>>{label}>>>traffic>>>uplink`), and the control plane's only means of attributing
/// usage. The format therefore has exactly one definition. The compiler previously built it
/// with `format!` here while the control plane assembled
/// `user_id || '@' || tenant_id || '#' || ingress_id` in SQL when collecting counters, two
/// independent copies where editing one made the usage records permanently unreconcilable.
pub fn grant_label(user: &str, tenant: &str, ingress: &str) -> String {
    format!("{user}@{tenant}#{ingress}")
}

/// Split a string assembled by `grant_label` back into its three ids.
///
/// The split is unambiguous: the slug character set is `[a-z0-9._-]` (see
/// `is_valid_slug`), tenant paths are `.`-separated (`platform.acme.sub`), and none of
/// the three contains `@` or `#`.
///
/// With it, the control plane resolves attribution by label through the
/// `(tenant_id, user_id, ingress_id)` primary key rather than comparing an assembled
/// string against every row in the table.
pub fn parse_grant_label(label: &str) -> Option<(&str, &str, &str)> {
    let (user, rest) = label.split_once('@')?;
    let (tenant, ingress) = rest.split_once('#')?;
    if user.is_empty() || tenant.is_empty() || ingress.is_empty() {
        return None;
    }
    Some((user, tenant, ingress))
}

/// The end-to-end probe credential's label: `probe#{ingress}`.
///
/// Deliberately shaped unlike `grant_label`: it contains no `@`, so `parse_grant_label`
/// cannot split it. That is by design rather than coincidence. The probe client ships down
/// the same grant channel as a real user so that it can reach the ingress and be hot-synced,
/// but it is not a user. Billing, subscriptions and user listings identify users by
/// `{user}@{tenant}#{ingress}`, fail to match this label, and therefore exclude it without
/// any added filter condition.
///
/// If the prefix could collide with a real slug, probe traffic would be billed to a user.
/// `probe#` cannot be a valid `{user}@{tenant}` prefix, because it contains no `@`, so no
/// collision is possible.
pub fn probe_label(ingress: &str) -> String {
    format!("{PROBE_LABEL_PREFIX}{ingress}")
}

/// Whether this label belongs to a probe credential. The control plane uses it to discard
/// probe traffic explicitly when collecting counters. Counting it as unrecognized would also
/// work, but that count is a health indicator, and incrementing it every 30 seconds from an
/// internal probe removes its value.
pub fn is_probe_label(label: &str) -> bool {
    label
        .strip_prefix(PROBE_LABEL_PREFIX)
        .is_some_and(|ingress| !ingress.is_empty())
}

const PROBE_LABEL_PREFIX: &str = "probe#";

/// The probe credential's UUID, derived from the ingress's REALITY private key and its
/// ingress id.
///
/// It is a credential that reaches this ingress, so it must not be guessable. The private
/// key is part of the derivation for that reason: the ingress id alone, which is public and
/// appears in every subscription link, does not yield the uuid.
///
/// It is also a pure function: the control plane stores no additional secret, and repeated
/// compilations yield the same value. Storing one would introduce a state where the database
/// value and the compiled value disagree, whose symptom is a probe unable to reach its own
/// ingress.
pub fn probe_uuid(private_key: &str, ingress: &str) -> String {
    // `\0` as the separator: the private key is base64 and the ingress a slug, neither of
    // which contains it, so the assembled string corresponds one to one with
    // (private key, ingress) and no two distinct inputs collide on one digest.
    let mut hasher = Sha256::new();
    hasher.update(b"brocade/probe-identity/v1");
    hasher.update(private_key.as_bytes());
    hasher.update([0]);
    hasher.update(ingress.as_bytes());
    let digest = hasher.finalize();

    // The digest is truncated to 16 bytes and stamped with RFC 4122's version 4 and
    // variant bits. xray accepts any string, but an invalid uuid in the artifacts causes
    // every reader to stop and verify it.
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;

    let hex = |range: std::ops::Range<usize>| {
        bytes[range]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(0..4),
        hex(4..6),
        hex(6..8),
        hex(8..10),
        hex(10..16)
    )
}

#[cfg(test)]
mod probe_identity_tests {
    use super::{grant_label, is_probe_label, parse_grant_label, probe_label, probe_uuid};

    /// A probe label has to be one `parse_grant_label` does not recognize. If this breaks,
    /// probe traffic is attributed to a real user's account and that user is overcharged.
    #[test]
    fn probe_label_is_not_a_grant_label() {
        let label = probe_label("app-hk-01.i1");
        assert_eq!(label, "probe#app-hk-01.i1");
        assert_eq!(parse_grant_label(&label), None);
        assert!(is_probe_label(&label));
    }

    #[test]
    fn grant_label_is_not_a_probe_label() {
        assert!(!is_probe_label(&grant_label("alice", "platform", "i1")));
        // The prefix alone, with no ingress, does not qualify
        assert!(!is_probe_label("probe#"));
        assert!(!is_probe_label("probe"));
    }

    /// The same input always yields the same uuid, which is why the control plane does not
    /// store it.
    #[test]
    fn probe_uuid_is_deterministic() {
        let a = probe_uuid("kEY+base64/private==", "app-hk-01.i1");
        let b = probe_uuid("kEY+base64/private==", "app-hk-01.i1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "版本位：{a}");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "{a}");
    }

    /// Changing either the private key or the ingress has to yield a different value. The
    /// first provides the credential's security, and the second keeps two ingresses' probe
    /// identities from being interchangeable.
    #[test]
    fn probe_uuid_separates_inputs() {
        let base = probe_uuid("secret-a", "i1");
        assert_ne!(base, probe_uuid("secret-b", "i1"));
        assert_ne!(base, probe_uuid("secret-a", "i2"));
        // Concatenation ambiguity: ("secret-a", "xi1") and ("secret-ax", "i1") must not
        // collide. The `\0` separator is what prevents it.
        assert_ne!(probe_uuid("secret-a", "xi1"), probe_uuid("secret-ax", "i1"));
    }
}

#[cfg(test)]
mod grant_label_tests {
    use super::{grant_label, parse_grant_label};

    #[test]
    fn label_round_trips() {
        for (user, tenant, ingress) in [
            ("alice", "platform", "app-hk-01.i1"),
            ("bob.2", "platform.acme.sub", "app-jp.i-2"),
            ("c_3", "t-1", "in_9"),
        ] {
            let label = grant_label(user, tenant, ingress);
            assert_eq!(
                parse_grant_label(&label),
                Some((user, tenant, ingress)),
                "{label}"
            );
        }
    }

    /// A malformed string has to yield no attribution rather than a partial one:
    /// attributing usage to the wrong account is harder to diagnose than attributing it to
    /// none.
    #[test]
    fn malformed_labels_are_rejected() {
        for bad in [
            "alice",           // no separator
            "alice@platform",  // no ingress
            "alice#platform",  // separators in the wrong order
            "@platform#in",    // empty user
            "alice@#in",       // empty tenant
            "alice@platform#", // empty ingress
            "chain-a@hk-01",   // a relay hop credential, which is not a grant label
        ] {
            assert_eq!(parse_grant_label(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod slug_tests {
    use super::is_valid_slug;

    #[test]
    fn slug_accepts_the_documented_charset_and_rejects_the_rest() {
        assert!(is_valid_slug("hk-01"));
        assert!(is_valid_slug("platform.acme"));
        assert!(is_valid_slug("i_main"));
        assert!(is_valid_slug("a"));

        assert!(
            !is_valid_slug("HK-01"),
            "大写会在 xray 里分裂出第二个计数器"
        );
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("hk 01"), "空白会让按空白切分的日志管道错位");
        assert!(!is_valid_slug("alice@corp.com"), "@ 是 label 的结构分隔符");
        assert!(!is_valid_slug("c-smart#a"), "# 是 label 的结构分隔符");
        assert!(!is_valid_slug("a>>>b"), ">>> 是统计指标名的分隔符");
        assert!(!is_valid_slug(&"a".repeat(33)));
        assert!(is_valid_slug(&"a".repeat(32)));
    }
}

#[cfg(test)]
mod transport_tests {
    use super::{RealitySettings, RealityXhttp, Transport, Xhttp, XhttpMode};

    fn reality() -> RealitySettings {
        RealitySettings {
            dest: "www.example.com:443".to_owned(),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: "chrome".to_owned(),
            flow: None,
            fallback_mode: Default::default(),
            fallback_guard: true,
            fallback_limits: Default::default(),
        }
    }

    /// Every shape has to place the REALITY parameters under the same key names, because
    /// everything downstream reads them by name from one `transport` object. This shipped broken
    /// once: the XHTTP shape nested them under `reality` while the plain shape kept them at the
    /// top level, so the console read `undefined` for `server_names` and threw on every edit of
    /// an XHTTP ingress. Types, tests and the compiler all passed, because nothing compared the
    /// two shapes against each other.
    #[test]
    fn both_shapes_spell_the_reality_parameters_the_same_way() {
        let plain = serde_json::to_value(Transport::VlessReality(reality())).unwrap();
        let over_http = serde_json::to_value(Transport::VlessRealityXhttp(RealityXhttp {
            reality: reality(),
            xhttp: Xhttp {
                path: "/probe".to_owned(),
                host: None,
                mux: None,
                mode: XhttpMode::Auto,
            },
        }))
        .unwrap();

        for key in ["dest", "server_names", "fingerprint", "flow"] {
            assert_eq!(
                plain.get(key),
                over_http.get(key),
                "两种形状的 {key} 不在同一个位置上"
            );
        }
        assert_eq!(over_http.get("kind").unwrap(), "vless-reality-xhttp");
        assert!(over_http.get("xhttp").is_some());
        assert!(
            over_http.get("reality").is_none(),
            "REALITY 参数不该多一层嵌套"
        );
    }

    /// The value also has to deserialize back, or a stored revision cannot be read.
    #[test]
    fn a_transport_survives_a_round_trip() {
        for transport in [
            Transport::VlessReality(reality()),
            Transport::VlessRealityXhttp(RealityXhttp {
                reality: reality(),
                xhttp: Xhttp {
                    path: "/probe".to_owned(),
                    host: None,
                    mux: Some(16),
                    mode: XhttpMode::StreamOne,
                },
            }),
        ] {
            let text = serde_json::to_string(&transport).unwrap();
            let back: Transport = serde_json::from_str(&text).unwrap();
            assert_eq!(back, transport, "{text}");
        }
    }
}
