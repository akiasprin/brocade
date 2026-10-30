//! On-demand TCP ping. No payload, no Xray access, no automatic/periodic target probing.
use crate::{http::HttpClient, options::Options};
use brocade_core::forwarding::target_ip_is_blocked;
use brocade_deployment::forwarding_probe::{
    ForwardingProbeReport, ForwardingProbeTarget, TcpPingCode, TcpPingResult, TcpPingStatus,
};
use std::{
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(3);
static DNS_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct PendingReport {
    created: Instant,
    report: ForwardingProbeReport,
}

pub(crate) fn run(options: Options) {
    let mut pending = None;
    let mut failed = false;
    let mut wake =
        crate::wake::Cursor::new(brocade_deployment::agent_channel::WakeDomain::ForwardingProbe);
    loop {
        let pause = match round(&options, &mut pending) {
            Ok(true) => {
                failed = false;
                3
            }
            Ok(false) => {
                failed = false;
                300
            }
            Err(error) => {
                if !failed {
                    eprintln!("forwarding TCP ping: {error}");
                }
                failed = true;
                15
            }
        };
        wake.wait_connected(
            Duration::from_secs(pause),
            Duration::from_secs(if pause == 3 { 60 } else { pause }),
        );
    }
}

fn round(options: &Options, pending: &mut Option<PendingReport>) -> Result<bool, String> {
    let client = HttpClient::new(&options.server)?;
    if pending
        .as_ref()
        .is_some_and(|report| report.created.elapsed() >= Duration::from_secs(60))
    {
        *pending = None;
    }
    if pending.is_none() {
        let response = client.request(
            "GET",
            "/agent/v1/forwarding/tcp-ping-target",
            &options.token,
            None,
        )?;
        if matches!(response.status, 404 | 405) {
            return Ok(false);
        }
        if response.status != 200 {
            return Err(format!("target HTTP {}", response.status));
        }
        let target: Option<ForwardingProbeTarget> =
            serde_json::from_str(&response.body).map_err(|_| "invalid TCP ping target")?;
        let Some(target) = target else {
            return Ok(true);
        };
        let result = tcp_ping(
            &target.target_host,
            target.target_port,
            target.allow_private_targets,
            TIMEOUT,
        );
        *pending = Some(PendingReport {
            created: Instant::now(),
            report: ForwardingProbeReport {
                probe_id: target.probe_id,
                result,
            },
        });
    }
    let body = serde_json::to_string(&pending.as_ref().expect("pending was filled above").report)
        .map_err(|error| error.to_string())?;
    let response = client.request(
        "POST",
        "/agent/v1/forwarding/tcp-ping-report",
        &options.token,
        Some(&body),
    )?;
    if (200..300).contains(&response.status) || matches!(response.status, 400 | 403 | 404 | 409) {
        *pending = None;
        return Ok(true);
    }
    Err(format!("report HTTP {}", response.status))
}

fn millis(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

struct DnsPermit;
impl Drop for DnsPermit {
    fn drop(&mut self) {
        DNS_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn resolve(host: &str, port: u16, timeout: Duration) -> Result<Vec<SocketAddr>, TcpPingCode> {
    // Hold the permit inside getaddrinfo even when the caller times out. At most two OS DNS
    // lookups can outlive a request; a slow resolver cannot accumulate unbounded threads.
    DNS_WORKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < 2).then_some(count + 1)
        })
        .map_err(|_| TcpPingCode::DnsBusy)?;
    let permit = DnsPermit;
    let host = host.to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("tcp-ping-dns".to_owned())
        .spawn(move || {
            let _permit = permit;
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.take(16).collect());
            let _ = sender.send(result);
        })
        .map_err(|_| TcpPingCode::DnsBusy)?;
    receiver
        .recv_timeout(timeout)
        .map_err(|_| TcpPingCode::Timeout)?
        .map_err(|_| TcpPingCode::DnsFailed)
}

