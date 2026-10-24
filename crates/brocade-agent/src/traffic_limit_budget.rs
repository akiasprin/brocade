//! One loopback budget shared by all Xray generations. No kernel modules or new dependencies.
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

static INITIALIZE: Mutex<()> = Mutex::new(());
static BUDGET: OnceLock<Arc<Budget>> = OnceLock::new();
const MAX_CLIENTS: usize = 32;
pub(crate) const STATS_NAME: &str = "brocade>>>machine_limit>>>rate_bps";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ApplicationControl {
    pub enabled: bool,
    pub rate_bps: u64,
    #[serde(default)]
    progress: bool,
    endpoint: String,
    token: String,
}
struct Budget {
    rate: AtomicU64,
    next: Mutex<Instant>,
    clients: AtomicUsize,
    control: Mutex<ApplicationControl>,
    path: PathBuf,
}

pub(crate) fn control_path(dir: &Path) -> PathBuf {
    dir.join("traffic-limit-xray.json")
}

fn valid_control(control: &ApplicationControl) -> bool {
    control.token.len() == 32
        && control
            .token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && control.rate_bps <= brocade_deployment::traffic_limit::MAX_TRAFFIC_RATE_BPS
        && (control.rate_bps == 0 || control.rate_bps >= 1_000)
        && (control.enabled || control.rate_bps == 0)
}

/// A newly launched Xray may pass traffic without a budget only when the durable state proves
/// that no cap was active. Unknown/corrupt state must not turn a restart into a limit bypass.
pub(crate) fn required_at_start(dir: &Path) -> bool {
    match crate::fsutil::read_bounded(&control_path(dir), 4096) {
        Ok(Some(bytes)) => serde_json::from_slice::<ApplicationControl>(&bytes)
            .map(|control| !valid_control(&control) || (control.enabled && control.rate_bps != 0))
            .unwrap_or(true),
        Ok(None) => {
            match crate::fsutil::read_bounded(&dir.join("traffic-limit-desired.json"), 128 * 1024) {
                Ok(Some(bytes)) => serde_json::from_slice::<
                    brocade_deployment::traffic_limit::TrafficLimitDesired,
                >(&bytes)
                .map(|desired| desired.policy.enabled && desired.policy.layer.application())
                .unwrap_or(true),
                Ok(None) => false,
                Err(_) => true,
            }
        }
        Err(_) => true,
    }
}

pub(crate) fn initialize(dir: &Path) -> Result<(), String> {
    initialize_with_policy(dir, None)
}

