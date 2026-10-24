//! On-demand, non-durable NIC rates.
//!
//! The connection is always initiated by the Agent, so neither the browser nor the control plane
//! needs inbound access to a machine. A connected socket is cheap and idle; samples begin only on
//! `Start`, are never spooled, and disappear if either side restarts.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use brocade_core::model::parse_grant_label;
use brocade_deployment::protocol::{
    AgentRealtimeCommand, AgentRealtimeSample, RealtimeUserTrafficRate, RealtimeUserTrafficReport,
    RealtimeUserTrafficSelector, RealtimeUserTrafficState, AGENT_PROTOCOL_VERSION,
    MAX_REALTIME_FORWARDING_TARGETS, MAX_REALTIME_USER_TRAFFIC_ROWS,
};
use tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Error as WebSocketError, Message,
    WebSocket,
};

use crate::{
    http::{HttpClient, Stream},
    options::Options,
};

mod diagnostics;
const INITIAL_RECONNECT: Duration = Duration::from_secs(1);
const MAX_RECONNECT: Duration = Duration::from_secs(300);
/// NIC rates stay at the requested live cadence, while the much larger worker snapshots refresh
/// only often enough for their 15-second UI freshness budget. This removes repeated serialization
/// and transport of an unchanged fleet-shaped report from every one-second sample.
const DIAGNOSTICS_REFRESH: Duration = Duration::from_secs(5);
const MAX_FRAME_BYTES: usize = 512 * 1024;
const MAX_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
fn accepted_interval(interval_millis: u32) -> bool {
    matches!(interval_millis, 1_000 | 2_000 | 5_000)
}

#[derive(Debug, Clone)]
struct NicReading {
    interface: String,
    rx_bytes: u64,
    tx_bytes: u64,
    sampled_at_unix_millis: i64,
    instant: Instant,
}

#[derive(Default)]
struct RateSampler {
    previous: Option<NicReading>,
    sequence: u64,
    pending_gap: bool,
}

type UserTrafficCounters = BTreeMap<String, (u64, u64)>;

#[derive(Default)]
struct UserTrafficSampler {
    previous: Option<(Instant, UserTrafficCounters)>,
}

impl UserTrafficSampler {
    fn reset(&mut self) {
        self.previous = None;
    }

    fn advance(
        &mut self,
        state_dir: &Path,
        selector: &RealtimeUserTrafficSelector,
        instant: Instant,
    ) -> RealtimeUserTrafficReport {
        if crate::serving_xray_process_in(state_dir).is_none() {
            match crate::accounting::confirmed_stopped_counters(state_dir) {
                Ok(Some(counters)) => {
                    self.previous = Some((
                        instant,
                        counters
                            .into_iter()
                            .map(|counter| {
                                (
                                    counter.label,
                                    (counter.uplink_bytes, counter.downlink_bytes),
                                )
                            })
                            .collect(),
                    ));
                    let mut report = user_traffic_report(RealtimeUserTrafficState::Ready, selector);
                    report.elapsed_millis = Some(1_000);
                    return report;
                }
                Ok(None) => {}
                Err(error) => eprintln!("realtime: stopped accounting unavailable: {error}"),
            }
        }
        let current = match read_user_counters(state_dir, !selector.forwarding().is_empty()) {
            Ok(current) => current,
            Err(_) => {
                self.reset();
                return user_traffic_report(RealtimeUserTrafficState::Unavailable, selector);
            }
        };
        self.advance_counters(current, selector, instant)
    }

