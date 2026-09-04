//! On-disk retry spools, and the runtime observation reporting that hangs off
//! them.
//!
//! Both spools (usage, convergence results) share one mechanism: write to disk,
//! then send. Whatever cannot reach the control plane must not be lost —
//! convergence already happened on this machine, and that result is its only
//! record.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use brocade_deployment::protocol::{
    LocalReconcileReport, NodeRuntimeReport, NodeVersions, SpoolBacklog,
};

use crate::{
    current_unix_secs, fsutil::atomic_write_private, http::HttpClient, observe_geodata,
    options::Options, run_command, wg::wireguard_health_snapshot,
};

/// One on-disk retry spool.
///
/// Two users, sharing the property that a locally established fact must survive a
/// network outage: a lost usage reading is traffic given away, a lost convergence
/// result is a distribution lease waited out for nothing. They differ only in
/// filename, endpoint, and depth, hence one descriptor rather than two copies —
/// two copies eventually get fixed on one side only.
#[derive(Clone, Copy)]
pub(crate) struct Spool {
    pub(crate) file: &'static str,
    pub(crate) endpoint: &'static str,
    pub(crate) max: usize,
    /// Log prefix, and the unit noun in the over-limit message
    pub(crate) what: &'static str,
    pub(crate) unit: &'static str,
    /// Statuses which prove this exact body can never become valid. Authentication failures,
    /// throttling, timeouts and conflicts are deliberately absent: all can recover unchanged.
    pub(crate) terminal_statuses: &'static [u16],
}

/// Spool depth. One long outage must not fill the disk, and an old reading loses
/// value quickly: the counters are cumulative, so as long as any newer reading
/// gets through, the dropped windows merge into one long window with every byte
/// still accounted for — only the time resolution coarsens.
pub(crate) const USAGE_SPOOL: Spool = Spool {
    file: "usage.spool.jsonl",
    endpoint: "/agent/v1/usage",
    max: 720,
    what: "usage",
    unit: "读数",
    terminal_statuses: &[400, 410, 422],
};

/// The observation spool is far shorter than the usage one because it cannot
/// naturally accumulate: an entry appears only after desired was fetched and one
/// convergence ran, and when the control plane is unreachable `desired_request`
/// fails first, so that point is never reached. Tens of entries can only mean the
/// control plane lost just its reporting endpoint, and old entries are worthless
/// then anyway.
pub(crate) const OBSERVATION_SPOOL: Spool = Spool {
    file: "observation.spool.jsonl",
    endpoint: "/agent/v1/observation",
    max: 64,
    what: "observation",
    unit: "条收敛结果",
    terminal_statuses: &[400, 404, 409, 410, 422],
};

/// Cumulative count of dropped reports. Kept across restarts — it answers "has
/// this machine ever lost accounting", and a restart is exactly the moment most
/// likely to erase that fact.
const DROPPED_FILE: &str = "spool-dropped";
/// Last round's local reconcile result. In a file rather than memory for the same
/// reason: `reconcile_local` may have been run on its own by the `repair`
/// subcommand, which is a different process.
const LOCAL_RECONCILE_FILE: &str = "local-reconcile.json";

// Sampling may append while a reporter is waiting on the network. Keep file mutation and network
// serialization as two different locks: holding one lock across both would make the durable queue
// exist on paper while still letting a slow POST move the sampling clock.
static USAGE_FILE_LOCK: Mutex<()> = Mutex::new(());
static OBSERVATION_FILE_LOCK: Mutex<()> = Mutex::new(());
static OTHER_FILE_LOCK: Mutex<()> = Mutex::new(());
static USAGE_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static OBSERVATION_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static OTHER_DRAIN_LOCK: Mutex<()> = Mutex::new(());
static DROPPED_LOCK: Mutex<()> = Mutex::new(());

fn file_lock(spool: Spool) -> &'static Mutex<()> {
    match spool.file {
        file if file == USAGE_SPOOL.file => &USAGE_FILE_LOCK,
        file if file == OBSERVATION_SPOOL.file => &OBSERVATION_FILE_LOCK,
        _ => &OTHER_FILE_LOCK,
    }
}

fn drain_lock(spool: Spool) -> &'static Mutex<()> {
    match spool.file {
        file if file == USAGE_SPOOL.file => &USAGE_DRAIN_LOCK,
        file if file == OBSERVATION_SPOOL.file => &OBSERVATION_DRAIN_LOCK,
        _ => &OTHER_DRAIN_LOCK,
    }
}