fn initialize_with_policy(dir: &Path, policy: Option<(bool, Option<u64>)>) -> Result<(), String> {
    let _initializing = INITIALIZE
        .lock()
        .map_err(|_| "budget initialization lock")?;
    if BUDGET.get().is_some() {
        return Ok(());
    }
    let path = control_path(dir);
    let previous = match crate::fsutil::read_bounded(&path, 4096) {
        Ok(Some(bytes)) => {
            match serde_json::from_slice::<ApplicationControl>(&bytes) {
                Ok(control) if valid_control(&control) => Some(control),
                _ if policy.is_some() => {
                    // Only a validated local policy and current meter can recover an unknown cap.
                    // Startup without that evidence must retain its existing fail-closed behavior.
                    eprintln!("traffic-limit: rebuilding corrupt application control from evaluated policy");
                    None
                }
                _ => return Err("无效应用层限速检查点".into()),
            }
        }
        Ok(None) => None,
        Err(error) if policy.is_some() => {
            eprintln!("traffic-limit: rebuilding unreadable application control from evaluated policy: {error}");
            None
        }
        Err(e) => return Err(format!("读取应用层限速检查点失败：{e}")),
    };
    if previous.is_none() && policy.is_none() && required_at_start(dir) {
        return Err("应用层历史上限未知，等待有效策略与计量重新计算".into());
    }
    let listener = previous
        .as_ref()
        .and_then(|p| p.endpoint.parse::<std::net::SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback())
        .and_then(|a| TcpListener::bind(a).ok())
        .map(Ok)
        .unwrap_or_else(|| TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| format!("启动应用层限速预算失败：{e}"))?;
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|e| format!("创建应用层限速凭据失败：{e}"))?;
    let control = ApplicationControl {
        enabled: policy.map_or_else(|| previous.as_ref().is_some_and(|p| p.enabled), |p| p.0),
        rate_bps: policy.map_or_else(
            || previous.as_ref().map_or(0, |p| p.rate_bps),
            |p| if p.0 { p.1.unwrap_or(0) } else { 0 },
        ),
        progress: true,
        endpoint: listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .to_string(),
        token: previous
            .map(|p| p.token)
            .unwrap_or_else(|| entropy.iter().map(|b| format!("{b:02x}")).collect()),
    };
    let budget = Arc::new(Budget {
        rate: AtomicU64::new(control.rate_bps),
        next: Mutex::new(Instant::now()),
        clients: AtomicUsize::new(0),
        control: Mutex::new(control),
        path,
    });
    write_control(&budget)?;
    let worker_budget = Arc::clone(&budget);
    thread::Builder::new()
        .name("traffic-limit-budget".into())
        .spawn(move || {
            let budget = worker_budget;
            for connection in listener.incoming() {
                let Ok(stream) = connection else {
                    continue;
                };
                if budget.clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
                    budget.clients.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let budget = Arc::clone(&budget);
                let failed = Arc::clone(&budget);
                if thread::Builder::new()
                    .name("xray-limit-client".into())
                    .spawn(move || {
                        let _ = serve(stream, &budget);
                        budget.clients.fetch_sub(1, Ordering::AcqRel);
                    })
                    .is_err()
                {
                    failed.clients.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    BUDGET.set(budget).map_err(|_| "应用层限速预算重复初始化")?;
    Ok(())
}

fn write_control(budget: &Budget) -> Result<(), String> {
    let control = budget.control.lock().map_err(|_| "应用层限速状态锁损坏")?;
    crate::fsutil::atomic_write_private(
        &budget.path,
        &serde_json::to_vec(&*control).map_err(|e| e.to_string())?,
    )
}

pub(crate) fn configure(enabled: bool, rate: Option<u64>) -> Result<(), String> {
    let budget = BUDGET.get().ok_or("应用层限速预算尚未启动")?;
    configure_budget(budget, enabled, rate)
}

pub(crate) fn ensure_configured(
    dir: &Path,
    enabled: bool,
    rate: Option<u64>,
) -> Result<(), String> {
    if BUDGET.get().is_none() {
        if !enabled && !required_at_start(dir) {
            return Ok(());
        }
        initialize_with_policy(dir, Some((enabled, rate)))?;
    }
    configure(enabled, rate)
}

fn configure_budget(budget: &Budget, enabled: bool, rate: Option<u64>) -> Result<(), String> {
    let rate = if enabled { rate.unwrap_or(0) } else { 0 };
    let mut control = budget.control.lock().map_err(|_| "应用层限速状态锁损坏")?;
    if control.enabled == enabled && control.rate_bps == rate {
        // The in-memory budget can be healthy while its private file was lost or damaged.
        // Restore the same endpoint/token so existing Xray generations can recover too.
        let encoded = serde_json::to_vec(&*control).map_err(|e| e.to_string())?;
        if crate::fsutil::read_bounded(&budget.path, 4096)
            .is_ok_and(|bytes| bytes.as_deref() == Some(encoded.as_slice()))
        {
            return Ok(());
        }
        return crate::fsutil::atomic_write_private(&budget.path, &encoded);
    }
    let mut wanted = control.clone();
    wanted.enabled = enabled;
    wanted.rate_bps = rate;
    let encoded = serde_json::to_vec(&wanted).map_err(|e| e.to_string())?;
    crate::fsutil::atomic_write_private(&budget.path, &encoded)?;
    *control = wanted;
    budget.rate.store(rate, Ordering::Release);
    *budget.next.lock().map_err(|_| "应用层限速预算锁损坏")? = Instant::now();
    Ok(())
}

fn serve(stream: TcpStream, budget: &Budget) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let mut reader = BufReader::with_capacity(128, stream);
    loop {
        let mut line = Vec::new();
        // Bound even an unauthenticated local request; do not log the credential.
        let n = std::io::Read::by_ref(&mut reader)
            .take(96)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            return Err("invalid budget frame".into());
        }
        let text = std::str::from_utf8(&line).map_err(|_| "invalid budget frame")?;
        let mut fields = text.trim_end().split(' ');
        let token = fields.next().ok_or("invalid budget frame")?;
        let count = fields.next().ok_or("invalid budget frame")?;
        let progress_frames = match fields.next() {
            None => false,
            Some("progress-v1") if fields.next().is_none() => true,
            _ => return Err("invalid budget frame".into()),
        };
        if token != budget.control.lock().map_err(|_| "budget lock")?.token {
            return Err("budget authentication failed".into());
        }
        let count: u64 = count.parse().map_err(|_| "invalid budget size")?;
        if count > 16_384 {
            return Err("budget request too large".into());
        }
        let mut remaining = count;
        let mut progress = Instant::now();
        while remaining > 0 {
            let rate = budget.rate.load(Ordering::Acquire);
            if rate == 0 {
                break;
            }
            let part = remaining.min((rate / 80).clamp(1, 16_384));
            let duration =
                Duration::from_nanos((u128::from(part) * 8_000_000_000 / u128::from(rate)) as u64);
            let until = {
                let mut next = budget.next.lock().map_err(|_| "budget lock")?;
                *next = (*next).max(Instant::now()) + duration;
                *next
            };
            thread::sleep(until.saturating_duration_since(Instant::now()));
            remaining -= part;
            // Low rates or a hot rate reduction can legitimately take longer than the client's
            // outage deadline. Report progress without granting bytes before the budget allows.
            if progress_frames && remaining > 0 && progress.elapsed() >= Duration::from_secs(1) {
                reader
                    .get_mut()
                    .write_all(b"WAIT\n")
                    .map_err(|e| e.to_string())?;
                progress = Instant::now();
            }
        }
        writeln!(
            reader.get_mut(),
            "OK {}",
            budget.rate.load(Ordering::Acquire)
        )
        .map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn traffic_limit_missing_control_cannot_publish_uncapped_state_before_evaluation() {
        if std::env::var_os("BROCADE_MISSING_CONTROL_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "traffic_limit_budget::tests::traffic_limit_missing_control_cannot_publish_uncapped_state_before_evaluation", "--test-threads=1"])
                .env("BROCADE_MISSING_CONTROL_CHILD", "1")
                .output().unwrap();
            assert!(
                result.status.success(),
                "missing-control child failed: {}",
                String::from_utf8_lossy(&result.stdout)
            );
            return;
        }
        use brocade_deployment::traffic_limit::*;
        let dir = std::env::temp_dir().join(format!(
            "brocade-budget-missing-start-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let desired = TrafficLimitDesired {
            revision: 1,
            policy: TrafficLimitPolicy {
                enabled: true,
                layer: TrafficLimitLayer::Application,
                rules: vec![TrafficLimitRule {
                    threshold: TrafficLimitThreshold::Absolute { bytes: "1".into() },
                    rate_bps: 80_000,
                }],
            },
            quota_bytes: None,
            cycle_kind: NodeTrafficCycleKind::Monthly,
            accounting_mode: NodeTrafficAccountingMode::BidirectionalMax,
            reset_month: None,
            reset_day: 1,
            anchor: None,
        };
        fs::write(
            dir.join("traffic-limit-desired.json"),
            serde_json::to_vec(&desired).unwrap(),
        )
        .unwrap();
        assert!(required_at_start(&dir));
        assert!(
            initialize(&dir).is_err(),
            "unknown historical cap must wait for evaluation"
        );
        assert!(
            !control_path(&dir).exists(),
            "startup must not publish a false disabled checkpoint"
        );
        assert!(required_at_start(&dir));
        ensure_configured(&dir, true, Some(80_000)).unwrap();
        let control: ApplicationControl =
            serde_json::from_slice(&fs::read(control_path(&dir)).unwrap()).unwrap();
        assert!(control.enabled);
        assert_eq!(control.rate_bps, 80_000);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn traffic_limit_application_start_requirement_preserves_active_or_unknown_caps() {
        let dir =
            std::env::temp_dir().join(format!("brocade-budget-required-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!required_at_start(&dir));
        let mut control = ApplicationControl {
            enabled: false,
            rate_bps: 0,
            progress: true,
            endpoint: "127.0.0.1:1".into(),
            token: "a".repeat(32),
        };
        for (enabled, rate, required) in [(false, 0, false), (true, 0, false), (true, 1000, true)] {
            control.enabled = enabled;
            control.rate_bps = rate;
            fs::write(control_path(&dir), serde_json::to_vec(&control).unwrap()).unwrap();
            assert_eq!(required_at_start(&dir), required);
        }
        fs::write(control_path(&dir), b"invalid").unwrap();
        assert!(required_at_start(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn traffic_limit_application_repairs_corrupt_or_missing_control_without_rate_change() {
        let dir =
            std::env::temp_dir().join(format!("brocade-budget-repair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let budget = Budget {
            rate: AtomicU64::new(0),
            next: Mutex::new(Instant::now()),
            clients: AtomicUsize::new(0),
            path: control_path(&dir),
            control: Mutex::new(ApplicationControl {
                enabled: false,
                rate_bps: 0,
                progress: true,
                endpoint: "127.0.0.1:12345".into(),
                token: "a".repeat(32),
            }),
        };
        configure_budget(&budget, true, Some(80_000)).unwrap();
        for missing in [false, true] {
            if missing {
                fs::remove_file(&budget.path).unwrap();
            } else {
                fs::write(&budget.path, b"{truncated").unwrap();
            }
            configure_budget(&budget, true, Some(80_000)).unwrap();
            let control: ApplicationControl =
                serde_json::from_slice(&fs::read(&budget.path).unwrap())
                    .expect("an unchanged cap must still repair its durable control");
            assert!(valid_control(&control));
            assert!(control.enabled);
            assert_eq!(control.rate_bps, 80_000);
            assert_eq!(budget.rate.load(Ordering::Acquire), 80_000);
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn traffic_limit_application_persistence_failure_keeps_retryable_state() {
        let dir = std::env::temp_dir().join(format!("brocade-budget-write-{}", std::process::id()));
        let budget = Budget {
            rate: AtomicU64::new(0),
            next: Mutex::new(Instant::now()),
            clients: AtomicUsize::new(0),
            path: dir.join("nested/control.json"),
            control: Mutex::new(ApplicationControl {
                enabled: false,
                rate_bps: 0,
                progress: true,
                endpoint: "127.0.0.1:12345".into(),
                token: "a".repeat(32),
            }),
        };
        assert!(configure_budget(&budget, true, Some(512_000)).is_err());
        assert!(!budget.control.lock().unwrap().enabled);
        assert_eq!(budget.rate.load(Ordering::Acquire), 0);
        fs::create_dir_all(dir.join("nested")).unwrap();
        configure_budget(&budget, true, Some(512_000)).unwrap();
        assert_eq!(budget.rate.load(Ordering::Acquire), 512_000);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn traffic_limit_application_budget_is_shared_across_clients() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let budget = Arc::new(Budget {
            rate: AtomicU64::new(80_000),
            next: Mutex::new(Instant::now()),
            clients: AtomicUsize::new(0),
            path: PathBuf::new(),
            control: Mutex::new(ApplicationControl {
                enabled: true,
                rate_bps: 80_000,
                progress: true,
                endpoint: endpoint.to_string(),
                token: "a".repeat(32),
            }),
        });
        let server = thread::spawn(move || {
            let mut clients = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let budget = Arc::clone(&budget);
                clients.push(thread::spawn(move || serve(stream, &budget).unwrap()));
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        let started = Instant::now();
        let mut clients = Vec::new();
        for _ in 0..2 {
            clients.push(thread::spawn(move || {
                let stream = TcpStream::connect(endpoint).unwrap();
                let mut stream = BufReader::new(stream);
                writeln!(stream.get_mut(), "{} 2000", "a".repeat(32)).unwrap();
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                assert_eq!(line, "OK 80000\n");
            }));
        }
        for client in clients {
            client.join().unwrap();
        }
        server.join().unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(380),
            "two clients must consume one 10KB/s budget"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn traffic_limit_budget_progress_is_negotiated_without_breaking_legacy_clients() {
        for progress_frames in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let budget = Budget {
                rate: AtomicU64::new(1000),
                next: Mutex::new(Instant::now()),
                clients: AtomicUsize::new(0),
                control: Mutex::new(ApplicationControl {
                    enabled: true,
                    rate_bps: 1000,
                    progress: true,
                    endpoint: address.to_string(),
                    token: "a".repeat(32),
                }),
                path: PathBuf::new(),
            };
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                serve(stream, &budget).unwrap();
            });
            let stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let suffix = if progress_frames { " progress-v1" } else { "" };
            writeln!(stream.get_mut(), "{} 200{suffix}", "a".repeat(32)).unwrap();
            let mut saw_progress = false;
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "WAIT\n" {
                    saw_progress = true;
                } else {
                    assert_eq!(line, "OK 1000\n");
                    break;
                }
            }
            assert_eq!(saw_progress, progress_frames);
            drop(stream);
            server.join().unwrap();
        }
    }
    #[test]
    fn traffic_limit_application_budget_rejects_unbounded_and_unauthorized_requests() {
        for request in [
            format!("{} 16385\n", "a".repeat(32)),
            format!("{} 1\n", "b".repeat(32)),
            "x".repeat(96),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let budget = Budget {
                rate: AtomicU64::new(0),
                next: Mutex::new(Instant::now()),
                clients: AtomicUsize::new(0),
                path: PathBuf::new(),
                control: Mutex::new(ApplicationControl {
                    enabled: true,
                    rate_bps: 0,
                    progress: true,
                    endpoint: address.to_string(),
                    token: "a".repeat(32),
                }),
            };
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                assert!(serve(stream, &budget).is_err());
            });
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            server.join().unwrap();
        }
    }
}

#[cfg(test)]
mod real_xray_test {
    use super::*;
    use std::{
        fs,
        io::Read,
        process::{Command, Stdio},
    };
    struct Processes(Vec<std::process::Child>, PathBuf);
    impl Drop for Processes {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = fs::remove_dir_all(&self.1);
        }
    }
    fn port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
    #[test]
    #[ignore = "requires explicit BROCADE_XRAY_BIN built from the current fork"]
    fn traffic_limit_real_xray_generations_share_one_budget_for_existing_tcp_and_udp() {
        let binary = std::env::var_os("BROCADE_XRAY_BIN").expect("explicit current Xray binary");
        let dir = std::env::temp_dir().join(format!("brocade-limit-xray-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        initialize(&dir).unwrap();
        configure(true, Some(800_000)).unwrap();
        let mut processes = Processes(Vec::new(), dir.clone());
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut streams = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = echo.accept().unwrap();
                streams.push(thread::spawn(move || {
                    for size in [65_536, 8192] {
                        let mut data = vec![0; size];
                        stream.read_exact(&mut data).unwrap();
                        stream.write_all(&data).unwrap();
                    }
                }));
            }
            for stream in streams {
                stream.join().unwrap();
            }
        });
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        udp.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let udp_server = thread::spawn(move || {
            let mut bytes = [0; 8192];
            let (n, from) = udp.recv_from(&mut bytes).unwrap();
            udp.send_to(&bytes[..n], from).unwrap();
        });
        let mut listeners = Vec::new();
        let mut udp_listener = 0;
        for generation in 0..2 {
            let tcp = port();
            let api = port();
            let packet = port();
            udp_listener = packet;
            let config = serde_json::json!({"log":{"loglevel":"error"},"api":{"tag":"api","services":["StatsService"]},"stats":{},
                "inbounds":[{"tag":"data","listen":"127.0.0.1","port":tcp,"protocol":"dokodemo-door","settings":{"address":"127.0.0.1","port":echo_port,"network":"tcp"}},
                {"tag":"udp","listen":"127.0.0.1","port":packet,"protocol":"dokodemo-door","settings":{"address":"127.0.0.1","port":udp_port,"network":"udp"}},
                {"tag":"api","listen":"127.0.0.1","port":api,"protocol":"dokodemo-door","settings":{"address":"127.0.0.1"}}],
                "outbounds":[{"tag":"direct","protocol":"freedom"}],"routing":{"rules":[{"type":"field","inboundTag":["api"],"outboundTag":"api"}]}});
            let path = dir.join(format!("generation-{generation}.json"));
            fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
            processes.0.push(
                Command::new(&binary)
                    .args(["run", "-config"])
                    .arg(path)
                    .env("XRAY_BROCADE_MACHINE_LIMIT_FILE", control_path(&dir))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if crate::xray_grpc::query_stats(([127, 0, 0, 1], api).into(), STATS_NAME)
                    .is_ok_and(|s| s.iter().any(|s| s.name == STATS_NAME && s.value == 800_000))
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "Xray machine budget API not ready"
                );
                thread::sleep(Duration::from_millis(30));
            }
            listeners.push(tcp);
        }
        let started = Instant::now();
        let mut streams: Vec<_> = listeners
            .into_iter()
            .map(|p| TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, p)).unwrap())
            .collect();
        for (size, minimum, rate) in [
            (65_536, Duration::from_secs(2), 800_000),
            (8192, Duration::from_millis(400), 512_000),
        ] {
            configure(true, Some(rate)).unwrap();
            let round = Instant::now();
            streams = streams
                .into_iter()
                .map(|mut stream| {
                    thread::spawn(move || {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(15)))
                            .unwrap();
                        let payload = vec![42; size];
                        stream.write_all(&payload).unwrap();
                        let mut reply = vec![0; size];
                        stream.read_exact(&mut reply).unwrap();
                        assert_eq!(reply, payload);
                        stream
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect();
            assert!(
                round.elapsed() >= minimum,
                "each Xray generation incorrectly received its own full budget"
            );
        }
        let client = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let payload = vec![7; 4096];
        client
            .send_to(&payload, (std::net::Ipv4Addr::LOCALHOST, udp_listener))
            .unwrap();
        let mut reply = [0; 8192];
        let (n, _) = client.recv_from(&mut reply).unwrap();
        assert_eq!(&reply[..n], payload);
        // A newly capped API must not certify a still-live generation started without opt-in.
        let current = fs::read(dir.join("generation-1.json")).unwrap();
        fs::write(dir.join("xray.json"), &current).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(&current).unwrap();
        let mut legacy_api = 0;
        for inbound in legacy["inbounds"].as_array_mut().unwrap() {
            let selected = port();
            inbound["port"] = selected.into();
            if inbound["tag"] == "api" {
                legacy_api = selected;
            }
        }
        let legacy_config = dir.join("legacy.json");
        fs::write(&legacy_config, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let legacy_binary =
            std::env::var_os("BROCADE_TRAFFIC_LIMIT_LEGACY_XRAY").unwrap_or_else(|| binary.clone());
        let mut command = Command::new(&legacy_binary);
        command
            .args(["run", "-config"])
            .arg(legacy_config)
            .env_remove("XRAY_BROCADE_MACHINE_LIMIT_FILE")
            .env_remove("XRAY_BROCADE_MACHINE_LIMIT_REQUIRED")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if std::env::var_os("BROCADE_TRAFFIC_LIMIT_LEGACY_XRAY").is_some() {
            let version = Command::new(&legacy_binary)
                .arg("version")
                .output()
                .unwrap();
            assert!(version.status.success());
            assert!(
                !crate::xray_version_has_capability(
                    &String::from_utf8_lossy(&version.stdout),
                    "machine-traffic-limit-v1"
                ),
                "explicit legacy test binary must lack the new capability"
            );
            command.env("XRAY_BROCADE_MACHINE_LIMIT_FILE", control_path(&dir));
        }
        processes.0.push(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while crate::xray_grpc::query_stats(([127, 0, 0, 1], legacy_api).into(), "legacy-health")
            .is_err()
        {
            assert!(Instant::now() < deadline, "legacy Xray API did not start");
            thread::sleep(Duration::from_millis(30));
        }
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
            .to_owned();
        let sha256 = crate::file_sha256_hex(Path::new(&binary)).unwrap();
        let identity =
            |pid: u32, sha256: String| brocade_deployment::protocol::XrayUpgradeProcess {
                pid: pid as i32,
                pid_start_ticks: crate::process_ref(pid as i32).unwrap().start_ticks,
                boot_id: boot_id.clone(),
                sha256,
                alive: true,
                accepting: None,
                active_connections: None,
            };
        let runtime = brocade_deployment::protocol::XrayUpgradeRuntime {
            source: brocade_deployment::protocol::XrayUpgradeSource::BinaryRelease,
            release_id: 1,
            attempt: 1,
            strategy: brocade_deployment::protocol::XrayReleaseStrategy::Handoff,
            phase: brocade_deployment::protocol::XrayUpgradePhase::Draining,
            old: identity(
                processes.0[2].id(),
                crate::file_sha256_hex(Path::new(&legacy_binary)).unwrap(),
            ),
            candidate: Some(identity(processes.0[1].id(), sha256)),
            drain_deadline_unix_secs: None,
            blocked_reason: None,
        };
        crate::fsutil::atomic_write_private(
            &dir.join(crate::xrayupdate::UPGRADE_RUNTIME_FILE),
            &serde_json::to_vec(&runtime).unwrap(),
        )
        .unwrap();
        assert!(
            crate::traffic_limit::apply_application(&dir, true, Some(512_000)).is_err(),
            "a capped current API must not certify the uncapped legacy process"
        );
        processes.0[2].kill().unwrap();
        processes.0[2].wait().unwrap();
        crate::traffic_limit::apply_application(&dir, true, Some(512_000)).unwrap();
        drop(streams);
        server.join().unwrap();
        udp_server.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(20));
    }
}
