//! Bounded, content-addressed reuse for the pure compiler.
//!
//! A draft preview has several consumers (snapshot, diagnostics and artifact impact), and a slow
//! request can outlive the browser state that started it.  The cache therefore has two separate
//! properties:
//!
//! - only callers for the exact same serialized snapshot share a flight;
//! - the flight belongs to this process, not to the first HTTP request, so cancelling that request
//!   cannot strand the other waiters or leave an entry permanently in flight.
//!
//! Different snapshot keys never wait on one another.  A newer draft can start compiling while an
//! older one finishes in the background; the browser's draft generation decides which result may
//! be displayed.

use std::{
    collections::{HashMap, VecDeque},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex, OnceLock},
};

use brocade_core::{
    compile::{
        compile_prelude, compile_routing_app_with, finish_app_compile, finish_compile,
        CompileOutput, FinalAppCompile, PublishBlocked, RoutingAppCompile,
    },
    hash::sha256_hex,
    ir::routing::{RoutingChainCompile, RoutingChainInput, Step},
    model::ModelSnapshot,
    physical::{
        node::{self, NodePlan},
        probe::{self, ProbePlan},
        user::{self, UserPlan},
    },
};
use serde::Serialize;
use tokio::sync::Notify;

use crate::{Result, StoreError};

const COMPILE_CACHE_CAPACITY: usize = 16;
const ROUTING_APP_CACHE_CAPACITY: usize = 64;
const FINAL_APP_CACHE_CAPACITY: usize = 64;
const ROUTING_CHAIN_CACHE_CAPACITY: usize = 256;
const NODE_PROJECTION_CACHE_CAPACITY: usize = 128;
const USER_PROJECTION_CACHE_CAPACITY: usize = 256;
const PROBE_PROJECTION_CACHE_CAPACITY: usize = 128;
const INCREMENTAL_CACHE_VERSION: u8 = 1;
type SharedCompileResult = std::result::Result<Arc<CompileOutput>, String>;
type SharedMemoResult<T> = std::result::Result<Arc<T>, Arc<str>>;

struct MemoFlight<T> {
    result: Mutex<Option<SharedMemoResult<T>>>,
    ready: Condvar,
}

impl<T> MemoFlight<T> {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn wait(&self) -> Arc<T> {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(result) = result.as_ref() {
                return match result {
                    Ok(value) => Arc::clone(value),
                    Err(message) => panic!("{message}"),
                };
            }
            result = self
                .ready
                .wait(result)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn finish(&self, result: SharedMemoResult<T>) {
        *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.ready.notify_all();
    }
}

struct MemoState<T> {
    ready: HashMap<String, Arc<T>>,
    lru: VecDeque<String>,
    flights: HashMap<String, Arc<MemoFlight<T>>>,
}

impl<T> Default for MemoState<T> {
    fn default() -> Self {
        Self {
            ready: HashMap::new(),
            lru: VecDeque::new(),
            flights: HashMap::new(),
        }
    }
}

/// Synchronous stage memo used only from the compiler's blocking worker.  Locks protect indexes,
/// never compilation: different content keys build concurrently, while identical keys share one
/// immutable result.
pub(crate) struct StageMemo<T> {
    capacity: usize,
    state: Mutex<MemoState<T>>,
}

enum MemoLookup<T> {
    Ready(Arc<T>),
    Wait(Arc<MemoFlight<T>>),
    Lead(Arc<MemoFlight<T>>),
}

impl<T> StageMemo<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "stage cache capacity must be positive");
        Self {
            capacity,
            state: Mutex::new(MemoState::default()),
        }
    }

    pub(crate) fn get_or_insert_with<F>(&self, key: String, build: F) -> Arc<T>
    where
        F: FnOnce() -> T,
    {
        let lookup = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(value) = state.ready.get(&key).cloned() {
                if let Some(index) = state.lru.iter().position(|entry| entry == &key) {
                    state.lru.remove(index);
                }
                state.lru.push_back(key.clone());
                MemoLookup::Ready(value)
            } else if let Some(flight) = state.flights.get(&key) {
                MemoLookup::Wait(Arc::clone(flight))
            } else {
                let flight = Arc::new(MemoFlight::new());
                state.flights.insert(key.clone(), Arc::clone(&flight));
                MemoLookup::Lead(flight)
            }
        };

        match lookup {
            MemoLookup::Ready(value) => value,
            MemoLookup::Wait(flight) => flight.wait(),
            MemoLookup::Lead(flight) => match catch_unwind(AssertUnwindSafe(build)) {
                Ok(value) => {
                    let value = Arc::new(value);
                    {
                        let mut state = self
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.flights.remove(&key);
                        state.ready.insert(key.clone(), Arc::clone(&value));
                        state.lru.push_back(key);
                        while state.ready.len() > self.capacity {
                            if let Some(oldest) = state.lru.pop_front() {
                                state.ready.remove(&oldest);
                            }
                        }
                    }
                    flight.finish(Ok(Arc::clone(&value)));
                    value
                }
                Err(payload) => {
                    self.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .flights
                        .remove(&key);
                    flight.finish(Err(Arc::from("stage cache builder panicked")));
                    resume_unwind(payload)
                }
            },
        }
    }
}

