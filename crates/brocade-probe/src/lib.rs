//! End-to-end probing: the head of each chain dials once on its own behalf, to
//! see whether users can actually get through right now.
//!
//! ## Why start a throwaway xray instead of reusing the running one
//!
//! What must be tested is the path users take, and that path begins with a
//! REALITY handshake against the ingress, carrying credentials. The xray running
//! on this machine is that path's server end, and any request originating inside
//! it skips both the handshake and the ingress routing — measuring only whether
//! the relay segment works, which `link_health` already covers.
//!
//! So a client is required. Hand-writing a VLESS+REALITY client is out of the
//! question (REALITY's handshake was never meant for third-party
//! implementations), and the agent happens to have the xray binary at hand. Start
//! a short-lived process with an ordinary client config: socks in, vless out.
//! Field for field the same as the one on a user's phone.
//!
//! ## Why not add a probe inbound to the production xray
//!
//! That would indeed save a process, but it bypasses the REALITY handshake — and
//! wrong handshake parameters are both the most common fault and the kind static
//! validation cannot catch. Bypassing it, the probe misses exactly what it exists
//! to measure.
//!
//! ## Process management
//!
//! Kill only the process whose `Child` handle we hold. Killing by name with
//! `pkill -f xray` takes the production one down as well — severing the line in
//! order to check whether it works inverts cause and effect.

use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use brocade_deployment::protocol::{
    E2eExitVerdict, E2eProbe, E2eProbeStatus, E2eProbeTarget, E2eProbeTargetList,
};

/// Ceiling on waiting for the started process to listen on its socks port. A cold
/// xray start is usually tens of milliseconds; two seconds is headroom for a
/// loaded machine.
const STARTUP_WAIT: Duration = Duration::from_secs(2);
/// Polling interval for the listening port.
const STARTUP_POLL: Duration = Duration::from_millis(25);

/// How many attempts a chain gets after a failure.
///
/// Why this is required: every probe starts a fresh xray and completes a new
/// REALITY handshake with dest, so the first request is inherently slower than
/// those after it. Without a retry, one network hiccup leaves a red bar on the
/// sparkline — and that bar sends someone to investigate a chain that is not
/// broken. A false alarm costs far more than noticing a real fault one round
/// later.
///
/// Two attempts rather than three: a genuinely broken chain breaks the second
/// time too, and further attempts only cost time.
const ATTEMPTS: u32 = 2;

/// A pause between attempts. Hiccups last hundreds of milliseconds, so too short
/// a gap is no gap at all.
const RETRY_GAP: Duration = Duration::from_millis(700);

/// Cooperative cancellation shared by every item in one on-demand job.
///
/// The process handle remains owned by the worker which spawned it — cancellation never searches
/// the machine by process name, because doing that on an Agent would also kill the serving Xray.
/// A blocked socket wakes when its bounded timeout expires; every other boundary checks the flag
/// immediately and the process guard then reaps exactly its own child.
#[derive(Clone, Default)]
pub struct ProbeCancellation(Arc<AtomicBool>);

impl ProbeCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Runtime-only choices. The target remains a pure description of the user path; where Xray is
/// installed and whether the caller cancelled are properties of the machine running the probe.
#[derive(Clone)]
pub struct ProbeOptions {
    xray_binary: PathBuf,
    runtime_dir: PathBuf,
    cancellation: ProbeCancellation,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            xray_binary: PathBuf::from("xray"),
            runtime_dir: std::env::temp_dir(),
            cancellation: ProbeCancellation::default(),
        }
    }
}

impl ProbeOptions {
    pub fn new(xray_binary: impl Into<PathBuf>, cancellation: ProbeCancellation) -> Self {
        Self {
            xray_binary: xray_binary.into(),
            runtime_dir: std::env::temp_dir(),
            cancellation,
        }
    }

    /// Put credential-bearing configs in a caller-owned private directory. Console uses a
    /// systemd RuntimeDirectory; Agents retain the established system temporary directory.
    pub fn with_runtime_dir(mut self, runtime_dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = runtime_dir.into();
        self
    }

    pub fn cancellation(&self) -> &ProbeCancellation {
        &self.cancellation
    }
}

/// Probe a whole batch. Chains run serially: starting N xrays in parallel adds N
/// processes to this machine for the seconds the probe takes, and a machine with
/// many chains is precisely a loaded one. Probing must not become its burden.
pub fn probe_all(list: &E2eProbeTargetList) -> Vec<E2eProbe> {
    list.targets
        .iter()
        .map(|target| probe_one(target, &list.endpoint_url, list.timeout_secs))
        .collect()
}

/// Probe one chain. Failures retry (see `ATTEMPTS`) and any success counts as
/// working.
///
/// The last attempt's result is what gets reported: the timing of the successful
/// attempt is the chain's real timing, and failure details should be the most
/// recent — an intermediate attempt's error is already stale.
pub fn probe_one(target: &E2eProbeTarget, endpoint_url: &str, timeout_secs: u64) -> E2eProbe {
    probe_one_with_options(target, endpoint_url, timeout_secs, &ProbeOptions::default())
}

/// The same real protocol probe with an explicit Xray executable and cancellation scope. Console
/// uses this form; Agents keep the PATH-based wrapper above.
pub fn probe_one_with_options(
    target: &E2eProbeTarget,
    endpoint_url: &str,
    timeout_secs: u64,
    options: &ProbeOptions,
) -> E2eProbe {
    let mut last = probe_once(target, endpoint_url, timeout_secs, options);
    for _ in 1..ATTEMPTS {
        if !worth_retrying(&last) || options.cancellation.is_cancelled() {
            break;
        }
        let until = Instant::now() + RETRY_GAP;
        while Instant::now() < until && !options.cancellation.is_cancelled() {
            std::thread::sleep(Duration::from_millis(25));
        }
        if options.cancellation.is_cancelled() {
            break;
        }
        last = probe_once(target, endpoint_url, timeout_secs, options);
    }
    last
}

/// Whether this failure is worth another attempt.
///
/// `Unsupported` is not retried: it is a fact about this machine (no xray, no
/// free port, a misconfigured endpoint), unchanged by a hundred more attempts and
/// costing an extra round for nothing.
fn worth_retrying(probe: &E2eProbe) -> bool {
    matches!(
        probe.status,
        E2eProbeStatus::Timeout | E2eProbeStatus::ChainBroken | E2eProbeStatus::HandshakeFailed
    )
}

