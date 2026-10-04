//! The probing family: path MTU, relay-hop liveness, and end-to-end probe
//! scheduling. All three measure and report without changing any state on the
//! machine — entirely apart from the convergence path, which is why each runs on
//! its own thread (see `run_forever` in main.rs).
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use brocade_deployment::protocol::{
    E2eExitVerdict, E2eProbeRequest, E2eProbeStatus, E2eProbeTargetList, LinkHealth,
    LinkHealthRequest, LinkProbe, LinkProbeRequest, LinkProbeStatus, PingProbeEndpoint,
    PingProbeFamily, PingProbeReportRequest, PingProbeSample, PingProbeSkipReason,
    PingProbeTargetsResponse, ProbeTargetList, ProbeTransport,
};

use crate::{current_unix_secs, e2e, http::HttpClient, icmp, options::Options};

// Search interval. The 1500 ceiling is the ordinary Ethernet value; jumbo frames
// essentially do not occur on the public internet, so extra rounds spent on them
// do not pay. The 1000 floor matches store's overlay_mtu check — anything lower
// is almost certainly a misconfiguration rather than a genuinely narrow link.
pub(crate) const PROBE_MTU_MIN: u16 = 1000;
pub(crate) const PROBE_MTU_MAX: u16 = 1500;

/// Binary-search the path MTU toward one underlay host.
///
/// First an ordinary ping for reachability, then a bisection with DF set. The two
/// steps cannot merge: undistinguished, "the machine is powered off" and "the
/// link's MTU is small" reach the same conclusion, and the latter has the
/// operator drop the MTU network-wide.
pub(crate) fn probe_path_mtu(
    host: &str,
    transport: ProbeTransport,
) -> (LinkProbeStatus, Option<u16>, Option<u16>) {
    let pinger = match icmp::Pinger::open(host) {
        Ok(pinger) => pinger,
        // Failing to open a socket is our problem, not the link's. Report
        // Unsupported rather than passing it off as "peer unreachable" or "ICMP
        // filtered", which sends someone hunting a fault that does not exist.
        Err(error) => {
            eprintln!("probe: {host} 探不了：{error}");
            return (LinkProbeStatus::Unsupported, None, None);
        }
    };
    let (status, path_mtu) = probe_path_mtu_with(
        || pinger.reachable(),
        |mtu| matches!(pinger.fits(mtu), Ok(icmp::Probe::Fits)),
    );
    // How much to subtract is set by the endpoint, not a constant: on a
    // dual-stack endpoint wg may take v6, where the outer IP header costs 20
    // bytes more (`icmp::Pinger::wireguard_overhead`).
    let suggested = path_mtu.map(|mtu| mtu.saturating_sub(pinger.wireguard_overhead(transport)));
    (status, path_mtu, suggested)
}

/// The testable shape of the above: reachability and per-size fit come in as
/// parameters. Testing the search itself should not require standing up a machine
/// with an unusual MTU — that needs root, will not run under `cargo test`, and
/// the outcome is that this code never executes in a test at all.
pub(crate) fn probe_path_mtu_with(
    reachable: impl Fn() -> bool,
    fits: impl Fn(u16) -> bool,
) -> (LinkProbeStatus, Option<u16>) {
    if !reachable() {
        return (LinkProbeStatus::Unreachable, None);
    }
    if !fits(PROBE_MTU_MIN) {
        return (LinkProbeStatus::Blocked, None);
    }
    if fits(PROBE_MTU_MAX) {
        return (LinkProbeStatus::Ok, Some(PROBE_MTU_MAX));
    }

    // Invariant: low always fits, high never does. On convergence low is the
    // answer.
    let mut low = PROBE_MTU_MIN;
    let mut high = PROBE_MTU_MAX;
    while high - low > 1 {
        let probe = low + (high - low) / 2;
        if fits(probe) {
            low = probe;
        } else {
            high = probe;
        }
    }
    (LinkProbeStatus::Ok, Some(low))
}

// Last round's readings for liveness. Held in process memory rather than on
// disk: after a restart the first round has no baseline, and reporting "unknown"
// is more honest than reporting "dead" — a freshly started agent should not paint
// the whole link map red.
static HOP_BASELINE: Mutex<Option<(u64, BTreeMap<String, u64>)>> = Mutex::new(None);

/// Cumulative downlink counters for every forwarding outbound, read from xray.
///
/// Tags look like `outbound>>>out:{app}/{chain}>{to}>>>traffic>>>downlink`. Only
/// forwarding outbounds count; `out:egress` and `out:block` do not — the first
/// always carries traffic (so it measures no relay liveness), the second never
/// does.
pub(crate) fn read_hop_downlinks(api_port: u16) -> Result<BTreeMap<String, u64>, String> {
    Ok(hop_downlinks_from_stats(&crate::xray_grpc::query_stats(
        api_port,
        "outbound>>>out:",
    )?))
}

pub(crate) fn hop_downlinks_from_stats(
    stats: &[crate::xray_grpc::XrayStat],
) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for stat in stats {
        let Ok(value) = u64::try_from(stat.value) else {
            continue;
        };
        let Some(rest) = stat.name.strip_prefix("outbound>>>") else {
            continue;
        };
        let Some((tag, "traffic>>>downlink")) = rest.split_once(">>>") else {
            continue;
        };
        // A forwarding outbound's tag reads out:{app}/{chain}>{to}; that '>' is
        // what separates it from egress/block
        if hop_of_tag(tag).is_some() {
            out.insert(tag.to_owned(), value);
        }
    }
    out
}

