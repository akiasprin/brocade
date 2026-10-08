//! Bounded, process-local HTTP/SQL timings. Never retain SQL text, bind values, URLs, headers,
//! response bodies or individual request identities. Route templates are supplied by Axum.
//!
//! SQLx's query duration is client-observed execution/fetch time, not PostgreSQL CPU time.
//! Successful pool acquisition includes connection setup/validation. These timings may overlap
//! and must not be subtracted from wall time to invent an "application CPU" measurement.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Instant,
};

use axum::{
    body::HttpBody,
    extract::{MatchedPath, Request},
    http::HeaderValue,
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Metadata, Subscriber,
};

const WINDOW_MINUTES: u64 = 15;
const MAX_ROUTES: usize = 512;
const MAX_QUERIES: usize = 256;
// Upper bounds, in milliseconds. The last overflow bucket has no finite upper bound.
const BUCKET_MS: [u64; 18] = [
    1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 30_000, 60_000,
    120_000, 300_000,
];

tokio::task_local! {
    static REQUEST_TIMING: RequestTiming;
}

struct RequestTiming {
    service: Performance,
    totals: Arc<Mutex<DatabaseTotals>>,
    route: RouteKey,
}

#[derive(Clone)]
pub struct Performance(Arc<Inner>);

struct Inner {
    started: Instant,
    enabled: AtomicBool,
    sql_observer_installed: AtomicBool,
    windows: Mutex<VecDeque<Minute>>,
}

#[derive(Default)]
struct Minute {
    number: u64,
    routes: BTreeMap<RouteKey, RouteStats>,
    queries: BTreeMap<QueryKey, QueryStats>,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct RouteKey {
    route: String,
    method: &'static str,
    phase: &'static str,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct QueryKey {
    fingerprint: String,
    #[serde(flatten)]
    route: RouteKey,
}

#[derive(Clone, Default, Serialize)]
pub struct DatabaseTotals {
    queries: u64,
    query_ms: f64,
    successful_acquisitions: u64,
    acquire_ms: f64,
}

#[derive(Clone, Default, Serialize)]
struct Distribution {
    count: u64,
    total_ms: f64,
    max_ms: f64,
    buckets: [u64; 19],
}

impl Distribution {
    fn add(&mut self, ms: f64) {
        self.count += 1;
        self.total_ms += ms;
        self.max_ms = self.max_ms.max(ms);
        self.buckets[BUCKET_MS.partition_point(|bound| (*bound as f64) < ms)] += 1;
    }

    fn merge(&mut self, other: &Self) {
        self.count += other.count;
        self.total_ms += other.total_ms;
        self.max_ms = self.max_ms.max(other.max_ms);
        for (left, right) in self.buckets.iter_mut().zip(other.buckets) {
            *left += right;
        }
    }

    fn percentile_upper_bound(&self, percentile: u64) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let target = (self.count * percentile).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= target {
                return BUCKET_MS.get(index).copied();
            }
        }
        None
    }
}

#[derive(Clone, Default, Serialize)]
struct RouteStats {
    handler: Distribution,
    status_classes: [u64; 6],
    known_response_bytes: u64,
    unknown_length_responses: u64,
    database: DatabaseTotals,
    database_unavailable_requests: u64,
}

#[derive(Clone, Default, Serialize)]
struct QueryStats {
    elapsed: Distribution,
    rows_returned: u64,
    rows_affected: u64,
}

#[derive(Serialize)]
pub struct Snapshot {
    enabled: bool,
    sql_timings_available: bool,
    uptime_secs: u64,
    window_minutes: u64,
    bucket_upper_bounds_ms: &'static [u64],
    routes: Vec<RouteSnapshot>,
    queries: Vec<QuerySnapshot>,
}

#[derive(Serialize)]
struct RouteSnapshot {
    #[serde(flatten)]
    key: RouteKey,
    #[serde(flatten)]
    stats: RouteStats,
    p50_upper_bound_ms: Option<u64>,
    p95_upper_bound_ms: Option<u64>,
}

#[derive(Serialize)]
struct QuerySnapshot {
    #[serde(flatten)]
    key: QueryKey,
    #[serde(flatten)]
    stats: QueryStats,
    p95_upper_bound_ms: Option<u64>,
}

impl Default for Performance {
    fn default() -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            enabled: AtomicBool::new(true),
            sql_observer_installed: AtomicBool::new(false),
            windows: Mutex::new(VecDeque::new()),
        }))
    }
}