fn probe_once(
    target: &E2eProbeTarget,
    endpoint_url: &str,
    timeout_secs: u64,
    options: &ProbeOptions,
) -> E2eProbe {
    let timeout = Duration::from_secs(timeout_secs.clamp(1, 120));
    let base = |status: E2eProbeStatus, detail: Option<String>| E2eProbe {
        app_id: target.app_id.clone(),
        chain_id: target.chain_id.clone(),
        status,
        ttfb_ms: None,
        exit_ip: None,
        exit_loc: None,
        exit_verdict: E2eExitVerdict::Unknown,
        detail,
    };

    if options.cancellation.is_cancelled() {
        return base(E2eProbeStatus::Unsupported, Some("拨测已取消".to_owned()));
    }

    let Ok(endpoint) = HttpTarget::parse(endpoint_url) else {
        // A misconfigured endpoint is our own configuration problem, not a broken
        // chain. Folded into chain-broken, the whole fleet reports every chain
        // down at once and the operator investigates a pile of healthy ones.
        return base(
            E2eProbeStatus::Unsupported,
            Some(format!(
                "探测落点填得不对：{endpoint_url}（只支持 http://）"
            )),
        );
    };

    let Some(socks_port) = free_local_port() else {
        return base(
            E2eProbeStatus::Unsupported,
            Some("本机挑不出空闲端口给探测进程".to_owned()),
        );
    };

    let mut process = match spawn_xray(
        target,
        socks_port,
        &options.xray_binary,
        &options.runtime_dir,
    ) {
        Ok(process) => process,
        Err(error) => return base(E2eProbeStatus::Unsupported, Some(error)),
    };

    let outcome = (|| {
        if !wait_for_listen(socks_port, &options.cancellation) {
            // A started process that never listens usually means xray rejected
            // the config. Carry its output along — it is the only thing that can
            // say why it would not start.
            return Err((
                E2eProbeStatus::Unsupported,
                format!("探测进程没起来：{}", process.take_output()),
            ));
        }
        // Send one warm-up request and measure the second: the first pays for
        // xray's cold start (process init, loading geoip/geosite), which is the
        // probe's own overhead and would add a constant to every chain. The
        // handshake neither can nor should be skipped — xray does not reuse
        // outbound connections by default, and a user waits for it on every new
        // connection too. A failed warm-up fails outright without a second
        // request: a broken chain breaks again and only costs another timeout.
        if options.cancellation.is_cancelled() {
            return Err((E2eProbeStatus::Unsupported, "拨测已取消".to_owned()));
        }
        run_probe(socks_port, &endpoint, timeout, &options.cancellation)?;
        if options.cancellation.is_cancelled() {
            return Err((E2eProbeStatus::Unsupported, "拨测已取消".to_owned()));
        }
        run_probe(socks_port, &endpoint, timeout, &options.cancellation)
    })();

    process.kill();

    // On failure, carry along whatever the probe process said. This is not
    // optional: a failed probe and a genuinely broken chain look identical, and
    // without this output there is only guessing. The read must come after the
    // kill — with the process still alive it blocks forever.
    let outcome = outcome.map_err(|(status, detail)| {
        let said = process.take_output();
        (status, format!("{detail}｜探测进程说：{said}"))
    });

    match outcome {
        Ok(success) => {
            let verdict = judge_exit(target, success.exit_ip.as_deref());
            E2eProbe {
                app_id: target.app_id.clone(),
                chain_id: target.chain_id.clone(),
                status: E2eProbeStatus::Ok,
                ttfb_ms: Some(success.ttfb_ms),
                exit_ip: success.exit_ip,
                exit_loc: success.exit_loc,
                exit_verdict: verdict,
                detail: mismatch_detail(target, verdict),
            }
        }
        Err((status, detail)) => base(status, Some(detail)),
    }
}

struct ProbeSuccess {
    ttfb_ms: u32,
    exit_ip: Option<String>,
    exit_loc: Option<String>,
}

/// Exit check: whether the IP the endpoint saw is among those this chain should
/// have.
///
/// An empty expectation yields `Unknown`, not `Match` — it means no exit offers an
/// address the endpoint could report, because one is behind NAT or has none at all.
/// Counting it as a pass would show a misconfigured chain as healthy purely because
/// it cannot be falsified.
fn judge_exit(target: &E2eProbeTarget, exit_ip: Option<&str>) -> E2eExitVerdict {
    if target.expected_exit_ips.is_empty() {
        return E2eExitVerdict::Unknown;
    }
    match exit_ip {
        None => E2eExitVerdict::Unknown,
        Some(ip) if target.expected_exit_ips.iter().any(|want| want == ip) => E2eExitVerdict::Match,
        Some(_) => E2eExitVerdict::Mismatch,
    }
}

fn mismatch_detail(target: &E2eProbeTarget, verdict: E2eExitVerdict) -> Option<String> {
    match verdict {
        E2eExitVerdict::Mismatch => Some(format!(
            "通了，但出口 IP 不在这条链的出口机器上（期望 {}）",
            target.expected_exit_ips.join("、")
        )),
        E2eExitVerdict::Unknown if target.expected_exit_ips.is_empty() => {
            Some("出口机器在 NAT 后面或没有公网地址，核对不了从哪儿出去的".to_owned())
        }
        _ => None,
    }
}

// ── Client config ───────────────────────────────────────────────────────────

