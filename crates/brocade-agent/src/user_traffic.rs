//! Bounded loopback admission shared by every Xray generation on this node.
//! The Console owns global reservations; this service only caches short-lived credits and paces payload.
use crate::{http::HttpClient, options::Options};
use brocade_deployment::user_traffic::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
const MAX_SOURCES: usize = 4096;
const MAX_CLIENTS: usize = 256;
static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
static INITIALIZE: Mutex<()> = Mutex::new(());
#[derive(Serialize, Clone)]
struct Control {
    endpoint: String,
    token: String,
    generation: u64,
}
#[derive(Deserialize)]
struct LocalRequest {
    token: String,
    source: String,
    bytes: u64,
}
struct Entry {
    credits: u64,
    rate: u64,
    expires: Instant,
    next: Instant,
    unlimited: bool,
    managed: bool,
    generation: u64,
    suspended: bool,
    last: Instant,
}
impl Entry {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            credits: 0,
            rate: 0,
            expires: now,
            next: now,
            unlimited: false,
            managed: true,
            generation: 0,
            suspended: false,
            last: now,
        }
    }
}
struct PolicyCache {
    effects: BTreeMap<String, UserTrafficEffect>,
    updated: Option<Instant>,
}
struct Service {
    options: Options,
    token: String,
    entries: Mutex<BTreeMap<String, Arc<Mutex<Entry>>>>,
    known: Mutex<BTreeSet<String>>,
    unknown_checkpoint: AtomicBool,
    clients: AtomicUsize,
    control: Mutex<Control>,
    generation: AtomicU64,
    policies: Mutex<PolicyCache>,
}
pub(crate) fn control_path(dir: &Path) -> PathBuf {
    dir.join("user-traffic-control.json")
}
fn known_path(dir: &Path) -> PathBuf {
    dir.join("user-traffic-known.json")
}

