use super::{HealthCache, MuxCache, DIAGNOSTICS_REFRESH};
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Demand {
    enabled: bool,
    stopped: bool,
    generation: u64,
}

pub(super) struct Worker {
    demand: Arc<(Mutex<Demand>, Condvar)>,
    pub(super) health: HealthCache,
    pub(super) mux: MuxCache,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub(super) fn start(state_dir: PathBuf) -> Result<Self, String> {
        Self::with_sampler(DIAGNOSTICS_REFRESH, move || {
            // Optional diagnostics must not hold a stale result over a subscription change or
            // spend two independent ten-second retry budgets on one snapshot.
            let _deadline = crate::xray_grpc::deadline_after(Some(Duration::from_secs(2)));
            let address = std::fs::read_to_string(state_dir.join("xray.json"))
                .ok()
                .and_then(|content| crate::xray_api_address(&content));
            (
                address.and_then(|api| crate::xray_grpc::reverse_health(api).ok()),
                address.and_then(|api| crate::xray_grpc::mux_snapshot(api).ok()),
            )
        })
    }

    fn with_sampler(
        interval: Duration,
        mut sample: impl FnMut() -> (
                Option<brocade_deployment::protocol::ReverseHealthReport>,
                Option<brocade_deployment::protocol::MuxReport>,
            ) + Send
            + 'static,
    ) -> Result<Self, String> {
        let demand: Arc<(Mutex<Demand>, Condvar)> = Default::default();
        let health: HealthCache = Default::default();
        let mux: MuxCache = Default::default();
        let shared = demand.clone();
        let health_cache = health.clone();
        let mux_cache = mux.clone();
        let thread = thread::Builder::new()
            .name("realtime-diag".into())
            .spawn(move || {
                let (lock, ready) = &*shared;
                loop {
                    let state = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let state = ready
                        .wait_while(state, |state| !state.enabled && !state.stopped)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.stopped {
                        return;
                    }
                    let generation = state.generation;
                    drop(state);
                    let started = Instant::now();
                    let (health, mux) = sample();
                    let state = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.enabled && !state.stopped && state.generation == generation {
                        *health_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = health;
                        *mux_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = mux;
                    }
                    let (state, _) = ready
                        .wait_timeout_while(
                            state,
                            interval.saturating_sub(started.elapsed()),
                            |state| {
                                state.enabled && !state.stopped && state.generation == generation
                            },
                        )
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.stopped {
                        return;
                    }
                }
            })
            .map_err(|error| format!("cannot spawn realtime diagnostics: {error}"))?;
        Ok(Self {
            demand,
            health,
            mux,
            thread: Some(thread),
        })
    }

    fn set_enabled(&self, enabled: bool) {
        let (lock, ready) = &*self.demand;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.enabled != enabled {
            state.enabled = enabled;
            state.generation = state.generation.wrapping_add(1);
            if !enabled {
                *self
                    .health
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                *self
                    .mux
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            ready.notify_one();
        }
    }

    pub(super) fn subscribe(&self) -> Subscription<'_> {
        Subscription(self)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, ready) = &*self.demand;
        lock.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        ready.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) struct Subscription<'a>(&'a Worker);
impl Subscription<'_> {
    pub(super) fn set_enabled(&self, enabled: bool) {
        self.0.set_enabled(enabled);
    }
}
impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        self.0.set_enabled(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    #[test]
    fn diagnostics_sleep_without_demand_and_wake_on_subscription() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (sent, received) = mpsc::channel();
        let worker = Worker::with_sampler(Duration::from_millis(30), move || {
            count.fetch_add(1, Ordering::SeqCst);
            sent.send(()).unwrap();
            (None, None)
        })
        .unwrap();
        assert!(received.recv_timeout(Duration::from_millis(70)).is_err());
        let subscription = worker.subscribe();
        subscription.set_enabled(true);
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(subscription);
        thread::sleep(Duration::from_millis(70));
        let stopped_at = calls.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(70));
        assert_eq!(calls.load(Ordering::SeqCst), stopped_at);
        while received.try_recv().is_ok() {}
        let subscription = worker.subscribe();
        subscription.set_enabled(true);
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(calls.load(Ordering::SeqCst) > stopped_at);
    }
    #[test]
    fn late_diagnostics_are_discarded_after_subscription_release() {
        let (entered, wait_entered) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let worker = Worker::with_sampler(Duration::from_secs(5), move || {
            entered.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(2)).unwrap();
            (
                None,
                Some(brocade_deployment::protocol::MuxReport {
                    boot_id: "test".into(),
                    sequence: 1,
                    sampled_at_unix_ms: 1,
                    pools: Vec::new(),
                    workers: Vec::new(),
                    events: Vec::new(),
                }),
            )
        })
        .unwrap();
        let subscription = worker.subscribe();
        subscription.set_enabled(true);
        wait_entered.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(subscription);
        let mux = worker.mux.clone();
        release.send(()).unwrap();
        drop(worker);
        assert!(mux.lock().unwrap().is_none());
    }
}