/// `out:{app}/{chain}>{to}` → (app/chain, to). Remote listener references use
/// `out:{app}/{source-chain}~{listener-chain}>{to}`; the target-listener suffix distinguishes
/// two ports on one peer in Xray while telemetry remains keyed to the source chain and peer node.
/// A suffix ending in `@local` is a same-process loopback reference, not a link, and is omitted.
/// `None` for anything else (egress/block/local listener).
fn hop_of_tag(tag: &str) -> Option<(&str, &str)> {
    let (chain, peer) = tag.strip_prefix("out:")?.split_once('>')?;
    let Some((source, target)) = chain.split_once('~') else {
        return Some((chain, peer));
    };
    (!target.ends_with("@local")).then_some((source, peer))
}

/// Compare this round's readings against the last to judge each hop's liveness.
///
/// The first round has no baseline and returns nothing — it does not report
/// everything dead. The counters are cumulative and reset when xray restarts, so
/// a value below the previous one is treated as a fresh start rather than
/// negative growth.
pub(crate) fn judge_hops(now: BTreeMap<String, u64>, at: u64) -> Option<LinkHealthRequest> {
    let mut guard = HOP_BASELINE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = guard.replace((at, now.clone()));
    let (previous_at, previous) = previous?;
    let window_secs = at.saturating_sub(previous_at);

    let hops = hop_health_deltas(&now, &previous);

    (!hops.is_empty()).then_some(LinkHealthRequest {
        checked_at_unix_secs: at as i64,
        window_secs,
        hops,
    })
}

/// Fold Xray's per-outbound counters into the control plane's per-chain/per-peer identity.
///
/// Listener references keep their target listener in the outbound tag, so two rules on one
/// source chain may legitimately produce two counters for the same peer machine. Sending both
/// rows would collide with the store's `(chain, peer)` key; keeping only one would make liveness
/// depend on tag ordering. Sum their deltas and report one physical link instead.
pub(crate) fn hop_health_deltas(
    now: &BTreeMap<String, u64>,
    previous: &BTreeMap<String, u64>,
) -> Vec<LinkHealth> {
    let mut totals = BTreeMap::<(String, String), u64>::new();
    for (tag, value) in now {
        let Some((chain_id, peer)) = hop_of_tag(tag) else {
            continue;
        };
        let before = previous.get(tag).copied().unwrap_or(0);
        // A drop means xray restarted and the counter reset; this round has no trustworthy delta,
        // so treat that outbound as no data while retaining any sibling outbound's valid delta.
        let delta = value.saturating_sub(before);
        *totals
            .entry((chain_id.to_owned(), peer.to_owned()))
            .or_default() += delta;
    }
    totals
        .into_iter()
        .map(|((chain_id, peer_node_id), downlink_bytes)| LinkHealth {
            chain_id,
            peer_node_id,
            alive: downlink_bytes > 0,
            downlink_bytes,
        })
        .collect()
}

