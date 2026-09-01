//! The probing family: path MTU, relay-hop liveness, and end-to-end probe
//! scheduling. All three measure and report without changing any state on the
//! machine — entirely apart from the convergence path, which is why each runs on
//! its own thread (see `run_forever` in main.rs).
use std::{
    collections::BTreeMap,
    net::{Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use brocade_deployment::protocol::{
    E2eExitVerdict, E2eProbeRequest, E2eProbeStatus, E2eProbeTargetList, LinkHealth,
    LinkHealthRequest, LinkProbe, LinkProbeRequest, LinkProbeStatus, ProbeTargetList,
    ProbeTransport, TcpProbeReportRequest, TcpProbeSample, TcpProbeSettings,
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

/// `out:{app}/{chain}>{to}` → (app/chain, to). `None` for anything else
/// (egress/block).
fn hop_of_tag(tag: &str) -> Option<(&str, &str)> {
    tag.strip_prefix("out:")?.split_once('>')
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

    let hops = now
        .iter()
        .filter_map(|(tag, value)| {
            let (chain_id, peer) = hop_of_tag(tag)?;
            let before = previous.get(tag).copied().unwrap_or(0);
            // A drop means xray restarted and the counter reset; this round has
            // no trustworthy delta, so treat it as no data
            let delta = value.saturating_sub(before);
            Some(LinkHealth {
                chain_id: chain_id.to_owned(),
                peer_node_id: peer.to_owned(),
                alive: delta > 0,
                downlink_bytes: delta,
            })
        })
        .collect::<Vec<_>>();

    (!hops.is_empty()).then_some(LinkHealthRequest {
        checked_at_unix_secs: at as i64,
        window_secs,
        hops,
    })
}

fn probe_links(options: &Options) -> Result<LinkProbeRequest, String> {
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
    let list: ProbeTargetList =
        serde_json::from_str(&response.body).map_err(|error| error.to_string())?;

    let links = list
        .targets
        .into_iter()
        .map(|target| {
            let (status, path_mtu, suggested_wg_mtu) =
                probe_path_mtu(&target.host, target.transport);
            LinkProbe {
                peer_node_id: target.peer_node_id,
                endpoint_host: target.host,
                status,
                path_mtu,
                suggested_wg_mtu,
            }
        })
        .collect();

    Ok(LinkProbeRequest {
        probed_at_unix_secs: current_unix_secs()?,
        links,
    })
}

pub(crate) fn probe_cycle(options: &Options) -> Result<(), String> {
    let report = probe_links(options)?;
    if report.links.is_empty() {
        return Ok(());
    }
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

/// Probe every chain once and report the results.
///
/// Fetching the work list and reporting results are separate endpoints, as in the
/// MTU family: the list is a function of the model (compiled on demand by the
/// control plane), the results are facts the machine observed.
pub(crate) fn e2e_cycle(options: &Options) -> Result<E2eProbeTargetList, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/e2e-targets", &options.token, None)?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "e2e targets request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    let list: E2eProbeTargetList =
        serde_json::from_str(&response.body).map_err(|error| error.to_string())?;

    // This machine heads no chain. Report nothing — an empty batch would
    // conflate "no chains to probe" with "probed them all and every one
    // failed".
    if list.targets.is_empty() {
        return Ok(list);
    }

    let chains = e2e::probe_all(&list);
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
                    E2eExitVerdict::Unknown => "（核对不了）",
                }
            ),
            _ => println!(
                "e2e: {} 不通：{}",
                chain.chain_id,
                chain.detail.as_deref().unwrap_or("没有细节")
            ),
        }
    }

    let request = E2eProbeRequest {
        probed_at_unix_secs: current_unix_secs()?,
        chains,
    };
    let body = serde_json::to_string(&request).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/e2e-probe", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "e2e probe report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(list)
}

pub(crate) fn e2e_once(options: Options) -> Result<(), String> {
    e2e_cycle(&options).map(|_| ())
}