/// An ordinary xray client config: socks in, vless out.
///
/// Its fields come from strictly the same source as the ones handed to users in a
/// subscription (projected by the control plane via `physical::probe`). Any
/// parameter that diverges measures the health of a different path — and presents
/// it as "the chain is fine", which is worse than not probing at all. That is why both
/// `stream_settings` below branch on what the target says rather than on a default: this
/// dialed plain TCP at every target once, and an ingress carried inside HTTP was refused at
/// the server's path check and reported down while it was carrying traffic perfectly well.
fn client_config(target: &E2eProbeTarget, socks_port: u16, log_path: &str) -> String {
    if let Some(hysteria) = &target.hysteria2 {
        return hysteria_client_config(target, hysteria, socks_port, log_path);
    }
    if let Some(anytls) = &target.anytls {
        return anytls_client_config(target, anytls, socks_port, log_path);
    }
    let mut settings = serde_json::json!({
        "vnext": [{
            "address": target.dial_host,
            "port": target.port,
            "users": [{
                "id": target.uuid,
                "encryption": "none",
            }],
        }],
    });
    let flow = match &target.tls {
        Some(tls) => tls.flow.as_deref(),
        None => target.reality.flow.as_deref(),
    };
    if let Some(flow) = flow {
        settings["vnext"][0]["users"][0]["flow"] = serde_json::json!(flow);
    }

    let config = serde_json::json!({
        // Logs go to a file rather than a pipe: xray writes `[Info]` to stdout, so
        // capturing stderr alone yields nothing, and a full pipe wedges the probe
        // process in write, which looks exactly like a dead chain. Level info,
        // not error — a failed REALITY handshake and rejected credentials are
        // both [Info]. Access logs off: the probe makes one connection and it can
        // say nothing the error log does not.
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            // Loopback only. Exposed, the probe port is a proxy anyone can use
            // without credentials.
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "vless",
            "settings": settings,
            "streamSettings": stream_settings(target),
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

fn anytls_client_config(
    target: &E2eProbeTarget,
    anytls: &brocade_deployment::protocol::E2eProbeAnyTls,
    socks_port: u16,
    log_path: &str,
) -> String {
    let mut settings = serde_json::json!({
        "address": target.dial_host,
        "port": target.port,
        "password": target.uuid,
    });
    if let Some(value) = anytls.idle_session_check_interval_secs {
        settings["idleSessionCheckInterval"] = serde_json::json!(value);
    }
    if let Some(value) = anytls.idle_session_timeout_secs {
        settings["idleSessionTimeout"] = serde_json::json!(value);
    }
    if let Some(value) = anytls.min_idle_session {
        settings["minIdleSession"] = serde_json::json!(value);
    }
    let mut stream_settings = if target.reality.public_key.is_empty() {
        let mut tls_settings = serde_json::json!({
            "serverName": anytls.server_name,
        });
        if let Some(pin) = &anytls.pinned_peer_cert_sha256 {
            tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
        }
        serde_json::json!({
            "security": "tls",
            "tlsSettings": tls_settings,
        })
    } else {
        serde_json::json!({
            "security": "reality",
            "realitySettings": {
                "serverName": target.reality.server_name,
                "fingerprint": target.reality.fingerprint,
                "publicKey": target.reality.public_key,
                "shortId": target.reality.short_id,
            },
        })
    };
    stream_settings["sockopt"] = serde_json::json!({ "tcpFastOpen": true });
    let config = serde_json::json!({
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "anytls",
            "settings": settings,
            "streamSettings": stream_settings,
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

fn hysteria_client_config(
    target: &E2eProbeTarget,
    hysteria: &brocade_deployment::protocol::E2eProbeHysteria2,
    socks_port: u16,
    log_path: &str,
) -> String {
    let mut quic = serde_json::Map::new();
    quic.insert(
        "congestion".to_owned(),
        serde_json::json!(hysteria.congestion),
    );
    if hysteria.congestion != "bbr" {
        if let Some(up) = &hysteria.up {
            quic.insert("brutalUp".to_owned(), serde_json::json!(up));
        }
        if let Some(down) = &hysteria.down {
            quic.insert("brutalDown".to_owned(), serde_json::json!(down));
        }
    }
    if let Some(profile) = &hysteria.bbr_profile {
        quic.insert("bbrProfile".to_owned(), serde_json::json!(profile));
    }
    for (key, value) in [
        (
            "initStreamReceiveWindow",
            hysteria.init_stream_receive_window,
        ),
        ("maxStreamReceiveWindow", hysteria.max_stream_receive_window),
        (
            "initConnectionReceiveWindow",
            hysteria.init_connection_receive_window,
        ),
        (
            "maxConnectionReceiveWindow",
            hysteria.max_connection_receive_window,
        ),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), serde_json::json!(value));
        }
    }
    for (key, value) in [
        ("maxIdleTimeout", hysteria.max_idle_timeout_secs),
        ("keepAlivePeriod", hysteria.keep_alive_period_secs),
    ] {
        if let Some(value) = value {
            quic.insert(key.to_owned(), serde_json::json!(value));
        }
    }
    if hysteria.disable_path_mtu_discovery {
        quic.insert(
            "disablePathMTUDiscovery".to_owned(),
            serde_json::json!(true),
        );
    }
    let mut finalmask = serde_json::Map::new();
    finalmask.insert("quicParams".to_owned(), serde_json::Value::Object(quic));
    if let Some(password) = &hysteria.salamander_password {
        finalmask.insert(
            "udp".to_owned(),
            serde_json::json!([{
                "type": "salamander",
                "settings": { "password": password },
            }]),
        );
    }
    let mut tls_settings = serde_json::json!({
        "serverName": hysteria.server_name,
    });
    if let Some(pin) = &hysteria.pinned_peer_cert_sha256 {
        tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
    }
    let config = serde_json::json!({
        "log": {
            "loglevel": "info",
            "error": log_path,
            "access": "none",
        },
        "inbounds": [{
            "tag": "probe-in",
            "listen": "127.0.0.1",
            "port": socks_port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false },
        }],
        "outbounds": [{
            "tag": "probe-out",
            "protocol": "hysteria",
            "settings": {
                "version": 2,
                "address": target.dial_host,
                "port": target.port,
            },
            "streamSettings": {
                "network": "hysteria",
                "security": "tls",
                "tlsSettings": tls_settings,
                "hysteriaSettings": {
                    "version": 2,
                    "auth": target.uuid,
                },
                "finalmask": serde_json::Value::Object(finalmask),
            },
        }],
    });
    serde_json::to_string(&config).unwrap_or_default()
}

/// The client half of what the ingress's own artifact says, assembled from the two axes the
/// target names: which certificate to expect, and whether the stream is carried inside HTTP.
fn stream_settings(target: &E2eProbeTarget) -> serde_json::Value {
    let mut settings = match &target.tls {
        // A certificate of the machine's own: nothing to configure but the name, which the
        // client checks the certificate against — so a wrong one fails here rather than at the
        // far end.
        // No `allowInsecure` here, and none on the Hysteria 2 side either: xray removed the
        // field in v26.2.6 and rejects the whole config from v26.6.1 on, so writing it does not
        // weaken the probe, it stops the probe process from starting at all. A machine whose
        // certificate does not verify therefore fails here — correctly, because a subscriber's
        // client fails in the same place. Pinning the certificate (`pinnedPeerCertSha256`) is the
        // replacement. Self-signed certificates carry that digest in the frozen probe target.
        Some(tls) => {
            let mut tls_settings = serde_json::json!({
                "serverName": tls.server_name,
            });
            if let Some(pin) = &tls.pinned_peer_cert_sha256 {
                tls_settings["pinnedPeerCertSha256"] = serde_json::json!(pin);
            }
            serde_json::json!({
                "security": "tls",
                "tlsSettings": tls_settings,
            })
        }
        None => serde_json::json!({
            "security": "reality",
            "realitySettings": {
                "serverName": target.reality.server_name,
                "fingerprint": target.reality.fingerprint,
                "publicKey": target.reality.public_key,
                "shortId": target.reality.short_id,
            },
        }),
    };
    match &target.xhttp {
        None => settings["network"] = serde_json::json!("tcp"),
        Some(xhttp) => {
            settings["network"] = serde_json::json!("xhttp");
            let mut options = serde_json::json!({ "path": xhttp.path });
            if let Some(host) = &xhttp.host {
                options["host"] = serde_json::json!(host);
            }
            if let Some(xmux) = &xhttp.xmux {
                let mut value = serde_json::Map::new();
                if let Some(concurrency) = xmux.max_concurrency {
                    value.insert("maxConcurrency".to_owned(), serde_json::json!(concurrency));
                }
                if let Some(connections) = xmux.max_connections {
                    value.insert("maxConnections".to_owned(), serde_json::json!(connections));
                }
                value.insert(
                    "hMaxRequestTimes".to_owned(),
                    xhttp_range(&xmux.h_max_request_times),
                );
                value.insert(
                    "hMaxReusableSecs".to_owned(),
                    xhttp_range(&xmux.h_max_reusable_secs),
                );
                if let Some(period) = xmux.h_keep_alive_period_secs {
                    value.insert("hKeepAlivePeriod".to_owned(), serde_json::json!(period));
                }
                options["xmux"] = serde_json::Value::Object(value);
            }
            if let Some(range) = &xhttp.x_padding_bytes {
                options["xPaddingBytes"] = xhttp_range(range);
            }
            // Absent means both ends resolve it the same way by themselves. Named, it has to
            // match: a server told to expect one upload shape refuses every client naming another.
            if let Some(mode) = &xhttp.mode {
                options["mode"] = serde_json::json!(mode);
            }
            settings["xhttpSettings"] = options;
        }
    }
    settings
}

fn xhttp_range(range: &brocade_deployment::protocol::E2eProbeXhttpRange) -> serde_json::Value {
    if range.from == range.to {
        serde_json::json!(range.from)
    } else {
        serde_json::json!(format!("{}-{}", range.from, range.to))
    }
}

// ── Process ─────────────────────────────────────────────────────────────────

/// A probe process, holding its `Child`. Always reaped on Drop — a forgotten kill
/// on any of the probe's failure paths accumulates orphaned xrays on this
/// machine, every one still listening on a loopback port.
struct ProbeProcess {
    child: Child,
    /// The config file must outlive the process, so its handle stays here.
    _config: TempFile,
    /// xray's error log. Deleted along with Drop.
    log: TempFile,
}

impl ProbeProcess {
    fn kill(&mut self) {
        // Kill this one PID only. Killing by name takes the production xray with
        // it — severing the line in order to check whether it works inverts cause
        // and effect.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// What it said. All failure diagnosis rests on this.
    ///
    /// It reads the log file rather than a pipe — xray writes `[Info]` to stdout,
    /// and stdout cannot be piped either (a full pipe wedges the process). Letting
    /// it write its own file sidesteps both traps.
    fn take_output(&mut self) -> String {
        let text = std::fs::read_to_string(&self.log.path).unwrap_or_default();
        // Take the last few lines: errors are at the end, the start is always the
        // version banner and "Reading config".
        let lines = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return "（没有输出）".to_owned();
        }
        lines
            .iter()
            .rev()
            .take(3)
            .rev()
            .copied()
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_xray(
    target: &E2eProbeTarget,
    socks_port: u16,
    xray_binary: &Path,
    runtime_dir: &Path,
) -> Result<ProbeProcess, String> {
    let log = TempFile::write(runtime_dir, "", ".log")?;
    let file = TempFile::write(
        runtime_dir,
        &client_config(target, socks_port, &log.path),
        ".json",
    )?;
    // Both streams go to the same file. A rejected config is reported on **stdout**,
    // and stderr stays empty: at that point xray's logging system is not initialized
    // and the `log.error` path has not taken effect. Discarding stdout is why every
    // VLESS+TLS chain reported "探测进程没起来：（没有输出）" after xray 26.4.25 removed
    // `allowInsecure` — the reason existed and was written to a discarded stream.
    //
    // A file rather than a pipe: a full pipe blocks the process in write, which is
    // indistinguishable from a dead chain. Both handles are opened `append`, so the
    // two streams interleave by whole writes rather than overwriting each other.
    let stdout = std::fs::OpenOptions::new()
        .append(true)
        .open(&log.path)
        .map_err(|error| format!("打不开探测日志：{error}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|error| format!("打不开探测日志：{error}"))?;
    let child = Command::new(xray_binary)
        .args(["-config", &file.path])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("起不了探测进程 {}：{error}", xray_binary.display()))?;
    Ok(ProbeProcess {
        child,
        _config: file,
        log,
    })
}

struct TempFile {
    path: String,
}

/// Sequence number for temporary files. It must be global — inside the function
/// every call would start from 0, and one process's config and log would take the
/// same name and overwrite each other.
static TEMP_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl TempFile {
    /// `suffix` is mandatory, and a config file must be `.json`: xray identifies
    /// the format by extension and exits with `Failed to get format` without one —
    /// a line emitted before the logging system initializes, so it leaves no trace
    /// even in the log file.
    fn write(runtime_dir: &Path, content: &str, suffix: &str) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt;

        // The name carries the PID and a global counter: probes are serial on one
        // machine, but a manual `e2e-once` can collide with the daemon, and a
        // single probe needs two files of its own.
        let path = runtime_dir
            .join(format!(
                "brocade-probe-{}-{}{suffix}",
                std::process::id(),
                TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
            .to_string_lossy()
            .into_owned();
        // `create_new` prevents a pre-created symlink from redirecting a credential-bearing
        // config, and mode is applied at creation rather than tightened in a later race window.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| format!("创建探测配置失败：{error}"))?;
        if let Err(error) = file.write_all(content.as_bytes()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(format!("写探测配置失败：{error}"));
        }
        Ok(Self { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Let the kernel pick a free port, then release it to xray.
///
/// The window in between is theoretically racy and practically not: the bind is on
/// loopback, and the only contender for ephemeral ports on this machine is us.
/// Losing the race costs only an `Unsupported` for this round; the next one is
/// fine.
fn free_local_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    listener.local_addr().ok().map(|addr| addr.port())
}

fn wait_for_listen(port: u16, cancellation: &ProbeCancellation) -> bool {
    let deadline = Instant::now() + STARTUP_WAIT;
    while Instant::now() < deadline && !cancellation.is_cancelled() {
        if TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), STARTUP_POLL)
            .is_ok()
        {
            return true;
        }
        std::thread::sleep(STARTUP_POLL);
    }
    false
}

// ── One request through socks ───────────────────────────────────────────────

struct HttpTarget {
    host: String,
    port: u16,
    path: String,
}

impl HttpTarget {
    /// Plain http only. Not laziness: what is measured is the chain itself, and
    /// mixing in the endpoint site's TLS handshake makes the number no longer the
    /// chain's alone.
    fn parse(url: &str) -> Result<Self, ()> {
        let rest = url.strip_prefix("http://").ok_or(())?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        if authority.is_empty() {
            return Err(());
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse().map_err(|_| ())?),
            None => (authority.to_owned(), 80),
        };
        Ok(Self { host, port, path })
    }
}

fn run_probe(
    socks_port: u16,
    endpoint: &HttpTarget,
    timeout: Duration,
    cancellation: &ProbeCancellation,
) -> Result<ProbeSuccess, (E2eProbeStatus, String)> {
    let started = Instant::now();
    let deadline = started + timeout;

    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, socks_port)),
        timeout,
    )
    .map_err(|error| {
        (
            E2eProbeStatus::Unsupported,
            format!("连不上本机的探测口：{error}"),
        )
    })?;
    // Writes go only to a loopback Xray and are tiny. Keep them bounded separately; reads use a
    // short polling timeout below so cancellation reaps the exact child promptly rather than
    // leaving it alive until the entire network timeout expires.
    let _ = stream.set_write_timeout(Some(timeout.min(Duration::from_secs(1))));
    let _ = stream.set_nodelay(true);

    socks5_connect(
        &mut stream,
        &endpoint.host,
        endpoint.port,
        deadline,
        cancellation,
    )
    .map_err(|error| {
        // Failure in the socks stage means xray could not establish the
        // connection. From the probe's side, "the REALITY handshake was refused"
        // and "the ingress never answered" are one phenomenon, and genuinely have
        // one next step: look at the xray on the chain's head.
        (
            E2eProbeStatus::HandshakeFailed,
            format!("连不上自己的入口：{error}"),
        )
    })?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: brocade-probe\r\nConnection: close\r\n\r\n",
        endpoint.path, endpoint.host
    );
    stream.write_all(request.as_bytes()).map_err(|error| {
        (
            E2eProbeStatus::ChainBroken,
            format!("请求发不出去：{error}"),
        )
    })?;

    // The moment of the first byte is TTFB. It counts the handshake, every
    // forwarding hop, and the exit reaching the internet — exactly what a user
    // waits through, and the only latency with business meaning.
    let mut first = [0_u8; 4096];
    let read = read_until(&mut stream, &mut first, deadline, cancellation).map_err(|error| {
        let status = if error.kind() == std::io::ErrorKind::WouldBlock
            || error.kind() == std::io::ErrorKind::TimedOut
        {
            E2eProbeStatus::Timeout
        } else {
            E2eProbeStatus::ChainBroken
        };
        (status, format!("等不到回应：{error}"))
    })?;
    if read == 0 {
        return Err((
            E2eProbeStatus::ChainBroken,
            "连接被对端关掉了，一个字节都没收到".to_owned(),
        ));
    }
    let ttfb_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    // Read the rest of the body — the exit IP is in it. Falling short is not a
    // failure: TTFB is already taken and reachability already settled, and a
    // missing exit IP only means the check cannot be made.
    let mut body = Vec::from(&first[..read]);
    let mut chunk = [0_u8; 4096];
    while body.len() < 8192 {
        match read_until(&mut stream, &mut chunk, deadline, cancellation) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&body);

    if !status_line_is_success(&text) {
        return Err((
            E2eProbeStatus::ChainBroken,
            format!("落点回了个错：{}", text.lines().next().unwrap_or("").trim()),
        ));
    }

    Ok(ProbeSuccess {
        ttfb_ms,
        exit_ip: trace_field(&text, "ip"),
        exit_loc: trace_field(&text, "loc"),
    })
}

/// 2xx and 3xx both count. A 301 from the endpoint proves the request did travel
/// the whole chain and arrive — which is what is being measured; whether it wants
/// to redirect us has nothing to do with the chain.
fn status_line_is_success(response: &str) -> bool {
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..400).contains(&code))
}