pub(crate) fn fetch_probe_targets(options: &Options) -> Result<ProbeTargetList, String> {
    // Targets come from the control plane, not from the local wireguard.conf.
    // For a peer behind phantun that file's `Endpoint` is by design
    // `127.0.0.1:<local port>` — probing it measures our own loopback, reads
    // 65536, and the bisection hits the ceiling and reports a plausible-looking
    // 1500.
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/probe-targets", &options.token, None)?;
    if response.status != 200 {
        return Err(format!(
            "probe targets request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body).map_err(|error| error.to_string())
}

pub(crate) fn collect_link_probe_report(
    list: &ProbeTargetList,
) -> Result<Option<LinkProbeRequest>, String> {
    if list.targets.is_empty() {
        return Ok(None);
    }
    let links = list
        .targets
        .iter()
        .map(|target| {
            let (status, path_mtu, suggested_wg_mtu) =
                probe_path_mtu(&target.host, target.transport);
            LinkProbe {
                peer_node_id: target.peer_node_id.clone(),
                endpoint_host: target.host.clone(),
                status,
                path_mtu,
                suggested_wg_mtu,
            }
        })
        .collect();

    Ok(Some(LinkProbeRequest {
        probed_at_unix_secs: current_unix_secs()?,
        links,
    }))
}

pub(crate) fn send_link_probe_report(
    options: &Options,
    report: &LinkProbeRequest,
) -> Result<(), String> {
    // Print before reporting. Whoever is chasing an MTU problem is reading logs
    // on this machine; making them open the console to see the probe they just
    // ran hides the most useful step, and leaves nothing at all when the control
    // plane is unreachable.
    for link in &report.links {
        match (link.status, link.path_mtu, link.suggested_wg_mtu) {
            (LinkProbeStatus::Ok, Some(path), Some(wg)) => println!(
                "probe: {} 经 {} 路径 MTU {path}，建议 wg MTU {wg}",
                link.peer_node_id, link.endpoint_host
            ),
            (LinkProbeStatus::Unreachable, ..) => println!(
                "probe: {} 经 {} ping 不通",
                link.peer_node_id, link.endpoint_host
            ),
            (LinkProbeStatus::Unsupported, ..) => {
                println!("probe: 这台机器上开不了 ICMP socket，量不了路径 MTU（上面一行有原因）")
            }
            _ => println!(
                "probe: {} 经 {} ICMP 被挡，探不出路径 MTU",
                link.peer_node_id, link.endpoint_host
            ),
        }
    }
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/link-probe", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "link probe failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

pub(crate) fn probe_cycle(options: &Options) -> Result<(), String> {
    let targets = fetch_probe_targets(options)?;
    if let Some(report) = collect_link_probe_report(&targets)? {
        send_link_probe_report(options, &report)?;
    }
    Ok(())
}

/// Probe every chain once and report the results.
///
/// Fetching the work list and reporting results are separate endpoints, as in the
/// MTU family: the list is a function of the model (compiled on demand by the
/// control plane), the results are facts the machine observed.
pub(crate) fn fetch_e2e_probe_targets(options: &Options) -> Result<E2eProbeTargetList, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/e2e-targets", &options.token, None)?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "e2e targets request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body).map_err(|error| error.to_string())
}

pub(crate) fn collect_e2e_probe_report(
    list: &E2eProbeTargetList,
) -> Result<Option<E2eProbeRequest>, String> {
    // This machine heads no chain. Report nothing — an empty batch would
    // conflate "no chains to probe" with "probed them all and every one
    // failed".
    if list.targets.is_empty() {
        return Ok(None);
    }

    let chains = e2e::probe_all(list);
    for chain in &chains {
        match chain.status {
            E2eProbeStatus::Ok => println!(
                "e2e: {} 通，{}ms，出口 {}{}",
                chain.chain_id,
                chain.ttfb_ms.unwrap_or_default(),
                chain.exit_ip.as_deref().unwrap_or("?"),
                match chain.exit_verdict {
                    E2eExitVerdict::Match => "（对得上）",
                    E2eExitVerdict::Mismatch => "（对不上！流量没走完这条链）",
                    E2eExitVerdict::Unknown => "（未核对）",
                }
            ),
            _ => println!(
                "e2e: {} 不通：{}",
                chain.chain_id,
                chain.detail.as_deref().unwrap_or("没有细节")
            ),
        }
    }

    Ok(Some(E2eProbeRequest {
        probed_at_unix_secs: current_unix_secs()?,
        chains,
    }))
}

pub(crate) fn send_e2e_probe_report(
    options: &Options,
    request: &E2eProbeRequest,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/e2e-probe", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "e2e probe report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

pub(crate) fn e2e_cycle(options: &Options) -> Result<E2eProbeTargetList, String> {
    let list = fetch_e2e_probe_targets(options)?;
    if let Some(report) = collect_e2e_probe_report(&list)? {
        send_e2e_probe_report(options, &report)?;
    }
    Ok(list)
}

pub(crate) fn e2e_once(options: Options) -> Result<(), String> {
    e2e_cycle(&options).map(|_| ())
}

/// One endpoint to probe this round. A dual-stack target yields one probe per configured family;
/// a target from a Console older than the dual-stack protocol yields one probe without a family,
/// which is probed the pre-dual-stack way and reported without a family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedPingProbe {
    /// Operator-facing target name, used only in the node-local journal.
    pub(crate) name: String,
    /// Series address, reported back as `PingProbeSample::target`.
    pub(crate) address: String,
    pub(crate) family: Option<PingProbeFamily>,
}

/// The cached probing plan. The long-running agent keeps the last successful one so a slow
/// control plane cannot move the sampling clock; equality decides whether a refresh wakes the
/// sampler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PingProbePlan {
    pub(crate) probes: Vec<PlannedPingProbe>,
    pub(crate) interval_secs: u32,
    pub(crate) timeout_ms: u32,
}

impl PingProbePlan {
    fn from_response(response: PingProbeTargetsResponse) -> Self {
        match response {
            PingProbeTargetsResponse::Current(settings) => {
                let probes = settings
                    .targets
                    .iter()
                    .flat_map(|target| {
                        PingProbeFamily::ALL.into_iter().filter_map(move |family| {
                            let text = target.endpoint_text(family)?;
                            // The Console validated the endpoint. Should it still not parse, keep
                            // a slot under its raw text so the journal records why it was skipped.
                            let address = PingProbeEndpoint::parse(target.kind, text)
                                .map(|endpoint| endpoint.series_address(target.kind))
                                .unwrap_or_else(|_| format!("{}://{text}", target.kind.scheme()));
                            Some(PlannedPingProbe {
                                name: target.name.clone(),
                                address,
                                family: Some(family),
                            })
                        })
                    })
                    .collect();
                Self {
                    probes,
                    interval_secs: settings.interval_secs,
                    timeout_ms: settings.timeout_ms,
                }
            }
            PingProbeTargetsResponse::Legacy(settings) => Self {
                probes: settings
                    .targets
                    .into_iter()
                    .map(|target| PlannedPingProbe {
                        name: target.name,
                        address: target.address,
                        family: None,
                    })
                    .collect(),
                interval_secs: settings.interval_secs,
                timeout_ms: settings.timeout_ms,
            },
        }
    }
}

/// Fetch the fleet's active TCP and ICMP targets and turn them into this round's probes;
/// `ping-probe-once` composes this with collection and reporting below.
pub(crate) fn fetch_ping_probe_settings(options: &Options) -> Result<PingProbePlan, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/ping-probe-targets", &options.token, None)?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "PING probe targets request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    serde_json::from_str::<PingProbeTargetsResponse>(&response.body)
        .map(PingProbePlan::from_response)
        .map_err(|error| error.to_string())
}