impl Performance {
    pub fn global() -> Self {
        static INSTANCE: OnceLock<Performance> = OnceLock::new();
        INSTANCE.get_or_init(Self::default).clone()
    }

    /// Install once at Console startup. SQLx events are consumed, never formatted to logs.
    pub fn initialize() -> Result<(), Box<dyn std::error::Error>> {
        let enabled = match std::env::var("BROCADE_PERFORMANCE_ENABLED").as_deref() {
            Ok("true" | "1") | Err(std::env::VarError::NotPresent) => true,
            Ok("false" | "0") => false,
            _ => return Err("BROCADE_PERFORMANCE_ENABLED must be true or false".into()),
        };
        let service = Self::global();
        service.0.enabled.store(enabled, Ordering::Relaxed);
        if enabled {
            tracing::subscriber::set_global_default(SqlObserver)?;
            service
                .0
                .sql_observer_installed
                .store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn update(&self, update: impl FnOnce(&mut Minute)) {
        let minute = self.0.started.elapsed().as_secs() / 60;
        // Poisoned telemetry must not break serving. Drop this observation, visibly unavailable
        // in snapshot(), rather than silently claiming a healthy measurement.
        let Ok(mut windows) = self.0.windows.lock() else {
            return;
        };
        retain_window(&mut windows, minute);
        if windows.back().is_none_or(|window| window.number != minute) {
            windows.push_back(Minute {
                number: minute,
                ..Minute::default()
            });
        }
        if let Some(window) = windows.back_mut() {
            update(window);
        }
    }

    pub fn snapshot(&self) -> Result<Snapshot, &'static str> {
        let mut windows = self
            .0
            .windows
            .lock()
            .map_err(|_| "performance collector unavailable")?;
        retain_window(&mut windows, self.0.started.elapsed().as_secs() / 60);
        let mut routes = BTreeMap::<RouteKey, RouteStats>::new();
        let mut queries = BTreeMap::<QueryKey, QueryStats>::new();
        for window in windows.iter() {
            for (key, stats) in &window.routes {
                let target = routes.entry(key.clone()).or_default();
                target.handler.merge(&stats.handler);
                for (left, right) in target.status_classes.iter_mut().zip(stats.status_classes) {
                    *left += right;
                }
                target.known_response_bytes += stats.known_response_bytes;
                target.unknown_length_responses += stats.unknown_length_responses;
                target.database.queries += stats.database.queries;
                target.database.query_ms += stats.database.query_ms;
                target.database.successful_acquisitions += stats.database.successful_acquisitions;
                target.database.acquire_ms += stats.database.acquire_ms;
                target.database_unavailable_requests += stats.database_unavailable_requests;
            }
            for (fingerprint, stats) in &window.queries {
                let target = queries.entry(fingerprint.clone()).or_default();
                target.elapsed.merge(&stats.elapsed);
                target.rows_returned += stats.rows_returned;
                target.rows_affected += stats.rows_affected;
            }
        }
        Ok(Snapshot {
            enabled: self.0.enabled.load(Ordering::Relaxed),
            sql_timings_available: self.0.sql_observer_installed.load(Ordering::Relaxed),
            uptime_secs: self.0.started.elapsed().as_secs(),
            window_minutes: WINDOW_MINUTES,
            bucket_upper_bounds_ms: &BUCKET_MS,
            routes: routes
                .into_iter()
                .map(|(key, stats)| RouteSnapshot {
                    p50_upper_bound_ms: stats.handler.percentile_upper_bound(50),
                    p95_upper_bound_ms: stats.handler.percentile_upper_bound(95),
                    key,
                    stats,
                })
                .collect(),
            queries: queries
                .into_iter()
                .map(|(key, stats)| QuerySnapshot {
                    p95_upper_bound_ms: stats.elapsed.percentile_upper_bound(95),
                    key,
                    stats,
                })
                .collect(),
        })
    }

    async fn observe(&self, request: Request, next: Next) -> Response {
        if !self.0.enabled.load(Ordering::Relaxed) {
            return next.run(request).await;
        }
        let route = request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str)
            .unwrap_or("<unmatched>");
        // Reading the diagnostic endpoint must not measure itself or fill its own top queries.
        if route == "/diagnostics/performance" {
            return next.run(request).await;
        }
        let key = RouteKey {
            route: route.to_owned(),
            method: safe_method(request.method().as_str()),
            phase: if self.0.started.elapsed().as_secs() < 60 {
                "startup"
            } else {
                "steady"
            },
        };
        let totals = Arc::new(Mutex::new(DatabaseTotals::default()));
        let start = Instant::now();
        let mut response = REQUEST_TIMING
            .scope(
                RequestTiming {
                    service: self.clone(),
                    totals: totals.clone(),
                    route: key.clone(),
                },
                next.run(request),
            )
            .await;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
        let known_bytes = response.body().size_hint().exact();
        let status_class = usize::from(response.status().as_u16() / 100).min(5);
        let database = totals.lock().ok().map(|value| value.clone());
        self.update(|window| {
            let key = if window.routes.contains_key(&key) || window.routes.len() < MAX_ROUTES {
                key
            } else {
                RouteKey {
                    route: "<overflow>".to_owned(),
                    method: "OTHER",
                    phase: "mixed",
                }
            };
            let stats = window.routes.entry(key).or_default();
            stats.handler.add(elapsed_ms);
            stats.status_classes[status_class] += 1;
            match known_bytes {
                Some(bytes) => stats.known_response_bytes += bytes,
                None => stats.unknown_length_responses += 1,
            }
            if let Some(database) = &database {
                stats.database.queries += database.queries;
                stats.database.query_ms += database.query_ms;
                stats.database.successful_acquisitions += database.successful_acquisitions;
                stats.database.acquire_ms += database.acquire_ms;
            } else {
                stats.database_unavailable_requests += 1;
            }
        });
        let timing = if let Some(database) =
            database.filter(|_| self.0.sql_observer_installed.load(Ordering::Relaxed))
        {
            format!(
                "handler;dur={elapsed_ms:.3}, db;dur={:.3}, pool;dur={:.3}",
                database.query_ms, database.acquire_ms
            )
        } else {
            format!("handler;dur={elapsed_ms:.3}")
        };
        if let Ok(value) = HeaderValue::from_str(&timing) {
            response.headers_mut().insert("server-timing", value);
        }
        response
    }
}