pub(crate) fn initialize(options: &Options) -> Result<(), String> {
    let _guard = INITIALIZE
        .lock()
        .map_err(|_| "user budget initialization lock")?;
    if SERVICE.get().is_some() {
        return Ok(());
    }
    let known = crate::fsutil::read_bounded(&known_path(&options.state_dir), MAX_SOURCES * 1024);
    let (known, unknown_checkpoint) = match known {
        Ok(None) => (BTreeSet::new(), false),
        Ok(Some(bytes)) => match serde_json::from_slice::<BTreeSet<String>>(&bytes) {
            Ok(keys) if keys.len() <= MAX_SOURCES => (keys, false),
            _ => (BTreeSet::new(), true),
        },
        Err(_) => (BTreeSet::new(), true),
    };
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "user budget entropy unavailable")?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "user budget listener unavailable")?;
    let control = Control {
        endpoint: listener
            .local_addr()
            .map_err(|_| "user budget endpoint unavailable")?
            .to_string(),
        token: token.clone(),
        generation: 0,
    };
    crate::fsutil::atomic_write_private(
        &control_path(&options.state_dir),
        &serde_json::to_vec(&control).map_err(|_| "user budget control serialization")?,
    )?;
    let service = Arc::new(Service {
        options: options.clone(),
        token,
        entries: Mutex::new(BTreeMap::new()),
        known: Mutex::new(known),
        unknown_checkpoint: AtomicBool::new(unknown_checkpoint),
        clients: AtomicUsize::new(0),
        control: Mutex::new(control.clone()),
        generation: AtomicU64::new(0),
        policies: Mutex::new(PolicyCache {
            effects: BTreeMap::new(),
            updated: None,
        }),
    });
    let shared = Arc::clone(&service);
    thread::Builder::new()
        .name("user-budget-listener".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if shared.clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
                    shared.clients.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let active = Arc::clone(&shared);
                let failed = Arc::clone(&shared);
                if thread::Builder::new()
                    .name("user-budget-client".into())
                    .spawn(move || {
                        let _ = serve(stream, &active);
                        active.clients.fetch_sub(1, Ordering::AcqRel);
                    })
                    .is_err()
                {
                    failed.clients.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|_| "user budget listener worker unavailable")?;
    SERVICE
        .set(service)
        .map_err(|_| "user budget initialized twice")?;
    Ok(())
}
fn remember(service: &Service, source: &str, limited: bool) -> Result<(), String> {
    let mut known = service
        .known
        .lock()
        .map_err(|_| "user budget checkpoint lock")?;
    if limited == known.contains(source) {
        return Ok(());
    }
    if limited && known.len() >= MAX_SOURCES {
        return Err("user budget checkpoint capacity reached".into());
    }
    let mut updated = known.clone();
    if limited {
        updated.insert(source.into());
    } else {
        updated.remove(source);
    }
    crate::fsutil::atomic_write_private(
        &known_path(&service.options.state_dir),
        &serde_json::to_vec(&updated).map_err(|_| "user budget checkpoint encoding")?,
    )?;
    *known = updated;
    Ok(())
}
fn required_path(dir: &Path) -> PathBuf {
    dir.join("user-traffic-required.json")
}
pub(crate) fn required_at_start(dir: &Path) -> bool {
    let required = match crate::fsutil::read_bounded(&required_path(dir), 16) {
        Ok(None) => false,
        Ok(Some(data)) => serde_json::from_slice::<bool>(&data).unwrap_or(true),
        Err(_) => true,
    };
    let known = match crate::fsutil::read_bounded(&known_path(dir), MAX_SOURCES * 1024) {
        Ok(None) => false,
        Ok(Some(data)) => serde_json::from_slice::<BTreeSet<String>>(&data)
            .map(|keys| !keys.is_empty())
            .unwrap_or(true),
        Err(_) => true,
    };
    required || known
}
pub(crate) fn verify_executable(dir: &Path, program: &Path) -> Result<(), String> {
    if !required_at_start(dir) {
        return Ok(());
    }
    let output = crate::command::run_command_with_timeout(
        program.to_str().ok_or("invalid Xray executable path")?,
        &["version"],
        Duration::from_secs(2),
    )?;
    if !crate::xray_version_has_capability(&output, "user-traffic-limit-v1") {
        return Err("仍有用户超额限速策略，不能启动缺少 user-traffic-limit-v1 能力的 Xray".into());
    }
    Ok(())
}
fn source_entry(service: &Service, source: &str) -> Result<Arc<Mutex<Entry>>, String> {
    let mut entries = service
        .entries
        .lock()
        .map_err(|_| "user budget cache lock")?;
    if let Some(entry) = entries.get(source) {
        return Ok(Arc::clone(entry));
    }
    // An active waiter holds a second Arc and cannot be evicted or duplicated.
    entries.retain(|_, entry| {
        Arc::strong_count(entry) > 1
            || entry
                .lock()
                .is_ok_and(|entry| entry.last.elapsed() < Duration::from_secs(10))
    });
    if entries.len() >= MAX_SOURCES {
        return Err("user budget cache capacity reached".into());
    }
    let entry = Arc::new(Mutex::new(Entry::new()));
    entries.insert(source.into(), Arc::clone(&entry));
    Ok(entry)
}
#[derive(Clone, Copy)]
enum Admission {
    Allowed,
    Open,
    Pass,
    Suspended,
}
fn assigned(service: &Service, source: &str) -> Result<Option<Admission>, String> {
    let policies = service
        .policies
        .lock()
        .map_err(|_| "user policy cache lock")?;
    if policies
        .updated
        .is_none_or(|at| at.elapsed() >= Duration::from_millis(BUDGET_TTL_MS))
    {
        return Ok(None);
    }
    Ok(match policies.effects.get(source) {
        Some(UserTrafficEffect::Unlimited { managed: true }) => Some(Admission::Open),
        Some(UserTrafficEffect::Unlimited { managed: false }) => Some(Admission::Pass),
        Some(UserTrafficEffect::Suspended) => Some(Admission::Suspended),
        _ => None,
    })
}
fn synchronize(service: &Service) -> Result<(), String> {
    let response = HttpClient::new(&service.options.server)?.request_timeout(
        "GET",
        "/agent/v1/user-traffic/policies",
        &service.options.token,
        None,
        Duration::from_secs(2),
    )?;
    if response.status == 404 {
        return Ok(());
    }
    if response.status != 200 {
        return Err(format!(
            "user policy synchronization HTTP {}",
            response.status
        ));
    }
    let snapshot: UserTrafficSnapshot =
        serde_json::from_str(&response.body).map_err(|_| "invalid user policy assignments")?;
    if snapshot.sha256.len() != 64 || !snapshot.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid user policy snapshot identity".into());
    }
    let rows = snapshot.sources;
    if rows.len() > MAX_SOURCES {
        return Err("user policy assignments exceed capacity".into());
    }
    let mut effects = BTreeMap::new();
    for row in rows {
        if row.revision == 0
            || matches!(row.effect,UserTrafficEffect::Limited{rate_bps} if !(1000..=1_000_000_000_000).contains(&rate_bps))
        {
            return Err("invalid user policy assignment".into());
        }
        if row.source.len() > 512 || effects.insert(row.source, row.effect).is_some() {
            return Err("invalid or duplicate user policy source".into());
        }
    }
    let mut policies = service
        .policies
        .lock()
        .map_err(|_| "user policy cache lock")?;
    // Persist the managed-source requirement before publishing it to live Xray generations.
    // A crash or failed control-file write must not permit an incompatible binary at restart.
    let managed: BTreeSet<String> = effects
        .iter()
        .filter_map(|(source, effect)| {
            (!matches!(effect, UserTrafficEffect::Unlimited { managed: false }))
                .then_some(source.clone())
        })
        .collect();
    let mut known = service
        .known
        .lock()
        .map_err(|_| "user budget checkpoint lock")?;
    if *known != managed || service.unknown_checkpoint.load(Ordering::Acquire) {
        crate::fsutil::atomic_write_private(
            &known_path(&service.options.state_dir),
            &serde_json::to_vec(&managed).map_err(|_| "user budget checkpoint encoding")?,
        )?;
        *known = managed;
        service.unknown_checkpoint.store(false, Ordering::Release);
    }
    if effects != policies.effects {
        let mut control = service
            .control
            .lock()
            .map_err(|_| "user budget control lock")?;
        let mut next = control.clone();
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or("user policy generation exhausted")?;
        // Publish the durable generation first. A client which observes it waits behind this cache lock.
        crate::fsutil::atomic_write_private(
            &control_path(&service.options.state_dir),
            &serde_json::to_vec(&next).map_err(|_| "user policy control encoding")?,
        )?;
        service.generation.store(next.generation, Ordering::Release);
        *control = next;
        policies.effects = effects;
    }
    policies.updated = Some(Instant::now());
    drop(known);
    drop(policies);
    let body =
        serde_json::to_string(&snapshot.sha256).map_err(|_| "invalid user policy receipt")?;
    let receipt = HttpClient::new(&service.options.server)?.request_timeout(
        "POST",
        "/agent/v1/user-traffic/policies/ack",
        &service.options.token,
        Some(&body),
        Duration::from_secs(2),
    )?;
    if receipt.status != 204 {
        return Err(format!("user policy receipt HTTP {}", receipt.status));
    }
    Ok(())
}
fn serve(mut stream: TcpStream, service: &Service) -> Result<(), String> {
    stream
        .set_nodelay(true)
        .map_err(|_| "user budget socket option")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|_| "user budget timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "user budget timeout")?;
    let reader_stream = stream.try_clone().map_err(|_| "user budget connection")?;
    let mut reader = BufReader::new(reader_stream);
    loop {
        let mut line = String::new();
        let size = (&mut reader)
            .take(2049)
            .read_line(&mut line)
            .map_err(|_| "user budget request")?;
        if size == 0 {
            return Ok(());
        }
        if size > 2048 || !line.ends_with('\n') {
            return Err("user budget request too large".into());
        }
        let request: LocalRequest =
            serde_json::from_str(&line).map_err(|_| "invalid user budget request")?;
        if request.token != service.token
            || request.bytes == 0
            || request.bytes > 16384
            || request.source.len() > 512
        {
            return Err("user budget request rejected".into());
        }
        let result = match assigned(service, &request.source)? {
            Some(result) => Ok(result),
            None => {
                let entry = source_entry(service, &request.source)?;
                admit(service, &request.source, request.bytes, &entry, &mut stream)
            }
        };
        match result {
            Ok(Admission::Allowed) => stream.write_all(b"OK\n"),
            Ok(Admission::Open) => stream.write_all(b"OPEN\n"),
            Ok(Admission::Pass) => stream.write_all(b"PASS\n"),
            Ok(Admission::Suspended) => stream.write_all(b"STOP\n"),
            Err(_) => stream.write_all(b"ERR\n"),
        }
        .map_err(|_| "user budget receipt")?;
    }
}
fn progress(stream: &mut TcpStream, last: &mut Instant) -> Result<(), String> {
    if last.elapsed() >= Duration::from_millis(500) {
        stream
            .write_all(b"WAIT\n")
            .map_err(|_| "user budget canceled")?;
        *last = Instant::now();
    }
    Ok(())
}
fn admit(
    service: &Service,
    source: &str,
    mut bytes: u64,
    entry: &Mutex<Entry>,
    stream: &mut TcpStream,
) -> Result<Admission, String> {
    let mut last_progress = Instant::now();
    while bytes > 0 {
        progress(stream, &mut last_progress)?;
        let mut state = entry.lock().map_err(|_| "user budget entry lock")?;
        state.last = Instant::now();
        let generation = service.generation.load(Ordering::Acquire);
        if state.generation != generation {
            state.credits = 0;
            state.unlimited = false;
            state.suspended = false;
            state.expires = Instant::now();
            state.generation = generation;
        }

        if state.expires <= Instant::now() {
            state.credits = 0;
            state.unlimited = false;
            state.suspended = false;
        }
        if state.unlimited {
            return Ok(if state.managed {
                Admission::Open
            } else {
                Admission::Pass
            });
        }
        if state.suspended {
            return Ok(Admission::Suspended);
        }
        if state.credits == 0 {
            // Holding only this source's lock prevents duplicated application of a reservation. Other users stay independent.
            let started = Instant::now();
            let grant = fetch(service, source)?;
            match grant {
                UserBudgetGrant::Unlimited { valid_ms, managed } => {
                    remember(service, source, managed)?;
                    state.unlimited = true;
                    state.managed = managed;
                    state.expires = started + Duration::from_millis(valid_ms.min(BUDGET_TTL_MS));
                }
                UserBudgetGrant::Suspended { valid_ms } => {
                    remember(service, source, true)?;
                    state.suspended = true;
                    state.expires = started + Duration::from_millis(valid_ms.min(BUDGET_TTL_MS));
                }
                UserBudgetGrant::Limited {
                    bytes,
                    rate_bps,
                    valid_ms,
                } => {
                    if bytes == 0
                        || bytes > MAX_BUDGET_BYTES
                        || !(1000..=1_000_000_000_000).contains(&rate_bps)
                        || valid_ms > BUDGET_TTL_MS
                    {
                        return Err("invalid global budget grant".into());
                    }
                    remember(service, source, true)?;
                    state.credits = bytes;
                    state.rate = rate_bps;
                    state.expires = started + Duration::from_millis(valid_ms);
                    state.next = Instant::now();
                }
                UserBudgetGrant::Retry { after_ms } => {
                    drop(state);
                    let until = Instant::now() + Duration::from_millis(after_ms.clamp(1, 1000));
                    while Instant::now() < until {
                        progress(stream, &mut last_progress)?;
                        thread::sleep(Duration::from_millis(10));
                    }
                    continue;
                }
            }
            continue;
        }
        let amount = bytes.min(state.credits).min((state.rate / 800).max(1));
        let now = Instant::now();
        if state.next > now {
            drop(state);
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        state.credits -= amount;
        state.next = now + Duration::from_secs_f64(amount as f64 * 8.0 / state.rate as f64);
        bytes -= amount;
    }
    Ok(Admission::Allowed)
}
fn fetch(service: &Service, source: &str) -> Result<UserBudgetGrant, String> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "user budget entropy")?;
    let request = UserBudgetRequest {
        source: source.into(),
        request_id: random.iter().map(|b| format!("{b:02x}")).collect(),
        bytes: MAX_BUDGET_BYTES,
    };
    let body = serde_json::to_string(&request).map_err(|_| "user budget encoding")?;
    let response = HttpClient::new(&service.options.server)?.request_timeout(
        "POST",
        "/agent/v1/user-traffic/budget",
        &service.options.token,
        Some(&body),
        Duration::from_secs(2),
    )?;
    if response.status == 404 {
        let known = service
            .known
            .lock()
            .map_err(|_| "user budget checkpoint lock")?;
        if service.unknown_checkpoint.load(Ordering::Acquire)
            || known.contains(source)
            || required_at_start(&service.options.state_dir)
        {
            return Err("old Console cannot release a previously limited user".into());
        }
        return Ok(UserBudgetGrant::Unlimited {
            valid_ms: BUDGET_TTL_MS,
            managed: false,
        });
    }
    if response.status != 200 {
        return Err(format!("user budget returned HTTP {}", response.status));
    }
    serde_json::from_str(&response.body).map_err(|_| "invalid user budget response".into())
}