fn routing_apps() -> &'static StageMemo<RoutingAppCompile> {
    static CACHE: OnceLock<StageMemo<RoutingAppCompile>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(ROUTING_APP_CACHE_CAPACITY))
}

fn final_apps() -> &'static StageMemo<FinalAppCompile> {
    static CACHE: OnceLock<StageMemo<FinalAppCompile>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(FINAL_APP_CACHE_CAPACITY))
}

fn routing_chains() -> &'static StageMemo<RoutingChainCompile> {
    static CACHE: OnceLock<StageMemo<RoutingChainCompile>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(ROUTING_CHAIN_CACHE_CAPACITY))
}

fn node_projections() -> &'static StageMemo<NodePlan> {
    static CACHE: OnceLock<StageMemo<NodePlan>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(NODE_PROJECTION_CACHE_CAPACITY))
}

fn user_projections() -> &'static StageMemo<UserPlan> {
    static CACHE: OnceLock<StageMemo<UserPlan>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(USER_PROJECTION_CACHE_CAPACITY))
}

fn probe_projections() -> &'static StageMemo<ProbePlan> {
    static CACHE: OnceLock<StageMemo<ProbePlan>> = OnceLock::new();
    CACHE.get_or_init(|| StageMemo::new(PROBE_PROJECTION_CACHE_CAPACITY))
}

fn digest(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value)
        .expect("compiler cache keys contain only infallibly serializable model values");
    sha256_hex(&bytes)
}

struct CompileFlight {
    result: Mutex<Option<SharedCompileResult>>,
    notify: Notify,
}

impl CompileFlight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    async fn wait(&self) -> SharedCompileResult {
        loop {
            // Create the notification future before checking the result a second time.  A leader
            // completing between those operations then either leaves a stored result or wakes
            // this already-registered waiter; no completion can be missed.
            let notified = self.notify.notified();
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }

    fn finish(&self, result: SharedCompileResult) {
        *self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.notify.notify_waiters();
    }
}

#[derive(Default)]
struct CompileCacheState {
    ready: HashMap<String, Arc<CompileOutput>>,
    lru: VecDeque<String>,
    flights: HashMap<String, Arc<CompileFlight>>,
}

struct CompileCache {
    capacity: usize,
    state: Mutex<CompileCacheState>,
}

enum CacheLookup {
    Ready(Arc<CompileOutput>),
    Wait(Arc<CompileFlight>),
    Lead(Arc<CompileFlight>),
}