    fn advance_counters(
        &mut self,
        current: UserTrafficCounters,
        selector: &RealtimeUserTrafficSelector,
        instant: Instant,
    ) -> RealtimeUserTrafficReport {
        let Some((previous_at, previous)) = self.previous.replace((instant, current.clone()))
        else {
            return user_traffic_report(RealtimeUserTrafficState::Warming, selector);
        };
        let elapsed = instant.saturating_duration_since(previous_at);
        let Some(elapsed_millis) = u32::try_from(elapsed.as_millis()).ok() else {
            self.reset();
            return user_traffic_report(RealtimeUserTrafficState::Warming, selector);
        };
        if !(100..=60_000).contains(&elapsed_millis) {
            self.reset();
            return user_traffic_report(RealtimeUserTrafficState::Warming, selector);
        }

        let selected = match selector {
            RealtimeUserTrafficSelector::All { .. } => None,
            RealtimeUserTrafficSelector::Labels { labels, .. } => {
                Some(labels.iter().map(String::as_str).collect::<BTreeSet<_>>())
            }
        };
        let mut users = Vec::new();
        let mut has_gap = false;
        for (label, (uplink, downlink)) in &current {
            let forwarding = label
                .strip_prefix("fwd:")
                .and_then(|id| id.parse::<i64>().ok())
                .and_then(|id| {
                    selector
                        .forwarding()
                        .iter()
                        .find(|target| target.rule_id == id)
                });
            if forwarding.is_none()
                && selected
                    .as_ref()
                    .is_some_and(|labels| !labels.contains(label.as_str()))
            {
                continue;
            }
            let Some((previous_uplink, previous_downlink)) = previous.get(label) else {
                has_gap = true;
                continue;
            };
            if uplink < previous_uplink || downlink < previous_downlink {
                has_gap = true;
                continue;
            }
            let identity = if let Some(target) = forwarding {
                (
                    target.user_id.as_str(),
                    target.tenant_id.as_str(),
                    None,
                    Some(target.rule_id),
                )
            } else if let Some((user, tenant, ingress)) = parse_grant_label(label) {
                (user, tenant, Some(ingress.to_owned()), None)
            } else {
                continue;
            };
            let rate = |value: u64, before: u64| {
                u64::try_from(
                    u128::from(value - before)
                        .saturating_mul(1000)
                        .checked_div(u128::from(elapsed_millis))
                        .unwrap_or(0),
                )
                .unwrap_or(u64::MAX)
            };
            let uplink_bytes_per_sec = rate(*uplink, *previous_uplink);
            let downlink_bytes_per_sec = rate(*downlink, *previous_downlink);
            if matches!(selector, RealtimeUserTrafficSelector::All { .. })
                && uplink_bytes_per_sec == 0
                && downlink_bytes_per_sec == 0
            {
                continue;
            }
            users.push(RealtimeUserTrafficRate {
                tenant_id: identity.1.to_owned(),
                user_id: identity.0.to_owned(),
                ingress_id: identity.2,
                forwarding_rule_id: identity.3,
                uplink_bytes_per_sec,
                downlink_bytes_per_sec,
            });
        }
        users.sort_by(|left, right| {
            let left_total = left
                .uplink_bytes_per_sec
                .saturating_add(left.downlink_bytes_per_sec);
            let right_total = right
                .uplink_bytes_per_sec
                .saturating_add(right.downlink_bytes_per_sec);
            right_total.cmp(&left_total).then_with(|| {
                (
                    &left.tenant_id,
                    &left.user_id,
                    &left.ingress_id,
                    left.forwarding_rule_id,
                )
                    .cmp(&(
                        &right.tenant_id,
                        &right.user_id,
                        &right.ingress_id,
                        right.forwarding_rule_id,
                    ))
            })
        });
        let truncated = users.len() > MAX_REALTIME_USER_TRAFFIC_ROWS;
        users.truncate(MAX_REALTIME_USER_TRAFFIC_ROWS);
        RealtimeUserTrafficReport {
            state: RealtimeUserTrafficState::Ready,
            all_users: matches!(selector, RealtimeUserTrafficSelector::All { .. }),
            elapsed_millis: Some(elapsed_millis),
            users,
            truncated,
            has_gap,
            forwarding_supported: true,
            forwarding_expected: !selector.forwarding().is_empty(),
        }
    }
}

fn user_traffic_report(
    state: RealtimeUserTrafficState,
    selector: &RealtimeUserTrafficSelector,
) -> RealtimeUserTrafficReport {
    RealtimeUserTrafficReport {
        state,
        all_users: matches!(selector, RealtimeUserTrafficSelector::All { .. }),
        elapsed_millis: None,
        users: Vec::new(),
        truncated: false,
        has_gap: false,
        forwarding_supported: true,
        forwarding_expected: !selector.forwarding().is_empty(),
    }
}

fn read_user_counters(state_dir: &Path, forwarding: bool) -> Result<UserTrafficCounters, String> {
    let content = fs::read_to_string(state_dir.join("xray.json"))
        .map_err(|error| format!("cannot read xray config: {error}"))?;
    let api_address = crate::xray_api_address(&content).ok_or("xray API address is absent")?;
    let mut counters: UserTrafficCounters = crate::read_xray_usage_counters(api_address)?
        .into_iter()
        .filter(|counter| parse_grant_label(&counter.label).is_some())
        .map(|counter| {
            (
                counter.label,
                (counter.uplink_bytes, counter.downlink_bytes),
            )
        })
        .collect();
    if forwarding {
        counters.extend(
            crate::forwarding::read_counters(api_address)?
                .into_iter()
                .map(|(id, counters)| (format!("fwd:{id}"), counters)),
        );
    }
    Ok(counters)
}

fn valid_user_traffic_selector(selector: &Option<RealtimeUserTrafficSelector>) -> bool {
    let Some(selector) = selector else {
        return true;
    };
    let forwarding = selector.forwarding();
    if forwarding.len() > MAX_REALTIME_FORWARDING_TARGETS
        || forwarding.iter().any(|target| {
            target.rule_id <= 0
                || target.tenant_id.is_empty()
                || target.tenant_id.len() > 128
                || target.user_id.is_empty()
                || target.user_id.len() > 64
        })
        || forwarding
            .iter()
            .map(|t| t.rule_id)
            .collect::<BTreeSet<_>>()
            .len()
            != forwarding.len()
    {
        return false;
    }
    match selector {
        RealtimeUserTrafficSelector::All { .. } => true,
        RealtimeUserTrafficSelector::Labels { labels, .. } => {
            (!labels.is_empty() || !forwarding.is_empty())
                && labels.len() <= MAX_REALTIME_USER_TRAFFIC_ROWS
                && labels.iter().all(|label| {
                    !label.is_empty() && label.len() <= 512 && parse_grant_label(label).is_some()
                })
                && labels.iter().collect::<BTreeSet<_>>().len() == labels.len()
        }
    }
}