/// Probe one cached plan and produce the compact report without contacting the control plane.
/// Every endpoint — each family of each target, TCP and ICMP alike — is dispatched independently
/// in parallel. DNS and local capability work finish before a probe starts its timer. The wire
/// report carries only attempted + latency + a coarse skip reason; detailed diagnostics stay in the
/// node-local journal and never include a resolved address or configured endpoint.
pub(crate) fn collect_ping_probe_report(
    plan: &PingProbePlan,
) -> Result<Option<PingProbeReportRequest>, String> {
    if plan.probes.is_empty() {
        return Ok(None);
    }

    let timeout = Duration::from_millis(u64::from(plan.timeout_ms));
    let samples = thread::scope(|scope| {
        let handles = plan
            .probes
            .iter()
            .map(|probe| {
                (
                    probe,
                    scope.spawn(move || probe_ping_target(&probe.address, probe.family, timeout)),
                )
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(probe, handle)| {
                let outcome = handle.join().unwrap_or_else(|_| PingProbeOutcome {
                    sample: skipped_ping_sample(
                        &probe.address,
                        probe.family,
                        PingProbeSkipReason::Unavailable,
                    ),
                    diagnostic: PingProbeDiagnostic::WorkerPanicked,
                });
                log_ping_probe_diagnostic(&probe.name, probe.family, &outcome.diagnostic);
                outcome.sample
            })
            .collect::<Vec<_>>()
    });

    Ok(Some(PingProbeReportRequest {
        probed_at_unix_secs: current_unix_secs()?,
        samples,
    }))
}

/// Send one already timestamped observation. Callers deliberately do not retry this request: Ping
/// is freshness-oriented diagnostic data, so the runtime keeps at most one newer pending report.
pub(crate) fn send_ping_probe_report(
    options: &Options,
    request: &PingProbeReportRequest,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/ping-probe", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "PING probe report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

/// Fetch, probe, and send one complete round for the explicit `ping-probe-once` command.
pub(crate) fn ping_probe_cycle(options: &Options) -> Result<PingProbePlan, String> {
    let plan = fetch_ping_probe_settings(options)?;
    if let Some(report) = collect_ping_probe_report(&plan)? {
        send_ping_probe_report(options, &report)?;
    }
    Ok(plan)
}

#[derive(Debug)]
struct PingProbeOutcome {
    sample: PingProbeSample,
    diagnostic: PingProbeDiagnostic,
}

#[derive(Debug)]
enum PingProbeDiagnostic {
    InvalidTarget,
    ResolutionFailed {
        elapsed_ms: u128,
        kind: std::io::ErrorKind,
        errno: Option<i32>,
        message: String,
    },
    ResolutionEmpty {
        elapsed_ms: u128,
    },
    /// The name resolved, but to no address of the requested family.
    FamilyUnresolved {
        elapsed_ms: u128,
        resolved: usize,
    },
    RouteUnavailable {
        elapsed_ms: u128,
        resolved: usize,
    },
    TcpConnect {
        resolution_ms: u128,
        resolved: usize,
        filtered: usize,
        trace: TcpConnectTrace,
    },
    IcmpEcho {
        resolution_ms: u128,
        resolved: usize,
        filtered: usize,
        family: &'static str,
        result: IcmpEchoResult,
    },
    WorkerPanicked,
}

#[derive(Debug)]
enum IcmpEchoResult {
    Reply(u32),
    NoResponse,
    Unavailable(String),
}

#[derive(Debug)]
struct TcpConnectTrace {
    attempted: bool,
    latency_us: Option<u32>,
    attempts: Vec<TcpConnectAttempt>,
    budget_exhausted: bool,
    deadline_overflow: bool,
}

#[derive(Debug)]
struct TcpConnectAttempt {
    candidate: usize,
    family: &'static str,
    elapsed_ms: u128,
    result: TcpConnectAttemptResult,
}

#[derive(Debug)]
enum TcpConnectAttemptResult {
    Connected,
    ConnectedAfterDeadline,
    Failed {
        kind: std::io::ErrorKind,
        errno: Option<i32>,
        message: String,
    },
}

fn probe_ping_target(
    address: &str,
    family: Option<PingProbeFamily>,
    timeout: Duration,
) -> PingProbeOutcome {
    if address.starts_with("tcp://") {
        probe_tcp_target(address, family, timeout)
    } else if address.starts_with("icmp://") {
        probe_icmp_target(address, family, timeout)
    } else {
        PingProbeOutcome {
            sample: skipped_ping_sample(address, family, PingProbeSkipReason::Unavailable),
            diagnostic: PingProbeDiagnostic::InvalidTarget,
        }
    }
}

/// Resolved candidates that may be probed: of the requested family (all families for a legacy
/// probe) and with a usable local route.
struct PingCandidates {
    usable: Vec<SocketAddr>,
    resolved: usize,
    filtered: usize,
    resolution_ms: u128,
}

/// Resolve and filter outside the probe timer. Every way of ending up with nothing to probe is a
/// skipped round with its own reason, never packet loss.
fn ping_candidates(
    address: &str,
    family: Option<PingProbeFamily>,
    host: &str,
    port: u16,
) -> Result<PingCandidates, Box<PingProbeOutcome>> {
    let skip = |reason, diagnostic| {
        Box::new(PingProbeOutcome {
            sample: skipped_ping_sample(address, family, reason),
            diagnostic,
        })
    };
    let resolution_started = Instant::now();
    let addresses = match (host, port).to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            return Err(skip(
                PingProbeSkipReason::ResolveFailed,
                PingProbeDiagnostic::ResolutionFailed {
                    elapsed_ms: resolution_started.elapsed().as_millis(),
                    kind: error.kind(),
                    errno: error.raw_os_error(),
                    message: error.to_string(),
                },
            ));
        }
    };
    let resolution_ms = resolution_started.elapsed().as_millis();
    if addresses.is_empty() {
        return Err(skip(
            PingProbeSkipReason::ResolveFailed,
            PingProbeDiagnostic::ResolutionEmpty {
                elapsed_ms: resolution_ms,
            },
        ));
    }
    let of_family = addresses_of_family(&addresses, family);
    if of_family.is_empty() {
        return Err(skip(
            PingProbeSkipReason::NoAddress,
            PingProbeDiagnostic::FamilyUnresolved {
                elapsed_ms: resolution_ms,
                resolved: addresses.len(),
            },
        ));
    }
    let usable = usable_probe_addresses(&of_family, route_usable);
    if usable.is_empty() {
        return Err(skip(
            PingProbeSkipReason::NoRoute,
            PingProbeDiagnostic::RouteUnavailable {
                elapsed_ms: resolution_ms,
                resolved: addresses.len(),
            },
        ));
    }
    Ok(PingCandidates {
        filtered: of_family.len() - usable.len(),
        usable,
        resolved: addresses.len(),
        resolution_ms,
    })
}