/// A cloudflare-trace-style body, one `key=value` per line.
fn trace_field(response: &str, key: &str) -> Option<String> {
    response
        .lines()
        .find_map(|line| line.trim().strip_prefix(&format!("{key}=")))
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

// ── SOCKS5 (a small slice of RFC 1928) ──────────────────────────────────────

fn socks5_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    deadline: Instant,
    cancellation: &ProbeCancellation,
) -> Result<(), String> {
    // Handshake: offer no-auth only
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .map_err(|error| error.to_string())?;
    let mut greeting = [0_u8; 2];
    read_exact_until(stream, &mut greeting, deadline, cancellation)
        .map_err(|error| error.to_string())?;
    if greeting != [0x05, 0x00] {
        return Err(format!("socks 握手被拒：{greeting:?}"));
    }

    // Send the hostname as-is rather than resolving locally. Local resolution
    // would take this machine's resolver and leave the chain's DNS policy (a
    // node's `dns` field) unmeasured — and that policy is part of the chain.
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err("落点主机名太长".to_owned());
    }
    let mut request = vec![0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8];
    request.extend_from_slice(host_bytes);
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .map_err(|error| error.to_string())?;

    let mut head = [0_u8; 4];
    read_exact_until(stream, &mut head, deadline, cancellation)
        .map_err(|error| error.to_string())?;
    if head[1] != 0x00 {
        return Err(format!("socks 建不了连接：REP={}", head[1]));
    }
    // Consume the bound address; only the bytes after it are application data
    let skip = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0_u8; 1];
            read_exact_until(stream, &mut len, deadline, cancellation)
                .map_err(|error| error.to_string())?;
            usize::from(len[0])
        }
        other => return Err(format!("socks 回了个没见过的地址类型 {other}")),
    };
    let mut rest = vec![0_u8; skip + 2];
    read_exact_until(stream, &mut rest, deadline, cancellation)
        .map_err(|error| error.to_string())?;
    Ok(())
}