fn bump_dropped(state_dir: &Path, by: u64) {
    let _guard = DROPPED_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = read_dropped_unlocked(state_dir).saturating_add(by);
    let _ = fs::write(state_dir.join(DROPPED_FILE), now.to_string());
}

fn read_dropped(state_dir: &Path) -> u64 {
    let _guard = DROPPED_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read_dropped_unlocked(state_dir)
}

fn read_dropped_unlocked(state_dir: &Path) -> u64 {
    fs::read_to_string(state_dir.join(DROPPED_FILE))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

pub(crate) fn record_local_reconcile(
    state_dir: &Path,
    actions: &[String],
    error: Option<&str>,
) -> Result<(), String> {
    let report = LocalReconcileReport {
        at: current_unix_secs().unwrap_or(0),
        actions: actions.to_vec(),
        error: error.map(str::to_owned),
    };
    let text = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    atomic_write_private(&state_dir.join(LOCAL_RECONCILE_FILE), text.as_bytes())
}

/// What is actually installed on this machine.
///
/// Every field may be `None` — unreadable does not mean absent, it may just not
/// be on PATH. Report `None` rather than guess: an invented version number is
/// worse than none.
fn observe_wg_backend(state_dir: &Path) -> Option<String> {
    // An absent config or the explicit disable marker means there is no WireGuard backend to
    // classify. Looking only at `/sys/module/wireguard` used to call every WG-off machine
    // "userspace", even though wg-quick had not selected or started any implementation.
    if !state_dir.join("wireguard.conf").exists() || state_dir.join("wireguard.disabled").exists() {
        return None;
    }

    Some(
        if Path::new("/sys/module/wireguard").exists() {
            "kernel"
        } else {
            "userspace"
        }
        .to_owned(),
    )
}

fn observe_versions(state_dir: &Path) -> NodeVersions {
    NodeVersions {
        // The sha256 of the running binary rather than `CARGO_PKG_VERSION`: nobody bumps a
        // workspace version on the way to a node, so that number claimed every build since it was
        // last touched was the same thing. See `identity.rs`.
        agent: crate::identity::self_identity().to_owned(),
        xray: first_line(run_command("xray", &["version"]).ok()),
        phantun: first_line(run_command("phantun-client", &["--version"]).ok()),
        wg_tools: first_line(run_command("wg", &["--version"]).ok()),
        // Whether the kernel module is present decides between the in-kernel
        // path and a fallback to wireguard-go / boringtun. Their `wg show`
        // output is identical and throughput differs by an order of magnitude —
        // without asking explicitly this is never discovered.
        wg_backend: observe_wg_backend(state_dir),
    }
}

fn first_line(output: Option<String>) -> Option<String> {
    let text = output?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

pub(crate) fn collect_runtime_report(state_dir: &Path) -> Result<NodeRuntimeReport, String> {
    let versions = observe_versions(state_dir);
    let certificate = crate::certfile::observe(state_dir);
    let geodata = observe_geodata();
    let local_reconcile = fs::read_to_string(state_dir.join(LOCAL_RECONCILE_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let spool = collect_spool_backlog(state_dir)?;
    Ok(NodeRuntimeReport {
        observed_at_unix_secs: Some(current_unix_secs()?),
        versions,
        certificate,
        geodata,
        local_reconcile,
        wireguard_health: wireguard_health_snapshot(state_dir),
        spool,
    })
}

pub(crate) fn collect_spool_backlog(state_dir: &Path) -> Result<SpoolBacklog, String> {
    Ok(SpoolBacklog {
        observation: spool_read(OBSERVATION_SPOOL, state_dir)?.len() as u32,
        usage: spool_read(USAGE_SPOOL, state_dir)?.len() as u32,
        dropped: read_dropped(state_dir),
    })
}

pub(crate) fn send_runtime_report(
    options: &Options,
    report: &NodeRuntimeReport,
) -> Result<(), String> {
    let client = HttpClient::new(&options.server)?;
    let body = serde_json::to_string(report).map_err(|error| error.to_string())?;
    let response = client.request("POST", "/agent/v1/runtime", &options.token, Some(&body))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "runtime report failed: HTTP {} {}",
            response.status, response.body
        ));
    }
    Ok(())
}

pub(crate) fn runtime_cycle(options: &Options) -> Result<(), String> {
    let report = collect_runtime_report(&options.state_dir)?;
    send_runtime_report(options, &report)
}

fn spool_path(spool: Spool, state_dir: &Path) -> PathBuf {
    state_dir.join(spool.file)
}

pub(crate) fn spool_push<T: serde::Serialize>(
    spool: Spool,
    state_dir: &Path,
    item: &T,
) -> Result<(), String> {
    let _guard = file_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut lines = spool_read_unlocked(spool, state_dir)?;
    lines.push(serde_json::to_string(item).map_err(|error| error.to_string())?);
    if lines.len() > spool.max {
        let dropped = lines.len() - spool.max;
        eprintln!(
            "{}: 队列超过 {} 条，丢掉最旧的 {dropped} {}",
            spool.what, spool.max, spool.unit
        );
        lines.drain(..dropped);
        // A drop must leave a trace. Logging alone confines it to that machine's
        // stderr, and what was dropped is accounting — the symptom is "this
        // machine had no traffic this month", indistinguishable in the UI from
        // genuinely having none. The counter is on disk and only grows;
        // non-zero means accounting was permanently lost.
        bump_dropped(state_dir, dropped as u64);
    }
    spool_write_unlocked(spool, state_dir, &lines)
}

pub(crate) fn spool_read(spool: Spool, state_dir: &Path) -> Result<Vec<String>, String> {
    let _guard = file_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    spool_read_unlocked(spool, state_dir)
}

fn spool_read_unlocked(spool: Spool, state_dir: &Path) -> Result<Vec<String>, String> {
    match fs::read_to_string(spool_path(spool, state_dir)) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

// Only after fsync and atomic rename does it count as on disk. The sample taken
// before restarting xray depends on this most: the very next step is the restart,
// the data inside xray is about to vanish, and a truncated spool is no copy at all.
fn spool_write_unlocked(spool: Spool, state_dir: &Path, lines: &[String]) -> Result<(), String> {
    let path = spool_path(spool, state_dir);
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    atomic_write_private(&path, body.as_bytes())
}

/// Send the spool's contents in order, removing each one that lands. Stop on
/// failure — later entries are newer, and sending them first makes the control
/// plane compute deltas in the wrong order. The same holds for the observation
/// spool: one machine's convergence results within a release are ordered, and
/// out-of-order delivery overwrites new with old.
pub(crate) fn spool_drain(spool: Spool, options: &Options) -> Result<bool, String> {
    // Only reporters serialize here. Producers use the separate file lock and can append while
    // any request below is blocked in DNS, connect, TLS or response IO.
    let _drain_guard = drain_lock(spool)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = spool_read(spool, &options.state_dir)?;
    if lines.is_empty() {
        return Ok(false);
    }
    let client = HttpClient::new(&options.server)?;
    let mut sent = 0;
    let mut failure = None;
    for line in &lines {
        match client.request("POST", spool.endpoint, &options.token, Some(line)) {
            Ok(response) if (200..300).contains(&response.status) => {
                sent += 1;
                println!("{}", response.body);
            }
            // The control plane says outright that this payload is wrong (4xx);
            // resending changes nothing, and discarding is what lets the queue
            // advance. The observation spool depends on this most: a finished or
            // cancelled deployment, or a vanished target row, all answer 4xx
            // (409/404), and that observation will never land — filing it as 5xx
            // and retrying blocks the head of the queue forever.
            Ok(response) if spool.terminal_statuses.contains(&response.status) => {
                sent += 1;
                bump_dropped(&options.state_dir, 1);
                eprintln!(
                    "{}: 控制面拒绝了一条，丢弃：HTTP {} {}",
                    spool.what, response.status, response.body
                );
            }
            Ok(response) => {
                failure = Some(format!(
                    "{} request failed: HTTP {}",
                    spool.what, response.status
                ));
                break;
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if sent > 0 {
        let _file_guard = file_lock(spool)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut current = spool_read_unlocked(spool, &options.state_dir)?;
        if current.len() < sent || current[..sent] != lines[..sent] {
            return Err(format!(
                "{} spool changed at its head while reporting",
                spool.what
            ));
        }
        current.drain(..sent);
        spool_write_unlocked(spool, &options.state_dir, &current)?;
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(sent > 0),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        io::{Read, Write},
        net::TcpListener,
        path::{Path, PathBuf},
        sync::mpsc,
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use crate::options::{ApplyMode, Options};

    use super::{
        first_line, observe_wg_backend, read_dropped, spool_drain, spool_push, spool_read, Spool,
        OBSERVATION_SPOOL,
    };

    const TEST_SPOOL: Spool = Spool {
        file: "test.spool.jsonl",
        endpoint: "/agent/v1/test",
        max: 16,
        what: "test",
        unit: "条",
        terminal_statuses: &[400, 404, 410, 422],
    };

    #[test]
    fn wg_backend_exists_only_while_wireguard_is_enabled() {
        let dir = state_dir("wg-backend-state");
        assert_eq!(observe_wg_backend(&dir), None);

        fs::write(dir.join("wireguard.conf"), "[Interface]\n").unwrap();
        assert!(matches!(
            observe_wg_backend(&dir).as_deref(),
            Some("kernel" | "userspace")
        ));

        fs::write(dir.join("wireguard.disabled"), "").unwrap();
        assert_eq!(observe_wg_backend(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }

    fn state_dir(name: &str) -> PathBuf {
        let dir =
            env::temp_dir().join(format!("brocade-agent-spool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A control plane that answers with scripted status codes and nothing else.
    /// Returns its address plus a handle yielding the bodies it actually
    /// received — half of the spool's semantics is what got sent, which cannot be
    /// seen from what remains.
    fn fake_control_plane(script: Vec<u16>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let mut received = Vec::new();
            for status in script {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                received.push(read_request_body(&mut stream));
                let body = format!("{{\"status\":{status}}}");
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            received
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    fn read_request_body(stream: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 1024];
        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..read]);
            let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= head_end + 4 + length {
                return String::from_utf8_lossy(&raw[head_end + 4..head_end + 4 + length])
                    .into_owned();
            }
        }
        String::new()
    }

    fn options(server: &str, state_dir: &Path) -> Options {
        Options {
            command: "apply-once".to_owned(),
            server: server.to_owned(),
            token: "t".to_owned(),
            state_dir: state_dir.to_path_buf(),
            apply_mode: ApplyMode::StateDir,
        }
    }

    /// When the control plane says outright that an entry is malformed, drop it
    /// and move on. Keeping it for retry blocks the head of the queue: a finished
    /// or cancelled deployment, or a vanished target row, can answer 400/404, that
    /// observation never lands, and everything useful behind it is stuck too.
    #[test]
    fn a_rejected_item_is_dropped_so_the_queue_keeps_moving() {
        let dir = state_dir("drain-4xx");
        for n in 0..3 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let (server, handle) = fake_control_plane(vec![400, 200, 200]);
        assert!(spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap());

        let received = handle.join().unwrap();
        assert_eq!(received.len(), 3, "被拒的那条之后还要继续发后面的");
        assert!(received[0].contains("\"n\":0"));
        assert!(received[2].contains("\"n\":2"));
        assert!(
            spool_read(TEST_SPOOL, &dir).unwrap().is_empty(),
            "三条都处理完了，队列该空"
        );
        assert_eq!(read_dropped(&dir), 1, "服务端永久拒绝也必须进入丢失计数");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_conflict_is_retryable_and_stays_at_the_head() {
        let dir = state_dir("drain-conflict");
        for n in 0..2 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }
        let (server, handle) = fake_control_plane(vec![409]);
        let error = spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap_err();
        assert!(error.contains("409"));
        assert_eq!(handle.join().unwrap().len(), 1);
        assert_eq!(spool_read(TEST_SPOOL, &dir).unwrap().len(), 2);
        assert_eq!(read_dropped(&dir), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn usage_never_discards_auth_throttle_timeout_or_conflict_responses() {
        for status in [401, 403, 404, 408, 409, 425, 429] {
            assert!(
                !super::USAGE_SPOOL.terminal_statuses.contains(&status),
                "HTTP {status} can recover with the same queued body"
            );
        }
        for status in [400, 410, 422] {
            assert!(super::USAGE_SPOOL.terminal_statuses.contains(&status));
        }
    }

    /// A 5xx means "cannot deliver right now": stop where you are and leave the
    /// unsent entries as they were. Pressing on scrambles the order the control
    /// plane sees — usage is cumulative and read as deltas, observations are
    /// ordered within a release, and out-of-order delivery overwrites new with
    /// old.
    #[test]
    fn a_server_failure_stops_the_drain_and_keeps_the_rest_in_order() {
        let dir = state_dir("drain-5xx");
        for n in 0..3 {
            spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let (server, handle) = fake_control_plane(vec![200, 503]);
        let error = spool_drain(TEST_SPOOL, &options(&server, &dir)).unwrap_err();
        assert!(error.contains("503"), "错误里要带上状态码，实际 {error}");

        let received = handle.join().unwrap();
        assert_eq!(received.len(), 2, "撞上 5xx 就不该再发第三条");

        let left = spool_read(TEST_SPOOL, &dir).unwrap();
        assert_eq!(left.len(), 2, "发成功的那条去掉，剩下两条留着");
        assert!(left[0].contains("\"n\":1"), "留下的要保持原顺序");
        assert!(left[1].contains("\"n\":2"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_blocked_reporter_does_not_block_a_new_durable_append() {
        let dir = state_dir("append-during-report");
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 0 })).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request_body(&mut stream);
            accepted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });

        let drain_options = options(&server, &dir);
        let drain = thread::spawn(move || spool_drain(TEST_SPOOL, &drain_options));
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 1 })).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "append waited for network IO"
        );
        release_tx.send(()).unwrap();
        drain.join().unwrap().unwrap();
        server_thread.join().unwrap();

        let left = spool_read(TEST_SPOOL, &dir).unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].contains("\"n\":1"));
        let _ = fs::remove_dir_all(dir);
    }

    /// With the control plane unreachable, nothing may be lost. This is the whole
    /// reason the spool exists: convergence already happened on this machine, and
    /// that result is its only record.
    #[test]
    fn an_unreachable_control_plane_loses_nothing() {
        let dir = state_dir("drain-unreachable");
        spool_push(TEST_SPOOL, &dir, &serde_json::json!({ "n": 0 })).unwrap();

        // Bind then drop, to obtain a port almost certainly nobody listens on.
        let port = {
            let probe = TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let options = options(&format!("http://127.0.0.1:{port}"), &dir);
        assert!(spool_drain(TEST_SPOOL, &options).is_err());
        assert_eq!(spool_read(TEST_SPOOL, &dir).unwrap().len(), 1);

        let _ = fs::remove_dir_all(dir);
    }

    /// An empty spool must not dial out. This path runs every cycle, and opening a
    /// connection for something that is not there adds load to the control plane
    /// for nothing.
    #[test]
    fn an_empty_spool_does_not_dial_out() {
        let dir = state_dir("drain-empty");
        // An address nobody listens on: actually dialing returns Err here.
        let options = options("http://127.0.0.1:1", &dir);
        assert!(!spool_drain(TEST_SPOOL, &options).unwrap());
        let _ = fs::remove_dir_all(dir);
    }

    /// The dropped counter must survive restarts. It answers "has this machine
    /// ever lost accounting", and a restart is exactly the moment most likely to
    /// erase that fact — the symptom is "this machine had no traffic this month",
    /// indistinguishable in the UI from genuinely having none.
    #[test]
    fn the_dropped_counter_survives_and_only_grows() {
        let dir = state_dir("dropped");
        assert_eq!(read_dropped(&dir), 0, "没丢过就是 0，不是读不出来");

        let tiny = Spool {
            file: "tiny.spool.jsonl",
            max: 2,
            ..TEST_SPOOL
        };
        for n in 0..5 {
            spool_push(tiny, &dir, &serde_json::json!({ "n": n })).unwrap();
        }
        assert_eq!(read_dropped(&dir), 3);

        // Keep dropping on a different spool: the counter is this machine's
        // total, not per-spool.
        let other = Spool {
            file: "other.spool.jsonl",
            max: 1,
            ..TEST_SPOOL
        };
        for n in 0..3 {
            spool_push(other, &dir, &serde_json::json!({ "n": n })).unwrap();
        }
        assert_eq!(read_dropped(&dir), 5);

        let _ = fs::remove_dir_all(dir);
    }

    /// The spool file is read back on the next replay, so one over-limit round
    /// must not deform the on-disk format.
    #[test]
    fn the_spool_file_stays_one_json_object_per_line() {
        let dir = state_dir("format");
        for n in 0..3 {
            spool_push(OBSERVATION_SPOOL, &dir, &serde_json::json!({ "n": n })).unwrap();
        }

        let text = fs::read_to_string(dir.join(OBSERVATION_SPOOL.file)).unwrap();
        assert!(text.ends_with('\n'), "每条以换行收尾，追加才不会粘在一起");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }

        let _ = fs::remove_dir_all(dir);
    }

    /// An unreadable version reports None. Inventing one is worse than none — the
    /// console would show a plausible-looking version unrelated to what is
    /// actually installed.
    #[test]
    fn a_blank_version_line_reads_as_none_not_as_empty_string() {
        assert_eq!(first_line(None), None);
        assert_eq!(first_line(Some(String::new())), None);
        assert_eq!(first_line(Some("   \n".to_owned())), None);
        assert_eq!(
            first_line(Some(
                "Xray 1.8.4 (Xray, Penetrates)\nA unified platform\n".to_owned()
            )),
            Some("Xray 1.8.4 (Xray, Penetrates)".to_owned())
        );
    }
}