fn probe_tcp_target(
    address: &str,
    family: Option<PingProbeFamily>,
    timeout: Duration,
) -> PingProbeOutcome {
    let Some((host, port)) = parse_tcp_target(address) else {
        return PingProbeOutcome {
            sample: skipped_ping_sample(address, family, PingProbeSkipReason::Unavailable),
            diagnostic: PingProbeDiagnostic::InvalidTarget,
        };
    };
    let candidates = match ping_candidates(address, family, &host, port) {
        Ok(candidates) => candidates,
        Err(outcome) => return *outcome,
    };
    // Resolution and the route filter are deliberately above this call: `Instant` lives inside it,
    // so only attempts to establish TCP are included in the duration. Candidates are tried in
    // order within one family only, so a black-holed IPv6 address can no longer spend the budget
    // an IPv4 address of the same name needed.
    let trace = connect_resolved(&candidates.usable, timeout);
    PingProbeOutcome {
        sample: PingProbeSample {
            target: address.to_owned(),
            family,
            attempted: trace.attempted,
            latency_us: trace.latency_us,
            skip_reason: (!trace.attempted).then_some(PingProbeSkipReason::Unavailable),
        },
        diagnostic: PingProbeDiagnostic::TcpConnect {
            resolution_ms: candidates.resolution_ms,
            resolved: candidates.resolved,
            filtered: candidates.filtered,
            trace,
        },
    }
}

fn probe_icmp_target(
    address: &str,
    family: Option<PingProbeFamily>,
    timeout: Duration,
) -> PingProbeOutcome {
    let Some(host) = parse_icmp_target(address) else {
        return PingProbeOutcome {
            sample: skipped_ping_sample(address, family, PingProbeSkipReason::Unavailable),
            diagnostic: PingProbeDiagnostic::InvalidTarget,
        };
    };
    let candidates = match ping_candidates(address, family, &host, 0) {
        Ok(candidates) => candidates,
        Err(outcome) => return *outcome,
    };
    let target = candidates.usable[0];
    let (attempted, latency_us, result) = match icmp::echo_latency(target, timeout) {
        Ok(icmp::EchoLatency::Reply(elapsed)) => {
            let latency = u32::try_from(elapsed.as_micros()).unwrap_or(u32::MAX);
            (true, Some(latency), IcmpEchoResult::Reply(latency))
        }
        Ok(icmp::EchoLatency::NoResponse) => (true, None, IcmpEchoResult::NoResponse),
        Err(error) => (false, None, IcmpEchoResult::Unavailable(error)),
    };
    PingProbeOutcome {
        sample: PingProbeSample {
            target: address.to_owned(),
            family,
            attempted,
            latency_us,
            skip_reason: (!attempted).then_some(PingProbeSkipReason::Unavailable),
        },
        diagnostic: PingProbeDiagnostic::IcmpEcho {
            resolution_ms: candidates.resolution_ms,
            resolved: candidates.resolved,
            filtered: candidates.filtered,
            family: family_label(target),
            result,
        },
    }
}

fn skipped_ping_sample(
    address: &str,
    family: Option<PingProbeFamily>,
    reason: PingProbeSkipReason,
) -> PingProbeSample {
    PingProbeSample {
        target: address.to_owned(),
        family,
        attempted: false,
        latency_us: None,
        skip_reason: Some(reason),
    }
}

fn addresses_of_family(
    addresses: &[SocketAddr],
    family: Option<PingProbeFamily>,
) -> Vec<SocketAddr> {
    addresses
        .iter()
        .copied()
        .filter(|address| family.is_none_or(|family| PingProbeFamily::of(address.ip()) == family))
        .collect()
}

fn usable_probe_addresses(
    addresses: &[SocketAddr],
    mut route_usable: impl FnMut(&SocketAddr) -> bool,
) -> Vec<SocketAddr> {
    addresses
        .iter()
        .copied()
        .filter(|address| route_usable(address))
        .collect()
}

/// Ask the kernel whether it can select a route and source address for this destination. UDP
/// `connect` performs only a local route lookup; no packet is sent until a write, which never
/// happens here. This is a capability gate rather than another probe and produces no measurement.
/// A family the kernel cannot open a socket for (IPv6 disabled) has no usable route either.
fn route_usable(address: &SocketAddr) -> bool {
    let local = if address.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    UdpSocket::bind(local)
        .and_then(|socket| socket.connect(address))
        .is_ok()
}

fn family_label(address: SocketAddr) -> &'static str {
    if address.is_ipv4() {
        "IPv4"
    } else {
        "IPv6"
    }
}

fn connect_resolved(addresses: &[SocketAddr], timeout: Duration) -> TcpConnectTrace {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(timeout) else {
        return TcpConnectTrace {
            attempted: false,
            latency_us: None,
            attempts: Vec::new(),
            budget_exhausted: false,
            deadline_overflow: true,
        };
    };
    let mut attempts = Vec::with_capacity(addresses.len());
    for (index, address) in addresses.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return TcpConnectTrace {
                attempted: !attempts.is_empty(),
                latency_us: None,
                attempts,
                budget_exhausted: true,
                deadline_overflow: false,
            };
        }
        let attempt_started = Instant::now();
        let family = family_label(*address);
        match TcpStream::connect_timeout(address, remaining) {
            Ok(_) => {
                let attempt_ms = attempt_started.elapsed().as_millis();
                let elapsed = started.elapsed();
                // `connect_timeout` may return a successful socket just after the deadline because
                // the task was scheduled late. The configured boundary is semantic, not merely a
                // syscall hint: anything slower is a missing sample.
                let late = elapsed > timeout;
                attempts.push(TcpConnectAttempt {
                    candidate: index + 1,
                    family,
                    elapsed_ms: attempt_ms,
                    result: if late {
                        TcpConnectAttemptResult::ConnectedAfterDeadline
                    } else {
                        TcpConnectAttemptResult::Connected
                    },
                });
                return TcpConnectTrace {
                    attempted: true,
                    latency_us: (!late)
                        .then(|| u32::try_from(elapsed.as_micros()).unwrap_or(u32::MAX)),
                    attempts,
                    budget_exhausted: false,
                    deadline_overflow: false,
                };
            }
            Err(error) => attempts.push(TcpConnectAttempt {
                candidate: index + 1,
                family,
                elapsed_ms: attempt_started.elapsed().as_millis(),
                result: TcpConnectAttemptResult::Failed {
                    kind: error.kind(),
                    errno: error.raw_os_error(),
                    message: error.to_string(),
                },
            }),
        }
    }
    TcpConnectTrace {
        attempted: !attempts.is_empty(),
        latency_us: None,
        attempts,
        budget_exhausted: false,
        deadline_overflow: false,
    }
}