const CANCEL_POLL: Duration = Duration::from_millis(100);

fn read_until(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    deadline: Instant,
    cancellation: &ProbeCancellation,
) -> std::io::Result<usize> {
    loop {
        if cancellation.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "probe canceled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "probe timeout",
            ));
        }
        stream.set_read_timeout(Some(remaining.min(CANCEL_POLL)))?;
        match stream.read(buffer) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut buffer: &mut [u8],
    deadline: Instant,
    cancellation: &ProbeCancellation,
) -> std::io::Result<()> {
    while !buffer.is_empty() {
        match read_until(stream, buffer, deadline, cancellation)? {
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "early eof",
                ));
            }
            read => buffer = &mut buffer[read..],
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(expected: &[&str]) -> E2eProbeTarget {
        E2eProbeTarget {
            app_id: Some("app".to_owned()),
            chain_id: "c1".to_owned(),
            chain_name: "链".to_owned(),
            ingress_id: "i1".to_owned(),
            dial_host: "127.0.0.1".to_owned(),
            port: 8443,
            uuid: "u".to_owned(),
            hysteria2: None,
            anytls: None,
            reality: brocade_deployment::protocol::E2eProbeReality {
                public_key: "pk".to_owned(),
                short_id: "sid".to_owned(),
                server_name: "example.com".to_owned(),
                fingerprint: "chrome".to_owned(),
                flow: Some("xtls-rprx-vision".to_owned()),
            },
            tls: None,
            xhttp: None,
            expected_exit_ips: expected.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[test]
    fn credential_configs_are_private_and_removed_with_the_guard() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "brocade-probe-test-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = {
            let file = TempFile::write(&dir, "credential", ".json").unwrap();
            assert_eq!(
                std::fs::metadata(&file.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            file.path.clone()
        };
        assert!(!Path::new(&path).exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn cancellation_interrupts_a_blocked_socket_without_waiting_for_the_full_timeout() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(250));
        });
        let mut client = TcpStream::connect(address).unwrap();
        let cancellation = ProbeCancellation::default();
        let trigger = cancellation.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            trigger.cancel();
        });
        let started = Instant::now();
        let error = read_until(
            &mut client,
            &mut [0_u8; 1],
            Instant::now() + Duration::from_secs(5),
            &cancellation,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    /// "Cannot check" must be its own outcome. Counted as a pass, a misconfigured
    /// chain shows as healthy purely because it cannot be falsified — the one
    /// failure mode this feature must never have.
    #[test]
    fn empty_expectation_is_unknown_not_match() {
        assert_eq!(
            judge_exit(&target(&[]), Some("203.0.113.9")),
            E2eExitVerdict::Unknown
        );
    }

    #[test]
    fn exit_matches_when_the_address_is_one_of_the_expected() {
        let target = target(&["198.51.100.7", "2001:db8::7"]);
        assert_eq!(
            judge_exit(&target, Some("198.51.100.7")),
            E2eExitVerdict::Match
        );
        assert_eq!(
            judge_exit(&target, Some("2001:db8::7")),
            E2eExitVerdict::Match
        );
        assert_eq!(
            judge_exit(&target, Some("203.0.113.9")),
            E2eExitVerdict::Mismatch
        );
    }

    /// The probe config must carry flow. It decides whether XTLS Vision is used,
    /// and those are two different data planes — omitted, the handshake still
    /// succeeds but a different path is being measured.
    /// A target carried inside HTTP has to produce a client carried inside HTTP. Dialing plain
    /// TCP at one is refused by the server's path check, and the chain reports down while it is
    /// carrying traffic.
    #[test]
    fn a_target_inside_http_produces_a_client_inside_http() {
        let mut t = target(&[]);
        t.xhttp = Some(brocade_deployment::protocol::E2eProbeXhttp {
            path: "/probe".to_owned(),
            host: Some("upload.route.example".to_owned()),
            xmux: Some(brocade_deployment::protocol::E2eProbeXhttpXmux {
                max_concurrency: Some(16),
                max_connections: None,
                h_max_request_times: brocade_deployment::protocol::E2eProbeXhttpRange {
                    from: 600,
                    to: 900,
                },
                h_max_reusable_secs: brocade_deployment::protocol::E2eProbeXhttpRange {
                    from: 1800,
                    to: 3000,
                },
                h_keep_alive_period_secs: Some(15),
            }),
            x_padding_bytes: Some(brocade_deployment::protocol::E2eProbeXhttpRange {
                from: 200,
                to: 600,
            }),
            mode: Some("stream-one".to_owned()),
        });
        let out: serde_json::Value =
            serde_json::from_str(&client_config(&t, 1080, "/tmp/x")).unwrap();
        let stream = &out["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], "xhttp");
        assert_eq!(stream["xhttpSettings"]["path"], "/probe");
        assert_eq!(stream["xhttpSettings"]["host"], "upload.route.example");
        assert_eq!(stream["xhttpSettings"]["xmux"]["maxConcurrency"], 16);
        assert_eq!(
            stream["xhttpSettings"]["xmux"]["hMaxRequestTimes"],
            "600-900"
        );
        assert_eq!(
            stream["xhttpSettings"]["xmux"]["hMaxReusableSecs"],
            "1800-3000"
        );
        assert_eq!(stream["xhttpSettings"]["xmux"]["hKeepAlivePeriod"], 15);
        assert_eq!(stream["xhttpSettings"]["xPaddingBytes"], "200-600");
        assert!(stream["xhttpSettings"].get("scMaxEachPostBytes").is_none());
        assert!(stream["xhttpSettings"]
            .get("scMinPostsIntervalMs")
            .is_none());
        assert!(stream["xhttpSettings"].get("uplinkChunkSize").is_none());
        assert_eq!(stream["xhttpSettings"]["mode"], "stream-one");
    }

    /// A target presenting its own certificate must be verified against it, not against a
    /// borrowed site's public key — and the REALITY block beside it is filler that has to stay
    /// out of the client entirely.
    #[test]
    fn a_target_with_its_own_certificate_produces_a_tls_client() {
        let mut t = target(&[]);
        t.tls = Some(brocade_deployment::protocol::E2eProbeTls {
            server_name: "a1b2.example.net".to_owned(),
            pinned_peer_cert_sha256: Some(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            ),
            flow: None,
        });
        let out: serde_json::Value =
            serde_json::from_str(&client_config(&t, 1080, "/tmp/x")).unwrap();
        let stream = &out["outbounds"][0]["streamSettings"];
        assert_eq!(stream["security"], "tls");
        assert_eq!(stream["tlsSettings"]["serverName"], "a1b2.example.net");
        assert_eq!(
            stream["tlsSettings"]["pinnedPeerCertSha256"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(stream["tlsSettings"].get("fingerprint").is_none());
        // xray rejects a config carrying `allowInsecure` outright (removed in v26.2.6, fatal
        // from v26.6.1). Writing it does not loosen verification, it prevents the probe process
        // from starting — which reported as "探测进程没起来：（没有输出）" on every VLESS+TLS chain.
        assert!(stream["tlsSettings"].get("allowInsecure").is_none());
        assert!(stream.get("realitySettings").is_none());
        assert!(out["outbounds"][0]["settings"]["vnext"][0]["users"][0]
            .get("flow")
            .is_none());
    }

    #[test]
    fn an_anytls_target_produces_an_anytls_client() {
        let mut t = target(&[]);
        t.reality = brocade_deployment::protocol::E2eProbeReality {
            public_key: String::new(),
            short_id: String::new(),
            server_name: String::new(),
            fingerprint: String::new(),
            flow: None,
        };
        t.anytls = Some(brocade_deployment::protocol::E2eProbeAnyTls {
            server_name: "anytls.example.net".to_owned(),
            pinned_peer_cert_sha256: Some(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            ),
            idle_session_check_interval_secs: Some(11),
            idle_session_timeout_secs: Some(22),
            min_idle_session: Some(3),
        });
        t.port = 19443;
        let out: serde_json::Value =
            serde_json::from_str(&client_config(&t, 1080, "/tmp/x")).unwrap();
        let outbound = &out["outbounds"][0];
        assert_eq!(outbound["protocol"], "anytls");
        assert_eq!(outbound["settings"]["address"], "127.0.0.1");
        assert_eq!(outbound["settings"]["port"], 19443);
        assert_eq!(outbound["settings"]["password"], "u");
        assert_eq!(outbound["settings"]["idleSessionCheckInterval"], 11);
        assert_eq!(outbound["settings"]["idleSessionTimeout"], 22);
        assert_eq!(outbound["settings"]["minIdleSession"], 3);
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["serverName"],
            "anytls.example.net"
        );
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["pinnedPeerCertSha256"],
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(outbound["streamSettings"]["sockopt"]["tcpFastOpen"], true);
        assert!(outbound["streamSettings"].get("network").is_none());
    }

    #[test]
    fn xray_accepts_the_retained_self_signed_anytls_probe_pins() {
        let binary = std::env::var_os("BROCADE_XRAY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tools/xray"));
        if !binary.is_file() {
            eprintln!("skipping: place xray at .tools/xray");
            return;
        }
        let mut t = target(&[]);
        t.reality = brocade_deployment::protocol::E2eProbeReality {
            public_key: String::new(),
            short_id: String::new(),
            server_name: String::new(),
            fingerprint: String::new(),
            flow: None,
        };
        t.anytls = Some(brocade_deployment::protocol::E2eProbeAnyTls {
            server_name: "private.apple.com".to_owned(),
            pinned_peer_cert_sha256: Some(
                concat!(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                )
                .to_owned(),
            ),
            idle_session_check_interval_secs: None,
            idle_session_timeout_secs: None,
            min_idle_session: None,
        });
        let config = client_config(&t, 1080, "/tmp/brocade-probe-test.log");
        let file = TempFile::write(&std::env::temp_dir(), &config, ".json").unwrap();
        let checked = Command::new(binary)
            .args(["-test", "-c", &file.path])
            .output()
            .expect("run xray -test");
        let stdout = String::from_utf8_lossy(&checked.stdout);
        let stderr = String::from_utf8_lossy(&checked.stderr);
        if stdout.contains("unknown config id: anytls")
            || stderr.contains("unknown config id: anytls")
        {
            eprintln!("skipping: installed xray predates AnyTLS");
            return;
        }
        assert!(
            checked.status.success(),
            "xray rejected self-signed AnyTLS probe:\n{}\n{}\n{config}",
            stdout,
            stderr
        );
    }

    #[test]
    fn an_anytls_reality_target_produces_a_reality_client() {
        let mut t = target(&[]);
        t.anytls = Some(brocade_deployment::protocol::E2eProbeAnyTls {
            server_name: "example.com".to_owned(),
            pinned_peer_cert_sha256: None,
            idle_session_check_interval_secs: None,
            idle_session_timeout_secs: None,
            min_idle_session: None,
        });
        let out: serde_json::Value =
            serde_json::from_str(&client_config(&t, 1080, "/tmp/x")).unwrap();
        let stream = &out["outbounds"][0]["streamSettings"];
        assert_eq!(stream["security"], "reality");
        assert_eq!(stream["realitySettings"]["serverName"], "example.com");
        assert_eq!(stream["realitySettings"]["publicKey"], "pk");
        assert_eq!(stream["realitySettings"]["shortId"], "sid");
        assert_eq!(stream["realitySettings"]["fingerprint"], "chrome");
        assert_eq!(stream["sockopt"]["tcpFastOpen"], true);
        assert!(stream.get("tlsSettings").is_none());
    }

    #[test]
    fn a_hysteria2_target_produces_the_same_quic_client_shape_as_its_subscription() {
        let mut t = target(&[]);
        t.hysteria2 = Some(brocade_deployment::protocol::E2eProbeHysteria2 {
            server_name: "hy2.example.net".to_owned(),
            pinned_peer_cert_sha256: Some(
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
            ),
            congestion: "force-brutal".to_owned(),
            up: Some("20 mbps".to_owned()),
            down: Some("100 mbps".to_owned()),
            bbr_profile: Some("conservative".to_owned()),
            init_stream_receive_window: Some(131_072),
            max_stream_receive_window: Some(262_144),
            init_connection_receive_window: Some(327_680),
            max_connection_receive_window: Some(655_360),
            max_idle_timeout_secs: Some(30),
            keep_alive_period_secs: Some(10),
            disable_path_mtu_discovery: true,
            salamander_password: Some("obfs-secret".to_owned()),
        });
        let out: serde_json::Value =
            serde_json::from_str(&client_config(&t, 1080, "/tmp/x")).unwrap();
        let outbound = &out["outbounds"][0];
        assert_eq!(outbound["protocol"], "hysteria");
        assert_eq!(outbound["settings"]["version"], 2);
        assert_eq!(outbound["streamSettings"]["network"], "hysteria");
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["serverName"],
            "hy2.example.net"
        );
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["pinnedPeerCertSha256"],
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        );
        assert_eq!(outbound["streamSettings"]["hysteriaSettings"]["auth"], "u");
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["congestion"],
            "force-brutal"
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["brutalDown"],
            "100 mbps"
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["bbrProfile"],
            "conservative"
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["maxConnectionReceiveWindow"],
            655_360
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["keepAlivePeriod"],
            10
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["quicParams"]["disablePathMTUDiscovery"],
            true
        );
        assert_eq!(
            outbound["streamSettings"]["finalmask"]["udp"][0]["settings"]["password"],
            "obfs-secret"
        );
        assert!(outbound["settings"].get("vnext").is_none());
        assert!(outbound["streamSettings"].get("realitySettings").is_none());
    }

    #[test]
    fn client_config_carries_every_reality_parameter() {
        let config: serde_json::Value =
            serde_json::from_str(&client_config(&target(&[]), 10800, "/tmp/probe-test.log"))
                .unwrap();
        let out = &config["outbounds"][0];
        assert_eq!(
            out["settings"]["vnext"][0]["users"][0]["flow"],
            "xtls-rprx-vision"
        );
        assert_eq!(out["settings"]["vnext"][0]["users"][0]["id"], "u");
        let reality = &out["streamSettings"]["realitySettings"];
        assert_eq!(reality["serverName"], "example.com");
        assert_eq!(reality["fingerprint"], "chrome");
        assert_eq!(reality["publicKey"], "pk");
        assert_eq!(reality["shortId"], "sid");
    }

    /// The probe port listens on loopback only. Exposed, it is a proxy anyone can
    /// use without credentials.
    #[test]
    fn probe_inbound_never_listens_outside() {
        let config: serde_json::Value =
            serde_json::from_str(&client_config(&target(&[]), 10800, "/tmp/probe-test.log"))
                .unwrap();
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
    }

    fn probe_with(status: E2eProbeStatus) -> E2eProbe {
        E2eProbe {
            app_id: None,
            chain_id: "c".to_owned(),
            status,
            ttfb_ms: None,
            exit_ip: None,
            exit_loc: None,
            exit_verdict: E2eExitVerdict::Unknown,
            detail: None,
        }
    }

    /// What is worth retrying is failure on the chain's side, not facts about this
    /// machine. Retrying `Unsupported` too makes a machine without xray run twice
    /// for nothing every round.
    #[test]
    fn only_chain_side_failures_are_retried() {
        for status in [
            E2eProbeStatus::Timeout,
            E2eProbeStatus::ChainBroken,
            E2eProbeStatus::HandshakeFailed,
        ] {
            assert!(worth_retrying(&probe_with(status)), "{status:?}");
        }
        assert!(!worth_retrying(&probe_with(E2eProbeStatus::Unsupported)));
        assert!(!worth_retrying(&probe_with(E2eProbeStatus::Ok)));
    }

    /// Logs must land in a file. xray writes `[Info]` to stdout, so capturing stderr
    /// alone never gets them — the symptom is every failure detail ending in "(no
    /// output)" at exactly the moment detail matters most.
    #[test]
    fn probe_config_writes_its_log_to_a_file() {
        let config: serde_json::Value =
            serde_json::from_str(&client_config(&target(&[]), 10800, "/tmp/x.log")).unwrap();
        assert_eq!(config["log"]["error"], "/tmp/x.log");
        assert_eq!(config["log"]["loglevel"], "info");
        assert_eq!(config["log"]["access"], "none");
    }

    /// Temporary filenames must genuinely not collide. With the counter inside the
    /// function every call starts from 0, and one probe's config and log take the
    /// same name and overwrite each other — the symptom being xray reading an empty
    /// config while the report says the chain is down.
    #[test]
    fn temp_files_do_not_collide() {
        let runtime_dir = std::env::temp_dir();
        let a = TempFile::write(&runtime_dir, "a", ".json").unwrap();
        let b = TempFile::write(&runtime_dir, "b", ".log").unwrap();
        assert_ne!(a.path, b.path);
        // The config must be .json: xray identifies the format by extension and
        // exits without even writing a log when there is none.
        assert!(a.path.ends_with(".json"), "{}", a.path);
        assert_eq!(std::fs::read_to_string(&a.path).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(&b.path).unwrap(), "b");
    }

    #[test]
    fn http_target_parsing() {
        let parsed = HttpTarget::parse("http://cp.cloudflare.com/cdn-cgi/trace").unwrap();
        assert_eq!(parsed.host, "cp.cloudflare.com");
        assert_eq!(parsed.port, 80);
        assert_eq!(parsed.path, "/cdn-cgi/trace");

        let with_port = HttpTarget::parse("http://example.net:8080").unwrap();
        assert_eq!(with_port.port, 8080);
        assert_eq!(with_port.path, "/");

        // https is explicitly unsupported: the endpoint site's TLS handshake would
        // mix into the number
        assert!(HttpTarget::parse("https://example.net/").is_err());
    }

    #[test]
    fn trace_body_fields() {
        let body = "HTTP/1.1 200 OK\r\n\r\nfl=abc\nip=198.51.100.7\nloc=TW\ntls=off\n";
        assert_eq!(trace_field(body, "ip").as_deref(), Some("198.51.100.7"));
        assert_eq!(trace_field(body, "loc").as_deref(), Some("TW"));
        assert_eq!(trace_field(body, "nope"), None);
        assert!(status_line_is_success(body));
    }

    /// A 5xx from the endpoint means it did not get through, a 301 that it did.
    /// Conflated, a redirecting endpoint reports the whole fleet as down.
    #[test]
    fn redirects_count_as_reachable_but_errors_do_not() {
        assert!(status_line_is_success("HTTP/1.1 301 Moved\r\n"));
        assert!(status_line_is_success("HTTP/1.1 204 No Content\r\n"));
        assert!(!status_line_is_success("HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(!status_line_is_success("garbage"));
    }
}
