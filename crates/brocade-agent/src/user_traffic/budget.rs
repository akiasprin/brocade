//! Bounded local pacing per authenticated user/ingress, shared across connections and Xray generations.
use super::*;
use std::{collections::VecDeque, sync::Condvar};

pub(super) struct State {
    pub(super) waiters: VecDeque<u64>,
    ticket: u64,
    rate: u64,
    next: Instant,
    last: Instant,
}

pub(super) struct Pool {
    pub(super) state: Mutex<State>,
    changed: Condvar,
}

pub(super) fn source_entry(service: &Service, source: &str) -> Result<Arc<Pool>, String> {
    let mut entries = service
        .entries
        .lock()
        .map_err(|_| "user limiter cache lock")?;
    if let Some(pool) = entries.get(source) {
        return Ok(Arc::clone(pool));
    }
    entries.retain(|_, pool| {
        Arc::strong_count(pool) > 1
            || pool
                .state
                .lock()
                .is_ok_and(|state| state.last.elapsed() < Duration::from_secs(10))
    });
    if entries.len() >= MAX_SOURCES {
        return Err("user limiter cache capacity reached".into());
    }
    let now = Instant::now();
    let pool = Arc::new(Pool {
        state: Mutex::new(State {
            waiters: VecDeque::new(),
            ticket: 0,
            rate: 0,
            next: now,
            last: now,
        }),
        changed: Condvar::new(),
    });
    entries.insert(source.into(), Arc::clone(&pool));
    Ok(pool)
}

struct Waiting {
    pool: Arc<Pool>,
    ticket: u64,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.state.lock() {
            state.waiters.retain(|ticket| *ticket != self.ticket);
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
        let mut state = pool.state.lock().map_err(|_| "user limiter lock")?;
        if state.waiters.len() >= MAX_CLIENTS {
            return Err("user limiter waiter capacity reached".into());
        }
        state.ticket = state
            .ticket
            .checked_add(1)
            .ok_or("user limiter ticket exhausted")?;
        let ticket = state.ticket;
        state.waiters.push_back(ticket);
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
        let generation = service.generation.load(Ordering::Acquire);
        if let Some(result) = assigned(service, source)? {
            return Ok(result);
        }
        let rate = match service
            .policies
            .lock()
            .map_err(|_| "user policy cache lock")?
            .effects
            .get(source)
        {
            Some(UserTrafficEffect::Limited { rate_bps }) => *rate_bps,
            _ => continue,
        };
        let mut state = pool.state.lock().map_err(|_| "user limiter lock")?;
        if generation != service.generation.load(Ordering::Acquire) {
            continue;
        }
        let now = Instant::now();
        state.last = now;
        // Refreshes of other users and unchanged snapshots must not reset this pacing clock.
        if state.rate != rate {
            state.rate = rate;
            state.next = now;
        }
        if state.waiters.front() == Some(&ticket) && state.next <= now {
            // Rotate at most ten milliseconds of traffic, so a bulk stream cannot starve
            // another connection. Idle time never accumulates an unbounded burst allowance.
            let amount = remaining.min((rate / 800).max(1));
            state.next = now + Duration::from_secs_f64(amount as f64 * 8.0 / rate as f64);
            remaining -= amount;
            if remaining == 0 {
                return Ok(Admission::Allowed);
            }
            state.waiters.pop_front();
            state.waiters.push_back(ticket);
            pool.changed.notify_all();
        }
        let wait = if state.next > now {
            state
                .next
                .duration_since(now)
                .min(Duration::from_millis(50))
        } else {
            Duration::from_millis(50)
        };
        let _ = pool
            .changed
            .wait_timeout(state, wait)
            .map_err(|_| "user limiter wait lock")?;
    }
}
