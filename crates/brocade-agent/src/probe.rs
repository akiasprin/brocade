//! The probing family: path MTU, relay-hop liveness, and end-to-end probe
//! scheduling. All three measure and report without changing any state on the
//! machine — entirely apart from the convergence path, which is why each runs on
//! its own thread (see `run_forever` in main.rs).
use std::{collections::BTreeMap, sync::Mutex};

use brocade_deployment::protocol::{
    E2eExitVerdict, E2eProbeRequest, E2eProbeStatus, E2eProbeTargetList, LinkHealth,
    LinkHealthRequest, LinkProbe, LinkProbeRequest, LinkProbeStatus, ProbeTargetList,
    ProbeTransport,
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