pub(crate) fn spawn(options: Options) -> Result<(), String> {
    if let Err(error) = initialize(&options) {
        eprintln!("user traffic initialization: {error}");
    }
    let policy_options = options.clone();
    thread::Builder::new()
        .name("user-traffic-policies".into())
        .spawn(move || loop {
            if let Some(service) = SERVICE.get() {
                if let Err(error) = synchronize(service) {
                    eprintln!("user traffic policies: {error}");
                }
            } else {
                let _ = initialize(&policy_options);
            }
            thread::sleep(Duration::from_millis(1000));
        })
        .map_err(|_| "user traffic policy worker")?;
    thread::Builder::new()
        .name("user-traffic-support".into())
        .spawn(move || loop {
            if let Err(error) = initialize(&options) {
                eprintln!("user traffic initialization: {error}");
            }
            let ready = supported(&options.state_dir).unwrap_or(false);
            let report = if ready { "true" } else { "false" };
            if let Ok(client) = HttpClient::new(&options.server) {
                if let Ok(response) = client.request_timeout(
                    "POST",
                    "/agent/v1/user-traffic/support",
                    &options.token,
                    Some(report),
                    Duration::from_secs(2),
                ) {
                    if response.status == 200 {
                        if let Ok(required) = serde_json::from_str::<bool>(&response.body) {
                            if let Err(error) = crate::fsutil::atomic_write_private(
                                &required_path(&options.state_dir),
                                if required { b"true" } else { b"false" },
                            ) {
                                eprintln!("user traffic requirement checkpoint: {error}");
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(15));
        })
        .map_err(|_| "user traffic support worker")?;
    Ok(())
}
fn supported(dir: &Path) -> Result<bool, String> {
    let config = std::fs::read_to_string(dir.join("xray.json")).map_err(|_| "Xray not ready")?;
    let api = crate::xray_api_address(&config).ok_or("Xray API not ready")?;
    let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(2)));
    if !crate::xray_grpc::query_stats(api, "brocade>>>user_limit>>>supported")?
        .iter()
        .any(|s| s.name == "brocade>>>user_limit>>>supported" && s.value == 1)
    {
        return Ok(false);
    }
    if let Some(runtime) = crate::xrayupdate::read_upgrade_runtime(dir)? {
        let expected =
            control_path(&std::fs::canonicalize(dir).map_err(|_| "state directory unavailable")?);
        for process in std::iter::once(&runtime.old)
            .chain(runtime.candidate.as_ref())
            .filter(|p| p.alive)
        {
            if !crate::traffic_limit::generation_supports_capability(
                process,
                "user-traffic-limit-v1",
            )? {
                return Ok(false);
            }
            let identity = || {
                crate::process_ref(process.pid)
                    .is_some_and(|p| p.start_ticks == process.pid_start_ticks)
            };
            if !identity() {
                return Ok(false);
            }
            let environment = crate::fsutil::read_bounded(
                &PathBuf::from(format!("/proc/{}/environ", process.pid)),
                64 * 1024,
            )?
            .ok_or("generation environment unavailable")?;
            if !identity()
                || !environment.split(|b| *b == 0).any(|e| {
                    e.strip_prefix(b"XRAY_BROCADE_USER_LIMIT_FILE=")
                        .is_some_and(|p| p == expected.as_os_str().as_encoded_bytes())
                })
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