impl RateSampler {
    fn reset(&mut self) {
        self.previous = None;
        self.pending_gap = true;
    }

    fn advance(
        &mut self,
        reading: NicReading,
        expected_interval: Duration,
    ) -> Option<AgentRealtimeSample> {
        let Some(previous) = self.previous.replace(reading.clone()) else {
            self.pending_gap = true;
            return None;
        };
        let elapsed = reading.instant.saturating_duration_since(previous.instant);
        if reading.interface != previous.interface
            || reading.rx_bytes < previous.rx_bytes
            || reading.tx_bytes < previous.tx_bytes
            || elapsed < Duration::from_millis(100)
            || elapsed > Duration::from_secs(60)
        {
            self.pending_gap = true;
            return None;
        }
        let elapsed_millis = u32::try_from(elapsed.as_millis()).ok()?;
        let rate = |current: u64, before: u64| {
            u64::try_from(
                u128::from(current - before)
                    .saturating_mul(1000)
                    .checked_div(u128::from(elapsed_millis))
                    .unwrap_or(0),
            )
            .unwrap_or(u64::MAX)
        };
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let has_timing_gap = elapsed > expected_interval.saturating_mul(2);
        let sample = AgentRealtimeSample {
            diagnostics_unchanged: false,
            mux_unchanged: false,
            reverse_health: None,
            mux: None,
            vpngate: None,
            user_traffic: None,
            sequence: self.sequence,
            sampled_at_unix_millis: reading.sampled_at_unix_millis,
            elapsed_millis,
            interface: reading.interface,
            rx_bytes_per_sec: rate(reading.rx_bytes, previous.rx_bytes),
            tx_bytes_per_sec: rate(reading.tx_bytes, previous.tx_bytes),
            has_gap: self.pending_gap || has_timing_gap,
        };
        self.pending_gap = false;
        Some(sample)
    }
}

type HealthCache =
    std::sync::Arc<std::sync::Mutex<Option<brocade_deployment::protocol::ReverseHealthReport>>>;
type MuxCache = std::sync::Arc<std::sync::Mutex<Option<brocade_deployment::protocol::MuxReport>>>;
pub(crate) type VpngateCache =
    std::sync::Arc<std::sync::Mutex<Option<brocade_deployment::protocol::VpngateRealtimeReport>>>;

fn populate_diagnostics(
    sample: &mut AgentRealtimeSample,
    requested: bool,
    due: bool,
    health: &HealthCache,
    mux: &MuxCache,
    vpngate: &VpngateCache,
    mux_baseline: &mut Option<Option<brocade_deployment::protocol::MuxReport>>,
) -> bool {
    if !requested || !due {
        sample.diagnostics_unchanged = true;
        return false;
    }
    sample.reverse_health = health.lock().ok().and_then(|value| value.clone());
    let current_mux = mux.lock().ok().and_then(|value| value.clone());
    if mux_baseline
        .as_ref()
        .is_some_and(|previous| same_mux_state(previous, &current_mux))
    {
        sample.mux_unchanged = true;
    } else {
        sample.mux = current_mux.clone();
        *mux_baseline = Some(current_mux);
    }
    sample.vpngate = vpngate.lock().ok().and_then(|value| value.clone());
    true
}

/// Snapshot timestamps, lease countdowns and successful keepalive counters change while the pool
/// carries no business. They remain locally observable but do not justify retransmitting the
/// entire worker set. Failures, topology, lifecycle events and business counters remain part of
/// the comparison and therefore produce a new baseline immediately.
fn same_mux_state(
    previous: &Option<brocade_deployment::protocol::MuxReport>,
    current: &Option<brocade_deployment::protocol::MuxReport>,
) -> bool {
    match (previous, current) {
        (None, None) => true,
        (Some(previous), Some(current)) => {
            let mut previous = previous.clone();
            let mut current = current.clone();
            normalize_idle_mux_fields(&mut previous);
            normalize_idle_mux_fields(&mut current);
            previous == current
        }
        _ => false,
    }
}

fn normalize_idle_mux_fields(report: &mut brocade_deployment::protocol::MuxReport) {
    report.sampled_at_unix_ms = 0;
    for pool in &mut report.pools {
        pool.probes = 0;
        pool.acks = 0;
    }
    for worker in &mut report.workers {
        worker.ack_age_ms = 0;
        worker.rtt_ms = 0;
        worker.probes = 0;
        worker.acks = 0;
        worker.lease_remaining_ms = 0;
        worker.queue_delay_ms = 0;
    }
}