fn tcp_ping(host: &str, port: u16, allow_private: bool, timeout: Duration) -> TcpPingResult {
    tcp_ping_with_connect(
        host,
        port,
        allow_private,
        timeout,
        TcpStream::connect_timeout,
    )
}

fn tcp_ping_with_connect(
    host: &str,
    port: u16,
    allow_private: bool,
    timeout: Duration,
    mut connect: impl FnMut(&SocketAddr, Duration) -> std::io::Result<TcpStream>,
) -> TcpPingResult {
    let started = Instant::now();
    let mut result = TcpPingResult::not_run(TcpPingCode::InvalidTarget);
    let host = host
        .strip_prefix('[')
        .and_then(|name| name.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty()
        || host.len() > 253
        || port == 0
        || host
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'\\')
    {
        return result;
    }
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        Ok(vec![SocketAddr::new(ip, port)])
    } else {
        let resolved = resolve(host, port, timeout);
        result.dns_ms = Some(millis(started));
        resolved
    };
    let addresses = match addresses {
        Ok(addresses) if addresses.is_empty() => {
            result.code = TcpPingCode::DnsEmpty;
            result.status = TcpPingStatus::Failed;
            return result;
        }
        Ok(addresses) => addresses,
        Err(code) => {
            result.code = code;
            result.status = TcpPingStatus::Failed;
            return result;
        }
    };
    if !allow_private
        && addresses
            .iter()
            .any(|address| target_ip_is_blocked(address.ip()))
    {
        result.code = TcpPingCode::AddressBlocked;
        return result;
    }
    result.status = TcpPingStatus::Failed;
    result.code = TcpPingCode::ConnectFailed;
    let address_count = addresses.len();
    for (index, address) in addresses.into_iter().enumerate() {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            result.code = TcpPingCode::Timeout;
            break;
        }
        let connected_at = Instant::now();
        result.resolved_ip = Some(address.ip().to_string());
        // DNS answers are failover candidates, not repeated samples. Stop at the first successful
        // handshake; do not write/read payload or retain a target connection.
        // Reserve time for fallback answers; a single/final address gets the full remaining budget.
        let attempt_timeout = if index + 1 < address_count {
            remaining.min(Duration::from_secs(1))
        } else {
            remaining
        };
        match connect(&address, attempt_timeout) {
            Ok(stream) => {
                result.status = TcpPingStatus::Reachable;
                result.code = TcpPingCode::Connected;
                result.connect_ms = Some(millis(connected_at));
                drop(stream);
                break;
            }
            Err(error) => {
                result.code = if error.kind() == std::io::ErrorKind::TimedOut {
                    TcpPingCode::Timeout
                } else {
                    TcpPingCode::ConnectFailed
                };
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn report_retry_reuses_the_same_single_ping_without_xray_or_config_files() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_port = target.local_addr().unwrap().port();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let control = thread::spawn(move || {
            let mut reports = Vec::new();
            for step in 0..3 {
                let (mut socket, _) = server.accept().unwrap();
                socket
                    // The workspace suite runs hundreds of tests concurrently. Give the local
                    // control stub the same scheduling headroom as the real client's I/O budget
                    // so CPU contention cannot masquerade as a retry failure.
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let headers = String::from_utf8(request).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|length| length.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).unwrap();
                let (status, response) = if step == 0 {
                    assert!(headers.starts_with("GET /agent/v1/forwarding/tcp-ping-target "));
                    (
                        200,
                        serde_json::to_string(&Some(ForwardingProbeTarget {
                            probe_id: "single-request".to_owned(),
                            rule_id: 42,
                            target_host: "127.0.0.1".to_owned(),
                            target_port,
                            allow_private_targets: true,
                        }))
                        .unwrap(),
                    )
                } else {
                    assert!(headers.starts_with("POST /agent/v1/forwarding/tcp-ping-report "));
                    reports.push(serde_json::from_slice::<ForwardingProbeReport>(&body).unwrap());
                    (if step == 1 { 503 } else { 204 }, String::new())
                };
                write!(socket,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
            }
            assert_eq!(reports[0], reports[1]);
            assert_eq!(reports[0].result.status, TcpPingStatus::Reachable);
        });
        let options = Options {
            command: "run".to_owned(),
            server: format!("http://{address}"),
            token: "test-token".to_owned(),
            state_dir: std::path::PathBuf::from("/nonexistent-tcp-ping-state"),
            apply_mode: crate::options::ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        };
        let mut pending = None;
        assert!(round(&options, &mut pending).is_err());
        assert!(pending.is_some());
        assert!(round(&options, &mut pending).unwrap());
        assert!(pending.is_none());
        control.join().unwrap();
        let (_connected, _) = target.accept().unwrap();
        target.set_nonblocking(true).unwrap();
        assert!(
            target.accept().is_err(),
            "report retry must not repeat the target connection"
        );
    }

    #[test]
    fn one_tcp_handshake_sends_no_application_payload_and_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut buffer = [0; 1];
            assert_eq!(stream.read(&mut buffer).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let result = tcp_ping("127.0.0.1", port, true, TIMEOUT);
        assert_eq!(result.status, TcpPingStatus::Reachable);
        assert_eq!(result.code, TcpPingCode::Connected);
        assert!(result.connect_ms.is_some());
        assert_eq!(result.dns_ms, None);
        assert_eq!(result.resolved_ip.as_deref(), Some("127.0.0.1"));
        server.join().unwrap();
    }

    #[test]
    fn refused_and_private_and_invalid_targets_are_not_success() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let refused = tcp_ping("127.0.0.1", port, true, TIMEOUT);
        assert_eq!(refused.status, TcpPingStatus::Failed);
        assert_eq!(refused.code, TcpPingCode::ConnectFailed);
        assert_eq!(refused.connect_ms, None);
        assert_eq!(
            tcp_ping("127.0.0.1", port, false, TIMEOUT).code,
            TcpPingCode::AddressBlocked
        );
        assert_eq!(
            tcp_ping("http://example.com", port, true, TIMEOUT).code,
            TcpPingCode::InvalidTarget
        );
    }

    #[test]
    fn expired_budget_does_not_connect_or_invent_a_latency() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let result = tcp_ping(
            "127.0.0.1",
            listener.local_addr().unwrap().port(),
            true,
            Duration::ZERO,
        );
        assert_eq!(result.status, TcpPingStatus::Failed);
        assert_eq!(result.code, TcpPingCode::Timeout);
        assert_eq!(result.connect_ms, None);
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }

    #[test]
    fn stalled_connection_gets_three_seconds_and_reports_timeout_without_latency() {
        let mut attempts = 0;
        let result = tcp_ping_with_connect("127.0.0.1", 443, true, TIMEOUT, |address, budget| {
            attempts += 1;
            assert_eq!(*address, "127.0.0.1:443".parse::<SocketAddr>().unwrap());
            assert!(budget <= Duration::from_secs(3));
            assert!(budget > Duration::from_secs(2), "budget was {budget:?}");
            Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
        });
        assert_eq!(attempts, 1);
        assert_eq!(result.status, TcpPingStatus::Failed);
        assert_eq!(result.code, TcpPingCode::Timeout);
        assert_eq!(result.connect_ms, None);
        assert_eq!(result.resolved_ip.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn dns_time_is_separate_and_ipv6_literals_are_supported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let result = tcp_ping(
            "localhost",
            listener.local_addr().unwrap().port(),
            true,
            TIMEOUT,
        );
        assert_eq!(result.status, TcpPingStatus::Reachable);
        assert!(result.dns_ms.is_some());
        let listener = TcpListener::bind("[::1]:0").unwrap();
        let result = tcp_ping(
            "[::1]",
            listener.local_addr().unwrap().port(),
            true,
            TIMEOUT,
        );
        assert_eq!(result.status, TcpPingStatus::Reachable);
        assert_eq!(result.dns_ms, None);
    }
}