/// Fetch the fleet's active TCP targets, probe each once, and send one compact round.
///
/// Resolution is completed before `connect_resolved` starts its timer. The wire report remains
/// deliberately compact (`connect_ms` or null), while the node-local journal keeps enough
/// diagnostics to tell malformed input, resolution failure, unavailable IPv6, an immediate socket
/// error and a real timeout apart. Diagnostics never include a resolved IP or configured endpoint;
/// the operator-selected target name plus IP family and errno are sufficient and do not turn the
/// journal into another inventory of infrastructure addresses.
pub(crate) fn tcp_probe_cycle(options: &Options) -> Result<TcpProbeSettings, String> {
    let client = HttpClient::new(&options.server)?;
    let response = client.request("GET", "/agent/v1/tcp-probe-targets", &options.token, None)?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "TCP probe targets request failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    let settings: TcpProbeSettings =
        serde_json::from_str(&response.body).map_err(|error| error.to_string())?;
    if settings.targets.is_empty() {
        return Ok(settings);
    }

    let timeout = Duration::from_millis(u64::from(settings.timeout_ms));
    let samples = thread::scope(|scope| {
        let handles = settings
            .targets
            .iter()
            .map(|target| {
                let name = target.name.clone();
                let address = target.address.clone();
                (
                    name,
                    address.clone(),
                    scope.spawn(move || probe_tcp_target(&address, timeout)),
                )
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|(name, target, handle)| {
                let outcome = handle.join().unwrap_or_else(|_| TcpProbeOutcome {
                    sample: Some(failed_tcp_sample(&target)),
                    diagnostic: TcpProbeDiagnostic::WorkerPanicked,
                });
                log_tcp_probe_diagnostic(&name, &outcome.diagnostic);
                outcome.sample
            })
            .collect::<Vec<_>>()
    });

    let request = TcpProbeReportRequest {
        probed_at_unix_secs: current_unix_secs()?,
        samples,
    };
    let body = serde_json::to_string(&request).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/tcp-probe", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "TCP probe report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(settings)
}

#[derive(Debug)]
struct TcpProbeOutcome {
    /// `None` means capability-based omission (currently an IPv6-only target on a node without an
    /// IPv6 route). Every actual failure remains an explicit null sample on the wire.
    sample: Option<TcpProbeSample>,
    diagnostic: TcpProbeDiagnostic,
}

#[derive(Debug)]
enum TcpProbeDiagnostic {
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
    Ipv6RouteUnavailable {
        elapsed_ms: u128,
        resolved: usize,
    },
    Connect {
        resolution_ms: u128,
        resolved: usize,
        filtered_ipv6: usize,
        trace: TcpConnectTrace,
    },
    WorkerPanicked,
}

#[derive(Debug)]
struct TcpConnectTrace {
    connect_ms: Option<u32>,
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

fn probe_tcp_target(address: &str, timeout: Duration) -> TcpProbeOutcome {
    let Some((host, port)) = parse_tcp_target(address) else {
        return TcpProbeOutcome {
            sample: Some(failed_tcp_sample(address)),
            diagnostic: TcpProbeDiagnostic::InvalidTarget,
        };
    };
    let resolution_started = Instant::now();
    let addresses = match (host.as_str(), port).to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            return TcpProbeOutcome {
                sample: Some(failed_tcp_sample(address)),
                diagnostic: TcpProbeDiagnostic::ResolutionFailed {
                    elapsed_ms: resolution_started.elapsed().as_millis(),
                    kind: error.kind(),
                    errno: error.raw_os_error(),
                    message: error.to_string(),
                },
            };
        }
    };
    let resolution_ms = resolution_started.elapsed().as_millis();
    if addresses.is_empty() {
        return TcpProbeOutcome {
            sample: Some(failed_tcp_sample(address)),
            diagnostic: TcpProbeDiagnostic::ResolutionEmpty {
                elapsed_ms: resolution_ms,
            },
        };
    };
    let usable = usable_tcp_addresses(&addresses, ipv6_route_usable);
    // An IPv6-only target is not a failed link on a machine with no route for IPv6. Omit it from
    // the report entirely: the control plane can distinguish absence from an explicit null
    // without collecting another capability or error-classification field.
    if usable.is_empty() && addresses.iter().all(SocketAddr::is_ipv6) {
        return TcpProbeOutcome {
            sample: None,
            diagnostic: TcpProbeDiagnostic::Ipv6RouteUnavailable {
                elapsed_ms: resolution_ms,
                resolved: addresses.len(),
            },
        };
    }
    // DNS resolution and the route-only IPv6 filter are deliberately above this call: `Instant`
    // lives inside it, so only attempts to establish TCP are included in the duration.
    let trace = connect_resolved(&usable, timeout);
    TcpProbeOutcome {
        sample: Some(TcpProbeSample {
            target: address.to_owned(),
            connect_ms: trace.connect_ms,
        }),
        diagnostic: TcpProbeDiagnostic::Connect {
            resolution_ms,
            resolved: addresses.len(),
            filtered_ipv6: addresses.len().saturating_sub(usable.len()),
            trace,
        },
    }
}

fn failed_tcp_sample(address: &str) -> TcpProbeSample {
    TcpProbeSample {
        target: address.to_owned(),
        connect_ms: None,
    }
}

fn usable_tcp_addresses(
    addresses: &[SocketAddr],
    mut ipv6_usable: impl FnMut(&SocketAddr) -> bool,
) -> Vec<SocketAddr> {
    addresses
        .iter()
        .copied()
        .filter(|address| address.is_ipv4() || ipv6_usable(address))
        .collect()
}