pub(crate) fn run(options: &Options, vpngate: VpngateCache) {
    let diagnostics = match diagnostics::Worker::start(options.state_dir.clone()) {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("realtime: {error}");
            return;
        }
    };
    let mut backoff = INITIAL_RECONNECT;
    loop {
        match connect(options) {
            Ok(socket) => {
                backoff = INITIAL_RECONNECT;
                if let Err(error) = serve(socket, &options.state_dir, &diagnostics, &vpngate) {
                    eprintln!("realtime: connection ended: {error}");
                }
            }
            Err(error) => eprintln!("realtime: connect failed: {error}"),
        }
        thread::sleep(backoff);
        backoff = backoff.saturating_mul(2).min(MAX_RECONNECT);
    }
}

fn connect(options: &Options) -> Result<WebSocket<Stream>, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let url = websocket_url(&options.server)?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| "invalid control-plane WebSocket URL".to_owned())?;
    let headers = request.headers_mut();
    headers.insert(
        "authorization",
        format!("Bearer {}", options.token)
            .parse()
            .map_err(|_| "node token is not a valid HTTP header".to_owned())?,
    );
    headers.insert(
        "user-agent",
        format!("brocade-agent/{}", crate::identity::self_identity())
            .parse()
            .expect("binary identity is a valid HTTP header"),
    );
    headers.insert(
        "x-brocade-protocol-version",
        AGENT_PROTOCOL_VERSION
            .to_string()
            .parse()
            .expect("protocol version is a valid HTTP header"),
    );
    // TCP, system DNS, TLS and Upgrade share one deadline. The stream enforces it beneath
    // rustls so TLS control messages cannot extend a blocked HTTP Upgrade indefinitely.
    let client = HttpClient::new(&options.server)?;
    let stream = client.connect_stream(deadline)?;
    let config = WebSocketConfig::default()
        // Commands and samples are tiny JSON values. Bounding both directions prevents a broken
        // or hostile peer from turning this optional channel into unbounded Agent memory.
        .read_buffer_size(4 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_WRITE_BUFFER_BYTES)
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (mut socket, _) = tungstenite::client::client_with_config(request, stream, Some(config))
        .map_err(|_| "WebSocket handshake failed or timed out".to_owned())?;
    // Tungstenite documents every I/O error except WouldBlock as fatal. A read timeout therefore
    // cannot be used as a periodic wake-up: treating TimedOut as recoverable corrupts its state,
    // while treating it as fatal reconnects every idle interval. Nonblocking I/O plus a socket
    // readiness wait gives sampling a precise clock and leaves WouldBlock recoverable.
    socket
        .get_mut()
        .finish_handshake()
        .map_err(|error| format!("WebSocket handshake completion failed: {:?}", error.kind()))?;
    Ok(socket)
}

fn websocket_url(server: &str) -> Result<String, String> {
    let server = server.trim_end_matches('/');
    if let Some(rest) = server.strip_prefix("https://") {
        return Ok(format!("wss://{rest}/agent/v1/realtime"));
    }
    if let Some(rest) = server.strip_prefix("http://") {
        return Ok(format!("ws://{rest}/agent/v1/realtime"));
    }
    Err("server URL must start with http:// or https://".to_owned())
}

