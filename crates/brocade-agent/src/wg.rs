//! The WireGuard layer: reading `wg show dump` and `wireguard.conf`, judging each
//! peer's liveness, and self-healing by escalating remedies.
//!
//! Split out of main.rs because it contends with convergence over the same
//! interface (`wg0`), so the exclusive `BACKBONE` lock belongs to this layer and
//! convergence acquires it here.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::SocketAddr,
    path::Path,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use crate::phantun::phantun_servers;
use crate::phantun::udp_bound;
use crate::{
    apply_phantun, apply_wireguard, current_unix_secs, icmp, run_command, run_shell, warn,
};

/// Every peer's overlay address.
pub(crate) fn peer_overlay_ips() -> Vec<String> {
    let Ok(output) = run_command("wg", &["show", "wg0", "dump"]) else {
        return Vec::new();
    };
    parse_wg_dump(&output)
        .into_iter()
        .filter_map(|peer| peer.overlay_ip)
        .collect()
}

/// Whether one overlay address answers.
///
/// `Err` means this machine cannot probe, not that the peer is unreachable. The
/// distinction is the same one argued at the top of `icmp.rs`, and it matters more
/// here than when probing path MTU: there an error produces an oversized MTU
/// suggestion, and here it makes the watchdog delete the interface.
///
/// This is why it does not shell out to `/bin/ping`. `command_success` maps a
/// missing ping binary, a rejected option, and an unanswered ping to one `false`,
/// so on a minimal image without ping every peer with a stale handshake is declared
/// dead and the watchdog rebuilds wg0 every two minutes on a working machine. With
/// a socket opened directly, a failure to open is a separate return value.
pub(crate) fn probe_reachable(ip: &str) -> Result<bool, String> {
    let pinger = icmp::Pinger::open(ip)?;
    // One lost packet is not a failure, because links drop packets. Two unanswered
    // pings are required, and only the failing side pays for the second.
    Ok(pinger.reachable() || pinger.reachable())
}

/// Probe a batch of addresses, one thread each.
///
/// Serial probing is insufficient: on a fully meshed fleet every peer can be
/// unreachable at once when this machine's uplink is down, a dozen peers each
/// hitting a timeout takes tens of seconds, and the convergence loop waits behind
/// it. Sending packets costs almost no CPU, so the waits are overlapped.
pub(crate) fn ping_all(ips: &[String]) -> BTreeMap<String, Result<bool, String>> {
    thread::scope(|scope| {
        let handles = ips
            .iter()
            .map(|ip| scope.spawn(move || (ip.clone(), probe_reachable(ip))))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    })
}

/// The `MTU =` line in wireguard.conf. It is a wg-quick-only key that `wg-quick
/// strip` removes, so the agent must read it itself and apply it to the interface
/// with `ip link set`.
pub(crate) fn wireguard_conf_mtu(path: &Path) -> Option<u16> {
    let content = fs::read_to_string(path).ok()?;
    content
        .lines()
        .find_map(|line| conf_value(line.trim(), "MTU")?.parse().ok())
}

/// The value in `Key = value`. The rendered config is aligned (`PublicKey  = ...`),
/// so both spaces and the equals sign have to be consumed, and the key has to be
/// followed directly by a separator so `MTU` does not match `MTUFoo`.
fn conf_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    if !rest.starts_with([' ', '\t', '=']) {
        return None;
    }
    Some(rest.trim_start_matches([' ', '\t', '=']).trim())
}

/// One `[Peer]` section from wireguard.conf.
pub(crate) struct ConfPeer {
    /// The `# <node name>` line rendered above `PublicKey`, used to state status
    /// in human terms.
    pub(crate) name: String,
    pub(crate) public_key: String,
    /// The `Endpoint` as written in the config. Absence is meaningful: it means
    /// the far side dials this link (`Dial::AtoB`/`BtoA`), so the endpoint is
    /// learned by roaming and there is no configured value to compare against.
    pub(crate) endpoint: Option<String>,
    pub(crate) overlay_ip: Option<String>,
}

pub(crate) fn wireguard_conf_peers(path: &Path) -> Vec<ConfPeer> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_wireguard_conf_peers(&content)
}

pub(crate) fn parse_wireguard_conf_peers(content: &str) -> Vec<ConfPeer> {
    let mut out: Vec<ConfPeer> = Vec::new();
    let mut pending: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("# ") {
            pending = Some(name.trim().to_owned());
        } else if let Some(key) = conf_value(line, "PublicKey") {
            // `PrivateKey` cannot collide here: it does not start with
            // `PublicKey`.
            out.push(ConfPeer {
                name: pending.take().unwrap_or_else(|| key.to_owned()),
                public_key: key.to_owned(),
                endpoint: None,
                overlay_ip: None,
            });
        } else if let Some(value) = conf_value(line, "AllowedIPs") {
            if let Some(peer) = out.last_mut() {
                peer.overlay_ip = value.split('/').next().map(str::to_owned);
            }
        } else if let Some(value) = conf_value(line, "Endpoint") {
            if let Some(peer) = out.last_mut() {
                peer.endpoint = Some(value.to_owned());
            }
        }
    }
    out
}

/// One peer as it exists at runtime.
pub(crate) struct LivePeer {
    pub(crate) public_key: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) overlay_ip: Option<String>,
    /// Unix seconds of the last successful handshake; 0 means never.
    pub(crate) latest_handshake: i64,
    /// Zero means disabled. Older wireguard-tools releases leave the old value in
    /// place when `PersistentKeepalive` disappears from a `syncconf` input.
    pub(crate) persistent_keepalive: u16,
}

/// The output of `wg show wg0 dump`.
///
/// dump rather than the three separate `latest-handshakes` / `endpoints` /
/// `allowed-ips` calls: it saves two execs, and those three read state at three
/// different instants, which can contradict each other once stitched together.
///
/// Format: the first line is the interface itself (private key, public key, listen
/// port, fwmark) in 4 fields, then one line of 8 fields per peer: public key,
/// preshared key, Endpoint, AllowedIPs, last handshake, bytes received, bytes
/// sent, keepalive. wg writes an absent value as `(none)`.
pub(crate) fn parse_wg_dump(output: &str) -> Vec<LivePeer> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 8 {
                return None;
            }
            Some(LivePeer {
                public_key: fields[0].to_owned(),
                endpoint: wg_value(fields[2]),
                // AllowedIPs may hold several entries, comma separated. The
                // backbone only carries overlay to overlay, so the first is the
                // peer itself.
                overlay_ip: wg_value(fields[3]).and_then(|value| {
                    value
                        .split(',')
                        .next()?
                        .split('/')
                        .next()
                        .map(str::to_owned)
                }),
                latest_handshake: fields[4].parse().unwrap_or(0),
                persistent_keepalive: fields[7].parse().unwrap_or(0),
            })
        })
        .collect()
}