fn retain_window(windows: &mut VecDeque<Minute>, minute: u64) {
    while windows
        .front()
        .is_some_and(|window| window.number + WINDOW_MINUTES <= minute)
    {
        windows.pop_front();
    }
}

fn safe_method(method: &str) -> &'static str {
    match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

pub async fn middleware(request: Request, next: Next) -> Response {
    Performance::global().observe(request, next).await
}

/// SQLx 0.8 emits numerical fields on these targets. Keep this allowlist covered by an integration
/// test when SQLx is upgraded. In particular, never forward its `db.statement` to a text logger.
struct SqlObserver;

impl Subscriber for SqlObserver {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // SQLx checks `enabled!` (hint metadata) before emitting its query event.
        !metadata.is_span()
            && matches!(metadata.target(), "sqlx::query" | "sqlx::pool::acquire")
            && REQUEST_TIMING.try_with(|_| ()).is_ok()
    }
    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::DEBUG)
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        if !matches!(
            event.metadata().target(),
            "sqlx::query" | "sqlx::pool::acquire"
        ) {
            return;
        }
        let mut fields = NumericFields::default();
        event.record(&mut fields);
        let _ = REQUEST_TIMING.try_with(|scope| {
            let Ok(mut totals) = scope.totals.lock() else {
                return;
            };
            if let Some(ms) = fields.acquire_ms {
                totals.successful_acquisitions += 1;
                totals.acquire_ms += ms;
            }
            if let Some(ms) = fields.query_ms {
                totals.queries += 1;
                totals.query_ms += ms;
                scope.service.update(|window| {
                    let fingerprint = fields
                        .fingerprint
                        .or(fields.summary_fingerprint)
                        .unwrap_or_else(|| "unknown".to_owned());
                    let key = QueryKey {
                        fingerprint,
                        route: scope.route.clone(),
                    };
                    let key = if window.queries.contains_key(&key)
                        || window.queries.len() < MAX_QUERIES
                    {
                        key
                    } else {
                        QueryKey {
                            fingerprint: "overflow".to_owned(),
                            route: RouteKey {
                                route: "<overflow>".to_owned(),
                                method: "OTHER",
                                phase: "mixed",
                            },
                        }
                    };
                    let stats = window.queries.entry(key).or_default();
                    stats.elapsed.add(ms);
                    stats.rows_returned += fields.rows_returned;
                    stats.rows_affected += fields.rows_affected;
                });
            }
        });
    }
}