/// Ask the kernel whether it can select an IPv6 route and source address for this destination.
/// UDP `connect` performs only a local route lookup; no packet is sent until a write, which never
/// happens here. This is a capability gate rather than another probe and produces no measurement.
fn ipv6_route_usable(address: &SocketAddr) -> bool {
    if !address.is_ipv6() {
        return true;
    }
    UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))
        .and_then(|socket| socket.connect(address))
        .is_ok()
}

fn connect_resolved(addresses: &[SocketAddr], timeout: Duration) -> TcpConnectTrace {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(timeout) else {
        return TcpConnectTrace {
            connect_ms: None,
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
                connect_ms: None,
                attempts,
                budget_exhausted: true,
                deadline_overflow: false,
            };
        }
        let attempt_started = Instant::now();
        let family = if address.is_ipv4() { "IPv4" } else { "IPv6" };
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
                    connect_ms: (!late)
                        .then(|| u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX)),
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
        connect_ms: None,
        attempts,
        budget_exhausted: false,
        deadline_overflow: false,
    }
}

fn log_tcp_probe_diagnostic(name: &str, diagnostic: &TcpProbeDiagnostic) {
    match diagnostic {
        TcpProbeDiagnostic::InvalidTarget => {
            println!("tcp-probe: {name} 无响应 · 目标格式无效");
        }
        TcpProbeDiagnostic::ResolutionFailed {
            elapsed_ms,
            kind,
            errno,
            message,
        } => {
            println!(
                "tcp-probe: {name} 无响应 · 解析失败 {} · {kind:?}{} · {message}",
                format_millis(*elapsed_ms),
                format_errno(*errno),
            );
        }
        TcpProbeDiagnostic::ResolutionEmpty { elapsed_ms } => {
            println!(
                "tcp-probe: {name} 无响应 · 解析未返回地址 · {}",
                format_millis(*elapsed_ms)
            );
        }
        TcpProbeDiagnostic::Ipv6RouteUnavailable {
            elapsed_ms,
            resolved,
        } => {
            println!(
                "tcp-probe: {name} 已跳过 · 机器没有可用 IPv6 路由 · 候选 {resolved} · 解析 {}",
                format_millis(*elapsed_ms)
            );
        }
        TcpProbeDiagnostic::Connect {
            resolution_ms,
            resolved,
            filtered_ipv6,
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
            if *filtered_ipv6 > 0 {
                details.push(format!("已过滤 {filtered_ipv6} 个无路由 IPv6 候选"));
            }
            if details.is_empty() {
                details.push("没有可尝试的候选地址".to_owned());
            }
            let result = trace
                .connect_ms
                .map_or_else(|| "无响应".to_owned(), |ms| format!("Connect {ms}ms"));
            println!(
                "tcp-probe: {name} {result} · 解析 {} · 候选 {resolved} · {}",
                format_millis(*resolution_ms),
                details.join("；")
            );
        }
        TcpProbeDiagnostic::WorkerPanicked => {
            println!("tcp-probe: {name} 无响应 · 探测线程 panic");
        }
    }
}

fn format_millis(value: u128) -> String {
    format!("{value}ms")
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

#[cfg(test)]
mod tcp_probe_tests {
    use std::{
        net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
        time::Duration,
    };

    use super::{
        connect_resolved, parse_tcp_target, probe_tcp_target, usable_tcp_addresses,
        TcpConnectAttemptResult, TcpProbeDiagnostic,
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
    fn unavailable_ipv6_is_removed_without_hiding_ipv4_fallbacks() {
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 443));
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 443));
        assert_eq!(usable_tcp_addresses(&[v6, v4], |_| false), vec![v4]);
        assert!(usable_tcp_addresses(&[v6], |_| false).is_empty());
        assert_eq!(usable_tcp_addresses(&[v6], |_| true), vec![v6]);
    }

    #[test]
    fn connect_trace_keeps_success_family_candidate_and_duration() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let trace = connect_resolved(&[address], Duration::from_secs(1));

        assert!(trace.connect_ms.is_some());
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
        assert_eq!(trace.connect_ms, None);
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
        let outcome = probe_tcp_target("not-a-tcp-target", Duration::from_secs(1));
        assert!(matches!(
            outcome.diagnostic,
            TcpProbeDiagnostic::InvalidTarget
        ));
        assert_eq!(outcome.sample.and_then(|sample| sample.connect_ms), None);
    }
}

pub(crate) fn tcp_probe_once(options: Options) -> Result<(), String> {
    tcp_probe_cycle(&options).map(|_| ())
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