/// wg writes `(none)` for a field with no value.
fn wg_value(field: &str) -> Option<String> {
    (field != "(none)").then(|| field.to_owned())
}

/// Whether two Endpoints name the same place.
///
/// String comparison is insufficient: wg prints IPv6 as `[addr]:port` while configs
/// use several forms. Both sides are parsed into a `SocketAddr` and compared, with a
/// literal comparison only when parsing fails. On a parse failure they count as
/// equal rather than unequal.
///
/// That direction is deliberate: unequal triggers `wg set endpoint`, and writing a
/// string this code cannot parse repeats every round at best, while treating them as
/// equal takes no action. Every Endpoint in this project is an IP:port built by
/// `host_port()` rather than a name, so the fallback is currently unreachable; if
/// names were allowed, it leaves the watchdog inactive rather than writing an
/// unparseable value.
pub(crate) fn endpoint_matches(want: &str, live: &str) -> bool {
    match (want.parse::<SocketAddr>(), live.parse::<SocketAddr>()) {
        (Ok(want), Ok(live)) => want == live,
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => want == live,
        (Err(_), Err(_)) => true,
    }
}

/// How old a handshake must be to count as stale.
///
/// Taken from WireGuard's own `REKEY_AFTER_TIME`: once a session reaches that age,
/// wg rehandshakes on its next outgoing data. Past this threshold without a
/// handshake, only two states are possible: no data is flowing through the tunnel,
/// in which case the ping supplies some, wg rehandshakes immediately, and the state
/// is `Alive`; or the link is down. One ping distinguishes them. This number also
/// rate-limits the ping itself, because the data a ping carries triggers the
/// rehandshake, so an idle peer is probed roughly every 120 seconds regardless of
/// how often the watchdog runs.
///
/// This was originally `REJECT_AFTER_TIME` (180). That value is also defensible,
/// but it is a minute later than necessary, and during that minute the link is
/// already down and wg has stopped retrying.
pub(crate) const HANDSHAKE_STALE_SECS: i64 = 120;

/// A peer's state at this moment.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerState {
    /// The handshake is fresh, so no packet needs to be sent.
    Fresh,
    /// The handshake is old or never occurred, which is not yet sufficient to
    /// declare the link down. WireGuard handshakes only when it has data to send,
    /// since keepalive sends keepalive packets rather than handshakes, so an idle
    /// tunnel reports a very old handshake time.
    Suspect,
    /// The ping was answered: the tunnel works and was idle.
    Alive,
    Down {
        detail: String,
    },
    /// Could not probe, as distinct from probed and unreachable. No ICMP socket can
    /// be opened on this machine, so the step after an expired handshake cannot be
    /// taken. The watchdog must not treat this as a fault: every remedy has a cost,
    /// and in this state the link's condition is unknown.
    Unprobed {
        reason: String,
    },
}

pub(crate) struct PeerCheck {
    pub(crate) name: String,
    pub(crate) public_key: String,
    pub(crate) overlay_ip: Option<String>,
    /// The Endpoint as written in the config. Absent means the far side dials this
    /// link.
    pub(crate) want_endpoint: Option<String>,
    /// Seconds since the last successful handshake; `None` means never.
    pub(crate) handshake_age: Option<i64>,
    /// `(configured, live)` when the Endpoint disagrees with the config.
    pub(crate) endpoint_drift: Option<(String, String)>,
    /// This peer can only arrive through this machine's phantun server tun, yet at
    /// runtime it carries a public Endpoint. This field holds that value.
    ///
    /// It is the only way to detect roaming on the passive side. The config writes
    /// no Endpoint for these peers, because the far side dials, so
    /// `endpoint_drift` is always None for them and `wg syncconf` does not modify
    /// an endpoint the config does not name. Once it drifts to a public address,
    /// every packet reaches the far side's blocked UDP port, and roaming moves the
    /// far side's Endpoint as well, keeping both ends on the unusable path.
    pub(crate) stray_endpoint: Option<String>,
    pub(crate) state: PeerState,
}