impl CompileCache {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "compile cache capacity must be positive");
        Self {
            capacity,
            state: Mutex::new(CompileCacheState::default()),
        }
    }

    fn lookup(&self, key: &str) -> CacheLookup {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(output) = state.ready.get(key).cloned() {
            if let Some(index) = state.lru.iter().position(|entry| entry == key) {
                state.lru.remove(index);
            }
            state.lru.push_back(key.to_owned());
            return CacheLookup::Ready(output);
        }
        if let Some(flight) = state.flights.get(key) {
            return CacheLookup::Wait(Arc::clone(flight));
        }
        let flight = Arc::new(CompileFlight::new());
        state.flights.insert(key.to_owned(), Arc::clone(&flight));
        CacheLookup::Lead(flight)
    }

    fn complete(&self, key: String, flight: &Arc<CompileFlight>, result: SharedCompileResult) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.flights.remove(&key);
            if let Ok(output) = &result {
                state.ready.insert(key.clone(), Arc::clone(output));
                if let Some(index) = state.lru.iter().position(|entry| entry == &key) {
                    state.lru.remove(index);
                }
                state.lru.push_back(key);
                while state.ready.len() > self.capacity {
                    let Some(oldest) = state.lru.pop_front() else {
                        break;
                    };
                    state.ready.remove(&oldest);
                }
            }
        }
        flight.finish(result);
    }

    async fn get_or_compile<F>(self: &Arc<Self>, key: String, compile: F) -> SharedCompileResult
    where
        F: FnOnce() -> Arc<CompileOutput> + Send + 'static,
    {
        let flight = match self.lookup(&key) {
            CacheLookup::Ready(output) => return Ok(output),
            CacheLookup::Wait(flight) => flight,
            CacheLookup::Lead(flight) => {
                let cache = Arc::clone(self);
                let owned_key = key;
                let owned_flight = Arc::clone(&flight);
                // The process owns the flight.  If the browser cancels the first request, this
                // task still completes, fills the bounded cache and wakes any identical request.
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(compile)
                        .await
                        .map_err(|error| format!("compiler task failed: {error}"));
                    cache.complete(owned_key, &owned_flight, result);
                });
                flight
            }
        };
        flight.wait().await
    }
}

fn cache() -> &'static Arc<CompileCache> {
    static CACHE: OnceLock<Arc<CompileCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(CompileCache::new(COMPILE_CACHE_CAPACITY)))
}