fn log_ping_probe_diagnostic(
    name: &str,
    family: Option<PingProbeFamily>,
    diagnostic: &PingProbeDiagnostic,
) {
    // `name` is the operator's label; the family distinguishes a target's two series.
    let name = match family {
        Some(PingProbeFamily::Ipv4) => format!("{name} [IPv4]"),
        Some(PingProbeFamily::Ipv6) => format!("{name} [IPv6]"),
        None => name.to_owned(),
    };
    match diagnostic {
        PingProbeDiagnostic::InvalidTarget => {
            println!("ping-probe: {name} 未探测 · 目标格式无效");
        }
        PingProbeDiagnostic::ResolutionFailed {
            elapsed_ms,
            kind,
            errno,
            message,
        } => {
            println!(
                "ping-probe: {name} 未探测 · 解析失败 {} · {kind:?}{} · {message}",
                format_millis(*elapsed_ms),
                format_errno(*errno),
            );
        }
        PingProbeDiagnostic::ResolutionEmpty { elapsed_ms } => {
            println!(
                "ping-probe: {name} 未探测 · 解析未返回地址 · {}",
                format_millis(*elapsed_ms)
            );
        }
        PingProbeDiagnostic::FamilyUnresolved {
            elapsed_ms,
            resolved,
        } => {
            println!(
                "ping-probe: {name} 未探测 · 解析结果没有该地址族的地址 · 候选 {resolved} · 解析 {}",
                format_millis(*elapsed_ms)
            );
        }
        PingProbeDiagnostic::RouteUnavailable {
            elapsed_ms,
            resolved,
        } => {
            println!(
                "ping-probe: {name} 已跳过 · 机器没有该地址族的可用路由 · 候选 {resolved} · 解析 {}",
                format_millis(*elapsed_ms)
            );
        }
        PingProbeDiagnostic::TcpConnect {
            resolution_ms,
            resolved,
            filtered,
            trace,
        } => {
            let mut details = trace
                .attempts
                .iter()
                .map(|attempt| {
                    let prefix = format!(
                        "{}#{} {}",
                        attempt.family,
                        attempt.candidate,
                        format_millis(attempt.elapsed_ms)
                    );
                    match &attempt.result {
                        TcpConnectAttemptResult::Connected => format!("{prefix} 成功"),
                        TcpConnectAttemptResult::ConnectedAfterDeadline => {
                            format!("{prefix} 超过总超时后成功")
                        }
                        TcpConnectAttemptResult::Failed {
                            kind,
                            errno,
                            message,
                        } => format!("{prefix} {kind:?}{} · {message}", format_errno(*errno)),
                    }
                })
                .collect::<Vec<_>>();
            if trace.budget_exhausted {
                details.push("总超时预算已耗尽，剩余候选未尝试".to_owned());
            }
            if trace.deadline_overflow {
                details.push("无法计算超时截止时间".to_owned());
            }
            if *filtered > 0 {
                details.push(format!("已过滤 {filtered} 个无路由候选"));
            }
            if details.is_empty() {
                details.push("没有可尝试的候选地址".to_owned());
            }
            let result = trace.latency_us.map_or_else(
                || {
                    if trace.attempted {
                        "无响应"
                    } else {
                        "未探测"
                    }
                    .to_owned()
                },
                |us| format!("TCP {}", format_micros(us)),
            );
            println!(
                "ping-probe: {name} {result} · 解析 {} · 候选 {resolved} · {}",
                format_millis(*resolution_ms),
                details.join("；")
            );
        }
        PingProbeDiagnostic::IcmpEcho {
            resolution_ms,
            resolved,
            filtered,
            family,
            result,
        } => {
            let result = match result {
                IcmpEchoResult::Reply(us) => format!("ICMP {}", format_micros(*us)),
                IcmpEchoResult::NoResponse => "ICMP 无响应".to_owned(),
                IcmpEchoResult::Unavailable(error) => format!("ICMP 未探测 · {error}"),
            };
            let filtered = if *filtered == 0 {
                String::new()
            } else {
                format!(" · 已过滤 {filtered} 个无路由候选")
            };
            println!(
                "ping-probe: {name} {result} · {family} · 解析 {} · 候选 {resolved}{filtered}",
                format_millis(*resolution_ms)
            );
        }
        PingProbeDiagnostic::WorkerPanicked => {
            println!("ping-probe: {name} 未探测 · 探测线程 panic");
        }
    }
}

fn format_millis(value: u128) -> String {
    format!("{value}ms")
}

fn format_micros(value: u32) -> String {
    if value < 10_000 {
        format!("{:.1}ms", f64::from(value) / 1_000.0)
    } else {
        format!("{}ms", value.saturating_add(500) / 1_000)
    }
}

fn format_errno(errno: Option<i32>) -> String {
    errno.map_or_else(String::new, |value| format!("(errno={value})"))
}