/// Whether this Endpoint is local loopback. Packets handed over by phantun always
/// carry a loopback source address.
pub(crate) fn endpoint_is_local(endpoint: &str) -> bool {
    let host = match endpoint.rsplit_once(':') {
        Some((host, _)) => host.trim_start_matches('[').trim_end_matches(']'),
        None => endpoint,
    };
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

pub(crate) fn handshake_age_text(age: Option<i64>) -> String {
    match age {
        Some(age) => format!("握手 {age} 秒前"),
        None => "从未握手成功".to_owned(),
    }
}

/// Read the runtime once and derive each peer's liveness.
pub(crate) fn check_wg_peers(conf: &Path) -> Result<Vec<PeerCheck>, String> {
    let dump = run_command("wg", &["show", "wg0", "dump"])?;
    let via_phantun = conf
        .parent()
        .map(peers_via_phantun_server)
        .unwrap_or_default();
    let mut checks = judge_wg_peers(
        &wireguard_conf_peers(conf),
        &parse_wg_dump(&dump),
        current_unix_secs()?,
        &via_phantun,
    );
    settle_suspects(&mut checks);
    Ok(checks)
}

/// The part of the verdict reachable without sending a packet. Doubtful peers get
/// `Suspect`, and `settle_suspects` decides their fate.
///
/// It iterates the peers in the config rather than those at runtime: a peer that
/// should be present but is missing from the runtime, after a `wg syncconf` that
/// failed midway or a manual `wg set peer remove`, cannot be found from the runtime
/// alone, because its line is absent.
pub(crate) fn judge_wg_peers(
    conf: &[ConfPeer],
    live: &[LivePeer],
    now: i64,
    via_phantun: &BTreeSet<String>,
) -> Vec<PeerCheck> {
    conf.iter()
        .map(|peer| {
            let seen = live.iter().find(|l| l.public_key == peer.public_key);
            let Some(seen) = seen else {
                return PeerCheck {
                    name: peer.name.clone(),
                    public_key: peer.public_key.clone(),
                    overlay_ip: peer.overlay_ip.clone(),
                    want_endpoint: peer.endpoint.clone(),
                    handshake_age: None,
                    endpoint_drift: None,
                    stray_endpoint: None,
                    state: PeerState::Down {
                        detail: "配置里有这个 peer，运行态的 wg0 里没有——配置没同步进去".to_owned(),
                    },
                };
            };

            let handshake_age = (seen.latest_handshake > 0).then(|| now - seen.latest_handshake);
            // Peers with no Endpoint in the config are not compared: theirs is
            // learned to begin with.
            let endpoint_drift = match (&peer.endpoint, &seen.endpoint) {
                (Some(want), Some(live)) if !endpoint_matches(want, live) => {
                    Some((want.clone(), live.clone()))
                }
                // The config specifies dialing out and the runtime has no endpoint,
                // which is a failure to dial.
                (Some(want), None) => Some((want.clone(), "(none)".to_owned())),
                _ => None,
            };
            // No Endpoint in the config, and this peer arrives through this
            // machine's phantun server, so its source address can only be loopback.
            // Any other value has drifted.
            let stray_endpoint = match (&peer.endpoint, &seen.endpoint) {
                (None, Some(live))
                    if via_phantun.contains(&peer.name) && !endpoint_is_local(live) =>
                {
                    Some(live.clone())
                }
                _ => None,
            };
            let state = match handshake_age {
                Some(age) if age <= HANDSHAKE_STALE_SECS => PeerState::Fresh,
                _ => PeerState::Suspect,
            };

            PeerCheck {
                name: peer.name.clone(),
                public_key: peer.public_key.clone(),
                overlay_ip: peer.overlay_ip.clone().or_else(|| seen.overlay_ip.clone()),
                want_endpoint: peer.endpoint.clone(),
                handshake_age,
                endpoint_drift,
                stray_endpoint,
                state,
            }
        })
        .collect()
}

/// The peers in `phantun.json` that arrive through this machine's server.
fn peers_via_phantun_server(state_dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(content) = fs::read_to_string(state_dir.join("phantun.json")) else {
        return out;
    };
    if state_dir.join("phantun.disabled").exists() {
        return out;
    }
    let Ok(plan) = serde_json::from_str::<serde_json::Value>(&content) else {
        return out;
    };
    for server in phantun_servers(&plan) {
        let Some(peers) = server.get("peers").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for peer in peers.iter().filter_map(serde_json::Value::as_str) {
            out.insert(peer.to_owned());
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
struct PhantunPassiveDrift {
    name: String,
    public_key: String,
    stale_endpoint: bool,
    stale_keepalive: bool,
}

/// Runtime fields that `wg syncconf` cannot reliably remove from a peer which is
/// changing from bare UDP to the passive side of phantun.
///
/// The peer name is deliberately cross-checked against `phantun.json`. A normal
/// passive WireGuard peer also has no configured Endpoint, but its public runtime
/// Endpoint is learned roaming state and must be preserved.
fn phantun_passive_drift(
    conf: &[ConfPeer],
    live: &[LivePeer],
    via_phantun: &BTreeSet<String>,
) -> Vec<PhantunPassiveDrift> {
    conf.iter()
        .filter(|peer| peer.endpoint.is_none() && via_phantun.contains(&peer.name))
        .filter_map(|peer| {
            let seen = live
                .iter()
                .find(|seen| seen.public_key == peer.public_key)?;
            let stale_endpoint = seen
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| !endpoint_is_local(endpoint));
            let stale_keepalive = seen.persistent_keepalive != 0;
            (stale_endpoint || stale_keepalive).then(|| PhantunPassiveDrift {
                name: peer.name.clone(),
                public_key: peer.public_key.clone(),
                stale_endpoint,
                stale_keepalive,
            })
        })
        .collect()
}

/// Finish a hot WireGuard apply by removing state which an omitted config field
/// does not remove on older `wg syncconf` implementations.
///
/// Peers with a stale public Endpoint are removed, then the complete stripped
/// config is replayed once. Replaying the source config instead of rebuilding a
/// peer from `allowed-ips` preserves every current and future peer field (multiple
/// AllowedIPs, preshared keys, and so on). A stale keepalive on an already-local
/// peer can be cleared in place without interrupting the healthy learned Endpoint.
pub(crate) fn reset_stale_phantun_passive_peers(
    conf: &Path,
    stripped_conf: &Path,
) -> Result<usize, String> {
    let via_phantun = conf
        .parent()
        .map(peers_via_phantun_server)
        .unwrap_or_default();
    if via_phantun.is_empty() {
        return Ok(0);
    }

    // Read after the first syncconf. A packet may already have arrived through
    // phantun and corrected the old public Endpoint to loopback; removing that
    // peer would turn successful self-recovery into an avoidable interruption.
    let dump = run_command("wg", &["show", "wg0", "dump"])?;
    let drift = phantun_passive_drift(
        &wireguard_conf_peers(conf),
        &parse_wg_dump(&dump),
        &via_phantun,
    );
    if drift.is_empty() {
        return Ok(0);
    }

    let mut removed = 0_usize;
    let mut errors = Vec::new();
    for peer in &drift {
        if peer.stale_endpoint {
            match run_command(
                "wg",
                &["set", "wg0", "peer", peer.public_key.as_str(), "remove"],
            ) {
                Ok(_) => removed += 1,
                Err(error) => errors.push(format!("{} 摘不掉：{error}", peer.name)),
            }
        } else if peer.stale_keepalive {
            if let Err(error) = run_command(
                "wg",
                &[
                    "set",
                    "wg0",
                    "peer",
                    peer.public_key.as_str(),
                    "persistent-keepalive",
                    "0",
                ],
            ) {
                errors.push(format!("{} 的旧 keepalive 清不掉：{error}", peer.name));
            }
        }
    }

    if removed > 0 {
        let stripped = stripped_conf.to_string_lossy();
        if let Err(error) = run_command("wg", &["syncconf", "wg0", stripped.as_ref()]) {
            errors.push(format!(
                "摘掉 {removed} 个 stale peer 后，完整配置同步不回来：{error}"
            ));
        }
    }

    if errors.is_empty() {
        Ok(drift.len())
    } else {
        Err(errors.join("；"))
    }
}

/// Ping the doubtful ones to settle their fate.
///
/// The ping is both the test and the trigger: wg handshakes only when it has data
/// to send, so an answered ping establishes that the tunnel works and an unanswered
/// one establishes a failure. Only suspect peers are probed, since a peer with a
/// fresh handshake needs no packets.
fn settle_suspects(checks: &mut [PeerCheck]) {
    let targets = checks
        .iter()
        .filter(|check| check.state == PeerState::Suspect)
        .filter_map(|check| check.overlay_ip.clone())
        .collect::<Vec<_>>();
    apply_reachability(checks, &ping_all(&targets));
}

/// Turn probe results into states. Separate from sending packets so it can be
/// tested: an error here costs an interface, and treating unprobeable as
/// unreachable is not visible in review and only fails on a real machine.
pub(crate) fn apply_reachability(
    checks: &mut [PeerCheck],
    reachable: &BTreeMap<String, Result<bool, String>>,
) {
    for check in checks.iter_mut() {
        if check.state != PeerState::Suspect {
            continue;
        }
        let age = handshake_age_text(check.handshake_age);
        check.state = match check.overlay_ip.as_ref().and_then(|ip| reachable.get(ip)) {
            Some(Ok(true)) => PeerState::Alive,
            Some(Ok(false)) => PeerState::Down {
                detail: format!(
                    "{age}，且 {} 探不通——隧道断了",
                    check.overlay_ip.as_deref().unwrap_or("?")
                ),
            },
            // Unprobeable is not unreachable. See `probe_reachable`.
            Some(Err(reason)) => PeerState::Unprobed {
                reason: reason.clone(),
            },
            // A missing address is a config problem, unrelated to whether this
            // machine can probe, so this state is reachable.
            None => PeerState::Down {
                detail: format!("{age}，也查不到它的 overlay 地址"),
            },
        };
    }
}

// ---------------------------------------------------------------------------
// The WireGuard link watchdog: wg will not redial on its own.
//
// wg handshakes only when it has data to send, retrying every 5 seconds for 90
// seconds (`REKEY_ATTEMPT_TIME`) before stopping and starting over on the next
// data, with every attempt aimed at the same Endpoint. An incorrect Endpoint
// therefore leaves the link permanently down while the kernel reports no problem:
// the interface exists, the private key is loaded, the routes are installed, and
// `ip link show wg0` shows a normal interface. Those are the two conditions
// `reconcile_local` previously checked — whether wg0 is present and whether the MTU
// is correct — and under this failure both pass, so local reconcile succeeds, the
// control plane answers 204, and the link stays down with nothing dialing again.
//
// The Endpoint does become incorrect: on receiving any decryptable packet, wg
// overwrites that peer's Endpoint with the packet's source address, which is
// roaming, so a peer behind phantun is moved off the loopback port onto the far
// side's public UDP address, where inbound is blocked and the handshake time stays
// at 0. Roaming cannot be disabled, and wg does not restore the configured value.
// The IR can avoid generating dial directions that cause roaming, but it cannot
// repair a machine that has already drifted.
//
// The test is therefore the link itself (`check_wg_peers`), and repair escalates,
// each rung more disruptive than the last and each given a full round to take
// effect:
//
//   0. Endpoint points at a local phantun port with no listener → the cause is one
//      layer down; replay phantun
//   1. Endpoint disagrees with the config → rewrite it with `wg set`. The most
//      common cause, and the cheapest remedy
//   2. `wg syncconf` replays the config → restore lost peers and hand-edited
//      parameters
//   3. Bounce the interface (down/up) → replace the UDP socket, curing "this
//      machine's egress address changed"
//   4. Rebuild the interface → only once every peer is dead
//
// The bar for rung 4 is not caution but arithmetic: deleting wg0 takes the overlay
// address with it, and the backbone relay inbound listens on that very address
// (the same reason xray must stop before wg0 is withdrawn). Wagering the links
// that still work on one dead link does not pay; a machine where everything is
// dead has little left to lose.
// ---------------------------------------------------------------------------

// One number cannot serve both purposes.
// How often to check and how long the last remedy is given both used a single 60
// seconds, so shortening the detection interval also shortened the escalation
// interval, even though the two are bounded by different things. Split apart, each
// has its own basis:
//
// - Detection runs on its own thread (`WG_GUARD_INTERVAL`, 5 seconds). While
//   healthy, a round is one `wg show wg0 dump` and not a single packet.
// - Escalation goes by how long the fault has persisted, so looking more often
//   never brings it forward.

/// How often to look.
///
/// This number only matters for the two failures visible at a glance in the dump:
/// an Endpoint overwritten by roaming, and a peer dropped from the runtime. Both
/// are permanent, wg recovers from neither, and detecting them 5 seconds sooner
/// repairs them 5 seconds sooner.
///
/// Reachability cannot be evaluated this frequently; see `HANDSHAKE_STALE_SECS`,
/// whose floor is wg's own retry window rather than the check interval.
pub(crate) const WG_GUARD_INTERVAL: Duration = Duration::from_secs(5);

/// The observation window after acting. wg retries handshakes every 5 seconds
/// (`REKEY_TIMEOUT`), so a link the remedy repaired rehandshakes within about a
/// dozen seconds. This number is the time granted to that, not a rate limit on the
/// watchdog.
pub(crate) const WG_REMEDY_DWELL: u64 = 20;

/// How long the fault must persist before bouncing the interface is allowed. The
/// earlier remedies (replay phantun, push the Endpoint back, syncconf) are all
/// idempotent and do not interrupt live sessions; from this rung on, every peer's
/// session is cut, so the cheap measures deserve a few chances first.
const WG_BOUNCE_AFTER: u64 = 45;

/// How long the fault must persist before rebuilding the interface is allowed.
const WG_REBUILD_AFTER: u64 = 120;

/// Ceiling on the doubling backoff once the ladder is exhausted and nothing helped.
///
/// This was a fixed 30-minute cooldown after a rebuild, chosen without a basis. The
/// reasoning is that what restores a link is the ping each round rather than these
/// remedies: the ping makes wg handshake, and once the far side returns the link
/// recovers with no remedy involved. The remedies only correct incorrect local
/// state, such as a drifted Endpoint, a dropped peer, or a socket bound to an old
/// address, so once all of them have been tried, repeating them unchanged rarely
/// helps.
///
/// So it grows lazier without stopping: this machine's egress address changing once
/// more still deserves rescue. The backoff sequence 20→60→120→240→480→900 retries
/// at 2, 3, 5, 9, and 17 minutes, then every 15. That is far more eager than the old
/// 30-minute cooldown over the first five minutes, and still fewer actions per day.
pub(crate) const WG_BACKOFF_MAX: u64 = 15 * 60;

/// How often to restate a link that is still down with nothing left to try. With
/// detection at 5 seconds a round, logging plainly would give an unfixable link
/// fifteen thousand lines a day, drowning the few that carry information.
const WG_DOWN_LOG_EVERY: u64 = 300;

// All of the watchdog's timing uses `Instant`, not unix seconds.
// The wall clock jumps: on a newly booted machine, or a VM waking from suspend, NTP
// can move time forward by hours in one step. `now - down_since` then produces a
// very large value and the ladder goes straight to its last rung, and the first
// seconds after boot are when no peer has handshaked yet, so a boot followed by one
// NTP correction deletes the wg0 that was just created. A backward jump likewise
// clears the backoff.
//
// The one thing that cannot use the monotonic clock is handshake age: `wg` reports
// that in unix time, so only the wall clock can subtract from it.
struct WgGuard {
    /// When this run of consecutive failures began; `None` means the last round was
    /// healthy. The ladder is computed from it and cleared on recovery, so one
    /// period of failure does not determine the remedy order for the next.
    pub(crate) down_since: Option<Instant>,
    /// When the watchdog last acted. The observation window starts here; `None`
    /// means nothing has been tried yet and action may be taken immediately.
    pub(crate) acted_at: Option<Instant>,
    /// The current observation window. `WG_REMEDY_DWELL` while climbing the ladder;
    /// once the top is reached without recovery, it starts doubling.
    pub(crate) dwell: u64,
    /// When the down peers were last listed one by one, and which ones that was.
    ///
    /// Names rather than a count: when one peer recovers as another fails, the
    /// count is unchanged, and that round is the one most worth reporting.
    pub(crate) logged_at: Option<Instant>,
    pub(crate) logged_peers: Vec<String>,
}

impl Default for WgGuard {
    fn default() -> Self {
        Self {
            down_since: None,
            acted_at: None,
            dwell: WG_REMEDY_DWELL,
            logged_at: None,
            logged_peers: Vec::new(),
        }
    }
}

/// `None`, meaning it never happened, always counts as long past, so the first
/// occurrence may always act and always report.
fn elapsed_at_least(mark: Option<Instant>, secs: u64) -> bool {
    mark.is_none_or(|mark| mark.elapsed().as_secs() >= secs)
}

static WG_GUARD: Mutex<Option<WgGuard>> = Mutex::new(None);

/// Exclusive lock over the backbone layer (wg0, and the phantun beneath it).
///
/// The watchdog runs on its own thread and convergence on the apply loop, and both
/// modify this interface; a `wg syncconf` concurrent with an `ip link del` has
/// undefined results. This is the same class of problem as `meter` serializing
/// xray's counters, except that one contends over reads and this one over writes.
///
/// A process-wide static rather than a threaded-through parameter: one-shot commands
/// like `repair` and `apply-once` run the same convergence code, and threading it
/// through would mean inventing a lock they never use.
static BACKBONE: Mutex<()> = Mutex::new(());

pub(crate) fn backbone_lock() -> std::sync::MutexGuard<'static, ()> {
    BACKBONE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Do one thing with the watchdog's state. The state sits behind a small in-process
/// lock; do not run commands while holding it.
fn with_wg_guard<T>(act: impl FnOnce(&mut WgGuard) -> T) -> T {
    let mut guard = WG_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    act(guard.get_or_insert_with(WgGuard::default))
}

/// Record that an action was taken, and set how long until the next one.
///
/// One invariant: every remedy short of the top rung is rechecked at the shortest
/// interval, because the ladder still has a next move and whether the current
/// remedy worked is worth learning early. The doubling backoff begins only at the
/// top rung.
fn mark_acted(at_top: bool) {
    with_wg_guard(|guard| {
        guard.acted_at = Some(Instant::now());
        guard.dwell = if at_top {
            (guard.dwell * 2).clamp(60, WG_BACKOFF_MAX)
        } else {
            WG_REMEDY_DWELL
        };
    });
}

/// One watchdog round. `BACKBONE` is held throughout, because the result depends on
/// the current runtime state and convergence changing it midway would leave the
/// remedies acting on a stale observation. In the worst case convergence has just
/// written a new config and the watchdog deletes the interface based on the previous
/// evaluation.
///
/// The cost is that convergence may wait one round. In the healthy case a round is
/// one `wg show wg0 dump`, taking a few milliseconds; with peers down it hits ping
/// timeouts, and with the post-remedy poke the upper bound is about 6 seconds. Those
/// 6 seconds occur only on a machine whose backbone is already down, where letting
/// the watchdog finish first is the correct order.
pub(crate) fn guard_wireguard(state_dir: &Path, conf: &Path) {
    let _backbone = backbone_lock();

    let checks = match check_wg_peers(conf) {
        Ok(checks) => checks,
        Err(error) => {
            warn(format!(
                "wireguard: 读不出 wg0 的运行态，这一轮判不了链路死活：{error}"
            ));
            return;
        }
    };
    // No peer in the config: this machine is not in the backbone and has no link to
    // guard. The state is cleared with it, because a machine that goes from having
    // peers to having none, after being decommissioned or moved out of the backbone,
    // would otherwise keep the old fault origin, and adding peers back later would
    // carry an origin from days earlier straight to the last rung.
    if checks.is_empty() {
        with_wg_guard(|guard| *guard = WgGuard::default());
        return;
    }

    // An unprobeable peer stops here and never becomes a down peer. Without an ICMP
    // socket on this machine, whether an expired handshake means unreachable cannot
    // be determined, and every remedy has a cost. Deleting an interface based on an
    // unknown state is the most damaging error this code can make.
    let unprobed = checks
        .iter()
        .filter(|check| matches!(check.state, PeerState::Unprobed { .. }))
        .collect::<Vec<_>>();
    if !unprobed.is_empty() && with_wg_guard(|g| elapsed_at_least(g.logged_at, WG_DOWN_LOG_EVERY)) {
        with_wg_guard(|guard| guard.logged_at = Some(Instant::now()));
        if let Some(PeerState::Unprobed { reason }) = unprobed.first().map(|c| &c.state) {
            warn(format!(
                "wireguard: {} 个对端握手过期了，但这台机器上探不了活，判不了死活，\
                 链路自愈这一轮什么都做不了：{reason}",
                unprobed.len()
            ));
        }
    }

    let down = checks
        .iter()
        .filter(|check| matches!(check.state, PeerState::Down { .. }))
        .collect::<Vec<_>>();
    let drifted = checks
        .iter()
        .filter(|check| check.endpoint_drift.is_some() && drift_is_fatal(check))
        .collect::<Vec<_>>();
    let stray_pending = checks.iter().any(|check| check.stray_endpoint.is_some());

    if down.is_empty() && drifted.is_empty() && !stray_pending {
        // All reachable: clear the fault origin and reel the backoff back in, so the
        // next fault climbs the ladder from the start at the fastest cadence.
        with_wg_guard(|guard| {
            guard.down_since = None;
            guard.dwell = WG_REMEDY_DWELL;
            guard.logged_peers.clear();
        });
        return;
    }

    // Record what is broken before selecting a remedy. Every branch below returns,
    // and logging inside the branches would leave some paths repairing state with no
    // log line, which is the outcome self-healing must avoid.
    //
    // But not every round restates it: detection runs every 5 seconds, and logging an
    // unfixable link plainly would give fifteen thousand lines a day. The moment it
    // breaks, any change in which peers are down, and once every 5 minutes is
    // enough.
    let down_names = down
        .iter()
        .map(|check| check.name.clone())
        .collect::<Vec<_>>();
    let (down_since, dwell_ok, should_log) = with_wg_guard(|guard| {
        // The origin depends only on whether any peer is down. It cannot be cleared
        // in the all-reachable branch alone: when a peer recovers as an Endpoint
        // drifts, that branch is not reached, the origin keeps its previous value,
        // and the next fault starts at the rebuild rung.
        if down.is_empty() {
            guard.down_since = None;
            guard.dwell = WG_REMEDY_DWELL;
        } else if guard.down_since.is_none() {
            guard.down_since = Some(Instant::now());
        }
        let should_log = guard.logged_peers != down_names
            || elapsed_at_least(guard.logged_at, WG_DOWN_LOG_EVERY);
        if should_log {
            guard.logged_at = Some(Instant::now());
            guard.logged_peers.clone_from(&down_names);
        }
        (
            guard
                .down_since
                .map(|at| at.elapsed().as_secs())
                .unwrap_or(0),
            elapsed_at_least(guard.acted_at, guard.dwell),
            should_log,
        )
    });
    if should_log {
        for check in &down {
            if let PeerState::Down { detail } = &check.state {
                warn(format!("wireguard: {} {detail}", check.name));
            }
        }
    }

    // The last remedy is still inside its observation window, so this round checks
    // and does not act. This is what the previous 60-second value provided, and it is
    // independent of the check interval.
    if !dwell_ok {
        return;
    }

    // There is a rung before the first: the passive side drifted to a public address.
    // These peers have no Endpoint in the config, because the far side dials, so
    // there is no value to rewrite. The only remedy is to remove the peer from the
    // runtime so it discards the incorrectly learned address, then replay the full
    // config. Reconstructing it here from one AllowedIP would silently lose future
    // fields such as a preshared key. Without the removal it never recovers: every
    // few seconds it sends to that public address, which cannot get through and drags
    // the far side's Endpoint along by roaming, each machine pulling the other back
    // into a dead end.
    let stray = checks
        .iter()
        .filter(|check| check.stray_endpoint.is_some())
        .collect::<Vec<_>>();
    if !stray.is_empty() {
        let mut removed_any = false;
        for check in &stray {
            let Some(live) = &check.stray_endpoint else {
                continue;
            };
            warn(format!(
                "wireguard: {} 的 Endpoint 漂成了 {live}，可它是从本机 phantun 服务端进来的，\
                 源地址只可能是回环——配置里没给它写 Endpoint，顶不回去，只能摘掉重学",
                check.name
            ));
            let removed = run_command(
                "wg",
                &["set", "wg0", "peer", check.public_key.as_str(), "remove"],
            );
            if let Err(error) = removed {
                warn(format!("wireguard: {} 摘不掉：{error}", check.name));
                continue;
            }
            removed_any = true;
        }
        if removed_any {
            if let Err(error) = apply_wireguard(conf) {
                warn(format!(
                    "wireguard: 摘掉 stale peer 后完整配置同步不回来：{error}"
                ));
            }
        }
        mark_acted(false);
        poke(&stray);
        return;
    }

    // The first rung comes before judging liveness: at the instant roaming overwrites
    // an Endpoint the handshake is still fresh (it was made before the drift), and
    // waiting for it to go stale takes three minutes, during which every packet over
    // this link is lost.
    if !drifted.is_empty() {
        for check in &drifted {
            let Some((want, live)) = &check.endpoint_drift else {
                continue;
            };
            warn(format!(
                "wireguard: {} 的 Endpoint 变成了 {live}，配置里是 {want}——wg 收到能解密的\
                 报文就拿源地址覆盖 Endpoint（漫游），而它自己永远不会改回来。正在顶回去",
                check.name
            ));
            if let Err(error) = run_command(
                "wg",
                &[
                    "set",
                    "wg0",
                    "peer",
                    check.public_key.as_str(),
                    "endpoint",
                    want.as_str(),
                ],
            ) {
                warn(format!(
                    "wireguard: {} 的 Endpoint 顶不回去：{error}",
                    check.name
                ));
            }
        }
        mark_acted(false);
        poke(&drifted);
        return;
    }

    // Rung zero: when the cause is one layer down, do not act on wg. For a peer
    // behind phantun, wg dials local loopback and the phantun client forwards the
    // packets outward. With no listener on that port, restarting wg0 has no effect,
    // and the symptom is a correct configuration whose handshake never completes.
    let stalled = down
        .iter()
        .filter(|check| local_endpoint_dead(check))
        .map(|check| check.name.as_str())
        .collect::<Vec<_>>();
    if !stalled.is_empty() {
        warn(format!(
            "wireguard: {} 的 Endpoint 指着本机 phantun 的口，而那个口没人在听——\
             病根在 phantun 那一层，正在重放它",
            stalled.join("、")
        ));
        match fs::read_to_string(state_dir.join("phantun.json")) {
            // Local reconcile does not contact the control plane and has no
            // distribution source; the binary was installed by the last release.
            Ok(content) => {
                if let Err(error) = apply_phantun(&content, None) {
                    warn(format!("wireguard: phantun 重放失败：{error}"));
                }
            }
            Err(error) => warn(format!("wireguard: 读不到 phantun.json，重放不了：{error}")),
        }
        mark_acted(false);
        poke(&down);
        return;
    }

    let down_for = down_since;
    let rung = pick_rung(down_for, down.len(), checks.len());
    mark_acted(rung.is_top());

    match rung {
        Rung::Syncconf => {
            warn(format!(
                "wireguard: {} 个对端不通（已 {down_for} 秒）——重放 wireguard.conf（wg syncconf），\
                 补回丢掉的 peer、改回被动过的参数",
                down.len()
            ));
            replay_wireguard(conf, &down);
        }
        Rung::Bounce => {
            warn(format!(
                "wireguard: 重放没救回来，不通已 {down_for} 秒——弹一下 wg0，换掉那个 UDP socket，\
                 治「本机出口地址变了」"
            ));
            if let Err(error) = run_shell("ip link set wg0 down && ip link set wg0 up") {
                warn(format!("wireguard: wg0 弹不动：{error}"));
            }
            replay_wireguard(conf, &down);
        }
        Rung::Rebuild => {
            warn(format!(
                "wireguard: {} 个对端全都不通，已 {down_for} 秒——重建 wg0",
                down.len()
            ));
            let _ = run_shell("ip link del wg0 2>/dev/null || true");
            replay_wireguard(conf, &down);
        }
        Rung::SpareTheLive => warn(format!(
            "wireguard: {} 个对端不通、{} 个还通着，已 {down_for} 秒没治好。不重建 wg0——\
             删接口会把 overlay 地址一起带走，而中转 inbound 就 listen 在那个地址上；\
             拿一条死链路去赌还活着的那些不划算。这里需要人看一眼",
            down.len(),
            checks.len() - down.len()
        )),
    }
}

/// Which remedy this round calls for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rung {
    /// Replay the config. Idempotent, and does not interrupt live sessions.
    Syncconf,
    /// Bounce the interface. Cuts every peer's session.
    Bounce,
    /// Rebuild the interface. Takes the overlay address down for an instant.
    Rebuild,
    /// Some peers still work; do not gamble with them.
    SpareTheLive,
}

impl Rung {
    /// At the top rung: every local remedy has been applied, another pass rarely
    /// helps, and backoff begins. `SpareTheLive` also counts, because it means the
    /// ladder is exhausted while the interface may not be deleted, which likewise
    /// has no next move.
    pub(crate) fn is_top(&self) -> bool {
        matches!(self, Rung::Rebuild | Rung::SpareTheLive)
    }
}

/// The rung is selected by how long the fault has persisted rather than by round
/// number, so shortening the detection interval to 5 seconds does not advance the
/// more disruptive rungs.
///
/// It is a standalone pure function because these thresholds are the numbers in the
/// whole watchdog most likely to be casually adjusted and most easily broken: lower
/// them a little and a machine whose peer is powered off starts tearing down its own
/// overlay address every minute.
pub(crate) fn pick_rung(down_for: u64, down: usize, total: usize) -> Rung {
    if down_for < WG_BOUNCE_AFTER {
        return Rung::Syncconf;
    }
    if down_for < WG_REBUILD_AFTER {
        return Rung::Bounce;
    }
    // Last rung: touch the interface only when everything is dead. The repeat cadence
    // is the backoff's business, not decided here.
    if down < total {
        return Rung::SpareTheLive;
    }
    Rung::Rebuild
}

/// Whether this Endpoint drift must be pushed back at once, or can wait to be cured
/// along with the link going stale.
///
/// For a peer behind phantun the configured Endpoint is local loopback. Once roaming
/// overwrites it with the peer's public UDP address, packets bypass phantun and
/// reach a port whose inbound UDP is blocked, so the link stops carrying traffic.
///
/// Every other drift is corrected only once the link is already down. A dual-stack
/// peer dialing in from the other family is ordinary roaming, and rewriting the
/// endpoint on a working link produces a loop, where the agent rewrites it every
/// round and the peer roams every round, and the endpoint being rewritten is often
/// the reason it roamed.
///
/// The test is `Down` rather than not-Fresh. `Alive` has been confirmed by ping, so
/// that link passes packets and has only not rehandshaked while idle, which is not a
/// failure. Lowering `HANDSHAKE_STALE_SECS` from 180 to 120 made this distinction
/// significant, because more healthy idle links now reach `Alive`.
pub(crate) fn drift_is_fatal(check: &PeerCheck) -> bool {
    if matches!(check.state, PeerState::Down { .. }) {
        return true;
    }
    check
        .want_endpoint
        .as_deref()
        .and_then(|want| want.parse::<SocketAddr>().ok())
        .is_some_and(|addr| addr.ip().is_loopback())
}

/// This peer's Endpoint points at a local phantun port with no listener.
fn local_endpoint_dead(check: &PeerCheck) -> bool {
    let Some(addr) = check
        .want_endpoint
        .as_deref()
        .and_then(|want| want.parse::<SocketAddr>().ok())
    else {
        return false;
    };
    // Undeterminable (`None`) counts as not broken: this rung replays phantun, and
    // doing nothing is preferable.
    addr.ip().is_loopback() && udp_bound(addr.port()) == Some(false)
}

fn replay_wireguard(conf: &Path, down: &[&PeerCheck]) {
    if let Err(error) = apply_wireguard(conf) {
        warn(format!("wireguard: 重放配置失败：{error}"));
        return;
    }
    poke(down);
}

/// Send traffic after a remedy. A new handshake starts only when there is data to
/// send, so without this the result of the remedy is only observable once user
/// traffic arrives, while the next round's test is the handshake time itself.
fn poke(peers: &[&PeerCheck]) {
    let ips = peers
        .iter()
        .filter_map(|peer| peer.overlay_ip.clone())
        .collect::<Vec<_>>();
    let _ = ping_all(&ips);
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use serde_json::json;

    use super::{
        handshake_age_text, parse_wg_dump, parse_wireguard_conf_peers, peers_via_phantun_server,
        phantun_passive_drift, wireguard_conf_peers, PhantunPassiveDrift,
    };

    fn state_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("brocade-agent-wg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A peer arriving through this machine's phantun server can only have a loopback
    /// Endpoint. If this list is wrong, the liveness step treats a correct loopback
    /// Endpoint as roaming and rewrites it to a private address.
    #[test]
    fn peers_arriving_through_our_phantun_server_are_listed() {
        let dir = state_dir("via-phantun");
        fs::write(
            dir.join("phantun.json"),
            json!({
                "servers": [
                    { "tcp_port": 39743, "peers": ["hk-01", "sg-01"] },
                    { "tcp_port": 39744, "peers": ["jp-01"] },
                ],
            })
            .to_string(),
        )
        .unwrap();

        let found = peers_via_phantun_server(&dir);
        assert_eq!(found.len(), 3);
        assert!(found.contains("hk-01"));
        assert!(found.contains("jp-01"));

        let _ = fs::remove_dir_all(dir);
    }

    /// With phantun disabled the list must empty immediately. Kept, those peers'
    /// Endpoints have already reverted to public addresses while the liveness step
    /// still expects loopback, and every peer on the machine that ever went through
    /// phantun is declared drifted at once.
    #[test]
    fn the_disabled_marker_empties_the_list_even_if_the_plan_is_still_there() {
        let dir = state_dir("via-phantun-disabled");
        fs::write(
            dir.join("phantun.json"),
            json!({ "servers": [{ "tcp_port": 39743, "peers": ["hk-01"] }] }).to_string(),
        )
        .unwrap();
        assert_eq!(peers_via_phantun_server(&dir).len(), 1);

        fs::write(dir.join("phantun.disabled"), "上游没封 UDP").unwrap();
        assert!(
            peers_via_phantun_server(&dir).is_empty(),
            "停用之后就不该再有人算作走 phantun 进来"
        );

        let _ = fs::remove_dir_all(dir);
    }

    /// An unreadable file yields an empty list rather than an error. This step only
    /// relaxes the test, so its absence reports at most one extra drift, whereas
    /// aborting the liveness round would leave the machine unmonitored.
    #[test]
    fn an_unreadable_plan_yields_an_empty_list() {
        let dir = state_dir("via-phantun-broken");
        assert!(peers_via_phantun_server(&dir).is_empty(), "文件不在");

        fs::write(dir.join("phantun.json"), "{ 不是 JSON").unwrap();
        assert!(peers_via_phantun_server(&dir).is_empty(), "解析不了");

        fs::write(
            dir.join("phantun.json"),
            json!({ "servers": [{ "tcp_port": 39743 }] }).to_string(),
        )
        .unwrap();
        assert!(
            peers_via_phantun_server(&dir).is_empty(),
            "没有 peers 那一格"
        );

        let _ = fs::remove_dir_all(dir);
    }

    /// The older singular `server` is accepted here as well, for the same reason as
    /// on phantun's side: failing to read what was written before the upgrade would
    /// empty this list with no error.
    #[test]
    fn the_old_singular_server_spelling_is_read_here_too() {
        let dir = state_dir("via-phantun-old");
        fs::write(
            dir.join("phantun.json"),
            json!({ "server": { "tcp_port": 39743, "peers": ["hk-01"] } }).to_string(),
        )
        .unwrap();

        assert!(peers_via_phantun_server(&dir).contains("hk-01"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Apply-time cleanup is narrower than "every peer with no Endpoint": normal
    /// passive WireGuard peers keep their learned public address, a healthy phantun
    /// peer keeps its loopback address, and an old keepalive can be cleared without
    /// removing that healthy peer.
    #[test]
    fn apply_cleanup_selects_only_stale_phantun_passive_state() {
        let conf = parse_wireguard_conf_peers(
            "[Peer]\n# stale\nPublicKey = STALE=\nAllowedIPs = 10.66.0.2/32\n\
             [Peer]\n# local\nPublicKey = LOCAL=\nAllowedIPs = 10.66.0.3/32\n\
             [Peer]\n# keepalive\nPublicKey = KEEP=\nAllowedIPs = 10.66.0.4/32\n\
             [Peer]\n# ordinary\nPublicKey = PLAIN=\nAllowedIPs = 10.66.0.5/32\n",
        );
        let live = parse_wg_dump(
            "STALE=\t(none)\t203.0.113.7:51820\t10.66.0.2/32\t1\t1\t1\t25\n\
             LOCAL=\t(none)\t127.0.0.1:41000\t10.66.0.3/32\t1\t1\t1\toff\n\
             KEEP=\t(none)\t127.0.0.1:41001\t10.66.0.4/32\t1\t1\t1\t25\n\
             PLAIN=\t(none)\t198.51.100.8:51820\t10.66.0.5/32\t1\t1\t1\t25\n",
        );
        let via = ["stale", "local", "keepalive"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        assert_eq!(
            phantun_passive_drift(&conf, &live, &via),
            vec![
                PhantunPassiveDrift {
                    name: "stale".to_owned(),
                    public_key: "STALE=".to_owned(),
                    stale_endpoint: true,
                    stale_keepalive: true,
                },
                PhantunPassiveDrift {
                    name: "keepalive".to_owned(),
                    public_key: "KEEP=".to_owned(),
                    stale_endpoint: false,
                    stale_keepalive: true,
                },
            ]
        );
    }

    /// A missing config file means no peers, not a crash. A machine whose first
    /// convergence has not run yet is in exactly this state.
    #[test]
    fn a_missing_conf_file_means_no_peers() {
        let dir = state_dir("conf-missing");
        assert!(wireguard_conf_peers(&dir.join("wireguard.conf")).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// A peer that never handshook and one that handshook long ago are reported
    /// separately. Combined into one message, a newly installed machine that never
    /// connected and one that connected and then failed are identical in the log.
    #[test]
    fn never_handshaken_reads_differently_from_a_stale_handshake() {
        assert_eq!(handshake_age_text(None), "从未握手成功");
        assert_eq!(handshake_age_text(Some(0)), "握手 0 秒前");
        assert_eq!(handshake_age_text(Some(300)), "握手 300 秒前");
    }
}
