//! One bounded FIFO and one in-flight reservation per Console-derived user/app pool.
//! HTTP never holds the admission lock; every grant retains its own original expiry.
use super::*;
#[cfg(test)]
use std::sync::mpsc as blocking;
use std::{collections::VecDeque, sync::Condvar};
use tokio::sync::mpsc::{self, error::TrySendError, Receiver};

const TARGET_WINDOW: Duration = Duration::from_millis(500);
const MIN_PREFETCH: u64 = 8192;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_budget_adaptive_batches_and_low_water_prefetch() {
        let now = Instant::now();
        let mut state = State::new(100_000_000);
        state.retry_at = now;
        state.waiters.push_back(Waiter {
            ticket: 1,
            bytes: 16,
        });
        assert_eq!(state.target(), MIN_PREFETCH);
        state.observed_bytes_per_second = 32_000;
        assert_eq!(state.target(), 16_000);
        state.waiters.front_mut().unwrap().bytes = 16384;
        assert_eq!(state.target(), 6_250_000);
        state.rate = 1000;
        assert_eq!(state.target(), 62);
        state.rate = 100_000_000;
        state.observed_bytes_per_second = 0;
        state.waiters.front_mut().unwrap().bytes = 16;
        state.leases.push_back(Lease {
            bytes: MIN_PREFETCH,
            expires: now + Duration::from_secs(1),
        });
        assert!(!state.needs_fetch(now));
        state.consumed(MIN_PREFETCH / 2, now);
        assert!(state.needs_fetch(now));
        state.fetching = true;
        assert!(!state.needs_fetch(now));
    }
    #[test]
    fn user_budget_prefetch_keeps_each_lease_original_expiry() {
        let now = Instant::now();
        let mut state = State::new(100_000_000);
        state.leases.push_back(Lease {
            bytes: 100,
            expires: now,
        });
        state.leases.push_back(Lease {
            bytes: 200,
            expires: now + Duration::from_secs(1),
        });
        state.expire(now);
        assert_eq!(state.credits(), 200);
        state.consumed(16, now);
        assert_eq!(state.credits(), 184);
        state.expire(now + Duration::from_secs(1));
        assert_eq!(state.credits(), 0);
    }

    #[test]
    fn queue_delay_advances_refill_without_extending_the_grant() {
        let service = super::super::tests::test_service(
            &std::env::temp_dir(),
            "http://127.0.0.1:9",
            0,
            "127.0.0.1:9".parse().unwrap(),
        );
        let source = "queue@tenant#entry";
        service.known.lock().unwrap().insert(source.into());
        let pool = source_entry(&service, source).unwrap();
        let started = Instant::now();
        complete(
            FetchJob {
                service,
                pool: Arc::clone(&pool),
                generation: 0,
                enqueued: started - Duration::from_millis(200),
                request: UserBudgetRequest {
                    source: source.into(),
                    request_id: "a".repeat(32),
                    bytes: 40_000,
                },
            },
            started,
            Ok(UserBudgetGrant::Limited {
                bytes: 40_000,
                rate_bps: 100_000_000,
                valid_ms: 2000,
            }),
        );
        let mut state = pool.state.lock().unwrap();
        assert!(state.roundtrip >= Duration::from_millis(200));
        assert_eq!(state.leases[0].expires, started + Duration::from_secs(2));
        state.observed_bytes_per_second = 100_000;
        state.waiters.push_back(Waiter {
            ticket: 1,
            bytes: 16,
        });
        assert!(state.needs_fetch(Instant::now()));
        state.roundtrip = Duration::from_millis(50);
        assert!(!state.needs_fetch(Instant::now()));
    }

    #[test]
    fn independent_refills_start_before_any_slow_response_finishes() {
        crate::identity::self_identity();
        const USERS: usize = 32;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let service = super::super::tests::test_service(
            &std::env::temp_dir(),
            &format!("http://{address}"),
            0,
            address,
        );
        let (seen_tx, seen_rx) = blocking::channel();
        let (finish_tx, finish_rx) = blocking::channel();
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            let mut releases = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(2);
            for _ in 0..USERS {
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("concurrent budget accept failed: {error}"),
                    }
                };
                let seen = seen_tx.clone();
                let (release, wait) = blocking::channel();
                releases.push(release);
                workers.push(thread::spawn(move || {
                    let mut stream = stream;
                    let (_, body) = crate::http::tests::read_request(&mut stream);
                    let request: UserBudgetRequest = serde_json::from_slice(&body).unwrap();
                    seen.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(2)).unwrap();
                    let body = serde_json::to_vec(&UserBudgetGrant::Limited {
                        bytes: request.bytes,
                        rate_bps: 100_000_000,
                        valid_ms: 2000,
                    })
                    .unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                }));
            }
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            for release in releases {
                release.send(()).unwrap();
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let mut pools = Vec::new();
        for i in 0..USERS {
            let source = format!("parallel{i}@tenant#entry");
            service.known.lock().unwrap().insert(source.clone());
            let pool = source_entry(&service, &source).unwrap();
            {
                let mut state = pool.state.lock().unwrap();
                state.waiters.push_back(Waiter {
                    ticket: 1,
                    bytes: 16,
                });
                schedule(&service, &pool, &mut state, &source).unwrap();
            }
            pools.push(pool);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        for _ in 0..USERS {
            seen_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("slow responses must not prevent independent HTTP requests from starting");
        }
        finish_tx.send(()).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        for pool in pools {
            let mut state = pool.state.lock().unwrap();
            while state.fetching {
                assert!(Instant::now() < deadline);
                state = pool
                    .changed
                    .wait_timeout(state, Duration::from_millis(10))
                    .unwrap()
                    .0;
            }
            assert_eq!(state.credits(), MIN_PREFETCH);
        }
    }

    fn delayed_budget(
        listener: TcpListener,
        seen: blocking::Sender<()>,
        release: blocking::Receiver<()>,
    ) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut size = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                size = n.trim().parse::<usize>().unwrap();
            }
        }
        let mut body = vec![0; size];
        reader.read_exact(&mut body).unwrap();
        let request: UserBudgetRequest = serde_json::from_slice(&body).unwrap();
        seen.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(2)).unwrap();
        let grant = UserBudgetGrant::Limited {
            bytes: request.bytes,
            rate_bps: 100_000_000,
            valid_ms: 2000,
        };
        let body = serde_json::to_vec(&grant).unwrap();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
        stream.write_all(&body).unwrap();
    }

    #[test]
    fn user_budget_slow_prefetch_does_not_block_valid_credits_or_extend_their_expiry() {
        crate::identity::self_identity();
        let dir = std::env::temp_dir().join(format!("brocade-prefetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let service =
            super::super::tests::test_service(&dir, &format!("http://{address}"), 0, address);
        let source = "alice@tenant#one";
        let pool = source_entry(&service, source).unwrap();
        let expires = Instant::now() + Duration::from_secs(1);
        {
            let mut state = pool.state.lock().unwrap();
            state.rate = 100_000_000;
            state.leases.push_back(Lease {
                bytes: 8192,
                expires,
            });
        }
        let (seen_tx, seen_rx) = blocking::channel();
        let (release_tx, release_rx) = blocking::channel();
        let server = thread::spawn(move || delayed_budget(listener, seen_tx, release_rx));
        let pair = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(pair.local_addr().unwrap()).unwrap();
        let (mut stream, _) = pair.accept().unwrap();
        assert!(matches!(
            admit(&service, source, 4096, &pool, &mut stream).unwrap(),
            Admission::Allowed
        ));
        seen_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert!(matches!(
            admit(&service, source, 16, &pool, &mut stream).unwrap(),
            Admission::Allowed
        ));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "inflight HTTP must not hold admission lock"
        );
        assert!(pool.state.lock().unwrap().fetching);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while pool.state.lock().unwrap().fetching {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let state = pool.state.lock().unwrap();
        assert_eq!(state.leases.len(), 2);
        assert_eq!(state.leases.front().unwrap().expires, expires);
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_budget_policy_change_discards_inflight_grant() {
        crate::identity::self_identity();
        let dir = std::env::temp_dir().join(format!(
            "brocade-prefetch-generation-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let service =
            super::super::tests::test_service(&dir, &format!("http://{address}"), 0, address);
        let pool = source_entry(&service, "alice@tenant#one").unwrap();
        let (seen_tx, seen_rx) = blocking::channel();
        let (release_tx, release_rx) = blocking::channel();
        let server = thread::spawn(move || delayed_budget(listener, seen_tx, release_rx));
        {
            let mut state = pool.state.lock().unwrap();
            state.waiters.push_back(Waiter {
                ticket: 1,
                bytes: 16,
            });
            schedule(&service, &pool, &mut state, "alice@tenant#one").unwrap();
        }
        seen_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        service.generation.fetch_add(1, Ordering::AcqRel);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(pool.state.lock().unwrap().credits(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

struct Lease {
    bytes: u64,
    expires: Instant,
}
struct Waiter {
    ticket: u64,
    bytes: u64,
}
#[derive(Clone)]
struct Reservation {
    request: UserBudgetRequest,
    created: Instant,
}
struct State {
    leases: VecDeque<Lease>,
    waiters: VecDeque<Waiter>,
    ticket: u64,
    generation: u64,
    rate: u64,
    next: Instant,
    last: Instant,
    mode: Option<(Admission, Instant)>,
    fetching: bool,
    retry_at: Instant,
    reservation: Option<Reservation>,
    error: Option<String>,
    window_started: Instant,
    used: u64,
    observed_bytes_per_second: u64,
    roundtrip: Duration,
}
impl State {
    fn new(rate: u64) -> Self {
        let now = Instant::now();
        Self {
            leases: VecDeque::new(),
            waiters: VecDeque::new(),
            ticket: 0,
            generation: 0,
            rate,
            next: now,
            last: now,
            mode: None,
            fetching: false,
            retry_at: now,
            reservation: None,
            error: None,
            window_started: now,
            used: 0,
            observed_bytes_per_second: 0,
            roundtrip: Duration::from_millis(50),
        }
    }
    fn expire(&mut self, now: Instant) {
        self.leases.retain(|l| l.bytes > 0 && l.expires > now);
        if self.mode.is_some_and(|(_, until)| until <= now) {
            self.mode = None;
        }
    }
    fn credits(&self) -> u64 {
        self.leases.iter().map(|l| l.bytes).sum()
    }
    fn pending(&self) -> u64 {
        self.waiters.iter().map(|w| w.bytes).sum()
    }
    fn target(&self) -> u64 {
        // A newly backlogged stream can batch immediately. Tiny messages instead grow
        // the batch from measured consumption, without reserving a whole second.
        let ceiling = if self.rate == 0 {
            MIN_PREFETCH
        } else {
            (self.rate / 16).max(1)
        };
        let demand = if self.pending() >= 4096 && self.rate != 0 {
            ceiling
        } else {
            self.observed_bytes_per_second / 2
        };
        demand
            .max(MIN_PREFETCH)
            .max(self.pending().min(16384))
            .min(ceiling)
            .clamp(1, MAX_BUDGET_BYTES)
    }
    fn needs_fetch(&self, now: Instant) -> bool {
        if self.fetching || self.waiters.is_empty() || self.leases.len() >= 2 || now < self.retry_at
        {
            return false;
        }
        let margin = (self.roundtrip * 2).clamp(Duration::from_millis(100), TARGET_WINDOW);
        let expected_use = (u128::from(self.observed_bytes_per_second) * margin.as_nanos()
            / 1_000_000_000)
            .min(u128::from(MAX_BUDGET_BYTES)) as u64;
        let low_water = (self.target() / 2).max(expected_use).min(self.target());
        self.credits() <= low_water
            || self
                .leases
                .front()
                .is_some_and(|l| l.expires <= now + margin)
    }
    fn consumed(&mut self, amount: u64, now: Instant) {
        self.used += amount;
        let elapsed = now.saturating_duration_since(self.window_started);
        if elapsed >= Duration::from_millis(100) {
            let sample = (u128::from(self.used) * 1_000_000_000 / elapsed.as_nanos()) as u64;
            self.observed_bytes_per_second = sample.max(self.observed_bytes_per_second / 2);
            self.window_started = now;
            self.used = 0;
        }
        let mut remaining = amount;
        for lease in &mut self.leases {
            let part = remaining.min(lease.bytes);
            lease.bytes -= part;
            remaining -= part;
            if remaining == 0 {
                break;
            }
        }
    }
}

pub(super) struct Pool {
    key: String,
    state: Mutex<State>,
    changed: Condvar,
}
impl Pool {
    fn new(key: String, rate: u64) -> Self {
        Self {
            key,
            state: Mutex::new(State::new(rate)),
            changed: Condvar::new(),
        }
    }
}

fn identity(service: &Service, source: &str) -> Result<(String, u64), String> {
    let policy = service
        .policies
        .lock()
        .map_err(|_| "user policy cache lock")?;
    let fresh = policy
        .updated
        .is_some_and(|at| at.elapsed() < Duration::from_millis(BUDGET_TTL_MS));
    let key = if fresh {
        policy.groups.get(source)
    } else {
        None
    };
    let rate = match policy.effects.get(source) {
        Some(UserTrafficEffect::Limited { rate_bps }) => *rate_bps,
        _ => 0,
    };
    Ok((
        key.map_or_else(|| format!("source:{source}"), |k| format!("scope:{k}")),
        rate,
    ))
}

pub(super) fn source_entry(service: &Service, source: &str) -> Result<Arc<Pool>, String> {
    let (key, rate) = identity(service, source)?;
    let mut entries = service
        .entries
        .lock()
        .map_err(|_| "user budget cache lock")?;
    if let Some(pool) = entries.get(&key) {
        return Ok(Arc::clone(pool));
    }
    entries.retain(|_, pool| {
        Arc::strong_count(pool) > 1
            || pool
                .state
                .lock()
                .is_ok_and(|s| s.last.elapsed() < Duration::from_secs(10))
    });
    if entries.len() >= MAX_SOURCES {
        return Err("user budget cache capacity reached".into());
    }
    let pool = Arc::new(Pool::new(key.clone(), rate));
    entries.insert(key, Arc::clone(&pool));
    Ok(pool)
}

pub(super) struct FetchJob {
    service: Arc<Service>,
    pool: Arc<Pool>,
    generation: u64,
    request: UserBudgetRequest,
    enqueued: Instant,
}

pub(super) type FetchSender = mpsc::Sender<FetchJob>;

pub(super) fn start_fetchers() -> Result<FetchSender, String> {
    let client = crate::http::asynchronous::control_client(MAX_CLIENTS)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| "user budget runtime unavailable")?;
    let (sender, receiver) = mpsc::channel(MAX_CLIENTS);
    thread::Builder::new()
        .name("user-budget-fetch".into())
        .spawn(move || runtime.block_on(fetch_loop(receiver, client)))
        .map_err(|_| "user budget fetch worker unavailable")?;
    Ok(sender)
}

async fn fetch_loop(mut receiver: Receiver<FetchJob>, client: reqwest::Client) {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            Some(job) = receiver.recv(), if tasks.len() < MAX_CLIENTS => {
                tasks.spawn(fetch_one(job, client.clone()));
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if !matches!(result, Ok(Ok(()))) {
                    eprintln!("user budget completion worker failed");
                }
            }
            else => break,
        }
    }
}

async fn fetch_one(job: FetchJob, client: reqwest::Client) -> Result<(), String> {
    let started = Instant::now();
    let result = if job.service.generation.load(Ordering::Acquire) != job.generation {
        Err("user budget policy changed".into())
    } else {
        fetch(
            &job.service,
            &job.request,
            &client,
            job.enqueued + Duration::from_millis(BUDGET_TTL_MS),
        )
        .await
    };
    // Rare checkpoint writes must not block the socket reactor. This pool is also bounded.
    tokio::task::spawn_blocking(move || complete(job, started, result))
        .await
        .map_err(|_| "user budget completion worker failed".to_owned())
}

fn complete(job: FetchJob, started: Instant, result: Result<UserBudgetGrant, String>) {
    let result = if job.service.generation.load(Ordering::Acquire) != job.generation {
        Err("user budget policy changed".into())
    } else {
        result.and_then(|grant| {
            if !matches!(grant, UserBudgetGrant::Retry { .. }) {
                remember(
                    &job.service,
                    &job.request.source,
                    !matches!(grant, UserBudgetGrant::Unlimited { managed: false, .. }),
                )?;
            }
            Ok(grant)
        })
    };
    let Ok(mut state) = job.pool.state.lock() else {
        return;
    };
    if state.generation != job.generation
        || job.service.generation.load(Ordering::Acquire) != job.generation
    {
        job.pool.changed.notify_all();
        return;
    }
    state.fetching = false;
    let elapsed = job.enqueued.elapsed();
    // React immediately to growing queue delay, and decay conservatively after recovery.
    state.roundtrip = elapsed.max((state.roundtrip + elapsed) / 2);
    match result {
        Ok(UserBudgetGrant::Limited {
            bytes,
            rate_bps,
            valid_ms,
        }) => {
            if bytes == 0
                || bytes > job.request.bytes
                || bytes > MAX_BUDGET_BYTES
                || !(1000..=1_000_000_000_000).contains(&rate_bps)
                || valid_ms == 0
                || valid_ms > BUDGET_TTL_MS
            {
                state.error = Some("invalid global budget grant".into());
                state.retry_at = Instant::now() + Duration::from_millis(250);
            } else {
                state.rate = rate_bps;
                state.leases.push_back(Lease {
                    bytes,
                    expires: started + Duration::from_millis(valid_ms),
                });
                state.reservation = None;
                state.error = None;
            }
        }
        Ok(UserBudgetGrant::Retry { after_ms }) => {
            state.retry_at = Instant::now() + Duration::from_millis(after_ms.clamp(1, 1000));
        }
        Ok(UserBudgetGrant::Unlimited { valid_ms, managed }) => {
            state.leases.clear();
            state.mode = Some((
                if managed {
                    Admission::Open
                } else {
                    Admission::Pass
                },
                started + Duration::from_millis(valid_ms.min(BUDGET_TTL_MS)),
            ));
            state.reservation = None;
        }
        Ok(UserBudgetGrant::Suspended { valid_ms }) => {
            state.leases.clear();
            state.mode = Some((
                Admission::Suspended,
                started + Duration::from_millis(valid_ms.min(BUDGET_TTL_MS)),
            ));
            state.reservation = None;
        }
        Err(error) => {
            state.error = Some(error);
            state.retry_at = Instant::now() + Duration::from_millis(250);
        }
    }
    job.pool.changed.notify_all();
}

fn schedule(
    service: &Arc<Service>,
    pool: &Arc<Pool>,
    state: &mut State,
    source: &str,
) -> Result<(), String> {
    let now = Instant::now();
    if !state.needs_fetch(now) {
        return Ok(());
    }
    if state
        .reservation
        .as_ref()
        .is_none_or(|r| r.created.elapsed() >= Duration::from_millis(BUDGET_TTL_MS))
    {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "user budget entropy")?;
        state.reservation = Some(Reservation {
            request: UserBudgetRequest {
                source: source.into(),
                request_id: random.iter().map(|b| format!("{b:02x}")).collect(),
                bytes: state.target(),
            },
            created: now,
        });
    }
    let request = state
        .reservation
        .as_ref()
        .ok_or("user budget reservation missing")?
        .request
        .clone();
    let job = FetchJob {
        service: Arc::clone(service),
        pool: Arc::clone(pool),
        generation: state.generation,
        request,
        enqueued: now,
    };
    match service.fetcher.try_send(job) {
        Ok(()) => {
            state.fetching = true;
            state.error = None;
        }
        Err(TrySendError::Full(_)) => {
            state.retry_at = now + Duration::from_millis(5);
        }
        Err(TrySendError::Closed(_)) => return Err("user budget workers unavailable".into()),
    }
    Ok(())
}

struct Waiting {
    pool: Arc<Pool>,
    ticket: u64,
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.state.lock() {
            state.waiters.retain(|w| w.ticket != self.ticket);
            self.pool.changed.notify_all();
        }
    }
}

pub(super) fn admit(
    service: &Arc<Service>,
    source: &str,
    bytes: u64,
    pool: &Arc<Pool>,
    stream: &mut TcpStream,
) -> Result<Admission, String> {
    let ticket = {
        let mut state = pool.state.lock().map_err(|_| "user budget entry lock")?;
        if state.waiters.len() >= MAX_CLIENTS {
            return Err("user budget waiter capacity reached".into());
        }
        state.ticket = state
            .ticket
            .checked_add(1)
            .ok_or("user budget ticket exhausted")?;
        let ticket = state.ticket;
        state.waiters.push_back(Waiter { ticket, bytes });
        ticket
    };
    let _waiting = Waiting {
        pool: Arc::clone(pool),
        ticket,
    };
    let mut remaining = bytes;
    let mut last_progress = Instant::now();
    loop {
        progress(stream, &mut last_progress)?;
        if let Some(result) = assigned(service, source)? {
            return Ok(result);
        }
        let (key, hint) = identity(service, source)?;
        if key != pool.key {
            return Err("user budget membership changed or expired".into());
        }
        let mut state = pool.state.lock().map_err(|_| "user budget entry lock")?;
        let now = Instant::now();
        state.last = now;
        let generation = service.generation.load(Ordering::Acquire);
        if state.generation != generation {
            state.leases.clear();
            state.mode = None;
            state.fetching = false;
            state.reservation = None;
            state.error = None;
            state.retry_at = now;
            state.rate = hint;
            state.next = now;
            state.generation = generation;
        }
        state.expire(now);
        if let Some((mode, _)) = state.mode {
            return Ok(mode);
        }
        schedule(service, pool, &mut state, source)?;
        if state.credits() == 0 {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
        } else if state.waiters.front().is_some_and(|w| w.ticket == ticket) && state.next <= now {
            let amount = remaining
                .min(state.credits())
                .min((state.rate / 800).max(1));
            state.consumed(amount, now);
            state.next = now + Duration::from_secs_f64(amount as f64 * 8.0 / state.rate as f64);
            remaining -= amount;
            if let Some(waiter) = state.waiters.front_mut() {
                waiter.bytes = remaining;
            }
            // Prefetch while the current FIFO ticket still expresses demand. No HTTP runs here.
            schedule(service, pool, &mut state, source)?;
            if remaining == 0 {
                return Ok(Admission::Allowed);
            }
            if let Some(waiter) = state.waiters.pop_front() {
                state.waiters.push_back(waiter);
            }
            pool.changed.notify_all();
        }
        let mut wait = Duration::from_millis(50);
        if state.next > now {
            wait = wait.min(state.next.duration_since(now));
        }
        if state.retry_at > now {
            wait = wait.min(state.retry_at.duration_since(now));
        }
        let _ = pool
            .changed
            .wait_timeout(state, wait)
            .map_err(|_| "user budget wait lock")?;
    }
}