fn serve(
    mut socket: WebSocket<Stream>,
    state_dir: &Path,
    diagnostics_worker: &diagnostics::Worker,
    vpngate: &VpngateCache,
) -> Result<(), String> {
    let subscription = diagnostics_worker.subscribe();
    let mut active: Option<(Duration, bool, Option<RealtimeUserTrafficSelector>)> = None;
    let mut next_sample = Instant::now();
    let mut next_diagnostics = Instant::now();
    let mut mux_baseline = None;
    let mut sampler = RateSampler::default();
    let mut user_traffic_sampler = UserTrafficSampler::default();
    let mut write_pending = false;
    sampler.reset();

    loop {
        if write_pending {
            write_pending = flush_pending(&mut socket)?;
        }
        let mut would_block = false;
        match socket.read() {
            Ok(Message::Text(text)) => {
                let command = serde_json::from_str::<AgentRealtimeCommand>(text.as_str())
                    .map_err(|_| "control plane sent an invalid command".to_owned())?;
                match command {
                    AgentRealtimeCommand::Start {
                        interval_millis,
                        diagnostics,
                        user_traffic,
                    } if accepted_interval(interval_millis)
                        && valid_user_traffic_selector(&user_traffic) =>
                    {
                        let interval = Duration::from_millis(u64::from(interval_millis));
                        if active
                            .as_ref()
                            .is_none_or(|(current, _, _)| *current != interval)
                        {
                            sampler.reset();
                            user_traffic_sampler.reset();
                            next_sample = Instant::now();
                        }
                        // The baseline contains all observed counters, not just selected users.
                        // A watcher change or authorization renewal only changes which deltas we
                        // emit. Preserve the baseline; new/reset counters get their own gap, and
                        // advance_counters rejects a baseline that has become too old.
                        if diagnostics
                            && active
                                .as_ref()
                                .is_none_or(|(_, current_diagnostics, _)| !current_diagnostics)
                        {
                            next_diagnostics = Instant::now();
                            mux_baseline = None;
                        } else if !diagnostics {
                            mux_baseline = None;
                        }
                        subscription.set_enabled(diagnostics);
                        active = Some((interval, diagnostics, user_traffic));
                    }
                    AgentRealtimeCommand::Start { .. } => {
                        return Err(
                            "control plane sent an invalid realtime sampling command".to_owned()
                        );
                    }
                    AgentRealtimeCommand::Stop => {
                        subscription.set_enabled(false);
                        active = None;
                        mux_baseline = None;
                        sampler.reset();
                        user_traffic_sampler.reset();
                    }
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
            Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                would_block = true;
            }
            Err(error) => return Err(error.to_string()),
        }

        if let Some((interval, diagnostics, ref user_traffic_selector)) = active {
            let now = Instant::now();
            if now >= next_sample {
                if write_pending {
                    // Do not build an in-memory sample queue when the control plane is not
                    // reading. The already queued sample is retained by tungstenite; the next
                    // delivered point starts a new segment rather than drawing across the loss.
                    sampler.reset();
                    user_traffic_sampler.reset();
                } else {
                    match read_nic(now) {
                        Ok(reading) => {
                            let user_traffic = user_traffic_selector.as_ref().map(|selector| {
                                user_traffic_sampler.advance(state_dir, selector, now)
                            });
                            if let Some(mut sample) = sampler.advance(reading, interval) {
                                sample.user_traffic = user_traffic;
                                if populate_diagnostics(
                                    &mut sample,
                                    diagnostics,
                                    now >= next_diagnostics,
                                    &diagnostics_worker.health,
                                    &diagnostics_worker.mux,
                                    vpngate,
                                    &mut mux_baseline,
                                ) {
                                    next_diagnostics = now + DIAGNOSTICS_REFRESH;
                                }
                                let text = encode_sample(&mut sample)?;
                                write_pending = send_sample(&mut socket, text)?;
                            }
                        }
                        Err(_) => sampler.reset(),
                    }
                }
                next_sample += interval;
                if next_sample <= now {
                    next_sample = now + interval;
                }
            }
        }
        if would_block {
            // Drain all tungstenite/rustls buffered frames before waiting on the kernel socket.
            // Flush also covers automatic Pong/Close replies; POLLOUT wakes a blocked writer.
            write_pending = flush_pending(&mut socket)?;
            let timeout = active
                .as_ref()
                .map(|_| next_sample.saturating_duration_since(Instant::now()));
            socket
                .get_ref()
                .wait_for_io(write_pending, timeout)
                .map_err(|error| error.to_string())?;
        }
    }
}

/// Bound the combined optional reports, including a machine with many active users.
fn encode_sample(sample: &mut AgentRealtimeSample) -> Result<String, String> {
    let mut text = serde_json::to_string(sample).map_err(|error| error.to_string())?;
    while text.len() > MAX_FRAME_BYTES {
        let Some(report) = sample
            .user_traffic
            .as_mut()
            .filter(|report| !report.users.is_empty())
        else {
            return Err("realtime sample exceeds the frame bound".to_owned());
        };
        report.users.truncate(report.users.len() / 2);
        report.truncated = true;
        text = serde_json::to_string(sample).map_err(|error| error.to_string())?;
    }
    Ok(text)
}

/// Tungstenite queues the frame before returning WouldBlock. Retrying send would duplicate it;
/// remember only that flushing is pending. Other errors end the socket and trigger backoff.
fn send_sample(socket: &mut WebSocket<Stream>, text: String) -> Result<bool, String> {
    match socket.send(Message::Text(text.into())) {
        Ok(()) => Ok(false),
        Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(true)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn flush_pending(socket: &mut WebSocket<Stream>) -> Result<bool, String> {
    match socket.flush() {
        Ok(()) => Ok(false),
        Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(true)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn read_nic(instant: Instant) -> Result<NicReading, String> {
    let interface = crate::load::main_interface().ok_or("default-route interface not found")?;
    // `main_interface` comes from the kernel and Linux interface names cannot contain a slash,
    // but reject one anyway so this path can never become an arbitrary file read.
    if interface.contains('/') || interface.is_empty() {
        return Err("default-route interface name is invalid".to_owned());
    }
    let base = format!("/sys/class/net/{interface}/statistics");
    let read = |name: &str| -> Result<u64, String> {
        fs::read_to_string(format!("{base}/{name}"))
            .map_err(|error| error.to_string())?
            .trim()
            .parse::<u64>()
            .map_err(|error| error.to_string())
    };
    Ok(NicReading {
        interface,
        rx_bytes: read("rx_bytes")?,
        tx_bytes: read("tx_bytes")?,
        sampled_at_unix_millis: unix_millis(),
        instant,
    })
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ApplyMode;

    #[test]
    fn user_rates_use_non_resetting_deltas_and_exact_label_selection() {
        let start = Instant::now();
        let label = brocade_core::model::grant_label("alice", "platform", "entry");
        let other = brocade_core::model::grant_label("bob", "platform", "entry");
        let selector = RealtimeUserTrafficSelector::Labels {
            forwarding: Vec::new(),
            labels: vec![label.clone()],
        };
        let mut sampler = UserTrafficSampler::default();
        let first = BTreeMap::from([(label.clone(), (100, 200)), (other.clone(), (50, 50))]);
        assert_eq!(
            sampler.advance_counters(first, &selector, start).state,
            RealtimeUserTrafficState::Warming
        );
        let next = BTreeMap::from([(label.clone(), (2100, 6200)), (other, (5050, 5050))]);
        let report =
            sampler.advance_counters(next.clone(), &selector, start + Duration::from_secs(2));
        assert_eq!(report.users.len(), 1);
        assert_eq!(report.users[0].user_id, "alice");
        assert_eq!(report.users[0].uplink_bytes_per_sec, 1000);
        assert_eq!(report.users[0].downlink_bytes_per_sec, 3000);
        assert!(!report.all_users);
        assert!(!report.has_gap);
        let idle = sampler.advance_counters(next, &selector, start + Duration::from_secs(3));
        assert_eq!(idle.users[0].uplink_bytes_per_sec, 0);
        assert_eq!(idle.users[0].downlink_bytes_per_sec, 0);
        let reset = sampler.advance_counters(
            BTreeMap::from([(label, (1, 1))]),
            &selector,
            start + Duration::from_secs(4),
        );
        assert!(reset.has_gap);
        assert!(reset.users.is_empty());
    }

    #[test]
    fn user_rate_baselines_survive_selection_changes_and_isolate_new_counter_gaps() {
        let start = Instant::now();
        let alice = brocade_core::model::grant_label("alice", "platform", "entry");
        let bob = brocade_core::model::grant_label("bob", "platform", "entry");
        let carol = brocade_core::model::grant_label("carol", "platform", "entry");
        let selected = |labels| RealtimeUserTrafficSelector::Labels {
            labels,
            forwarding: Vec::new(),
        };
        let mut sampler = UserTrafficSampler::default();
        sampler.advance_counters(
            BTreeMap::from([(alice.clone(), (100, 200)), (bob.clone(), (50, 50))]),
            &selected(vec![alice.clone()]),
            start,
        );
        // A second watcher may select counters that were already observed but not emitted.
        let report = sampler.advance_counters(
            BTreeMap::from([(alice.clone(), (110, 220)), (bob.clone(), (80, 90))]),
            &selected(vec![alice.clone(), bob.clone()]),
            start + Duration::from_secs(1),
        );
        assert_eq!(report.state, RealtimeUserTrafficState::Ready);
        assert!(!report.has_gap);
        assert_eq!(report.users.len(), 2);
        assert_eq!(report.users[0].user_id, "bob");
        assert_eq!(report.users[0].downlink_bytes_per_sec, 40);

        // Removing a watcher narrows the output immediately without discarding other baselines.
        let report = sampler.advance_counters(
            BTreeMap::from([(alice.clone(), (120, 240)), (bob.clone(), (90, 100))]),
            &selected(vec![alice.clone()]),
            start + Duration::from_secs(2),
        );
        assert_eq!(report.users.len(), 1);
        assert_eq!(report.users[0].user_id, "alice");
        assert_eq!(report.users[0].downlink_bytes_per_sec, 20);

        // A genuinely new label only makes that label unknown; existing rates remain valid.
        let counters = BTreeMap::from([
            (alice.clone(), (130, 260)),
            (bob.clone(), (100, 110)),
            (carol, (500, 500)),
        ]);
        let report = sampler.advance_counters(
            counters.clone(),
            &RealtimeUserTrafficSelector::All {
                forwarding: Vec::new(),
            },
            start + Duration::from_secs(3),
        );
        assert_eq!(report.state, RealtimeUserTrafficState::Ready);
        assert!(report.has_gap);
        assert_eq!(report.users.len(), 2);

        // A long pause still requires a new baseline, regardless of the requested selector.
        let report = sampler.advance_counters(
            counters,
            &selected(vec![alice]),
            start + Duration::from_secs(65),
        );
        assert_eq!(report.state, RealtimeUserTrafficState::Warming);
        assert!(report.users.is_empty());
    }

    #[test]
    fn all_user_rates_omit_idle_rows_and_reject_invalid_selectors() {
        let label = brocade_core::model::grant_label("alice", "platform", "entry");
        let counters = BTreeMap::from([(label.clone(), (100, 200))]);
        let mut sampler = UserTrafficSampler::default();
        let start = Instant::now();
        sampler.advance_counters(
            counters.clone(),
            &RealtimeUserTrafficSelector::All {
                forwarding: Vec::new(),
            },
            start,
        );
        let idle = sampler.advance_counters(
            counters,
            &RealtimeUserTrafficSelector::All {
                forwarding: Vec::new(),
            },
            start + Duration::from_secs(1),
        );
        assert!(idle.users.is_empty());
        assert!(idle.all_users);
        assert!(!valid_user_traffic_selector(&Some(
            RealtimeUserTrafficSelector::Labels {
                forwarding: Vec::new(),
                labels: vec![label.clone(), label]
            }
        )));
        assert!(!valid_user_traffic_selector(&Some(
            RealtimeUserTrafficSelector::Labels {
                forwarding: Vec::new(),
                labels: vec!["raw-credential".to_owned()]
            }
        )));
    }

    #[test]
    fn forwarding_rates_use_rule_attribution_without_rebuilding_listeners() {
        use brocade_deployment::protocol::RealtimeForwardingTarget;
        let target = RealtimeForwardingTarget {
            rule_id: 7,
            tenant_id: "platform".to_owned(),
            user_id: "alice".to_owned(),
        };
        let selector = RealtimeUserTrafficSelector::Labels {
            labels: vec![],
            forwarding: vec![target.clone()],
        };
        assert!(valid_user_traffic_selector(&Some(selector.clone())));
        let start = Instant::now();
        let mut sampler = UserTrafficSampler::default();
        sampler.advance_counters(
            BTreeMap::from([
                ("fwd:7".to_owned(), (100, 200)),
                ("fwd:8".to_owned(), (10, 20)),
            ]),
            &selector,
            start,
        );
        let report = sampler.advance_counters(
            BTreeMap::from([
                ("fwd:7".to_owned(), (2100, 6200)),
                ("fwd:8".to_owned(), (5010, 5020)),
            ]),
            &selector,
            start + Duration::from_secs(2),
        );
        assert!(report.forwarding_supported && report.forwarding_expected);
        assert_eq!(report.users.len(), 1);
        assert_eq!(report.users[0].ingress_id, None);
        assert_eq!(report.users[0].forwarding_rule_id, Some(7));
        assert_eq!(report.users[0].user_id, "alice");
        assert_eq!(report.users[0].uplink_bytes_per_sec, 1000);
        assert_eq!(report.users[0].downlink_bytes_per_sec, 3000);
        let duplicate = RealtimeUserTrafficSelector::Labels {
            labels: vec![],
            forwarding: vec![target.clone(), target],
        };
        assert!(!valid_user_traffic_selector(&Some(duplicate)));
    }

    #[test]
    fn large_user_reports_fit_the_wire_bound_and_mark_truncation() {
        let mut sampler = RateSampler::default();
        let start = Instant::now();
        sampler.advance(reading("eth0", 1, 1, start), Duration::from_secs(1));
        let mut sample = sampler
            .advance(
                reading("eth0", 2, 2, start + Duration::from_secs(1)),
                Duration::from_secs(1),
            )
            .unwrap();
        sample.user_traffic = Some(RealtimeUserTrafficReport {
            state: RealtimeUserTrafficState::Ready,
            all_users: true,
            elapsed_millis: Some(1000),
            truncated: false,
            has_gap: false,
            forwarding_supported: true,
            forwarding_expected: false,
            users: (0..MAX_REALTIME_USER_TRAFFIC_ROWS)
                .map(|index| RealtimeUserTrafficRate {
                    tenant_id: "t".repeat(128),
                    user_id: format!("{index:064}"),
                    ingress_id: Some("i".repeat(64)),
                    forwarding_rule_id: None,
                    uplink_bytes_per_sec: u64::MAX,
                    downlink_bytes_per_sec: u64::MAX,
                })
                .collect(),
        });
        assert!(encode_sample(&mut sample).unwrap().len() <= MAX_FRAME_BYTES);
        assert!(sample.user_traffic.unwrap().truncated);
    }

    fn reading(interface: &str, rx: u64, tx: u64, instant: Instant) -> NicReading {
        NicReading {
            interface: interface.to_owned(),
            rx_bytes: rx,
            tx_bytes: tx,
            sampled_at_unix_millis: unix_millis(),
            instant,
        }
    }

    #[test]
    fn urls_preserve_the_control_plane_path_prefix() {
        assert_eq!(
            websocket_url("https://console.example/base/").unwrap(),
            "wss://console.example/base/agent/v1/realtime"
        );
        assert_eq!(
            websocket_url("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/agent/v1/realtime"
        );
        assert!(websocket_url("console.example").is_err());
    }

    #[test]
    fn rates_use_actual_elapsed_time_and_resets_create_a_gap() {
        let start = Instant::now();
        let mut sampler = RateSampler::default();
        assert!(sampler
            .advance(reading("eth0", 100, 200, start), Duration::from_secs(1))
            .is_none());
        let sample = sampler
            .advance(
                reading("eth0", 2100, 1200, start + Duration::from_secs(2)),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(sample.rx_bytes_per_sec, 1000);
        assert_eq!(sample.tx_bytes_per_sec, 500);
        assert!(sample.has_gap);

        assert!(sampler
            .advance(
                reading("ens3", 3000, 2000, start + Duration::from_secs(3)),
                Duration::from_secs(1),
            )
            .is_none());
        assert!(
            sampler
                .advance(
                    reading("ens3", 4000, 3000, start + Duration::from_secs(4)),
                    Duration::from_secs(1),
                )
                .unwrap()
                .has_gap
        );
    }

    #[test]
    fn a_counter_regression_is_never_turned_into_a_huge_rate() {
        let start = Instant::now();
        let mut sampler = RateSampler::default();
        sampler.advance(reading("eth0", 1000, 1000, start), Duration::from_secs(1));
        assert!(sampler
            .advance(
                reading("eth0", 10, 20, start + Duration::from_secs(1)),
                Duration::from_secs(1),
            )
            .is_none());
    }

    #[test]
    fn nic_only_demand_never_serializes_cached_worker_diagnostics() {
        let health: HealthCache = Default::default();
        let mux: MuxCache = std::sync::Arc::new(std::sync::Mutex::new(Some(
            brocade_deployment::protocol::MuxReport {
                boot_id: "boot".to_owned(),
                sequence: 1,
                sampled_at_unix_ms: unix_millis(),
                pools: Vec::new(),
                workers: Vec::new(),
                events: Vec::new(),
            },
        )));
        let vpngate: VpngateCache = Default::default();
        let mut sample = AgentRealtimeSample {
            diagnostics_unchanged: false,
            mux_unchanged: false,
            reverse_health: None,
            mux: None,
            vpngate: None,
            user_traffic: None,
            sequence: 1,
            sampled_at_unix_millis: unix_millis(),
            elapsed_millis: 1000,
            interface: "eth0".to_owned(),
            rx_bytes_per_sec: 1,
            tx_bytes_per_sec: 1,
            has_gap: false,
        };
        let mut mux_baseline = None;

        assert!(!populate_diagnostics(
            &mut sample,
            false,
            true,
            &health,
            &mux,
            &vpngate,
            &mut mux_baseline,
        ));
        assert!(sample.diagnostics_unchanged);
        assert!(sample.mux.is_none());

        sample.diagnostics_unchanged = false;
        assert!(populate_diagnostics(
            &mut sample,
            true,
            true,
            &health,
            &mux,
            &vpngate,
            &mut mux_baseline,
        ));
        assert!(sample.mux.is_some());

        let mut idle_delta = sample.clone();
        idle_delta.mux = None;
        idle_delta.mux_unchanged = false;
        mux.lock().unwrap().as_mut().unwrap().sampled_at_unix_ms += 5_000;
        assert!(populate_diagnostics(
            &mut idle_delta,
            true,
            true,
            &health,
            &mux,
            &vpngate,
            &mut mux_baseline,
        ));
        assert!(idle_delta.mux_unchanged);
        assert!(idle_delta.mux.is_none());

        let mut state_delta = idle_delta;
        state_delta.mux_unchanged = false;
        mux.lock().unwrap().as_mut().unwrap().sequence += 1;
        assert!(populate_diagnostics(
            &mut state_delta,
            true,
            true,
            &health,
            &mux,
            &vpngate,
            &mut mux_baseline,
        ));
        assert!(!state_delta.mux_unchanged);
        assert!(state_delta.mux.is_some());
    }

    #[test]
    #[allow(clippy::result_large_err)] // tungstenite's required handshake callback signature
    fn the_custom_agent_transport_completes_a_real_websocket_handshake() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept_hdr(
                stream,
                |request: &tungstenite::handshake::server::Request,
                 response: tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/base/agent/v1/realtime");
                    assert_eq!(request.headers()["authorization"], "Bearer node-secret");
                    Ok(response)
                },
            )
            .unwrap();
            socket
                .send(Message::Text(
                    serde_json::to_string(&AgentRealtimeCommand::Stop)
                        .unwrap()
                        .into(),
                ))
                .unwrap();
            socket.close(None).unwrap();
        });
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}/base"),
            token: "node-secret".to_owned(),
            state_dir: std::env::temp_dir(),
            apply_mode: ApplyMode::StateDir,
            vpngate_stats_window: std::time::Duration::from_secs(900),
        };
        let socket = connect(&options).unwrap();
        serve(
            socket,
            &options.state_dir,
            &diagnostics::Worker::start(options.state_dir.clone()).unwrap(),
            &Default::default(),
        )
        .unwrap();
        server.join().unwrap();
    }
    #[test]
    fn buffered_commands_and_ping_are_drained_before_readiness_wait() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for command in [
                AgentRealtimeCommand::Start {
                    interval_millis: 1_000,
                    diagnostics: false,
                    user_traffic: None,
                },
                AgentRealtimeCommand::Stop,
                AgentRealtimeCommand::Start {
                    interval_millis: 1_000,
                    diagnostics: false,
                    user_traffic: None,
                },
                AgentRealtimeCommand::Stop,
            ] {
                socket
                    .write(Message::Text(
                        serde_json::to_string(&command).unwrap().into(),
                    ))
                    .unwrap();
            }
            socket.write(Message::Ping(vec![7, 8, 9].into())).unwrap();
            socket.flush().unwrap();
            assert_eq!(socket.read().unwrap(), Message::Pong(vec![7, 8, 9].into()));
            socket.close(None).unwrap();
        });
        let options = Options {
            command: "run".into(),
            server: format!("http://{address}"),
            token: "test-node".into(),
            state_dir: std::env::temp_dir(),
            apply_mode: ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let worker = diagnostics::Worker::start(options.state_dir.clone()).unwrap();
        serve(
            connect(&options).unwrap(),
            &options.state_dir,
            &worker,
            &Default::default(),
        )
        .unwrap();
        server.join().unwrap();
        assert!(worker.health.lock().unwrap().is_none());
    }
}