fn routing_app_key(
    snapshot: &ModelSnapshot,
    app: &brocade_core::model::AppView,
    listener_roots: &std::collections::BTreeSet<(String, String)>,
) -> String {
    let chain_ids = app
        .chains
        .iter()
        .map(|chain| chain.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let app_listener_roots = listener_roots
        .iter()
        .filter(|(chain, _)| chain_ids.contains(chain.as_str()))
        .collect::<Vec<_>>();
    digest(&(
        INCREMENTAL_CACHE_VERSION,
        "routing-app",
        &snapshot.settings,
        &snapshot.nodes,
        &snapshot.node_egress_dns,
        &snapshot.users,
        &snapshot.external_outbounds,
        app,
        app_listener_roots,
    ))
}

fn chain_key(input: &RoutingChainInput) -> String {
    digest(&(INCREMENTAL_CACHE_VERSION, "routing-chain", input))
}

fn final_app_key(
    routing: &RoutingAppCompile,
    target_steps: &[Step],
    system: &brocade_core::ir::system::SystemIr,
) -> String {
    let targets = routing
        .app()
        .steps
        .iter()
        .flat_map(|step| {
            step.rules.iter().filter_map(|rule| {
                rule.action
                    .forward_ref(&step.chain)
                    .map(|forward| (forward.target_chain, forward.target_node))
            })
        })
        .collect::<std::collections::BTreeSet<_>>();
    let dependencies = target_steps
        .iter()
        .filter(|step| targets.contains(&(step.chain.as_str(), step.node.as_str())))
        .collect::<Vec<_>>();
    digest(&(
        INCREMENTAL_CACHE_VERSION,
        "final-app",
        routing.app(),
        dependencies,
        system,
    ))
}

pub(crate) fn compile_incremental(snapshot: &ModelSnapshot) -> CompileOutput {
    let revision = snapshot.revision;
    let mut semantic_snapshot = snapshot.clone();
    semantic_snapshot.revision = 0;
    let snapshot = &semantic_snapshot;
    let prelude = compile_prelude(snapshot);
    let routing_apps = snapshot
        .apps
        .iter()
        .map(|app| {
            let key = routing_app_key(snapshot, app, prelude.listener_roots());
            routing_apps().get_or_insert_with(key, || {
                compile_routing_app_with(snapshot, app, prelude.listener_roots(), |input| {
                    routing_chains()
                        .get_or_insert_with(chain_key(input), || input.compile())
                        .as_ref()
                        .clone()
                })
            })
        })
        .collect::<Vec<_>>();
    let target_steps = routing_apps
        .iter()
        .flat_map(|app| app.app().steps.iter().cloned())
        .collect::<Vec<_>>();
    let apps = routing_apps
        .iter()
        .map(|routing| {
            let key = final_app_key(routing, &target_steps, prelude.system());
            final_apps()
                .get_or_insert_with(key, || {
                    finish_app_compile(routing.as_ref().clone(), &target_steps, prelude.system())
                })
                .as_ref()
                .clone()
        })
        .collect::<Vec<_>>();
    let routing_apps = routing_apps
        .iter()
        .map(|app| app.as_ref().clone())
        .collect::<Vec<_>>();
    finish_compile(prelude, &routing_apps, apps).with_revision(revision)
}

pub(crate) fn project_node_with_key(
    output: &CompileOutput,
    node_id: &str,
) -> std::result::Result<(String, Arc<NodePlan>), PublishBlocked> {
    output.ensure_publishable()?;
    let view = output.unpublishable_view();
    let apps = node::scope_node_apps(view.apps, node_id);
    let mut system = view.system.clone();
    system.revision = 0;
    let key = digest(&(
        INCREMENTAL_CACHE_VERSION,
        "node-projection",
        node_id,
        &system,
        &apps,
    ));
    let plan = node_projections().get_or_insert_with(key.clone(), || {
        node::project_node(view.system, &apps, node_id)
    });
    Ok((key, plan))
}

pub(crate) fn project_node(
    output: &CompileOutput,
    node_id: &str,
) -> std::result::Result<Arc<NodePlan>, PublishBlocked> {
    project_node_with_key(output, node_id).map(|(_, plan)| plan)
}

pub(crate) fn project_user_with_key(
    output: &CompileOutput,
    tenant: &str,
    user_id: &str,
) -> std::result::Result<(String, Arc<UserPlan>), PublishBlocked> {
    output.ensure_publishable()?;
    let view = output.unpublishable_view();
    let apps = user::scope_user_apps(view.apps, tenant, user_id);
    let key = digest(&(
        INCREMENTAL_CACHE_VERSION,
        "user-projection",
        tenant,
        user_id,
        &apps,
    ));
    let plan = user_projections()
        .get_or_insert_with(key.clone(), || user::project_user(&apps, tenant, user_id));
    Ok((key, plan))
}

pub(crate) fn project_user(
    output: &CompileOutput,
    tenant: &str,
    user_id: &str,
) -> std::result::Result<Arc<UserPlan>, PublishBlocked> {
    project_user_with_key(output, tenant, user_id).map(|(_, plan)| plan)
}

pub(crate) fn project_probe(
    output: &CompileOutput,
    node_id: &str,
) -> std::result::Result<Arc<ProbePlan>, PublishBlocked> {
    output.ensure_publishable()?;
    let view = output.unpublishable_view();
    let apps = probe::scope_probe_apps(view.apps, node_id);
    let key = digest(&(
        INCREMENTAL_CACHE_VERSION,
        "probe-projection",
        node_id,
        &apps,
    ));
    Ok(probe_projections().get_or_insert_with(key, || probe::project_probe(&apps, node_id)))
}

pub(crate) async fn compile_snapshot(snapshot: &ModelSnapshot) -> Result<Arc<CompileOutput>> {
    // The snapshot is already scoped for the authenticated actor.  Hashing that exact wire form
    // prevents cross-scope reuse and makes invalidation content-addressed: no mutable generation
    // table can get out of step with the compiler input.
    let key = sha256_hex(&serde_json::to_vec(snapshot)?);
    let snapshot = snapshot.clone();
    cache()
        .get_or_compile(key, move || Arc::new(compile_incremental(&snapshot)))
        .await
        .map_err(StoreError::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use brocade_core::{compile::compile, model::ModelSnapshot};

    use super::{compile_incremental, CompileCache, StageMemo};

    fn snapshot(revision: u64) -> ModelSnapshot {
        ModelSnapshot {
            revision,
            overlay_cidr: "10.88.0.0/16".parse().unwrap(),
            settings: Default::default(),
            nodes: Vec::new(),
            node_egress_dns: Vec::new(),
            users: Vec::new(),
            external_outbounds: Vec::new(),
            apps: Vec::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identical_inflight_compilations_run_once() {
        let cache = Arc::new(CompileCache::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let run = |cache: Arc<CompileCache>, calls: Arc<AtomicUsize>| async move {
            cache
                .get_or_compile("same".to_owned(), move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    Arc::new(compile(&snapshot(1)))
                })
                .await
                .unwrap()
        };

        let (first, second) = tokio::join!(
            run(Arc::clone(&cache), Arc::clone(&calls)),
            run(Arc::clone(&cache), Arc::clone(&calls)),
        );

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_snapshot_does_not_wait_for_an_older_compile() {
        let cache = Arc::new(CompileCache::new(2));
        let (first_started_tx, first_started_rx) = std::sync::mpsc::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
        let first_cache = Arc::clone(&cache);
        let first = tokio::spawn(async move {
            first_cache
                .get_or_compile("first".to_owned(), move || {
                    first_started_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                    Arc::new(compile(&snapshot(1)))
                })
                .await
                .unwrap()
        });
        tokio::task::spawn_blocking(move || first_started_rx.recv().unwrap())
            .await
            .unwrap();

        let (second_started_tx, second_started_rx) = std::sync::mpsc::channel();
        let second_cache = Arc::clone(&cache);
        let second = tokio::spawn(async move {
            second_cache
                .get_or_compile("second".to_owned(), move || {
                    second_started_tx.send(()).unwrap();
                    Arc::new(compile(&snapshot(2)))
                })
                .await
                .unwrap()
        });
        let second_started = tokio::task::spawn_blocking(move || {
            second_started_rx
                .recv_timeout(Duration::from_secs(1))
                .is_ok()
        })
        .await
        .unwrap();

        release_first_tx.send(()).unwrap();
        let (first, second) = tokio::join!(first, second);
        assert!(
            second_started,
            "a different snapshot waited for the older compile"
        );
        assert!(!Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
    }

    #[tokio::test]
    async fn ready_entries_are_bounded_and_lru() {
        let cache = Arc::new(CompileCache::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        for (key, revision) in [("a", 1), ("b", 2), ("a", 1), ("c", 3), ("b", 2)] {
            let calls = Arc::clone(&calls);
            cache
                .get_or_compile(key.to_owned(), move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Arc::new(compile(&snapshot(revision)))
                })
                .await
                .unwrap();
        }

        // a was refreshed before c arrived, so b was evicted and had to compile again.
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let state = cache
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.ready.len(), 2);
    }

    #[test]
    fn incremental_assembly_rebinds_the_requested_revision() {
        let snapshot = snapshot(42);
        assert_eq!(compile_incremental(&snapshot), compile(&snapshot));
        let compiled = compile_incremental(&snapshot);
        let view = compiled.unpublishable_view();
        assert_eq!(view.system.revision, 42);
    }

    #[test]
    fn stage_memo_reuses_content_and_evicts_by_lru() {
        let memo = StageMemo::new(2);
        let calls = AtomicUsize::new(0);
        let get = |key: &str| {
            memo.get_or_insert_with(key.to_owned(), || calls.fetch_add(1, Ordering::SeqCst))
        };

        assert_eq!(*get("a"), 0);
        assert_eq!(*get("b"), 1);
        assert_eq!(*get("a"), 0);
        assert_eq!(*get("c"), 2);
        assert_eq!(*get("b"), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn stage_memo_can_retry_after_a_builder_panics() {
        let memo = StageMemo::new(1);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            memo.get_or_insert_with("same".to_owned(), || -> usize {
                panic!("test builder panic")
            });
        }));
        assert!(failed.is_err());

        assert_eq!(*memo.get_or_insert_with("same".to_owned(), || 42), 42);
    }
}