#[derive(Default)]
struct NumericFields {
    query_ms: Option<f64>,
    acquire_ms: Option<f64>,
    rows_returned: u64,
    rows_affected: u64,
    fingerprint: Option<String>,
    summary_fingerprint: Option<String>,
}

impl Visit for NumericFields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        match field.name() {
            "elapsed_secs" => self.query_ms = Some(value * 1_000.0),
            // This misspelling is the actual SQLx 0.8 event contract.
            "aquired_after_secs" => self.acquire_ms = Some(value * 1_000.0),
            _ => {}
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "rows_returned" => self.rows_returned = value,
            "rows_affected" => self.rows_affected = value,
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if !matches!(field.name(), "db.statement" | "summary") || value.trim().is_empty() {
            return;
        }
        // Fingerprint only; SQL text and bind values never enter the retained snapshot.
        let fingerprint = format!("{:x}", Sha256::digest(value.trim().as_bytes()));
        match field.name() {
            "db.statement" => self.fingerprint = Some(fingerprint),
            "summary" => self.summary_fingerprint = Some(fingerprint),
            _ => {}
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        routing::get,
        Router,
    };
    use tower::ServiceExt;
    use tracing::instrument::WithSubscriber;

    fn test_route() -> RouteKey {
        RouteKey {
            route: "/test".to_owned(),
            method: "GET",
            phase: "steady",
        }
    }

