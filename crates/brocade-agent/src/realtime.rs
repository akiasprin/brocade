//! On-demand, non-durable NIC rates.
//!
//! The connection is always initiated by the Agent, so neither the browser nor the control plane
//! needs inbound access to a machine. A connected socket is cheap and idle; samples begin only on
//! `Start`, are never spooled, and disappear if either side restarts.

use std::{
    fs, thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use brocade_deployment::protocol::{
    AgentRealtimeCommand, AgentRealtimeSample, AGENT_PROTOCOL_VERSION,
};
use tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Error as WebSocketError, Message,
    WebSocket,
};

use crate::{
    http::{HttpClient, Stream},
    options::Options,
};

const IO_POLL: Duration = Duration::from_millis(20);
const IDLE_POLL: Duration = Duration::from_millis(250);
const INITIAL_RECONNECT: Duration = Duration::from_secs(1);
const MAX_RECONNECT: Duration = Duration::from_secs(300);
const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_WRITE_BUFFER_BYTES: usize = 64 * 1024;
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

pub(crate) fn run(options: &Options) {
    let mut backoff = INITIAL_RECONNECT;
    loop {
        match connect(options) {
            Ok(socket) => {
                backoff = INITIAL_RECONNECT;
                if let Err(error) = serve(socket) {
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
    let url = websocket_url(&options.server)?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| format!("invalid control-plane URL: {error}"))?;
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
    // Reuse the Agent's existing transport, root store and TLS configuration. Giving tungstenite
    // its own TLS feature would compile a second root database into every static Agent binary.
    let client = HttpClient::new(&options.server)?;
    let tcp = client.connect()?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let stream = client.wrap_tls(tcp)?;
    let config = WebSocketConfig::default()
        // Commands and samples are tiny JSON values. Bounding both directions prevents a broken
        // or hostile peer from turning this optional channel into unbounded Agent memory.
        .read_buffer_size(4 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_WRITE_BUFFER_BYTES)
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (socket, _) = tungstenite::client::client_with_config(request, stream, Some(config))
        .map_err(|error| format!("WebSocket handshake failed: {error}"))?;
    // Tungstenite documents every I/O error except WouldBlock as fatal. A read timeout therefore
    // cannot be used as a periodic wake-up: treating TimedOut as recoverable corrupts its state,
    // while treating it as fatal reconnects every idle interval. Nonblocking I/O plus a short
    // sleep gives sampling a precise clock and leaves the only recoverable result as WouldBlock.
    socket
        .get_ref()
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
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

fn serve(mut socket: WebSocket<Stream>) -> Result<(), String> {
    let mut active_interval = None;
    let mut next_sample = Instant::now();
    let mut sampler = RateSampler::default();
    let mut write_pending = false;
    sampler.reset();

    loop {
        if write_pending {
            write_pending = flush_pending(&mut socket)?;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let command = serde_json::from_str::<AgentRealtimeCommand>(text.as_str())
                    .map_err(|_| "control plane sent an invalid command".to_owned())?;
                match command {
                    AgentRealtimeCommand::Start { interval_millis }
                        if accepted_interval(interval_millis) =>
                    {
                        let interval = Duration::from_millis(u64::from(interval_millis));
                        if active_interval != Some(interval) {
                            sampler.reset();
                            next_sample = Instant::now();
                        }
                        active_interval = Some(interval);
                    }
                    AgentRealtimeCommand::Start { .. } => {
                        return Err("control plane sent an unsupported sample interval".to_owned());
                    }
                    AgentRealtimeCommand::Stop => {
                        active_interval = None;
                        sampler.reset();
                    }
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {}
            Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.to_string()),
        }

        if let Some(interval) = active_interval {
            let now = Instant::now();
            if now >= next_sample {
                if write_pending {
                    // Do not build an in-memory sample queue when the control plane is not
                    // reading. The already queued sample is retained by tungstenite; the next
                    // delivered point starts a new segment rather than drawing across the loss.
                    sampler.reset();
                } else {
                    match read_nic(now) {
                        Ok(reading) => {
                            if let Some(sample) = sampler.advance(reading, interval) {
                                let text = serde_json::to_string(&sample)
                                    .map_err(|error| error.to_string())?;
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
        let sleep = match active_interval {
            Some(_) => next_sample
                .saturating_duration_since(Instant::now())
                .min(IO_POLL),
            None => IDLE_POLL,
        };
        if !sleep.is_zero() {
            thread::sleep(sleep);
        }
    }
}

/// Tungstenite guarantees that a frame has been queued when `send` reaches `WouldBlock`.
/// Retrying the same message would therefore duplicate it; remember only that the queue still
/// needs flushing. Every other write error ends this socket and lets the outer backoff reconnect.
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
        };
        let socket = connect(&options).unwrap();
        serve(socket).unwrap();
        server.join().unwrap();
    }
}