fn parse_tcp_target(address: &str) -> Option<(String, u16)> {
    let authority = address.strip_prefix("tcp://")?;
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split_once("]:")?
    } else {
        let (host, port) = authority.rsplit_once(':')?;
        // IPv6 literals must be bracketed so the final colon is not ambiguous.
        if host.contains(':') {
            return None;
        }
        (host, port)
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let port = port.parse::<u16>().ok().filter(|port| *port > 0)?;
    Some((host.to_owned(), port))
}

fn parse_icmp_target(address: &str) -> Option<String> {
    let authority = address.strip_prefix("icmp://")?;
    if authority.is_empty()
        || authority.chars().any(char::is_whitespace)
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        return None;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let host = bracketed.strip_suffix(']')?;
        host.parse::<Ipv6Addr>().ok()?;
        return Some(host.to_owned());
    }
    (!authority
        .chars()
        .any(|char| matches!(char, ':' | '[' | ']')))
    .then(|| authority.to_owned())
}

#[cfg(test)]
mod ping_probe_tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
        time::Duration,
    };

    use brocade_deployment::protocol::{
        PingProbeFamily, PingProbeSkipReason, PingProbeTargetsResponse,
    };

    use super::{
        addresses_of_family, connect_resolved, parse_icmp_target, parse_tcp_target,
        probe_ping_target, usable_probe_addresses, PingProbeDiagnostic, PingProbePlan,
        TcpConnectAttemptResult,
    };

    #[test]
    fn tcp_target_parser_keeps_host_and_port_only() {
        assert_eq!(
            parse_tcp_target("tcp://example.com:443"),
            Some(("example.com".to_owned(), 443))
        );
        assert_eq!(
            parse_tcp_target("tcp://[2001:db8::1]:8443"),
            Some(("2001:db8::1".to_owned(), 8443))
        );
        assert_eq!(parse_tcp_target("icmp://1.1.1.1"), None);
        assert_eq!(parse_tcp_target("tcp://example.com"), None);
        assert_eq!(parse_tcp_target("tcp://2001:db8::1:443"), None);
        assert_eq!(parse_tcp_target("tcp://example.com:0"), None);
    }

    #[test]
    fn icmp_target_parser_accepts_hosts_and_bracketed_ipv6_without_a_port() {
        assert_eq!(
            parse_icmp_target("icmp://1.1.1.1"),
            Some("1.1.1.1".to_owned())
        );
        assert_eq!(
            parse_icmp_target("icmp://example.com"),
            Some("example.com".to_owned())
        );
        assert_eq!(
            parse_icmp_target("icmp://[2001:db8::1]"),
            Some("2001:db8::1".to_owned())
        );
        assert_eq!(parse_icmp_target("icmp://1.1.1.1:80"), None);
        assert_eq!(parse_icmp_target("icmp://2001:db8::1"), None);
    }

    #[test]
    fn each_family_probes_only_its_own_addresses() {
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 443));
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 443));
        assert_eq!(
            addresses_of_family(&[v6, v4], Some(PingProbeFamily::Ipv4)),
            vec![v4]
        );
        assert_eq!(
            addresses_of_family(&[v6, v4], Some(PingProbeFamily::Ipv6)),
            vec![v6]
        );
        // A legacy probe keeps the pre-dual-stack behaviour: every resolved address.
        assert_eq!(addresses_of_family(&[v6, v4], None), vec![v6, v4]);
    }

    #[test]
    fn unroutable_candidates_are_removed_without_hiding_the_others() {
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 443));
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 443));
        assert_eq!(
            usable_probe_addresses(&[v6, v4], |address| address.is_ipv4()),
            vec![v4]
        );
        assert!(usable_probe_addresses(&[v6], |_| false).is_empty());
        assert_eq!(usable_probe_addresses(&[v6], |_| true), vec![v6]);
    }

    #[test]
    fn a_dual_stack_target_plans_one_probe_per_configured_family() {
        let response: PingProbeTargetsResponse = serde_json::from_value(serde_json::json!({
            "targets": [
                { "name": "CF", "kind": "icmp", "ipv4": "1.1.1.1", "ipv6": "2606:4700:4700::1111" },
                { "name": "GitHub", "kind": "tcp", "ipv4": "github.com:443", "ipv6": null },
            ],
            "interval_secs": 60,
            "timeout_ms": 420
        }))
        .unwrap();
        let plan = PingProbePlan::from_response(response);
        let probes = plan
            .probes
            .iter()
            .map(|probe| (probe.address.as_str(), probe.family))
            .collect::<Vec<_>>();
        assert_eq!(
            probes,
            [
                ("icmp://1.1.1.1", Some(PingProbeFamily::Ipv4)),
                ("icmp://[2606:4700:4700::1111]", Some(PingProbeFamily::Ipv6)),
                ("tcp://github.com:443", Some(PingProbeFamily::Ipv4)),
            ]
        );

        // An older Console answers with one URI per target; those are probed without a family.
        let legacy: PingProbeTargetsResponse = serde_json::from_value(serde_json::json!({
            "targets": [{ "name": "old", "address": "tcp://example.com:443" }],
            "interval_secs": 30,
            "timeout_ms": 420
        }))
        .unwrap();
        let plan = PingProbePlan::from_response(legacy);
        assert_eq!(plan.interval_secs, 30);
        assert_eq!(plan.probes[0].family, None);
    }

    #[test]
    fn a_literal_of_the_other_family_is_skipped_as_no_address() {
        // The Console never stores this; the Agent still refuses to measure the wrong family.
        let outcome = probe_ping_target(
            "icmp://127.0.0.1",
            Some(PingProbeFamily::Ipv6),
            Duration::from_millis(200),
        );
        assert!(!outcome.sample.attempted);
        assert_eq!(outcome.sample.family, Some(PingProbeFamily::Ipv6));
        assert_eq!(
            outcome.sample.skip_reason,
            Some(PingProbeSkipReason::NoAddress)
        );
        assert!(matches!(
            outcome.diagnostic,
            PingProbeDiagnostic::FamilyUnresolved { .. }
        ));
    }

    #[test]
    fn tcp_probe_reports_its_family_and_latency() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback listener");
        let port = listener.local_addr().expect("listener address").port();
        let outcome = probe_ping_target(
            &format!("tcp://127.0.0.1:{port}"),
            Some(PingProbeFamily::Ipv4),
            Duration::from_secs(1),
        );
        assert!(outcome.sample.attempted);
        assert!(outcome.sample.latency_us.is_some());
        assert_eq!(outcome.sample.family, Some(PingProbeFamily::Ipv4));
        assert_eq!(outcome.sample.skip_reason, None);
    }

    #[test]
    fn connect_trace_keeps_success_family_candidate_and_duration() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let trace = connect_resolved(&[address], Duration::from_secs(1));

        assert!(trace.latency_us.is_some());
        assert!(trace.attempted);
        assert!(!trace.budget_exhausted);
        assert!(!trace.deadline_overflow);
        assert_eq!(trace.attempts.len(), 1);
        assert_eq!(trace.attempts[0].candidate, 1);
        assert_eq!(trace.attempts[0].family, "IPv4");
        assert!(matches!(
            trace.attempts[0].result,
            TcpConnectAttemptResult::Connected
        ));
    }

    #[test]
    fn connect_trace_keeps_socket_error_instead_of_collapsing_it_to_no_response() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve loopback port");
        let address = listener.local_addr().expect("listener address");
        drop(listener);

        let trace = connect_resolved(&[address], Duration::from_secs(1));
        assert_eq!(trace.latency_us, None);
        assert!(trace.attempted);
        assert_eq!(trace.attempts.len(), 1);
        assert!(matches!(
            &trace.attempts[0].result,
            TcpConnectAttemptResult::Failed {
                kind: std::io::ErrorKind::ConnectionRefused,
                errno: Some(_),
                message,
            } if !message.is_empty()
        ));
    }

    #[test]
    fn malformed_target_has_an_explicit_local_diagnostic() {
        let outcome = probe_ping_target("not-a-ping-target", None, Duration::from_secs(1));
        assert!(matches!(
            outcome.diagnostic,
            PingProbeDiagnostic::InvalidTarget
        ));
        assert!(!outcome.sample.attempted);
        assert_eq!(outcome.sample.latency_us, None);
    }
}