    #[test]
    fn histogram_and_window_have_explicit_bounds() {
        let mut histogram = Distribution::default();
        for _ in 0..95 {
            histogram.add(12.0);
        }
        for _ in 0..5 {
            histogram.add(250.0);
        }
        assert_eq!(histogram.percentile_upper_bound(95), Some(20));
        assert_eq!(histogram.percentile_upper_bound(99), Some(500));
        let mut windows = VecDeque::from([
            Minute {
                number: 1,
                ..Minute::default()
            },
            Minute {
                number: 2,
                ..Minute::default()
            },
        ]);
        retain_window(&mut windows, 16);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].number, 2);
    }

    #[tokio::test]
    async fn observer_keeps_numeric_timings_but_not_sql_or_bind_values() {
        let service = Performance::default();
        let totals = Arc::new(Mutex::new(DatabaseTotals::default()));
        let scope = RequestTiming {
            service: service.clone(),
            totals: totals.clone(),
            route: test_route(),
        };
        REQUEST_TIMING.scope(scope, async {
            tracing::event!(target: "sqlx::query", tracing::Level::DEBUG,
                elapsed_secs = 0.012, rows_returned = 3u64, rows_affected = 0u64,
                db.statement = "SELECT secret FROM credentials WHERE token = 'never-retain-this'",
                summary = "SELECT secret FROM credentials");
            tracing::event!(target: "sqlx::pool::acquire", tracing::Level::DEBUG, aquired_after_secs = 0.003);
        }).with_subscriber(SqlObserver).await;
        let totals = totals.lock().unwrap();
        assert_eq!(totals.queries, 1);
        assert_eq!(totals.query_ms, 12.0);
        assert_eq!(totals.acquire_ms, 3.0);
        let snapshot = serde_json::to_string(&service.snapshot().unwrap()).unwrap();
        assert!(!snapshot.contains("secret"));
        assert!(!snapshot.contains("credentials"));
        assert!(!snapshot.contains("never-retain"));
        assert_eq!(
            service.snapshot().unwrap().queries[0].stats.rows_returned,
            3
        );
    }

    #[tokio::test]
    async fn http_uses_route_templates_and_does_not_consume_the_body() {
        let service = Performance::default();
        let observer = service.clone();
        let app = Router::new()
            .route("/nodes/{node}", get(|| async { "payload" }))
            .layer(axum::middleware::from_fn(move |request, next| {
                let observer = observer.clone();
                async move { observer.observe(request, next).await }
            }));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/nodes/private-node?token=private-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.headers().contains_key("server-timing"));
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            "payload"
        );
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.routes[0].key.route, "/nodes/{node}");
        assert_eq!(snapshot.routes[0].stats.known_response_bytes, 7);
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private"));
    }

    #[tokio::test]
    async fn concurrent_requests_have_independent_sql_totals() {
        let service = Performance::default();
        let run = |duration| {
            let totals = Arc::new(Mutex::new(DatabaseTotals::default()));
            let scope = RequestTiming {
                service: service.clone(),
                totals: totals.clone(),
                route: test_route(),
            };
            async move {
                REQUEST_TIMING
                    .scope(scope, async {
                        tokio::task::yield_now().await;
                        tracing::event!(target: "sqlx::query", tracing::Level::DEBUG,
                        elapsed_secs = duration, db.statement = "SELECT 1");
                    })
                    .with_subscriber(SqlObserver)
                    .await;
                let value = totals.lock().unwrap().query_ms;
                value
            }
        };
        let (first, second) = tokio::join!(run(0.01), run(0.02));
        assert_eq!((first, second), (10.0, 20.0));
    }

    #[tokio::test]
    async fn unique_sql_is_capped_without_losing_overflow_totals() {
        let service = Performance::default();
        let scope = RequestTiming {
            service: service.clone(),
            totals: Arc::new(Mutex::new(DatabaseTotals::default())),
            route: test_route(),
        };
        REQUEST_TIMING
            .scope(scope, async {
                for number in 0..1_000 {
                    let sql = format!("SELECT {number}");
                    tracing::event!(target: "sqlx::query", tracing::Level::DEBUG,
                    elapsed_secs=0.001, db.statement=sql.as_str());
                }
            })
            .with_subscriber(SqlObserver)
            .await;
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.queries.len(), MAX_QUERIES + 1);
        assert_eq!(
            snapshot
                .queries
                .iter()
                .map(|query| query.stats.elapsed.count)
                .sum::<u64>(),
            1000
        );
        assert_eq!(
            snapshot
                .queries
                .iter()
                .find(|query| query.key.fingerprint == "overflow")
                .unwrap()
                .stats
                .elapsed
                .count,
            1000 - MAX_QUERIES as u64
        );
    }

    #[tokio::test]
    async fn streaming_response_is_not_buffered_and_disabled_metrics_add_no_header() {
        let service = Performance::default();
        let observer = service.clone();
        let app = Router::new()
            .route(
                "/stream",
                get(|| async {
                    Body::from_stream(futures_util::stream::pending::<
                        Result<axum::body::Bytes, std::io::Error>,
                    >())
                }),
            )
            .layer(axum::middleware::from_fn(move |request, next| {
                let observer = observer.clone();
                async move { observer.observe(request, next).await }
            }));
        let request = || {
            Request::builder()
                .uri("/stream")
                .body(Body::empty())
                .unwrap()
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            app.clone().oneshot(request()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.headers().contains_key("server-timing"));
        assert_eq!(
            service.snapshot().unwrap().routes[0]
                .stats
                .unknown_length_responses,
            1
        );
        service.0.enabled.store(false, Ordering::Relaxed);
        let response = app.oneshot(request()).await.unwrap();
        assert!(!response.headers().contains_key("server-timing"));
        assert_eq!(service.snapshot().unwrap().routes[0].stats.handler.count, 1);
    }

    #[tokio::test]
    #[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
    async fn sqlx_events_really_include_query_and_pool_timings() {
        if std::env::var("BROCADE_RUN_PG_TESTS").as_deref() != Ok("1") {
            return;
        }
        use testcontainers::{runners::AsyncRunner, ImageExt};
        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("16-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let store = brocade_store::PgStore::connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
        ))
        .await
        .unwrap();
        let service = Performance::default();
        let totals = Arc::new(Mutex::new(DatabaseTotals::default()));
        let scope = RequestTiming {
            service: service.clone(),
            totals: totals.clone(),
            route: test_route(),
        };
        REQUEST_TIMING
            .scope(scope, async {
                let value: String = sqlx::query_scalar("SELECT $1::text AS confidential_value")
                    .bind("do-not-retain-my-credential")
                    .fetch_one(store.pool())
                    .await
                    .unwrap();
                assert_eq!(value, "do-not-retain-my-credential");
            })
            .with_subscriber(SqlObserver)
            .await;
        let totals = totals.lock().unwrap();
        assert!(totals.queries >= 1);
        assert!(totals.successful_acquisitions >= 1);
        assert!(totals.query_ms > 0.0);
        let snapshot = service.snapshot().unwrap();
        let expected = format!(
            "{:x}",
            Sha256::digest(b"SELECT $1::text AS confidential_value")
        );
        assert!(snapshot
            .queries
            .iter()
            .any(|query| query.key.fingerprint == expected && query.stats.rows_returned == 1));
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("confidential_value"));
        assert!(!json.contains("do-not-retain"));
    }
}