pub(crate) fn ping_probe_once(options: Options) -> Result<(), String> {
    ping_probe_cycle(&options).map(|_| ())
}

pub(crate) fn probe_once(options: Options) -> Result<(), String> {
    probe_cycle(&options)
}

/// The kernel socket namespace an Xray inbound occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XrayListenProtocol {
    Tcp,
    Udp,
}

/// Every inbound's (tag, listen, port, protocol) from xray.json, for probing ports one by one.
pub(crate) fn xray_listen_ports(content: &str) -> Vec<(String, String, u16, XrayListenProtocol)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    value
        .get("inbounds")
        .and_then(serde_json::Value::as_array)
        .map(|inbounds| {
            inbounds
                .iter()
                .filter_map(|inbound| {
                    let tag = inbound.get("tag")?.as_str()?.to_owned();
                    let listen = inbound
                        .get("listen")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("0.0.0.0")
                        .to_owned();
                    let port = u16::try_from(inbound.get("port")?.as_u64()?).ok()?;
                    let protocol = if inbound.get("protocol").and_then(serde_json::Value::as_str)
                        == Some("hysteria")
                    {
                        XrayListenProtocol::Udp
                    } else {
                        XrayListenProtocol::Tcp
                    };
                    Some((tag, listen, port, protocol))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Which peer address belongs to which hop, read out of xray's own outbound configuration.
///
/// This is the join that makes `inetdiag`'s per-socket numbers mean something: netlink reports a
/// peer address, and only xray.json knows that 10.42.0.3 is the `sg-02` leg of `app/asia.c1`.
///
/// Keyed on the address alone, not address+port. A hop is one machine, and if two chains happen to
/// reach the same next hop on different ports their connections belong to the same physical line —
/// which is what this measures. Should two hops ever share an address, the later entry wins and
/// its chain gets the credit; that is a wrong attribution rather than a wrong measurement, and it
/// cannot happen today because a peer address identifies a node.
///
/// Addresses that are not literal IPs are skipped, and the hop simply goes unmeasured. Resolving
/// them here would mean this function's answer depends on DNS at the moment it runs, and a hop
/// silently attributed to whatever an expired record pointed at is worse than a hop with no data.
/// Backbone hops are overlay IPs anyway; a hostname here means a directly dialled upstream.
pub(crate) fn hop_targets(xray_json: &str) -> BTreeMap<std::net::IpAddr, (String, String)> {
    let mut out = BTreeMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(xray_json) else {
        return out;
    };
    let Some(outbounds) = value.get("outbounds").and_then(serde_json::Value::as_array) else {
        return out;
    };
    for outbound in outbounds {
        let Some(tag) = outbound.get("tag").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // Same filter as the liveness counters: only forwarding outbounds, never egress or block.
        let Some((chain_id, peer)) = hop_of_tag(tag) else {
            continue;
        };
        let Some(settings) = outbound.get("settings") else {
            continue;
        };
        // vnext for VLESS, servers for shadowsocks. Both are the same idea under different keys,
        // and a chain can mix them hop by hop.
        let entries = settings
            .get("vnext")
            .or_else(|| settings.get("servers"))
            .and_then(serde_json::Value::as_array);
        let Some(entries) = entries else {
            continue;
        };
        for entry in entries {
            let Some(address) = entry.get("address").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Ok(ip) = address.parse::<std::net::IpAddr>() else {
                continue;
            };
            out.insert(ip, (chain_id.to_owned(), peer.to_owned()));
        }
    }
    out
}
