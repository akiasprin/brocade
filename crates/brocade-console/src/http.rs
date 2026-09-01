use std::{
    collections::HashMap,
    convert::Infallible,
    env,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::Notify;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use brocade_core::{
    hash::{hex_lower, sha256_hex},
    model::{IpFamily, ModelSettings},
    physical::user::{SubscriptionFilter, SubscriptionProtocol},
};
use brocade_deployment::plan::DeploymentKind;
use brocade_deployment::protocol::{
    AgentObservationRequest, DeploymentWaveConfirmationRequest, NodeRuntimeReport, RouteIpReport,
    TargetConvergenceReport, UsageReportRequest,
};
use brocade_store::{
    AbandonNodeRequest, AdminContext, AdminInitRequest, AdminLoginRequest, AdminRole, AgentRelease,
    AuthenticatedAdmin, AuthenticatedNode, BinarySource, BrandingSettings,
    ChangeAdminPasswordRequest, CreateAdminOperatorRequest, CreateAppRequest, CreateChainRequest,
    CreateDeploymentRequest, CreateFrontRequest, CreateGrantRequest, CreateIngressRequest,
    CreateRollbackRequest, CreateTenantRequest, CreateUserRequest, DistributionSettings,
    E2eProbeRequest, LinkHealthRequest, LinkProbeRequest, LoadReportRequest, ModelOp,
    NodeLifecyclePhase, PgStore, PhantunBinaries, ProvisionNodeRequest, ProvisionNodeResult,
    ProvisionedNode, RegisterWarpBindingRequest, RemoveWarpBindingRequest, SetUserAppQuotaRequest,
    StoreError, UpdateAgentLogDefaultRequest, UpdateNodeLogPolicyRequest, UpdateNodeRequest,
    UpdateNodeStatusRequest, UpdateUserStatusRequest, UpdateWarpBindingRequest,
    VerifyDeploymentRequest, PUBLIC_OPERATOR_ID,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower_http::services::ServeDir;

const ADMIN_SESSION_COOKIE: &str = "brocade_session";
const ROUTE_IPV4_HEADER: &str = "x-brocade-route-ipv4";
const ROUTE_IPV6_HEADER: &str = "x-brocade-route-ipv6";
const AGENT_LOG_MAX_MIB_HEADER: &str = "x-brocade-log-max-mib";
/// Which architecture the asking agent was built for, in `uname -m`'s vocabulary. Only the node
/// knows this, and the control plane has no other source for it: enrolment records no
/// architecture, and an incorrect guess would hand a machine a binary that installs, verifies,
/// and then cannot run.
const AGENT_ARCH_HEADER: &str = "x-brocade-arch";
const AGENT_PROTOCOL_HEADER: &str = "x-brocade-protocol-version";
const SUBSCRIPTION_RATE_PER_MINUTE: u32 = 60;
const SUBSCRIPTION_CACHE_CONTROL: &str = "no-store, no-cache, max-age=0, must-revalidate";

tokio::task_local! {
    /// The result of the single authentication lookup performed at the admin-router boundary.
    /// Inner authorization and response-masking code reads this value, so one request cannot
    /// disagree with itself about who is signed in and database failures cannot fail open.
    static REQUEST_ADMIN: Option<AuthenticatedAdmin>;
}

fn request_admin() -> Option<AuthenticatedAdmin> {
    REQUEST_ADMIN.try_with(Clone::clone).ok().flatten()
}

/// The agents the control plane carries, one per architecture.
///
/// Cross-compiled and embedded by `build.rs` at compile time, so an unrebuilt agent is a compile
/// error rather than a stale URL discovered at runtime. The full reasoning is at the top of
/// `build.rs`.
///
/// Each entry is `(the uname -m name, the bytes, that byte string's sha256)`. The sha must come
/// from the same build as the bytes: the install script verifies the download against it, and a
/// mismatch means no installation.
pub const EMBEDDED_AGENTS: &[(&str, &[u8], &str)] = &[
    (
        "x86_64",
        include_bytes!(concat!(env!("OUT_DIR"), "/brocade-agent-x86_64")),
        env!("BROCADE_EMBEDDED_AGENT_SHA256_X86_64"),
    ),
    (
        "aarch64",
        include_bytes!(concat!(env!("OUT_DIR"), "/brocade-agent-aarch64")),
        env!("BROCADE_EMBEDDED_AGENT_SHA256_AARCH64"),
    ),
];

fn embedded_agent(arch: &str) -> Option<(&'static [u8], &'static str)> {
    EMBEDDED_AGENTS
        .iter()
        .find(|(name, ..)| *name == arch)
        .map(|(_, bytes, sha)| (*bytes, *sha))
}

/// The identity of *this build's* set of agents: the sha256 of every embedded agent's sha256,
/// joined in table order.
///
/// One id for the pair rather than two compared separately. The two architectures form one
/// artifact, and comparing them individually admits a state where x86_64 is cleared and aarch64
/// is not, which is never intended and which presents as half the fleet upgrading.
///
/// A release recorded in the database names this id, and the control plane serves an agent only
/// where the recorded id matches its own. Deploying a new control plane makes the recorded id
/// stop matching, so the fleet stays unchanged until an operator releases again. The alternative,
/// a boolean enabling auto-upgrade, would make every control-plane deployment replace the binary
/// on every machine at once.
pub fn embedded_release_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mut hasher = Sha256::new();
        for (_, _, sha) in EMBEDDED_AGENTS {
            hasher.update(sha.as_bytes());
            // A separator, so that two shas cannot be re-partitioned into a different pair that
            // hashes the same.
            hasher.update(b"\n");
        }
        hex_lower(&hasher.finalize())
    })
}

/// Everything the install command assembles from: the agent surface's public address, and the two
/// binaries the control plane distributes itself. Unrelated to store, and separated out so that
/// install_command can be tested without a database.
#[derive(Clone, Debug, Default)]
pub struct AgentDistribution {
    agent_public_url: String,
    /// With all of them `None` the embedded copy is used (`agent_bin`). A configured value wins —
    /// that route has to remain for a fleet holding other architectures, or for a CDN (see the
    /// `Exec format error` passage in `install.sh`).
    agent_binary_url: Option<String>,
    agent_binary_sha256: Option<String>,
    /// The control plane distributes xray itself: a node may not reach upstream, and this route
    /// pins the version with a sha256. Unconfigured, the install script falls back to downloading
    /// from the upstream release, so a single command still installs a working node.
    xray_binary_url: Option<String>,
    xray_binary_sha256: Option<String>,
    /// Which upstream xray release the fleet runs, as a tag (`v26.4.25`). Unset, the install
    /// script takes the newest, which is not a safe default here: the two features this control
    /// plane depends on overlap in only one release. Geodata's runtime
    /// reload needs >= 26.4 (`app/geodata` landed 2026-04-25), while the VLESS reverse tunnel
    /// carries traffic out but nothing back from 26.5 onwards (upstream #6242, reproduced across
    /// 26.5.3 / 26.5.9 / 26.6.27 / 26.7.28 with the same config that works on 26.4.25).
    ///
    /// Kept as a tag rather than a pinned binary + sha because the machines fetch from upstream:
    /// a pinned binary would mean hosting it, and `xray_binary_url` already covers the fleet that
    /// cannot reach GitHub.
    xray_version: Option<String>,
    /// phantun is distributed by the control plane too, and this route matters more than xray's:
    /// what it rescues is precisely the machines SSH cannot reach. The install script can install
    /// it, but that route needs someone able to log in; this one travels in the desired state and
    /// the agent fetches it itself.
    phantun_server_url: Option<String>,
    phantun_server_sha256: Option<String>,
    phantun_client_url: Option<String>,
    phantun_client_sha256: Option<String>,
}

impl AgentDistribution {
    /// Supplied only where both are configured. With one of the two, the agent installs partway
    /// before finding the other absent, and the reported convergence failure names an unrelated
    /// cause.
    fn phantun(&self) -> Option<PhantunBinaries> {
        Some(PhantunBinaries {
            server: BinarySource {
                url: self.phantun_server_url.clone()?,
                sha256: self.phantun_server_sha256.clone()?,
            },
            client: BinarySource {
                url: self.phantun_client_url.clone()?,
                sha256: self.phantun_client_sha256.clone()?,
            },
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    store: PgStore,
    geoip: crate::geoip::GeoIpLookup,
    dist: AgentDistribution,
    subscription_public_url: Option<String>,
    subscription_rate: Arc<Mutex<HashMap<String, SubscriptionRateWindow>>>,
    // A quota change wakes the enforcement loop immediately rather than waiting for its next
    // tick. Lowering a quota has to revoke immediately and raising one has to restore
    // immediately, without an operator waiting a minute for the UI to update.
    //
    // Under a multi-instance deployment only this process's loop is woken and the others wait
    // out their own cycles. That degradation is acceptable: those rounds perform the same work,
    // later.
    quota_wake: Arc<Notify>,
    // Permission writes put durable rows in the grants outbox.  This signal is only the fast path;
    // the worker also scans on a timer, so losing a notification or restarting the process loses
    // no work.
    grants_wake: Arc<Notify>,
    // The same mechanism for certificate issuance. Here the button it backs is more than a
    // convenience: an operator who has just corrected a wrong DNS token should not have to wait
    // out the retry floor to see whether it now works.
    cert_wake: Arc<Notify>,
    // On-demand authorization verification. Jobs and their latest results are intentionally
    // process-local: this is an operator action, not durable health telemetry.
    grant_probes: crate::grant_probe::GrantProbeService,
}

#[derive(Clone, Copy)]
struct SubscriptionRateWindow {
    minute: u64,
    requests: u32,
}

/// Where the agent face is assumed to be when `BROCADE_AGENT_PUBLIC_URL` says nothing.
///
/// It follows the default topology, with both faces on one listener, because this string is
/// printed into the command an operator runs on a new machine, and a fallback naming a port with
/// no listener produces an enrolment that fails at the first fetch. A deployment that splits the
/// faces passes its own value, and `main.rs` derives it from the address the agent face bound.
pub const DEFAULT_AGENT_ORIGIN: &str = "http://127.0.0.1:8080";

impl AppState {
    pub fn new(store: PgStore) -> Self {
        Self::with_quota_wake(store, Arc::new(Notify::new()))
    }

    pub fn with_quota_wake(store: PgStore, quota_wake: Arc<Notify>) -> Self {
        Self::with_agent_origin(store, quota_wake, DEFAULT_AGENT_ORIGIN.to_owned())
    }

    /// The distribution as it stands right now: what the console has stored, over the environment
    /// this process started with, over the built-in default.
    ///
    /// Resolved per call rather than held on the state. The two stored fields are editable from
    /// the settings page, and a copy taken at startup would delay an edit until the service
    /// restarted, which is the requirement storing them removed. Both callers are install-time
    /// endpoints, so the extra query is negligible.
    ///
    /// A stored value takes precedence over the environment. The console is where the operator
    /// entered it, and a deployment whose env file overrode that would display one address and
    /// distribute another.
    async fn distribution(&self) -> Result<AgentDistribution, StoreError> {
        let stored = self.store.distribution().await?;
        let mut dist = self.dist.clone();
        if let Some(url) = stored.agent_public_url {
            dist.agent_public_url = url;
        }
        if stored.xray_version.is_some() {
            dist.xray_version = stored.xray_version;
        }
        Ok(dist)
    }

    /// Attaches the issuing worker's handle. Chained rather than passed in, because every
    /// constructor above already carries one argument that only some callers care about.
    pub fn and_cert_wake(mut self, cert_wake: Arc<Notify>) -> Self {
        self.cert_wake = cert_wake;
        self
    }

    pub fn and_grants_wake(mut self, grants_wake: Arc<Notify>) -> Self {
        self.grants_wake = grants_wake;
        self
    }

    /// Attaches the refresher's handle, same shape and same reason as the wakes above. Without it
    /// the state keeps a lookup nothing ever feeds, and the machine list renders without flags.
    pub fn and_geoip(mut self, geoip: crate::geoip::GeoIpLookup) -> Self {
        self.geoip = geoip;
        self
    }

    pub fn with_agent_origin(
        store: PgStore,
        quota_wake: Arc<Notify>,
        agent_origin: String,
    ) -> Self {
        let agent_public_url = env::var("BROCADE_AGENT_PUBLIC_URL")
            .ok()
            .and_then(normalize_url)
            .unwrap_or(agent_origin);
        let subscription_public_url = env::var("BROCADE_SUBSCRIPTION_PUBLIC_URL")
            .ok()
            .and_then(normalize_subscription_origin)
            // Debug/test builds keep the one-listener zero-config experience. Production must
            // state the HTTPS public origin explicitly; otherwise the admin endpoint answers 503
            // instead of handing out a loopback or internal URL.
            .or_else(|| cfg!(debug_assertions).then(|| agent_public_url.clone()));
        Self {
            store,
            geoip: crate::geoip::GeoIpLookup::default(),
            subscription_public_url,
            subscription_rate: Arc::new(Mutex::new(HashMap::new())),
            quota_wake,
            grants_wake: Arc::new(Notify::new()),
            // Replaced by main.rs with the worker's own handle. A standalone one here means that a
            // console built without the worker (the tests, brocade-preview) still answers the
            // route instead of failing to construct.
            cert_wake: Arc::new(Notify::new()),
            grant_probes: crate::grant_probe::GrantProbeService::from_env(),
            dist: AgentDistribution {
                agent_public_url,
                agent_binary_url: env::var("BROCADE_AGENT_BIN_URL")
                    .ok()
                    .and_then(normalize_url),
                agent_binary_sha256: env::var("BROCADE_AGENT_BIN_SHA256")
                    .ok()
                    .and_then(normalize_token),
                xray_binary_url: env::var("BROCADE_XRAY_BIN_URL")
                    .ok()
                    .and_then(normalize_url),
                xray_binary_sha256: env::var("BROCADE_XRAY_BIN_SHA256")
                    .ok()
                    .and_then(normalize_token),
                xray_version: env::var("BROCADE_XRAY_VERSION")
                    .ok()
                    .and_then(normalize_token),
                phantun_server_url: env::var("BROCADE_PHANTUN_SERVER_URL")
                    .ok()
                    .and_then(normalize_url),
                phantun_server_sha256: env::var("BROCADE_PHANTUN_SERVER_SHA256")
                    .ok()
                    .and_then(normalize_token),
                phantun_client_url: env::var("BROCADE_PHANTUN_CLIENT_URL")
                    .ok()
                    .and_then(normalize_url),
                phantun_client_sha256: env::var("BROCADE_PHANTUN_CLIENT_SHA256")
                    .ok()
                    .and_then(normalize_token),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ProvisionNodeHttpResponse {
    revision_id: u64,
    node: ProvisionedNode,
    enrollment: ProvisionEnrollmentHttpResponse,
}

#[derive(Debug, Serialize)]
struct ProvisionEnrollmentHttpResponse {
    token: String,
    token_prefix: String,
    // null means never expires; there is no longer a default 15-minute TTL (see insert_enrollment
    // in provision.rs)
    expires_at: Option<String>,
    script_url: String,
    script_sha256: String,
    install_command: String,
}

impl ProvisionNodeHttpResponse {
    fn new(result: ProvisionNodeResult, dist: &AgentDistribution) -> Self {
        let script_url = format!("{}/enroll/install.sh", dist.agent_public_url);
        let script_sha256 = install_script_sha256();
        let install_command = install_command(
            dist,
            &script_url,
            &script_sha256,
            InstallCredential::Enrollment(&result.enrollment.token),
        );

        Self {
            revision_id: result.revision_id,
            node: result.node,
            enrollment: ProvisionEnrollmentHttpResponse {
                token: result.enrollment.token,
                token_prefix: result.enrollment.token_prefix,
                expires_at: result.enrollment.expires_at,
                script_url,
                script_sha256,
                install_command,
            },
        }
    }
}

/// Mount the console front end on the admin surface: API routes take precedence and unmatched paths
/// fall through to the embedded files. The agent surface mounts nothing.
///
/// The bundle is embedded in this binary; `build.rs` explains why and `assets.rs` serves it. A
/// deployment is one file, and the front end cannot lag the API version it calls.
pub fn with_console_static(router: Router) -> Router {
    router.fallback(crate::assets::serve)
}

/// Serve the front end off a directory instead of from inside the binary.
///
/// The escape hatch behind `BROCADE_CONSOLE_DIST`, for iterating on the front end without
/// recompiling the control plane: `npm run build` and reload. A missing directory answers static
/// requests with 404 without affecting the API or blocking startup.
///
/// `ServeDir` sets no `Cache-Control`, so both kinds of file would fall back to the browser's
/// heuristic caching, which is `(now - last_modified) × 10%` when no header is present. That is
/// the case `assets.rs` describes, so the same two policies are applied here.
pub fn with_console_static_dir(router: Router, dist_dir: &str) -> Router {
    // The extra `Router` exists only to attach the middleware to the static branch: layering it on
    // the outer router would stamp Cache-Control onto the API routes too.
    router.fallback_service(
        Router::new()
            .fallback_service(ServeDir::new(dist_dir))
            .layer(axum::middleware::from_fn(static_cache_headers)),
    )
}

async fn static_cache_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let value =
        header::HeaderValue::from_static(crate::assets::cache_control_for(request.uri().path()));
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, value);
    response
}

pub fn admin_router(store: PgStore) -> Router {
    admin_router_with_state(AppState::new(store))
}

/// The variant sharing a wake signal with the quota loop. `main.rs` uses this one and tests the one
/// above.
pub fn admin_router_with_quota_wake(store: PgStore, quota_wake: Arc<Notify>) -> Router {
    admin_router_with_state(AppState::with_quota_wake(store, quota_wake))
}

/// The production workers' wakes. A separate constructor rather than another parameter on the
/// ones above, so callers with no certificate worker, namely the tests and `brocade-preview`,
/// keep their existing signature and receive a handle with no listener.
pub fn admin_router_with_wakes(
    store: PgStore,
    quota_wake: Arc<Notify>,
    grants_wake: Arc<Notify>,
    cert_wake: Arc<Notify>,
    geoip: crate::geoip::GeoIpLookup,
) -> Router {
    admin_router_with_state(
        AppState::with_quota_wake(store, quota_wake)
            .and_grants_wake(grants_wake)
            .and_cert_wake(cert_wake)
            .and_geoip(geoip),
    )
}

pub fn merged_router_with_wakes(
    store: PgStore,
    quota_wake: Arc<Notify>,
    grants_wake: Arc<Notify>,
    cert_wake: Arc<Notify>,
    geoip: crate::geoip::GeoIpLookup,
    agent_origin: String,
) -> Router {
    let state = AppState::with_agent_origin(store, quota_wake, agent_origin)
        .and_grants_wake(grants_wake)
        .and_cert_wake(cert_wake)
        .and_geoip(geoip);
    admin_router_with_state(state.clone()).merge(agent_routes().with_state(state))
}

/// How much of a response is read back in order to mask it. The largest thing a
/// masked viewer can ask for is the model snapshot of a whole fleet; artifacts,
/// which are the only unbounded bodies here, they cannot ask for at all.
const MASK_BODY_LIMIT: usize = 32 * 1024 * 1024;

/// Whether this role reads the console as a reviewer, which means the model for review rather
/// than the addresses for use.
fn role_masks_assets(role: AdminRole) -> bool {
    role == AdminRole::Readonly
}

/// Replace every asset identifier on the way out, for viewers who may not keep them.
///
/// A layer rather than a line in each handler. Per-endpoint masking is omitted from the next
/// endpoint added, and that failure is not visible: the page renders correctly while the
/// addresses are present in the response body. Every response passes through this one layer
/// instead.
///
/// Authentication is performed once by the outer `admin_auth_context` layer. Keeping the
/// identity in request-local state also makes masking fail closed: an authentication-store error
/// is answered before a handler can produce an unmasked body.
///
/// It hangs off the admin router alone. The agent surface must never be masked:
/// what it serves is the desired state a machine converges to, and an agent handed
/// `123.123.***.***` would write it into wg0 and take the backbone down.
async fn mask_assets(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if !response_is_json(&response) {
        return response;
    }
    let masked = request_admin().is_some_and(|admin| role_masks_assets(admin.role));
    if !masked {
        return response;
    }
    mask_response_body(response).await
}

/// What a visitor signed in as `public` may ask for: the read side of the console.
///
/// An allow-list of whole paths rather than a permission added to the role table. The roles
/// determine how much of the console an authenticated operator uses; this determines which
/// pages exist for the public account. Combining the two would give every future route an
/// inherited answer. As written, a route added later is closed to the public account until its
/// path is added here.
///
/// The list covers what the visitor-facing pages read: the model, the machines' agent state
/// and load, the per-machine traffic series, hop quality, the current revision's compile
/// output, and the read side of the users, tenants, quotas and usage pages.
/// Write methods never match: the method check above closes every non-GET to the public
/// account, and the deployments, settings, operator and artifact routes are excluded
/// entirely.
fn public_may(method: &axum::http::Method, path: &str) -> bool {
    // Signing out has to work, or a visitor cannot reach the login form to sign in as an
    // operator.
    if method == axum::http::Method::POST && path == "/auth/logout" {
        return true;
    }
    if method != axum::http::Method::GET {
        return false;
    }
    const PUBLIC_PATHS: &[&str] = &[
        "/healthz",
        "/auth/state",
        "/branding",
        "/whoami",
        "/model/snapshot",
        "/nodes/agent-state",
        "/revisions",
        "/load/nodes",
        "/usage/node-series",
        "/links/quality",
        "/links/mtu",
        "/links/health",
        "/probes/e2e",
        "/tenants",
        "/users",
        "/quotas",
        "/usage/samples",
        "/usage/monthly-summary",
    ];
    PUBLIC_PATHS.contains(&path)
        // `/compile/{revision}` and `/load/nodes/{node_id}`: the id is a path segment, so these
        // two cannot be written as exact strings.
        || path.starts_with("/compile/")
        || path.starts_with("/load/nodes/")
        // The response is the credential-free Serving authorization matrix. Executing it is a
        // POST to the same path and remains closed by the method gate above.
        || is_user_grant_probe_plan_path(path)
}

fn is_user_grant_probe_plan_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/users/") else {
        return false;
    };
    let mut parts = rest.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(tenant), Some(user), Some("grant-probes"), None) if !tenant.is_empty() && !user.is_empty()
    )
}

/// Hold the `public` account to the pages it is meant to open.
///
/// A layer, for the same reason as `mask_assets`: a rule repeated in every handler is omitted
/// from the next handler added, and here that failure exposes the users table to an anonymous
/// visitor. It sits outside the masking layer so a refused request is refused before a body
/// exists to mask.
///
/// The account is recognised by id. Its role, `readonly`, governs what it could do if it reached
/// a handler, so writes are refused twice, but the role cannot carry this rule: other
/// authenticated operators also hold `readonly`.
async fn public_scope(request: Request, next: Next) -> Response {
    let is_public = request_admin().is_some_and(|admin| admin.operator_id == PUBLIC_OPERATOR_ID);
    if is_public && !public_may(request.method(), request.uri().path()) {
        return ApiError::Forbidden.into_response();
    }
    next.run(request).await
}

/// Routes which establish or clear a session must remain reachable when the browser carries a
/// stale cookie. They authenticate their explicit request body (or revoke the supplied cookie)
/// themselves; all other admin routes use the one lookup below.
fn skips_admin_auth(method: &axum::http::Method, path: &str) -> bool {
    path == "/healthz"
        || path == "/auth/state"
        || (method == axum::http::Method::GET && path == "/branding")
        || (method == axum::http::Method::POST
            && matches!(path, "/auth/init" | "/auth/login" | "/auth/logout"))
}

async fn admin_auth_context(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if skips_admin_auth(request.method(), request.uri().path())
        || (bearer_token(request.headers()).is_none()
            && admin_session_cookie(request.headers()).is_none())
    {
        return REQUEST_ADMIN.scope(None, next.run(request)).await;
    }

    match authenticate_admin(&state, request.headers()).await {
        Ok(admin) => REQUEST_ADMIN.scope(Some(admin), next.run(request)).await,
        Err(error) => error.into_response(),
    }
}

fn response_is_json(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

async fn mask_response_body(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    // Every failure below returns an error instead of the original body. Returning a body
    // that could not be masked would defeat the layer. A reviewer who sees an error files a
    // bug report; a reviewer who sees real addresses reports nothing.
    let Ok(bytes) = axum::body::to_bytes(body, MASK_BODY_LIMIT).await else {
        return masking_failed("响应体太大，脱敏没做成");
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return masking_failed("响应体不是能解析的 JSON，脱敏没做成");
    };
    crate::mask::mask_json(&mut value);
    let Ok(masked) = serde_json::to_vec(&value) else {
        return masking_failed("脱敏后的响应体序列化失败");
    };
    // The length changed with the content, and a stale Content-Length truncates the
    // body at the client
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(masked))
}

fn masking_failed(detail: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": detail })),
    )
        .into_response()
}

fn admin_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/state", get(auth_state))
        .route("/auth/init", post(auth_init))
        .route("/auth/login", post(auth_login))
        .route("/auth/logout", post(auth_logout))
        .route("/whoami", get(whoami))
        // Public read: the login page must know its name and mark before a session exists. Writes
        // still require a system administrator in the handler below.
        .route("/branding", get(get_branding).put(update_branding))
        .route("/settings", get(get_settings).put(update_settings))
        // Its own route rather than a section of /settings: writing this one creates no revision
        // and triggers no release, and sharing a handler would give one PUT two halves with
        // different behavior, which leads to revisions being created for a domain typo.
        .route(
            "/distribution",
            get(get_distribution).put(update_distribution),
        )
        // Runtime disk-safety policy. Like distribution it compiles into no artifact and stamps
        // no revision, but the consumer is a running agent rather than an install command.
        .route(
            "/agent-log-policy",
            get(get_agent_log_policy).put(update_agent_log_default),
        )
        .route(
            "/agent-log-policy/nodes/{node_id}",
            put(update_node_log_policy),
        )
        // Alongside /distribution and for the same reason: no revision and no release. Separate
        // from it because the two are read on different schedules by different callers.
        // distribution is read when an operator installs a machine; this one is read on every
        // agent's own cycle.
        .route(
            "/agent-release",
            get(get_agent_release).put(update_agent_release),
        )
        // Certificates. The same family as the two above, with no revision and no release, but
        // with a worker behind them, which is why there is a third route: `scan` asks that worker
        // to run immediately rather than performing the work in the request.
        .route("/certs", get(get_certs))
        .route("/certs/domain", put(update_cert_domain))
        .route("/certs/scan", post(scan_certs))
        .route("/certs/groups", post(create_cert_group))
        .route(
            "/certs/groups/{label_id}",
            put(update_cert_group).delete(delete_cert_group),
        )
        .route("/certs/groups/{label_id}/spare", post(request_spare))
        .route(
            "/certs/certificates/{cert_id}/serve",
            post(serve_certificate),
        )
        .route("/revisions", get(list_revisions))
        .route(
            "/revisions/{revision_id}/discard-pending",
            post(discard_pending_changes),
        )
        .route("/model/snapshot", get(model_snapshot))
        // Two draft routes: the preview is read-only, executed normally and then rolled back, and
        // only the commit persists. The preview requires Edit rather than Read, because it takes
        // the real write path and authorization follows the write.
        .route("/model/preview", post(preview_draft))
        .route("/model/apply", post(apply_draft))
        .route("/model/preview/artifact", post(preview_draft_artifact))
        .route("/compile/{revision_id}", get(compile_revision))
        .route("/nodes/agent-state", get(node_agent_state))
        .route("/artifacts/index", get(artifact_index))
        .route(
            "/artifacts/content/{target_kind}/{target_id}/{artifact_kind}",
            get(artifact_content),
        )
        .route("/tenants", get(list_tenants).post(create_tenant))
        .route("/deployments/plan", post(plan_deployment))
        .route("/deployments/verify", post(verify_deployment))
        .route("/rollback", post(create_rollback_deployment))
        .route(
            "/deployments",
            get(list_deployments).post(create_deployment),
        )
        .route("/deployments/{deployment_id}", get(deployment_detail))
        .route(
            "/deployments/{deployment_id}/waves/{wave}/confirm",
            post(confirm_deployment_wave),
        )
        .route("/deployments/{deployment_id}/halt", post(halt_deployment))
        .route(
            "/deployments/{deployment_id}/cancel",
            post(cancel_deployment),
        )
        .route(
            "/deployments/{deployment_id}/cancel-rollback",
            post(cancel_deployment_and_rollback),
        )
        .route(
            "/deployments/{deployment_id}/targets/{node_id}/retry",
            post(retry_target),
        )
        .route("/nodes/provision", post(provision_node))
        .route("/nodes/{node_id}", put(update_node))
        .route("/nodes/{node_id}/status", put(update_node_status))
        .route("/nodes/{node_id}/lifecycle/abandon", post(abandon_node))
        .route("/nodes/{node_id}/cert-group", put(set_node_cert_group))
        .route("/nodes/{node_id}/agent-token", post(issue_node_token))
        .route("/nodes/{node_id}/agent-token", delete(revoke_node_token))
        .route("/users", get(list_users).post(create_user))
        .route(
            "/users/{tenant_id}/{user_id}/rotate-uuid",
            post(rotate_user_uuid),
        )
        .route(
            "/users/{tenant_id}/{user_id}/clash-subscription",
            get(clash_subscription_info),
        )
        .route(
            "/users/{tenant_id}/{user_id}/clash-subscription/haitun",
            post(issue_clash_haitun_subscription).delete(revoke_clash_haitun_subscription),
        )
        .route(
            "/users/{tenant_id}/{user_id}/status",
            put(update_user_status),
        )
        .route(
            "/users/{tenant_id}/{user_id}/grant-probes",
            get(user_grant_probe_plan).post(start_user_grant_probe),
        )
        .route("/grant-probes/capability", get(grant_probe_capability))
        .route(
            "/grant-probes/{probe_id}",
            get(grant_probe_status).delete(cancel_grant_probe),
        )
        .route("/grant-probes/{probe_id}/events", get(grant_probe_events))
        .route("/grants", post(upsert_grant))
        .route("/grants/automation", get(grant_automation_status))
        .route("/quotas", get(list_user_app_quotas).put(set_user_app_quota))
        .route("/apps", post(upsert_app))
        .route(
            "/tenants/{tenant_id}/tunnels/{outbound_id}/warp-bindings",
            post(register_warp_binding),
        )
        .route(
            "/tenants/{tenant_id}/tunnels/{outbound_id}/warp-bindings/{node_id}",
            put(update_warp_binding).delete(remove_warp_binding),
        )
        .route("/apps/{app_id}/chains", post(upsert_chain))
        .route("/apps/{app_id}/fronts", post(upsert_front))
        .route("/apps/{app_id}/ingresses", post(upsert_ingress))
        .route(
            "/apps/{app_id}/chains/{chain_id}/steps/{node_id}",
            delete(delete_step),
        )
        .route("/apps/{app_id}/chains/{chain_id}/prune", post(prune_chain))
        .route(
            "/admin/operators",
            get(list_admin_operators).post(create_admin_operator),
        )
        .route(
            "/admin/operators/{operator_id}/token",
            post(issue_admin_token),
        )
        .route(
            "/admin/operators/{operator_id}/token",
            delete(revoke_admin_token),
        )
        .route(
            "/admin/operators/{operator_id}/password",
            post(reset_admin_password),
        )
        .route("/admin/password", post(change_admin_password))
        .route("/usage/samples", get(list_usage_samples))
        .route("/usage/node-series", get(list_usage_node_series))
        .route("/usage/monthly-summary", get(usage_monthly_summary))
        .route("/load/nodes", get(load_nodes))
        .route("/load/nodes/{node_id}", get(load_node))
        // Under /links rather than /load: this is a property of a hop, and it sits next to
        // link_health and path MTU both in meaning and on the page that renders it.
        .route("/links/quality", get(link_quality))
        .route("/links/mtu", get(link_mtu))
        .route("/links/health", get(link_health))
        .route("/probes/e2e", get(e2e_probes))
        // Last, so it wraps every route above, including routes added later. See
        // `mask_assets`.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            mask_assets,
        ))
        // Outside the masking layer, so a route the public account may not have is refused
        // before there is a body to mask. See `public_scope`.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            public_scope,
        ))
        // Outermost: authenticate once, then make the same identity available to public-scope,
        // masking, and the handler authorization checks.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_context,
        ))
        .with_state(state)
}

pub fn agent_router(store: PgStore) -> Router {
    agent_router_with_origin(store, DEFAULT_AGENT_ORIGIN.to_owned())
}

/// The agent face on its own listener.
pub fn agent_router_with_origin(store: PgStore, agent_origin: String) -> Router {
    let state = AppState::with_agent_origin(store, Arc::new(Notify::new()), agent_origin);
    agent_routes()
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// Both faces on one listener, which is what a deployment gets unless it asks for them split.
///
/// Splitting them is a deployment decision rather than a property of the software: the two route
/// tables do not overlap, so one listener serves both unambiguously, and a fresh install then
/// needs one open port and one reverse-proxy entry instead of two. Where a split is wanted, such
/// as putting the console behind an allow-list while the agent face is reachable by the machines,
/// `main.rs` builds two listeners, which is where that choice belongs.
///
/// The console's own routes carry the `mask_assets` layer and the agent's do not. That holds
/// across the merge because `Router::layer` wraps the routes present when it is called; routes
/// merged in afterwards are unaffected. In the reverse order, every agent response would be read
/// back through the masking layer with no effect.
pub fn merged_router(store: PgStore, quota_wake: Arc<Notify>, agent_origin: String) -> Router {
    let state = AppState::with_agent_origin(store, quota_wake, agent_origin);
    // `/healthz` is the one path both faces answer, and merging two routers that each declare it
    // makes axum panic. The console's copy stands; `agent_routes` leaves it out.
    admin_router_with_state(state.clone()).merge(agent_routes().with_state(state))
}

/// The agent-facing routes, without `/healthz`. See `merged_router`.
fn agent_routes() -> Router<AppState> {
    Router::new()
        .route("/enroll/install.sh", get(install_script))
        .route("/enroll/dist", get(install_dist))
        .route("/brocade-agent/{arch}", get(agent_binary))
        .route("/sub/v1/{uuid}/clash.yaml", get(public_clash_subscription))
        .route(
            "/sub/v1/haitun/{token}/clash.yaml",
            get(public_haitun_clash_subscription),
        )
        .route("/agent/v1/enroll", post(agent_enroll))
        .route("/agent/v1/desired", get(agent_desired))
        .route("/agent/v1/observation", post(agent_observation))
        .route("/agent/v1/runtime", post(agent_runtime))
        .route("/agent/v1/agent-release", get(agent_release))
        .route("/agent/v1/usage", post(agent_usage))
        .route("/agent/v1/load", post(agent_load))
        .route("/agent/v1/link-probe", post(agent_link_probe))
        .route("/agent/v1/probe-targets", get(agent_probe_targets))
        .route("/agent/v1/link-health", post(agent_link_health))
        .route("/agent/v1/e2e-targets", get(agent_e2e_targets))
        .route("/agent/v1/e2e-probe", post(agent_e2e_probe))
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn public_clash_subscription(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    Query(query): Query<PublicClashSubscriptionQuery>,
    headers: HeaderMap,
) -> Response {
    // Reject malformed credentials before a database read, with the same public answer as a UUID
    // that does not exist. The raw value is never logged or included in an error.
    if !looks_like_uuid(&uuid) {
        return public_subscription_not_found();
    }
    if !subscription_rate_allowed(&state, &headers, &uuid) {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too many requests" })),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return subscription_no_store(response);
    }

    let Some(family) = public_subscription_family(&query) else {
        return public_subscription_not_found();
    };
    let Some(protocol) = public_subscription_protocol(&query) else {
        return public_subscription_not_found();
    };

    let subscription = match state
        .store
        .clash_subscription_by_uuid_filtered(&uuid, SubscriptionFilter { family, protocol })
        .await
    {
        Ok(subscription) => subscription,
        Err(StoreError::NotFound(_)) => return public_subscription_not_found(),
        Err(StoreError::Unavailable(_)) => return public_subscription_unavailable(),
        Err(error) => return subscription_no_store(ApiError::Store(error).into_response()),
    };

    public_clash_subscription_response(subscription)
}

async fn public_haitun_clash_subscription(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<PublicClashSubscriptionQuery>,
    headers: HeaderMap,
) -> Response {
    if !looks_like_uuid(&token) {
        return public_subscription_not_found();
    }
    if !subscription_rate_allowed(&state, &headers, &token) {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too many requests" })),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return subscription_no_store(response);
    }
    let Some(family) = public_subscription_family(&query) else {
        return public_subscription_not_found();
    };
    let Some(protocol) = public_subscription_protocol(&query) else {
        return public_subscription_not_found();
    };
    let subscription = match state
        .store
        .clash_subscription_by_haitun_token_filtered(
            &token,
            SubscriptionFilter { family, protocol },
        )
        .await
    {
        Ok(subscription) => subscription,
        Err(StoreError::NotFound(_)) => return public_subscription_not_found(),
        Err(StoreError::Unavailable(_)) => return public_subscription_unavailable(),
        Err(error) => return subscription_no_store(ApiError::Store(error).into_response()),
    };

    public_clash_subscription_response(subscription)
}

fn public_subscription_family(query: &PublicClashSubscriptionQuery) -> Option<Option<IpFamily>> {
    match query.family.as_deref() {
        None | Some("") | Some("both") => Some(None),
        Some("v4") => Some(Some(IpFamily::V4)),
        Some("v6") => Some(Some(IpFamily::V6)),
        Some(_) => None,
    }
}

fn public_subscription_protocol(
    query: &PublicClashSubscriptionQuery,
) -> Option<Option<SubscriptionProtocol>> {
    match query.protocol.as_deref() {
        None | Some("") | Some("both") => Some(None),
        Some("vless") => Some(Some(SubscriptionProtocol::Vless)),
        Some("hysteria2") => Some(Some(SubscriptionProtocol::Hysteria2)),
        Some(_) => None,
    }
}

fn public_clash_subscription_response(
    subscription: brocade_store::DynamicClashSubscription,
) -> Response {
    let filename = safe_filename_slug(&subscription.user_id);
    let mut response = subscription.content.into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/yaml; charset=utf-8"),
    );
    // `safe_filename_slug` limits this to an ASCII token. Keep it unquoted because some
    // subscription clients incorrectly preserve RFC-valid filename quotes as literal characters.
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename={filename}")) {
        response_headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response_headers.insert("profile-update-interval", HeaderValue::from_static("1"));
    let mut userinfo = format!(
        "upload={}; download={}",
        subscription.usage.upload_bytes, subscription.usage.download_bytes
    );
    if let Some(total) = subscription.usage.total_bytes {
        userinfo.push_str(&format!("; total={total}"));
    }
    if let Ok(value) = HeaderValue::from_str(&userinfo) {
        response_headers.insert("subscription-userinfo", value);
    }
    if let Ok(value) = HeaderValue::from_str(&subscription.usage.reset_at) {
        response_headers.insert("x-brocade-quota-reset-at", value);
    }
    if subscription.usage.has_gap {
        response_headers.insert("x-brocade-usage-gap", HeaderValue::from_static("true"));
    }
    subscription_no_store(response)
}

#[derive(Debug, Default, Deserialize)]
struct PublicClashSubscriptionQuery {
    family: Option<String>,
    protocol: Option<String>,
}

fn public_subscription_not_found() -> Response {
    subscription_no_store(
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "subscription not found" })),
        )
            .into_response(),
    )
}

fn public_subscription_unavailable() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "subscription temporarily unavailable" })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("15"));
    subscription_no_store(response)
}

fn subscription_no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(SUBSCRIPTION_CACHE_CONTROL),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response.headers_mut().remove(header::ETAG);
    response
}

fn subscription_rate_allowed(state: &AppState, headers: &HeaderMap, uuid: &str) -> bool {
    let source = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
        })
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "direct".to_owned());
    // The limiter needs a stable key but not the bearer credential itself. Hashing also ensures
    // a diagnostic dump of this operational map cannot disclose working subscription URLs.
    let key = sha256_hex(format!("{source}\0{uuid}").as_bytes());
    let minute = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60;
    let Ok(mut windows) = state.subscription_rate.lock() else {
        return false;
    };
    if windows.len() > 4096 {
        windows.retain(|_, window| window.minute + 1 >= minute);
    }
    let window = windows.entry(key).or_insert(SubscriptionRateWindow {
        minute,
        requests: 0,
    });
    if window.minute != minute {
        *window = SubscriptionRateWindow {
            minute,
            requests: 0,
        };
    }
    if window.requests >= SUBSCRIPTION_RATE_PER_MINUTE {
        return false;
    }
    window.requests += 1;
    true
}

fn looks_like_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn safe_filename_slug(value: &str) -> String {
    let slug = value
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if slug.is_empty() {
        "brocade".to_owned()
    } else {
        slug
    }
}

async fn auth_state(State(state): State<AppState>) -> ApiResult<Response> {
    let result = state.store.admin_auth_state().await?;
    Ok(Json(result).into_response())
}

#[derive(Debug, Serialize)]
struct InitAdminHttpResponse {
    admin: AuthenticatedAdmin,
    session_expires_at: String,
}

async fn auth_init(
    State(state): State<AppState>,
    Json(request): Json<AdminInitRequest>,
) -> ApiResult<Response> {
    let result = state.store.init_admin(request).await?;
    let cookie = session_cookie(&result.session.token);
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        Json(InitAdminHttpResponse {
            admin: result.admin,
            session_expires_at: result.session.expires_at,
        }),
    )
        .into_response())
}

#[derive(Debug, Serialize)]
struct LoginAdminHttpResponse {
    admin: AuthenticatedAdmin,
    session_expires_at: String,
}

async fn auth_login(
    State(state): State<AppState>,
    Json(request): Json<AdminLoginRequest>,
) -> ApiResult<Response> {
    let result = state.store.login_admin(request).await?;
    let cookie = session_cookie(&result.session.token);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(LoginAdminHttpResponse {
            admin: result.admin,
            session_expires_at: result.session.expires_at,
        }),
    )
        .into_response())
}

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let revoked = match admin_session_cookie(&headers) {
        Some(token) => state.store.revoke_admin_session(token).await?,
        None => false,
    };
    Ok((
        [(header::SET_COOKIE, expired_session_cookie())],
        Json(json!({ "revoked": revoked })),
    )
        .into_response())
}

/// The session, plus what this viewer is not allowed to see.
///
/// The flag exists so the front end can omit controls that would only be refused, such as an
/// artifact button that answers 403 or a share-link copy that produces nothing. The masking does
/// not depend on it: masking happens on the response path regardless of what the front end
/// renders.
#[derive(Debug, Serialize)]
struct WhoamiHttpResponse {
    #[serde(flatten)]
    admin: AuthenticatedAdmin,
    masked_assets: bool,
}

async fn whoami(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(WhoamiHttpResponse {
        masked_assets: role_masks_assets(admin.role),
        admin,
    })
    .into_response())
}

#[derive(Debug, Deserialize)]
struct RevisionQuery {
    revision: Option<u64>,
}

/// `GET /artifacts/content`'s query. The optional fields narrow a user subscription by network
/// family and/or protocol; absent dimensions retain their complete view.
#[derive(Debug, Deserialize)]
struct ArtifactContentQuery {
    revision: Option<u64>,
    family: Option<IpFamily>,
    protocol: Option<SubscriptionProtocol>,
    #[serde(default)]
    serving: bool,
}

#[derive(Debug, Deserialize)]
struct RevisionListQuery {
    limit: Option<u32>,
}

async fn list_revisions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RevisionListQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_revisions(&admin, query.limit.unwrap_or(50))
        .await?;
    Ok(Json(result).into_response())
}

async fn model_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RevisionQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let snapshot = state
        .store
        .redacted_snapshot(&admin, query.revision)
        .await?;
    Ok(Json(snapshot).into_response())
}

/// A draft's request body. Each of `ops` corresponds to an existing write interface; see
/// `brocade_store::ModelOp`.
#[derive(Debug, Deserialize)]
struct DraftRequest {
    #[serde(default)]
    ops: Vec<ModelOp>,
    #[serde(default)]
    note: Option<String>,
}

async fn preview_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DraftRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let preview = state.store.preview_draft(&admin, request.ops).await?;
    Ok(Json(preview).into_response())
}

/// The contents of one artifact within a draft. The selector matches the three segments of
/// `GET /artifacts/content`.
#[derive(Debug, Deserialize)]
struct DraftArtifactRequest {
    #[serde(default)]
    ops: Vec<ModelOp>,
    target_kind: String,
    target_id: String,
    artifact_kind: String,
}

async fn preview_draft_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DraftArtifactRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let content = state
        .store
        .preview_draft_artifact(
            &admin,
            request.ops,
            &request.target_kind,
            &request.target_id,
            &request.artifact_kind,
        )
        .await?;
    Ok(Json(content).into_response())
}

async fn apply_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DraftRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state
        .store
        .apply_draft(&admin, request.ops, request.note)
        .await?;
    // Waking on every committed draft is intentionally cheap.  The durable queue decides whether
    // the draft actually changed grants; duplicating that classification in the HTTP layer would
    // eventually drift from ModelOp.
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn compile_revision(
    State(state): State<AppState>,
    Path(revision_id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // `Read` rather than `ViewArtifacts`, despite the name. What this returns is the IR —
    // nodes, links, chains, hops and diagnostics — which is the source for the topology, the
    // chain list and the diagnostics badge. Refusing it would leave a reviewing role with an
    // empty console. The addresses inside it are structured JSON, so the masking layer covers
    // them; a rendered config file is what it cannot cover, and that is `artifact_content`
    // below.
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let output = state.store.compile_view(&admin, Some(revision_id)).await?;
    Ok(Json(output).into_response())
}

async fn node_agent_state(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let mut result = state.store.list_node_agent_states(&admin).await?;
    // Country is a best-effort decoration. The list remains usable when the configured database
    // is temporarily unreachable; GeoIpLookup holds retries to once an hour in that case.
    if let Ok(settings) = state.store.settings().await {
        let addresses = result
            .nodes
            .iter()
            .filter_map(|node| node.public_ipv4.clone())
            .collect::<Vec<_>>();
        let countries = state
            .geoip
            .countries(&settings.geodata.geoip_url, &addresses)
            .await;
        for node in &mut result.nodes {
            node.public_ipv4_country = node
                .public_ipv4
                .as_ref()
                .and_then(|address| countries.get(address))
                .cloned();
        }
    }
    Ok(Json(result).into_response())
}

async fn artifact_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RevisionQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state.store.artifact_index(&admin, query.revision).await?;
    Ok(Json(result).into_response())
}

async fn artifact_content(
    State(state): State<AppState>,
    Path((target_kind, target_id, artifact_kind)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<ArtifactContentQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    let filter = SubscriptionFilter {
        family: query.family,
        protocol: query.protocol,
    };
    let result = if query.serving {
        if target_kind != "user" || query.revision.is_some() {
            return Err(StoreError::InvalidData(
                "serving artifact must be a user artifact without an explicit revision".to_owned(),
            )
            .into());
        }
        state
            .store
            .serving_user_artifact_content(&admin, &target_id, &artifact_kind, filter)
            .await?
    } else {
        state
            .store
            .artifact_content(
                &admin,
                query.revision,
                &target_kind,
                &target_id,
                &artifact_kind,
                filter,
            )
            .await?
    };
    Ok(Json(result).into_response())
}

async fn list_tenants(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state.store.list_tenants(&admin).await?;
    Ok(Json(result).into_response())
}

async fn create_tenant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateTenantRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageTenants).await?;
    let result = state.store.create_tenant(&admin, request).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn get_branding(State(state): State<AppState>) -> ApiResult<Response> {
    Ok(Json(state.store.branding().await?).into_response())
}

async fn update_branding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(settings): Json<BrandingSettings>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(Json(state.store.update_branding(&admin, settings).await?).into_response())
}

async fn get_settings(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::Read).await?;
    let snapshot = state.store.settings_snapshot().await?;
    let mut response = Json(snapshot.settings).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", snapshot.revision_id))
            .expect("numeric revision is a valid ETag"),
    );
    Ok(response)
}

/// What is stored, plus what is in effect.
///
/// Both halves are needed and they are not the same: a deployment still running on env vars has
/// nothing stored, and a page showing only the stored values would present two empty boxes for a
/// domain that demonstrably works. `effective` is what the install command will actually say.
#[derive(Debug, Serialize)]
struct DistributionHttpResponse {
    stored: DistributionSettings,
    effective: DistributionSettings,
}

async fn distribution_response(state: &AppState) -> Result<DistributionHttpResponse, StoreError> {
    let effective = state.distribution().await?;
    Ok(DistributionHttpResponse {
        stored: state.store.distribution().await?,
        effective: DistributionSettings {
            agent_public_url: Some(effective.agent_public_url.clone()),
            xray_version: effective.xray_version.clone(),
        },
    })
}

async fn get_distribution(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(distribution_response(&state).await?).into_response())
}

async fn update_distribution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(settings): Json<DistributionSettings>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.store.update_distribution(&admin, settings).await?;
    // Read back rather than echo what was written: the store normalizes (a trailing slash goes,
    // blank clears), and the page has to show what took effect, not what was typed.
    Ok(Json(distribution_response(&state).await?).into_response())
}

async fn get_agent_log_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(state.store.agent_log_policy(&admin).await?).into_response())
}

async fn update_agent_log_default(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateAgentLogDefaultRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .update_agent_log_default(&admin, request)
        .await?;
    Ok(Json(state.store.agent_log_policy(&admin).await?).into_response())
}

async fn update_node_log_policy(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateNodeLogPolicyRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .update_node_log_policy(&admin, &node_id, request)
        .await?;
    Ok(Json(state.store.agent_log_policy(&admin).await?).into_response())
}

/// The recorded clearance, plus what this control plane is actually able to serve.
///
/// `available_release_id` is required. A clearance naming any other build serves nothing, which
/// is correct and produces no output, so without this field the console would show a release
/// enabled, the fleet unchanged, and no indication of the cause. The usual cause is that a new
/// control plane was deployed and the previous clearance no longer names a build present here.
///
/// The per-architecture shas are included because they are what an operator compares against when
/// a node reports an unexpected agent identity.
#[derive(Debug, Serialize)]
struct AgentReleaseHttpResponse {
    released: AgentRelease,
    available_release_id: &'static str,
    available_agents: Vec<AgentBuildHttpResponse>,
    /// Readable identification of this process and what it carries. The build id above only
    /// answers whether a machine is on this build.
    ///
    /// Console and agent versions are reported separately even though one workspace version
    /// currently supplies both, because they identify two different artifacts: the process
    /// serving the request, and the binary it would install on the fleet. A reader should not
    /// need to know they are compiled together.
    console_version: &'static str,
    agent_version: &'static str,
    /// The commit the *control plane* was built from, `unknown` outside a git checkout, with
    /// `-改动未提交` appended where the tree was dirty. It does not describe the agent's bytes:
    /// embedding a commit in those would make every documentation commit produce a new agent
    /// (`build.rs`, `describe_build`).
    build_commit: &'static str,
}

#[derive(Debug, Serialize)]
struct AgentBuildHttpResponse {
    arch: &'static str,
    sha256: &'static str,
}

/// The agents this process carries, recorded when a release is made.
///
/// `CARGO_PKG_VERSION` is the console's version and also the agent's: every crate inherits
/// `[workspace.package].version`, so the workspace has one number rather than six manifests with
/// no mechanism keeping them equal.
fn embedded_build_info() -> brocade_store::AgentBuildInfo<'static> {
    brocade_store::AgentBuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        commit: env!("BROCADE_AGENT_COMMIT"),
    }
}

async fn agent_release_response(state: &AppState) -> Result<AgentReleaseHttpResponse, StoreError> {
    let build = embedded_build_info();
    Ok(AgentReleaseHttpResponse {
        released: state.store.agent_release().await?,
        available_release_id: embedded_release_id(),
        available_agents: EMBEDDED_AGENTS
            .iter()
            .map(|(arch, _, sha256)| AgentBuildHttpResponse { arch, sha256 })
            .collect(),
        console_version: build.version,
        agent_version: build.version,
        build_commit: build.commit,
    })
}

async fn get_agent_release(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(agent_release_response(&state).await?).into_response())
}

/// `SystemAdmin`, matching `update_distribution` and for a stronger reason: what this authorizes
/// is every machine in the fleet replacing the binary it runs as root. An operator who can publish
/// a release already reaches every node's configuration, but not the code that applies it.
/// Everything the certificate page shows.
///
/// `sealing_available` is here because the page has to report that this control plane cannot
/// store a credential *before* one is entered and the save fails. The key it refers to is process
/// configuration rather than data, so no other route can report it.
#[derive(serde::Serialize)]
struct CertsResponse {
    sealing_available: bool,
    domain: Option<brocade_store::CertDomain>,
    /// The groups, each with its certificates and the machines drawing from it.
    groups: Vec<brocade_store::CertGroup>,
    /// What each machine reports holding. Separate from `groups` because it is per machine while
    /// a certificate is per group — during a roll the two disagree, and that disagreement is the
    /// thing worth showing.
    nodes: Vec<brocade_store::NodeCertificateState>,
    /// The two well-known ACME directories, so the form can offer both without either side
    /// hard-coding a value the other does not know.
    letsencrypt: &'static str,
    letsencrypt_staging: &'static str,
}

async fn certs_response(
    state: &AppState,
    admin: &brocade_store::AdminContext,
) -> Result<CertsResponse, StoreError> {
    // One domain is used today, while the table holds several. Taking the first makes it
    // explicit which one the page edits rather than merging them.
    let domain = state.store.cert_domains().await?.into_iter().next();
    let (groups, nodes) = if domain.is_some() {
        (
            state.store.cert_groups(admin).await?,
            state.store.node_certificate_state(admin).await?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(CertsResponse {
        sealing_available: brocade_store::secrets::sealing_available(),
        domain,
        groups,
        nodes,
        letsencrypt: brocade_store::ACME_LETSENCRYPT,
        letsencrypt_staging: brocade_store::ACME_LETSENCRYPT_STAGING,
    })
}

async fn get_certs(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    Ok(Json(certs_response(&state, &admin).await?).into_response())
}

#[derive(serde::Deserialize)]
struct CertGroupInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

async fn create_cert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CertGroupInput>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    // The group hangs off the one domain this page edits, so a caller never names it — and cannot
    // create a group under a domain that does not exist yet.
    let domain = state
        .store
        .cert_domains()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| StoreError::InvalidData("还没有配证书域，先在上面填好保存".to_owned()))?;
    let name = input.name.unwrap_or_default();
    let id = state
        .store
        .create_cert_label(&admin, &domain.id, &name, input.note.as_deref())
        .await?;
    Ok(Json(serde_json::json!({ "id": id })).into_response())
}

async fn update_cert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(label_id): Path<String>,
    Json(input): Json<CertGroupInput>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .update_cert_label(
            &admin,
            &label_id,
            input.name.as_deref(),
            input.note.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn delete_cert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(label_id): Path<String>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.store.delete_cert_label(&admin, &label_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Asks for one more certificate in this group, held as a spare until somebody serves it.
async fn request_spare(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(label_id): Path<String>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let id = state
        .store
        .request_spare_certificate(&admin, &label_id)
        .await?;
    Ok(Json(serde_json::json!({ "id": id })).into_response())
}

/// Makes a spare the one the group's machines present. The SNI does not change; the bytes do.
async fn serve_certificate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(cert_id): Path<String>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.store.promote_certificate(&admin, &cert_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Deserialize)]
struct NodeCertGroupInput {
    /// `null` takes the machine out of every group. It then has no certificate, and the compiler
    /// refuses any TLS or Hysteria 2 ingress on it — which is the correct state for a machine that
    /// serves neither.
    #[serde(default)]
    label_id: Option<String>,
}

/// Moves a machine to another certificate group, or out of all of them.
///
/// This changes the machine's SNI. Every subscription already handed out for its TLS and Hysteria
/// 2 ingresses names the old group and stops working — the console warns before calling this, and
/// nothing here second-guesses that decision.
async fn set_node_cert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Json(input): Json<NodeCertGroupInput>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .set_node_cert_label(&admin, &node_id, input.label_id.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn update_cert_domain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<brocade_store::CertDomainInput>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.store.upsert_cert_domain(&admin, input).await?;
    // Saving a domain is also when a wrong token gets corrected, so the worker is asked to retry
    // immediately rather than after the retry floor.
    state.cert_wake.notify_one();
    Ok(Json(certs_response(&state, &admin).await?).into_response())
}

async fn scan_certs(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state.cert_wake.notify_one();
    // Returns the current state rather than the scan's outcome: the scan takes about half a
    // minute per node, and holding the request open would tie the page to an unpredictable
    // duration. The page polls, and the rows report the result.
    Ok(Json(certs_response(&state, &admin).await?).into_response())
}

async fn update_agent_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(release): Json<AgentRelease>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    state
        .store
        .update_agent_release(&admin, release, embedded_build_info())
        .await?;
    // Read back rather than echo: the store trims and de-duplicates the node list, and the page
    // must show what took effect.
    Ok(Json(agent_release_response(&state).await?).into_response())
}

async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(value): Json<serde_json::Value>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let expected_revision = settings_if_match(&headers)?;
    require_complete_settings(&value)?;
    let settings: ModelSettings = serde_json::from_value(value)
        .map_err(|error| StoreError::InvalidData(format!("invalid settings: {error}")))?;
    let result = state
        .store
        .update_settings_at_revision(&admin, settings, expected_revision)
        .await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

fn settings_if_match(headers: &HeaderMap) -> ApiResult<u64> {
    let value = headers
        .get(header::IF_MATCH)
        .ok_or(ApiError::PreconditionRequired)?
        .to_str()
        .map_err(|_| ApiError::PreconditionRequired)?
        .trim();
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .parse()
        .map_err(|_| ApiError::PreconditionRequired)
}

fn require_complete_settings(value: &serde_json::Value) -> ApiResult<()> {
    value.as_object().ok_or_else(|| {
        ApiError::Store(StoreError::InvalidData(
            "settings must be a JSON object".to_owned(),
        ))
    })?;
    const REQUIRED: &[&[&str]] = &[
        &["reality_client", "min_client_ver"],
        &["reality_client", "max_client_ver"],
        &["reality_client", "max_time_diff_ms"],
        &["reality_site", "dest"],
        &["reality_site", "server_names"],
        &["reality_site", "fingerprint"],
        &["reality_site", "flow"],
        &["overlay", "keepalive_secs"],
        &["overlay", "mtu"],
        &["ports", "ingress_base"],
        &["ports", "hop_base"],
        &["ports", "hy2_base"],
        &["probe", "endpoint_url"],
        &["probe", "timeout_secs"],
        &["probe", "interval_secs"],
        &["geodata", "cron"],
        &["geodata", "geoip_url"],
        &["geodata", "geosite_url"],
        &["connection", "conn_idle_secs"],
        &["connection", "uplink_only_secs"],
        &["connection", "downlink_only_secs"],
        &["connection", "buffer_size_kb"],
        &["connection", "handshake_secs"],
        &["stats_user_online"],
    ];
    let missing: Vec<String> = REQUIRED
        .iter()
        .filter(|path| {
            path.iter()
                .try_fold(value, |current, key| current.get(*key))
                .is_none()
        })
        .map(|path| path.join("."))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Store(StoreError::InvalidData(format!(
            "settings request is missing required fields: {}",
            missing.join(", ")
        ))))
    }
}

/// The distribution manifest. The script asks here when it received no `--agent-bin-url` and the
/// like.
///
/// It exists for upgrades. `install_command` assembles these URLs into the command for a first
/// enrollment, but a later re-run of the script usually has only `--server`, because the original
/// command and its arguments are no longer available. Without the URLs the script keeps the
/// binary already on the machine, producing a new unit configured against an old binary and a
/// service that does not start.
///
/// Unauthenticated: it holds only URLs and sha256s, and install.sh and the binaries themselves are
/// public anyway.
///
/// The agent the control plane was built with, selected by architecture.
///
/// Unauthenticated, like `install.sh`: this is a public, verifiable build artifact. The install
/// script verifies it against the sha256 from `/enroll/dist`, and that sha comes from the same
/// build as these bytes.
///
/// Architecture names use `uname -m`'s vocabulary, because `uname -m` is what selects them. An
/// unrecognized architecture answers 404 with an explanatory message rather than a binary for
/// another architecture: such a file downloads, passes the sha check, installs, and does not run,
/// and `Exec format error` is several layers removed from the cause.
async fn agent_binary(Path(arch): Path<String>) -> Response {
    let Some((bytes, _)) = embedded_agent(&arch) else {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "这个控制面没带 {arch} 架构的 agent（带了：{}）。\n\
                 自己编一个，用 --agent-bin-url 指过去：\n  \
                 cargo build --release --target <triple> -p brocade-agent\n",
                EMBEDDED_AGENTS
                    .iter()
                    .map(|(name, ..)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
            .into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"brocade-agent\"",
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn install_dist(State(state): State<AppState>) -> ApiResult<Response> {
    Ok(Json(dist_json(&state.distribution().await?)).into_response())
}

/// The distribution manifest's contents. Separate from the handler so it can be tested without a
/// database. A mistyped key anywhere in this JSON has one symptom: the install script falls back
/// to the binary already on the machine while the console reports nothing.
fn dist_json(d: &AgentDistribution) -> serde_json::Value {
    // The embedded copies are listed per architecture and the script selects with `uname -m`. The
    // URL and the sha have to be supplied as a pair: one set of bytes checked against another's
    // sha always fails verification, and that failure is difficult to trace.
    let mut dist = json!({
        // The copy the operator configured explicitly. It takes precedence where configured, as
        // with a multi-arch fleet on a CDN or a fleet holding architectures the embedding does
        // not cover such as armv7. Unconfigured it is null and the script selects from those
        // below by architecture.
        "agent_bin_url": d.agent_binary_url,
        "agent_bin_sha256": d.agent_binary_sha256,
        "xray_bin_url": d.xray_binary_url,
        "xray_bin_sha256": d.xray_binary_sha256,
        "xray_version": d.xray_version,
        "phantun_server_url": d.phantun_server_url,
        "phantun_server_sha256": d.phantun_server_sha256,
        "phantun_client_url": d.phantun_client_url,
        "phantun_client_sha256": d.phantun_client_sha256,
    });

    // Flattened into `agent_bin_url_<arch>` rather than nested in an object: the install script
    // parses with sh and sed and handles only one `"key": "value"` per line. A nested object makes
    // it split half a JSON document on commas, and the result resembles a URL until curl rejects
    // it, which is harder to diagnose than a parse failure.
    for (arch, _, sha256) in EMBEDDED_AGENTS {
        dist[format!("agent_bin_url_{arch}")] =
            json!(format!("{}/brocade-agent/{arch}", d.agent_public_url));
        dist[format!("agent_bin_sha256_{arch}")] = json!(sha256);
    }
    dist
}

async fn install_script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        INSTALL_SCRIPT,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct PlanDeploymentRequest {
    revision_id: u64,
}

async fn plan_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PlanDeploymentRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let plan = state
        .store
        .plan_deployment(&admin, request.revision_id)
        .await?;
    Ok(Json(plan).into_response())
}

async fn verify_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<VerifyDeploymentRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state.store.verify_deployment(&admin, request).await?;
    Ok(Json(result).into_response())
}

async fn create_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<CreateDeploymentRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Publish).await?;
    request.actor = Some(admin.operator_id().to_owned());
    let result = state.store.create_deployment(&admin, request).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn create_rollback_deployment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<CreateRollbackRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    request.actor = Some(admin.operator_id().to_owned());
    let result = state
        .store
        .create_rollback_deployment(&admin, request)
        .await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

#[derive(Debug, Deserialize)]
struct ListDeploymentsQuery {
    limit: Option<u32>,
    // `config` or `grants`; absent means both. An unrecognized value is treated as absent,
    // because the list is a read-only endpoint and a 400 over a mistyped filter reads as an
    // outage.
    kind: Option<String>,
}

async fn list_deployments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListDeploymentsQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let kind = query.kind.as_deref().and_then(DeploymentKind::parse);
    let result = state
        .store
        .list_deployments(&admin, query.limit.unwrap_or(50), kind)
        .await?;
    Ok(Json(result).into_response())
}

#[derive(Debug, Deserialize)]
struct DeploymentDetailQuery {
    include: Option<String>,
}

impl DeploymentDetailQuery {
    fn include_content(&self) -> bool {
        self.include
            .as_deref()
            .is_some_and(|value| value.split(',').any(|part| part.trim() == "content"))
    }
}

async fn deployment_detail(
    State(state): State<AppState>,
    Path(deployment_id): Path<i64>,
    Query(query): Query<DeploymentDetailQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let permission = if query.include_content() {
        AdminPermission::SystemAdmin
    } else {
        AdminPermission::Read
    };
    let admin = require_admin_context(&state, &headers, permission).await?;
    let detail = state
        .store
        .deployment_detail(&admin, deployment_id, query.include_content())
        .await?;
    Ok(Json(detail).into_response())
}

async fn confirm_deployment_wave(
    State(state): State<AppState>,
    Path((deployment_id, wave)): Path<(i64, u32)>,
    headers: HeaderMap,
    Json(_request): Json<DeploymentWaveConfirmationRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Publish).await?;
    let result = state
        .store
        .confirm_deployment_wave(
            &admin,
            deployment_id,
            wave,
            Some(admin.operator_id().to_owned()),
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn halt_deployment(
    State(state): State<AppState>,
    Path(deployment_id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Publish).await?;
    let result = state.store.halt_deployment(&admin, deployment_id).await?;
    Ok(Json(result).into_response())
}

async fn cancel_deployment(
    State(state): State<AppState>,
    Path(deployment_id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Publish).await?;
    let result = state.store.cancel_deployment(&admin, deployment_id).await?;
    Ok(Json(result).into_response())
}

async fn cancel_deployment_and_rollback(
    State(state): State<AppState>,
    Path(deployment_id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state
        .store
        .cancel_deployment_and_rollback(&admin, deployment_id)
        .await?;
    Ok(Json(result).into_response())
}

// Discard the changes committed but never released. The path carries the revision the client
// holds as current, and the server verifies it, so a commit that arrived first fails this attempt
// rather than discarding the newer work.
//
// It requires system-admin: it voids a whole span of unreleased history at once, in the same
// category as cancel-and-roll-back. This one changes no machine, but discarding the wrong span
// still removes configuration an operator has to recover.
async fn discard_pending_changes(
    State(state): State<AppState>,
    Path(revision_id): Path<u64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state
        .store
        .discard_pending_changes(&admin, revision_id)
        .await?;
    Ok(Json(result).into_response())
}

async fn retry_target(
    State(state): State<AppState>,
    Path((deployment_id, node_id)): Path<(i64, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Publish).await?;
    let result = state
        .store
        .retry_target(&admin, deployment_id, &node_id)
        .await?;
    Ok(Json(result).into_response())
}

async fn provision_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProvisionNodeRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state.store.provision_node(&admin, request).await?;
    // A new machine has no certificate row until a scan creates one, and scans run an hour apart.
    // Waiting for one means an hour in which every TLS or Hysteria 2 ingress on this machine is
    // rejected at compile time (`ingress.tls-no-certificate`), while the wizard reports the
    // machine as online and nothing identifies what is missing. Self-signing reduces that wait to
    // milliseconds, so the worker is woken here rather than left to its schedule.
    state.cert_wake.notify_one();
    let dist = state.distribution().await?;
    Ok((
        StatusCode::CREATED,
        Json(ProvisionNodeHttpResponse::new(result, &dist)),
    )
        .into_response())
}

async fn update_node(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateNodeRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state.store.update_node(&admin, &node_id, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

/// Re-signing invalidates the token on the machine immediately, so the response carries a command
/// that installs the new token, rather than returning a token string and leaving the transfer to
/// a manual copy.
#[derive(Debug, Serialize)]
struct IssuedNodeTokenHttpResponse {
    node_id: String,
    token: String,
    token_prefix: String,
    install_command: String,
}

async fn issue_node_token(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state.store.issue_node_token(&node_id).await?;
    let dist = state.distribution().await?;
    let script_url = format!("{}/enroll/install.sh", dist.agent_public_url);
    let script_sha256 = install_script_sha256();
    let install_command = install_command(
        &dist,
        &script_url,
        &script_sha256,
        InstallCredential::NodeToken(&result.token),
    );
    Ok((
        StatusCode::CREATED,
        Json(IssuedNodeTokenHttpResponse {
            node_id: result.node_id,
            token: result.token,
            token_prefix: result.token_prefix,
            install_command,
        }),
    )
        .into_response())
}

async fn revoke_node_token(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::SystemAdmin).await?;
    let revoked = state.store.revoke_node_token(&node_id).await?;
    Ok(Json(json!({ "node_id": node_id, "revoked": revoked })).into_response())
}

async fn agent_enroll(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let token = bearer_token(&headers).ok_or(ApiError::Unauthorized)?;
    let result = state.store.redeem_node_enrollment(token).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn list_admin_operators(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageOperators).await?;
    let operators = state.store.list_admin_operators(&admin).await?;
    Ok(Json(json!({ "operators": operators })).into_response())
}

async fn create_admin_operator(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAdminOperatorRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageOperators).await?;
    let result = state.store.create_admin_operator(&admin, request).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn issue_admin_token(
    State(state): State<AppState>,
    Path(operator_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageOperators).await?;
    let result = state.store.issue_admin_token(&admin, &operator_id).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUserRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.create_user(&admin, request).await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

#[derive(Debug, Deserialize)]
struct UserListQuery {
    tenant_id: Option<String>,
    include_disabled: Option<bool>,
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserListQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_users(
            &admin,
            query.tenant_id.as_deref(),
            query.include_disabled.unwrap_or(false),
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn rotate_user_uuid(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state
        .store
        .rotate_user_uuid(&admin, &tenant_id, &user_id)
        .await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

#[derive(Debug, Serialize)]
struct ClashSubscriptionInfoResponse {
    url: String,
    urls: ClashSubscriptionUrlsResponse,
    template: &'static str,
    haitun: ClashHaitunSubscriptionInfoResponse,
    remaining_bytes: Option<u64>,
    reset_at: String,
    usage_has_gap: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ClashSubscriptionUrlsResponse {
    both: String,
    v4: String,
    v6: String,
}

#[derive(Debug, Serialize)]
struct ClashHaitunSubscriptionInfoResponse {
    template: &'static str,
    status: &'static str,
    urls: Option<ClashSubscriptionUrlsResponse>,
    created_at: Option<String>,
    revoked_at: Option<String>,
}

async fn clash_subscription_info(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    let origin = state
        .subscription_public_url
        .as_deref()
        .ok_or(ApiError::Unavailable(
            "BROCADE_SUBSCRIPTION_PUBLIC_URL is not configured",
        ))?;
    let subscription = state
        .store
        .clash_subscription_for_user(&admin, &tenant_id, &user_id)
        .await?;
    let haitun = state
        .store
        .clash_haitun_link_for_user(&admin, &tenant_id, &user_id)
        .await?;
    let url = format!("{origin}/sub/v1/{}/clash.yaml", subscription.uuid);
    Ok(Json(ClashSubscriptionInfoResponse {
        url: url.clone(),
        urls: clash_subscription_urls(url),
        template: "SubBoost 标准版",
        haitun: clash_haitun_subscription_info(origin, haitun.as_ref()),
        remaining_bytes: subscription.usage.remaining_bytes,
        reset_at: subscription.usage.reset_at,
        usage_has_gap: subscription.usage.has_gap,
    })
    .into_response())
}

async fn issue_clash_haitun_subscription(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let origin = state
        .subscription_public_url
        .as_deref()
        .ok_or(ApiError::Unavailable(
            "BROCADE_SUBSCRIPTION_PUBLIC_URL is not configured",
        ))?;
    // Do not mint a bearer that can only return 404. This performs the same serving-model and
    // effective-entry checks as opening the normal Clash subscription.
    state
        .store
        .clash_subscription_for_user(&admin, &tenant_id, &user_id)
        .await?;
    let link = state
        .store
        .issue_clash_haitun_link(&admin, &tenant_id, &user_id)
        .await?;
    Ok(Json(clash_haitun_subscription_info(origin, Some(&link))).into_response())
}

async fn revoke_clash_haitun_subscription(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let link = state
        .store
        .revoke_clash_haitun_link(&admin, &tenant_id, &user_id)
        .await?;
    // A revoked response has no URL, so this operation remains available even when the public
    // subscription origin was removed from a broken deployment.
    Ok(Json(ClashHaitunSubscriptionInfoResponse {
        template: "koipy 测速",
        status: "revoked",
        urls: None,
        created_at: Some(link.created_at),
        revoked_at: link.revoked_at,
    })
    .into_response())
}

fn clash_subscription_urls(url: String) -> ClashSubscriptionUrlsResponse {
    ClashSubscriptionUrlsResponse {
        both: url.clone(),
        v4: format!("{url}?family=v4"),
        v6: format!("{url}?family=v6"),
    }
}

fn clash_haitun_subscription_info(
    origin: &str,
    link: Option<&brocade_store::ClashHaitunLink>,
) -> ClashHaitunSubscriptionInfoResponse {
    let Some(link) = link else {
        return ClashHaitunSubscriptionInfoResponse {
            template: "koipy 测速",
            status: "not-created",
            urls: None,
            created_at: None,
            revoked_at: None,
        };
    };
    if link.revoked_at.is_some() {
        return ClashHaitunSubscriptionInfoResponse {
            template: "koipy 测速",
            status: "revoked",
            urls: None,
            created_at: Some(link.created_at.clone()),
            revoked_at: link.revoked_at.clone(),
        };
    }

    let url = format!("{origin}/sub/v1/haitun/{}/clash.yaml", link.token);
    ClashHaitunSubscriptionInfoResponse {
        template: "koipy 测速",
        status: "active",
        urls: Some(clash_subscription_urls(url)),
        created_at: Some(link.created_at.clone()),
        revoked_at: None,
    }
}

/// Decommissioning and restoring a node. It requires system-admin, because whether a machine is
/// on the network is a system boundary rather than tenant administration.
async fn update_node_status(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateNodeStatusRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state
        .store
        .update_node_status(&admin, &node_id, request)
        .await?;
    if result.lifecycle.phase == NodeLifecyclePhase::Retired {
        cleanup_retired_warp_bindings(&state, &node_id).await;
    }
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn abandon_node(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AbandonNodeRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let unregister_warp = request.unregister_warp;
    let result = state.store.abandon_node(&admin, &node_id, request).await?;
    if unregister_warp {
        cleanup_retired_warp_bindings(&state, &node_id).await;
    }
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn update_user_status(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<UpdateUserStatusRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state
        .store
        .update_user_status(&admin, &tenant_id, &user_id, request)
        .await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartGrantProbeRequest {
    /// Empty means every effective Serving entry. A single-row action sends exactly one id; ids
    /// are checked against the freshly projected plan and never interpreted as addresses.
    #[serde(default)]
    item_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StartGrantProbeResponse {
    job: crate::grant_probe::ProbeJobSnapshot,
    reused: bool,
}

#[derive(Debug, Serialize)]
struct GrantProbePlanResponse {
    serving_revision: u64,
    serving_generation: u64,
    items: Vec<GrantProbePlanItemResponse>,
}

#[derive(Debug, Serialize)]
struct GrantProbePlanItemResponse {
    id: String,
    name: String,
    app_id: String,
    app_name: String,
    chain_id: String,
    ingress_id: String,
    family: &'static str,
    protocol: &'static str,
}

async fn user_grant_probe_plan(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // This projection deliberately contains no endpoint, port or credential (the executable
    // target remains server-side), so readonly reviewers may inspect the effective Serving
    // matrix. POST below still requires ViewArtifacts because it actually uses those secrets.
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let plan = state
        .store
        .user_grant_probe_plan(&admin, &tenant_id, &user_id)
        .await?;
    Ok(Json(GrantProbePlanResponse {
        serving_revision: plan.serving_revision,
        serving_generation: plan.serving_generation,
        items: plan
            .items
            .into_iter()
            .map(|item| GrantProbePlanItemResponse {
                id: item.id,
                name: item.name,
                app_id: item.app_id,
                app_name: item.app_name,
                chain_id: item.chain_id,
                ingress_id: item.ingress_id,
                family: item.family,
                protocol: item.protocol,
            })
            .collect(),
    })
    .into_response())
}

async fn grant_probe_capability(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin(&state, &headers, AdminPermission::ViewArtifacts).await?;
    Ok(Json(state.grant_probes.capability()).into_response())
}

async fn start_user_grant_probe(
    State(state): State<AppState>,
    Path((tenant_id, user_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<StartGrantProbeRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    // This call is the server-side publication gate. It reads the same frozen Serving projection
    // as subscriptions and refuses open releases, queued grant sync, dirty runtime, and partial
    // settlement. The browser's disabled button is only presentation and is never trusted.
    let plan = state
        .store
        .user_grant_probe_plan(&admin, &tenant_id, &user_id)
        .await?;
    let (job, reused) = state
        .grant_probes
        .start_for_user(
            state.store.clone(),
            &tenant_id,
            &user_id,
            plan,
            &request.item_ids,
        )
        .await?;
    Ok((
        if reused {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(StartGrantProbeResponse { job, reused }),
    )
        .into_response())
}

async fn grant_probe_status(
    State(state): State<AppState>,
    Path(probe_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    let job = state
        .grant_probes
        .snapshot_for(&probe_id)
        .ok_or_else(|| ApiError::Store(StoreError::NotFound(format!("grant probe {probe_id}"))))?;
    admin.require_tenant_access(&job.tenant_id, "grant probe")?;
    Ok(Json(job).into_response())
}

async fn cancel_grant_probe(
    State(state): State<AppState>,
    Path(probe_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    let current = state
        .grant_probes
        .snapshot_for(&probe_id)
        .ok_or_else(|| ApiError::Store(StoreError::NotFound(format!("grant probe {probe_id}"))))?;
    admin.require_tenant_access(&current.tenant_id, "grant probe")?;
    let job = state
        .grant_probes
        .cancel(&probe_id)
        .ok_or_else(|| ApiError::Store(StoreError::NotFound(format!("grant probe {probe_id}"))))?;
    Ok(Json(job).into_response())
}

async fn grant_probe_events(
    State(state): State<AppState>,
    Path(probe_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ViewArtifacts).await?;
    let (initial, receiver) = state
        .grant_probes
        .subscribe(&probe_id)
        .ok_or_else(|| ApiError::Store(StoreError::NotFound(format!("grant probe {probe_id}"))))?;
    admin.require_tenant_access(&initial.tenant_id, "grant probe")?;
    let first = tokio_stream::once(Ok::<Event, Infallible>(probe_sse_event(&initial)));
    let updates = BroadcastStream::new(receiver).filter_map(|message| match message {
        Ok(snapshot) => Some(Ok::<Event, Infallible>(probe_sse_event(&snapshot))),
        // A lagged browser does not need every intermediate frame: each event is a complete
        // snapshot, and the next one catches it up. A closed sender ends the stream naturally.
        Err(_) => None,
    });
    let mut response = Sse::new(first.chain(updates))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response();
    // This endpoint normally passes through nginx's generic location, whose response buffering
    // is enabled. Without this header a complete snapshot can sit in the proxy buffer while the
    // browser keeps displaying the running state forever. Polling in the browser is a fallback,
    // not a reason to delay the event stream.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    Ok(response)
}

fn probe_sse_event(snapshot: &crate::grant_probe::ProbeJobSnapshot) -> Event {
    Event::default().event("snapshot").data(
        serde_json::to_string(snapshot)
            .unwrap_or_else(|_| r#"{"status":"failed","message":"结果无法编码"}"#.to_owned()),
    )
}

async fn upsert_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateGrantRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.upsert_grant(&admin, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn grant_automation_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(state.store.grant_automation_status().await?).into_response())
}

async fn list_user_app_quotas(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserListQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_user_app_quotas(&admin, query.tenant_id.as_deref())
        .await?;
    Ok(Json(result).into_response())
}

// A quota is an operational parameter rather than model state, so the requirement is Edit rather
// than Publish: changing one produces nothing to release. The release is a consequence, since
// lowering a quota revokes access and raising it restores access, and that step belongs to the
// quota loop. This endpoint only wakes it.
async fn set_user_app_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetUserAppQuotaRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.set_user_app_quota(&admin, request).await?;
    state.quota_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn upsert_app(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAppRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::SystemAdmin).await?;
    let result = state.store.upsert_app(&admin, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterWarpBindingHttpRequest {
    node_id: String,
    /// The compatible registration endpoint submits Cloudflare's application terms timestamp.
    /// Requiring an explicit acknowledgement keeps an operator action distinct from merely
    /// opening the drawer or previewing a draft.
    accept_terms: bool,
}

#[derive(Debug, Serialize)]
struct RegisterWarpBindingHttpResponse {
    revision_id: u64,
    binding: Value,
    /// What Cloudflare returned, for comparison with the editable endpoint on the tunnel. The
    /// existing endpoint is never overwritten: changing it is ordinary model editing and remains
    /// reviewable in a draft.
    suggested_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateWarpBindingHttpRequest {
    /// NULL/absent means inherit the logical tunnel default for that field. `allowed_ips` and
    /// `domain_strategy` are the two wire fields of one operator-facing address policy.
    #[serde(default)]
    endpoint_address: Option<String>,
    #[serde(default)]
    endpoint_port: Option<u16>,
    #[serde(default)]
    mtu: Option<u16>,
    #[serde(default)]
    keep_alive: Option<u16>,
    #[serde(default)]
    allowed_ips: Option<Vec<String>>,
    #[serde(default)]
    no_kernel_tun: Option<bool>,
    #[serde(default)]
    domain_strategy: Option<String>,
    #[serde(default)]
    workers: Option<u16>,
}

async fn register_warp_binding(
    State(state): State<AppState>,
    Path((tenant_id, outbound_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<RegisterWarpBindingHttpRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    if !request.accept_terms {
        return Err(ApiError::Store(StoreError::InvalidData(
            "申请 WARP 设备前需要确认 Cloudflare Application Terms，并知悉该 WireGuard 兼容注册接口不是官方 Brocade 集成"
                .to_owned(),
        )));
    }
    // Check this before contacting Cloudflare. Otherwise an absent sealing key creates a valid
    // remote device whose only local copy cannot be committed.
    if !brocade_store::secrets::sealing_available() {
        return Err(ApiError::Store(StoreError::InvalidData(
            "BROCADE_SECRET_KEY 未配置，不能安全保存 WARP 私钥和设备令牌".to_owned(),
        )));
    }

    // The committed, actor-scoped snapshot is the preflight boundary. WARP cannot bind a tunnel
    // which exists only in a browser draft: draft preview may be discarded, while the provider
    // registration cannot be rolled back with that transaction.
    let snapshot = state.store.redacted_snapshot(&admin, None).await?;
    let tunnels = snapshot
        .snapshot
        .get("external_outbounds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tunnel = tunnels
        .iter()
        .find(|tunnel| {
            tunnel.get("tenant").and_then(Value::as_str) == Some(tenant_id.as_str())
                && tunnel.get("id").and_then(Value::as_str) == Some(outbound_id.as_str())
        })
        .ok_or_else(|| {
            ApiError::Store(StoreError::NotFound(format!(
                "committed WARP tunnel {tenant_id}/{outbound_id} not found"
            )))
        })?;
    if tunnel.pointer("/protocol/t").and_then(Value::as_str) != Some("warp") {
        return Err(ApiError::Store(StoreError::InvalidData(format!(
            "tunnel {tenant_id}/{outbound_id} is not Cloudflare WARP"
        ))));
    }
    if tunnel
        .get("bindings")
        .and_then(Value::as_array)
        .is_some_and(|bindings| {
            bindings.iter().any(|binding| {
                binding.get("node").and_then(Value::as_str) == Some(request.node_id.as_str())
            })
        })
    {
        return Err(ApiError::Store(StoreError::InvalidData(format!(
            "WARP tunnel {tenant_id}/{outbound_id} is already bound to {}",
            request.node_id
        ))));
    }
    let node = snapshot
        .snapshot
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node.get("id").and_then(Value::as_str) == Some(request.node_id.as_str())
            })
        })
        .ok_or_else(|| {
            ApiError::Store(StoreError::NotFound(format!(
                "node {} not found or is outside your tenant scope",
                request.node_id
            )))
        })?;
    let node_name = node
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&request.node_id);

    let keys = brocade_store::generate_wireguard_keypair()?;
    let registration = crate::warp::register(&keys.public_key, node_name)
        .await
        .map_err(ApiError::Provider)?;
    let result = state
        .store
        .register_warp_binding(
            &admin,
            RegisterWarpBindingRequest {
                outbound_id,
                node_id: request.node_id,
                device_id: registration.device_id,
                account_id: registration.account_id,
                access_token: registration.access_token,
                private_key: keys.private_key,
                peer_public_key: registration.peer_public_key,
                local_addresses: registration.local_addresses,
                reserved: registration.reserved,
                note: None,
            },
        )
        .await?;
    Ok(Json(RegisterWarpBindingHttpResponse {
        revision_id: result.revision_id,
        binding: result.binding,
        suggested_endpoint: registration.suggested_endpoint,
    })
    .into_response())
}

async fn update_warp_binding(
    State(state): State<AppState>,
    Path((tenant_id, outbound_id, node_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<UpdateWarpBindingHttpRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state
        .store
        .update_warp_binding(
            &admin,
            UpdateWarpBindingRequest {
                tenant_id,
                outbound_id,
                node_id,
                endpoint_address: request.endpoint_address,
                endpoint_port: request.endpoint_port,
                mtu: request.mtu,
                keep_alive: request.keep_alive,
                allowed_ips: request.allowed_ips,
                no_kernel_tun: request.no_kernel_tun,
                domain_strategy: request.domain_strategy,
                workers: request.workers,
                note: None,
            },
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn remove_warp_binding(
    State(state): State<AppState>,
    Path((tenant_id, outbound_id, node_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    // Every reversible check happens before the provider call. Once Cloudflare has retired the
    // credential, removing the exact matching local row is the repair path even if another model
    // edit happens in the narrow interval between these two operations.
    let removal = state
        .store
        .prepare_warp_binding_removal(&admin, &tenant_id, &outbound_id, &node_id)
        .await?;
    crate::warp::unregister(&removal.device_id, &removal.access_token)
        .await
        .map_err(ApiError::Provider)?;
    let result = state
        .store
        .remove_warp_binding(
            &admin,
            RemoveWarpBindingRequest {
                tenant_id,
                outbound_id,
                node_id,
                expected_device_id: removal.device_id,
                note: None,
            },
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn upsert_chain(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateChainRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.upsert_chain(&admin, &app_id, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn upsert_front(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateFrontRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.upsert_front(&admin, &app_id, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn upsert_ingress(
    State(state): State<AppState>,
    Path(app_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateIngressRequest>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.upsert_ingress(&admin, &app_id, request).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn delete_step(
    State(state): State<AppState>,
    Path((app_id, chain_id, node_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state
        .store
        .delete_step(&admin, &app_id, &chain_id, &node_id)
        .await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

// Remove the unreachable steps on this chain. The console calls it once after the whole rule tree
// has been written table by table, because the test needs the chain's complete rule table and the
// browser holds only one table at a time (see prune_chain in store). A no-op is a valid outcome:
// on an already clean chain nothing is deleted and the revision number is unchanged.
async fn prune_chain(
    State(state): State<AppState>,
    Path((app_id, chain_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Edit).await?;
    let result = state.store.prune_chain(&admin, &app_id, &chain_id).await?;
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

async fn revoke_admin_token(
    State(state): State<AppState>,
    Path(operator_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageOperators).await?;
    let revoked = state.store.revoke_admin_token(&admin, &operator_id).await?;
    Ok(Json(json!({ "operator_id": operator_id, "revoked": revoked })).into_response())
}

/// An administrator setting a password on someone's behalf. The generated one-time password appears
/// in this response alone, on the same lines as signing a token.
async fn reset_admin_password(
    State(state): State<AppState>,
    Path(operator_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::ManageOperators).await?;
    let result = state
        .store
        .reset_admin_password(&admin, &operator_id)
        .await?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

/// A self-service password change: the signed-in operator changes their own password regardless
/// of role. Even `readonly` has to be able to replace a password that was set for them.
async fn change_admin_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ChangeAdminPasswordRequest>,
) -> ApiResult<Response> {
    let admin = require_admin(&state, &headers, AdminPermission::Read).await?;
    let revoked = state
        .store
        .change_admin_password(&admin.operator_id, admin_session_cookie(&headers), request)
        .await?;
    Ok(
        Json(json!({ "operator_id": admin.operator_id, "sessions_revoked": revoked }))
            .into_response(),
    )
}

async fn agent_desired(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    // Operational policy rides on the response header so it is present even when the node is
    // otherwise converged and the body is 204. Older agents ignore it; newer agents can change
    // retention without inventing a fake deployment or restarting Xray.
    let log_max_mib = state
        .store
        .effective_node_log_max_mib(&node.node_id)
        .await?;
    let agent_version = user_agent(&headers).map(str::to_owned);
    let protocol_version = agent_protocol_version(&headers);
    state
        .store
        .record_node_poll_with_protocol(
            &node.node_id,
            agent_version.as_deref(),
            protocol_version.and_then(|version| i32::try_from(version).ok()),
        )
        .await?;
    if let Some(route) = route_from_headers(&headers) {
        state
            .store
            .record_node_route_ips(&node.node_id, &route)
            .await?;
    }

    if protocol_version.unwrap_or(0) < brocade_deployment::protocol::MIN_AGENT_PROTOCOL_VERSION {
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            "x-brocade-agent-upgrade-required",
            HeaderValue::from_static("1"),
        );
        return Ok(with_agent_log_policy(response, log_max_mib));
    }

    // The certificate check runs before the claim: it is a read, the claim is a take, and a
    // failure after the take would 500 a node that has just been handed a deployment. A failure
    // here degrades to "no certificate owed" rather than erroring the whole response — one
    // malformed cert row must not block a deployment.
    let certificate = if node.lifecycle_phase == NodeLifecyclePhase::Active {
        match state.store.cert_delta_for_node(&node.node_id).await {
            Ok(certificate) => certificate,
            Err(error) => {
                eprintln!(
                    "证书：{node} 的证书差异判定失败（{error}），这一轮不带证书",
                    node = node.node_id
                );
                None
            }
        }
    } else {
        None
    };

    let response = match state.store.claim_desired_for_node(&node.node_id).await? {
        Some(mut desired) => {
            // The distribution source is filled in at this layer: it is configuration of the
            // runtime environment, and store should not know env exists.
            desired.phantun_binary = state.dist.phantun();
            Json(
                brocade_deployment::protocol::DesiredStateResponse::Deployment {
                    deployment: desired,
                    certificate,
                },
            )
            .into_response()
        }
        None => match certificate {
            Some(material) => {
                Json(brocade_deployment::protocol::DesiredStateResponse::Certificate(material))
                    .into_response()
            }
            None => StatusCode::NO_CONTENT.into_response(),
        },
    };
    Ok(with_agent_log_policy(response, log_max_mib))
}

fn with_agent_log_policy(mut response: Response, max_mib: u32) -> Response {
    response.headers_mut().insert(
        AGENT_LOG_MAX_MIB_HEADER,
        HeaderValue::from_str(&max_mib.to_string()).expect("u32 is a valid HTTP header value"),
    );
    response
}

/// Which agent this node should be running, if it should be running a different one.
///
/// # Why this is its own endpoint
///
/// Not folded into `/agent/v1/desired`, which answers 204 whenever the node is owed neither a
/// deployment nor a certificate. A machine that has not shipped for a month is exactly the
/// machine most likely to be running a stale agent, and attaching the answer to a release would
/// make it unavailable in exactly that case. This is the same error as tying runtime
/// reconciliation to deployments, which `agent_runtime` above records.
///
/// # What it answers with
///
/// 204 unless three conditions hold: a release is recorded, its scope reaches this node, and the
/// build it names is the one this control plane carries. The third is the one to note: the control
/// plane can serve only the agents compiled into it, so a clearance for any other build cannot be
/// acted on. That is what prevents a control-plane redeploy from also performing a fleet-wide
/// agent release (`embedded_release_id`).
///
/// The node's own architecture arrives in a header, because only the node knows it. An
/// architecture this control plane does not carry answers 204 rather than an error: the machine
/// has no fault and there is nothing here for it, and an error would appear on the console as a
/// failing node rather than an armv7 machine in an x86/ARM fleet.
///
/// The response deliberately repeats the sha256 the agent could also read from `/enroll/dist`.
/// That endpoint is unauthenticated, fleet-wide, and shaped for `install.sh`'s sed parser, so
/// sending the pair together here returns both the bytes and the per-node decision in one answer.
///
/// # Why this path ignores `BROCADE_AGENT_BIN_URL`
///
/// That override exists for `install.sh`, covering a CDN or a fleet holding architectures this
/// control plane cannot cross-compile, and it is a single URL and sha for every architecture. That
/// shape cannot express self-update, because two architectures never share a sha256. The recorded
/// release id is also computed from the *embedded* shas, so serving other bytes under it would
/// make the console report a build the fleet is not running. Machines outside the embedded
/// architectures therefore upgrade the way they were installed, through `install.sh`.
async fn agent_release(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    let release = state.store.agent_release().await?;
    let legacy_protocol = agent_protocol_version(&headers).unwrap_or(0)
        < brocade_deployment::protocol::MIN_AGENT_PROTOCOL_VERSION;
    if node.lifecycle_phase != NodeLifecyclePhase::Active && !legacy_protocol {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if !(release.offers(&node.node_id, embedded_release_id())
        || legacy_protocol && release.offers_protocol_rescue(&node.node_id))
    {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let Some(arch) = headers
        .get(AGENT_ARCH_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|arch| !arch.is_empty())
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let Some((_, sha256)) = embedded_agent(arch) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };

    let dist = state.distribution().await?;
    Ok(Json(BinarySource {
        url: format!("{}/brocade-agent/{arch}", dist.agent_public_url),
        sha256: sha256.to_owned(),
    })
    .into_response())
}

fn agent_protocol_version(headers: &HeaderMap) -> Option<u32> {
    headers
        .get(AGENT_PROTOCOL_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The runtime reconcile. Low-frequency, at probing's cadence, and separate from observations: an
/// observation carries a `deployment_id` and exists only during a release, whereas versions,
/// local reconciles and backlog matter most when nothing is being released. A machine that has
/// not deployed for a month is the one most likely to have drifted undetected.
async fn agent_runtime(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<NodeRuntimeReport>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    state
        .store
        .record_node_runtime(&node.node_id, &request)
        .await?;
    // Separate from `record_node_runtime`: that call writes the versions and process values a
    // node reports about itself, while this compares against something the control plane issued.
    // Keeping them apart lets a node with no certificate row, enrolled since the last scan,
    // report its runtime normally and update nothing here.
    match &request.certificate {
        brocade_deployment::protocol::CertificateObservation::Unmanaged => {}
        brocade_deployment::protocol::CertificateObservation::Absent => {
            state
                .store
                .record_certificate_observation(&node.node_id, "absent", None)
                .await?;
        }
        brocade_deployment::protocol::CertificateObservation::Present { sha256 } => {
            state
                .store
                .record_certificate_observation(&node.node_id, "present", Some(sha256))
                .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn agent_observation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AgentObservationRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    if let Some(route) = &request.route {
        state
            .store
            .record_node_route_ips(&node.node_id, route)
            .await?;
    }
    let result = state
        .store
        .report_target_result(TargetConvergenceReport {
            deployment_id: request.deployment_id,
            node_id: node.node_id.clone(),
            result: request.result,
            observed_before: request.observed_before,
            observed_after: request.observed_after,
            error: request.error,
        })
        .await?;
    if state
        .store
        .node_lifecycle(&node.node_id)
        .await
        .is_ok_and(|lifecycle| lifecycle.phase == NodeLifecyclePhase::Retired)
    {
        cleanup_retired_warp_bindings(&state, &node.node_id).await;
    }
    // A successful configuration report may satisfy the prerequisite of queued permission work.
    // Wake it now instead of making that node wait for the periodic recovery scan.
    state.grants_wake.notify_one();
    Ok(Json(result).into_response())
}

/// Provider cleanup is deliberately after the convergence transaction. A provider outage must not
/// make the agent retry a report which the control plane has already accepted; it becomes visible
/// lifecycle cleanup debt and can be retried independently.
async fn cleanup_retired_warp_bindings(state: &AppState, node_id: &str) {
    let actor = AdminContext::system_admin("system:node-retirement");
    let bindings = match state.store.retired_node_warp_bindings(node_id).await {
        Ok(bindings) => bindings,
        Err(error) => {
            let message = format!("cannot list WARP bindings for retirement cleanup: {error}");
            let _ = state
                .store
                .set_node_lifecycle_cleanup_error(node_id, Some(&message))
                .await;
            return;
        }
    };
    let mut errors = Vec::new();
    for (tenant_id, outbound_id) in bindings {
        let removal = match state
            .store
            .prepare_warp_binding_removal(&actor, &tenant_id, &outbound_id, node_id)
            .await
        {
            Ok(removal) => removal,
            Err(error) => {
                errors.push(format!("{outbound_id}: {error}"));
                continue;
            }
        };
        if let Err(error) = crate::warp::unregister(&removal.device_id, &removal.access_token).await
        {
            errors.push(format!("{outbound_id}: {error}"));
            continue;
        }
        if let Err(error) = state
            .store
            .remove_warp_binding(
                &actor,
                RemoveWarpBindingRequest {
                    tenant_id,
                    outbound_id: outbound_id.clone(),
                    node_id: node_id.to_owned(),
                    expected_device_id: removal.device_id,
                    note: Some(format!(
                        "remove WARP binding {outbound_id} after node {node_id} retired"
                    )),
                },
            )
            .await
        {
            errors.push(format!("{outbound_id}: {error}"));
        }
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    if let Err(store_error) = state
        .store
        .set_node_lifecycle_cleanup_error(node_id, error.as_deref())
        .await
    {
        eprintln!("退役清理：无法记录 {node_id} 的 WARP 清理状态：{store_error}");
    }
}

async fn agent_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UsageReportRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    // Retirement convergence samples one final round immediately before disabling Xray. The
    // token remains authenticated in `retiring`, so usage must remain open through that exact
    // phase; after completion authentication itself closes because retired nodes are excluded.
    if !matches!(
        node.lifecycle_phase,
        NodeLifecyclePhase::Active | NodeLifecyclePhase::Retiring
    ) {
        return Err(ApiError::Store(StoreError::Forbidden(format!(
            "node {} is {}; usage reporting is closed",
            node.node_id,
            node.lifecycle_phase.as_str()
        ))));
    }
    let result = state
        .store
        .record_usage_report(&node.node_id, request)
        .await?;
    Ok(Json(result).into_response())
}

/// Host load and per-hop link quality. As with usage, `node_id` comes from the token rather than
/// from whatever the body claims to be.
///
/// A 4xx here is final and the agent discards the round. There is no spool behind this endpoint,
/// for the reason given on `LoadReportResult`: a stale CPU reading has no value, and a retry
/// queue for it would be a file that can fill a disk.
async fn agent_load(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoadReportRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    let result = state
        .store
        .record_load_report(&node.node_id, request)
        .await?;
    Ok(Json(result).into_response())
}

/// Which endpoints to probe. The agent must not derive them from wireguard.conf itself: for a peer
/// behind phantun that file's `Endpoint` is by design local loopback, and probing it measures
/// ourselves (`ProbeTargetList`).
async fn agent_probe_targets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    Ok(Json(state.store.probe_targets(&node.node_id).await?).into_response())
}

/// Path-MTU probe reports. As with usage, `node_id` is derived from the token rather than trusting
/// whoever the body claims to be.
async fn agent_link_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LinkProbeRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    let result = state
        .store
        .record_link_probe(&node.node_id, request)
        .await?;
    Ok(Json(result).into_response())
}

/// Link-liveness reports. As with usage, `node_id` is derived from the token.
async fn agent_link_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LinkHealthRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    Ok(Json(
        state
            .store
            .record_link_health(&node.node_id, request)
            .await?,
    )
    .into_response())
}

/// Which chains to probe as their head. It takes its own endpoint like `agent_probe_targets`:
/// probing must keep running when nothing is being released, while desired returns 204 when there
/// is no work.
async fn agent_e2e_targets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    Ok(Json(state.store.e2e_probe_targets(&node.node_id).await?).into_response())
}

/// End-to-end probe reports. As with usage, `node_id` is derived from the token rather than
/// trusting whoever the body claims to be.
async fn agent_e2e_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<E2eProbeRequest>,
) -> ApiResult<Response> {
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    Ok(Json(state.store.record_e2e_probe(&node.node_id, request).await?).into_response())
}

/// The end-to-end probe overview. It requires only Read rather than system-admin as MTU does: a
/// chain is the tenant's own, whether their chain works is a fact they should see, and store
/// filters by tenant_scope already.
async fn e2e_probes(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(json!({ "chains": state.store.e2e_probes(&admin).await? })).into_response())
}

// These two read properties of the backbone (per-hop liveness, path MTU) that cannot be split along
// tenant lines. So the permission is only Read and the real gate is at the store layer, which tests
// visibility (an empty tenant_scope) rather than rank: a global read-only operator should see them,
// and a tenant-admin narrowed to one branch should not.
async fn link_health(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(json!({ "hops": state.store.link_health(&admin).await? })).into_response())
}

/// Every machine's recent load windows, for the list page's sparkline column.
///
/// `windows` rather than a single latest reading: one bar cannot show a trend, and the trend is
/// what the column exists to show. A machine whose load is rising is the finding; its current
/// percentage is not.
async fn load_nodes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LoadQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_node_load(&admin, query.windows.unwrap_or(24).min(240))
        .await?;
    Ok(Json(result).into_response())
}

async fn load_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Query(query): Query<LoadQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .node_load_view(&admin, &node_id, detail_load_windows(query.windows))
        .await?;
    Ok(Json(result).into_response())
}

/// Per-hop link quality, optionally narrowed to one chain.
async fn link_quality(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LinkQualityQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .hop_link_list(&admin, query.chain_id.as_deref())
        .await?;
    Ok(Json(result).into_response())
}

#[derive(Debug, Deserialize)]
struct LoadQuery {
    windows: Option<u32>,
}

/// 24 h / 30 s = 2,880 windows. This applies only to the single-machine endpoint; the fleet
/// endpoint remains capped at 240 so one request cannot multiply a day of detail by the fleet.
fn detail_load_windows(requested: Option<u32>) -> u32 {
    requested.unwrap_or(24).min(2_880)
}

#[derive(Debug, Deserialize)]
struct LinkQualityQuery {
    chain_id: Option<String>,
}

async fn link_mtu(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    Ok(Json(state.store.link_mtu_view(&admin).await?).into_response())
}

#[derive(Debug, Deserialize)]
struct UsageSamplesQuery {
    limit: Option<u32>,
    tenant_id: Option<String>,
    user_id: Option<String>,
    node_id: Option<String>,
}

async fn list_usage_samples(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageSamplesQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_usage_samples(
            &admin,
            query.limit.unwrap_or(100),
            query.tenant_id.as_deref(),
            query.user_id.as_deref(),
            query.node_id.as_deref(),
        )
        .await?;
    Ok(Json(result).into_response())
}

#[derive(Debug, Deserialize)]
struct UsageSeriesQuery {
    /// How long the series covers, in seconds. The default is 12 minutes, because the bar chart
    /// at the right of the machine list has 24 cells, one per 30-second reporting window.
    window_secs: Option<u32>,
    /// Machine detail narrows long ranges to one node. Omitted by fleet and usage pages.
    node_id: Option<String>,
}

/// The per-machine usage series plus the month total. The bar chart at the right of the machine
/// list and its month-to-date reading both read this endpoint.
async fn list_usage_node_series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageSeriesQuery>,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state
        .store
        .list_usage_node_series(
            &admin,
            query.window_secs.unwrap_or(720),
            query.node_id.as_deref(),
        )
        .await?;
    Ok(Json(result).into_response())
}

async fn usage_monthly_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let admin = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let result = state.store.list_monthly_usage_summary(&admin).await?;
    Ok(Json(result).into_response())
}

async fn authenticate_agent(store: &PgStore, headers: &HeaderMap) -> ApiResult<AuthenticatedNode> {
    let token = bearer_token(headers).ok_or(ApiError::Unauthorized)?;
    store
        .authenticate_node_token(token)
        .await?
        .ok_or(ApiError::Unauthorized)
}

fn require_active_agent(node: &AuthenticatedNode) -> ApiResult<()> {
    if node.lifecycle_phase != NodeLifecyclePhase::Active {
        return Err(ApiError::Store(StoreError::Forbidden(format!(
            "node {} is {}; business observations are closed",
            node.node_id,
            node.lifecycle_phase.as_str()
        ))));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum AdminPermission {
    Read,
    /// Reading a compiled artifact or a compile output: a wg config, an xray
    /// config, a share link.
    ///
    /// Separate from `Read` because these cannot be masked and remain useful: they consist
    /// largely of addresses, and a config with `***` in place of the endpoints cannot be
    /// reviewed either. The reviewing role is therefore refused them outright, which at
    /// least produces a definite answer.
    ViewArtifacts,
    Edit,
    Publish,
    ManageTenants,
    ManageOperators,
    SystemAdmin,
}

async fn require_admin_context(
    state: &AppState,
    headers: &HeaderMap,
    permission: AdminPermission,
) -> ApiResult<AdminContext> {
    let admin = require_admin(state, headers, permission).await?;
    Ok(AdminContext::from_authenticated(&admin))
}

async fn require_admin(
    _state: &AppState,
    _headers: &HeaderMap,
    permission: AdminPermission,
) -> ApiResult<AuthenticatedAdmin> {
    let admin = request_admin().ok_or(ApiError::Unauthorized)?;
    if admin_has_permission(admin.role, permission) {
        Ok(admin)
    } else {
        Err(ApiError::Forbidden)
    }
}

async fn authenticate_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<AuthenticatedAdmin> {
    if let Some(token) = bearer_token(headers) {
        return state
            .store
            .authenticate_admin_token(token)
            .await?
            .ok_or(ApiError::Unauthorized);
    }

    let token = admin_session_cookie(headers).ok_or(ApiError::Unauthorized)?;
    state
        .store
        .authenticate_admin_session(token)
        .await?
        .ok_or(ApiError::Unauthorized)
}

fn admin_has_permission(role: AdminRole, permission: AdminPermission) -> bool {
    match permission {
        AdminPermission::Read => matches!(
            role,
            AdminRole::Readonly
                | AdminRole::Editor
                | AdminRole::Publisher
                | AdminRole::TenantAdmin
                | AdminRole::SystemAdmin
        ),
        // The same set as Edit today, kept as its own arm deliberately: the two grant
        // different capabilities, namely changing the model and reading the addresses,
        // and a role that reviews without editing needs them separated.
        AdminPermission::ViewArtifacts | AdminPermission::Edit => {
            matches!(
                role,
                AdminRole::Editor
                    | AdminRole::Publisher
                    | AdminRole::TenantAdmin
                    | AdminRole::SystemAdmin
            )
        }
        AdminPermission::Publish => matches!(
            role,
            AdminRole::Publisher | AdminRole::TenantAdmin | AdminRole::SystemAdmin
        ),
        AdminPermission::ManageTenants => {
            matches!(role, AdminRole::TenantAdmin | AdminRole::SystemAdmin)
        }
        AdminPermission::ManageOperators => {
            matches!(role, AdminRole::TenantAdmin | AdminRole::SystemAdmin)
        }
        AdminPermission::SystemAdmin => role == AdminRole::SystemAdmin,
    }
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
}

fn route_from_headers(headers: &HeaderMap) -> Option<RouteIpReport> {
    let route = RouteIpReport {
        ipv4: route_header(headers, ROUTE_IPV4_HEADER, RouteHeaderFamily::V4),
        ipv6: route_header(headers, ROUTE_IPV6_HEADER, RouteHeaderFamily::V6),
    };
    (route.ipv4.is_some() || route.ipv6.is_some()).then_some(route)
}

#[derive(Debug, Clone, Copy)]
enum RouteHeaderFamily {
    V4,
    V6,
}

fn route_header(headers: &HeaderMap, name: &str, family: RouteHeaderFamily) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    let ip = value.parse::<IpAddr>().ok()?;
    match (family, ip) {
        (RouteHeaderFamily::V4, IpAddr::V4(_)) | (RouteHeaderFamily::V6, IpAddr::V6(_)) => {
            Some(value.to_owned())
        }
        _ => None,
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?.trim();
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn admin_session_cookie(headers: &HeaderMap) -> Option<&str> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for part in value.split(';') {
            let part = part.trim();
            let Some((name, value)) = part.split_once('=') else {
                continue;
            };
            if name == ADMIN_SESSION_COOKIE && !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// `Secure` is present only in release builds.
///
/// A browser stores a cookie carrying `Secure` only in a secure context. `localhost` and
/// `127.0.0.1` are secure contexts and are accepted over plain HTTP; a LAN address is not. When
/// debugging with the control plane bound to `0.0.0.0` and opened from another machine at
/// `http://192.168.x.x:8080`, the login returns 200, the browser discards the cookie, and every
/// later request answers 401.
///
/// Both functions have to use the same value. Setting the cookie without `Secure` and clearing it
/// with `Secure` gives the browser two different cookies, so signing out does not clear the one
/// that exists.
const COOKIE_SECURE: &str = if cfg!(debug_assertions) {
    ""
} else {
    " Secure;"
};

fn session_cookie(token: &str) -> String {
    format!(
        "{ADMIN_SESSION_COOKIE}={token}; Path=/; HttpOnly;{COOKIE_SECURE} SameSite=Lax; Max-Age={}",
        brocade_store::ADMIN_SESSION_TTL_SECONDS
    )
}

fn expired_session_cookie() -> String {
    format!("{ADMIN_SESSION_COOKIE}=; Path=/; HttpOnly;{COOKIE_SECURE} SameSite=Lax; Max-Age=0")
}

fn normalize_token(token: String) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

fn normalize_url(url: String) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        None
    } else {
        Some(url.to_owned())
    }
}

fn normalize_subscription_origin(url: String) -> Option<String> {
    let url = normalize_url(url)?;
    if url.starts_with("https://")
        || (cfg!(debug_assertions)
            && (url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")))
    {
        Some(url)
    } else {
        eprintln!(
            "忽略 BROCADE_SUBSCRIPTION_PUBLIC_URL：生产订阅入口必须使用 HTTPS（本地 debug 仅允许 localhost）"
        );
        None
    }
}

/// Which credential the install command carries: a one-time enrollment token for a first
/// enrollment, or a directly issued long-lived node token for token rotation or a reinstall.
enum InstallCredential<'a> {
    Enrollment(&'a str),
    NodeToken(&'a str),
}

fn install_command(
    dist: &AgentDistribution,
    script_url: &str,
    script_sha256: &str,
    credential: InstallCredential<'_>,
) -> String {
    let (environment, value) = match credential {
        InstallCredential::Enrollment(token) => ("BROCADE_ENROLL_TOKEN", token),
        InstallCredential::NodeToken(token) => ("BROCADE_NODE_TOKEN", token),
    };
    let mut command = format!(
        "curl -fsSL {} -o /tmp/brocade-install.sh && echo {} | sha256sum -c - && {environment}={} sudo --preserve-env={environment} sh /tmp/brocade-install.sh --server {}",
        shell_quote(script_url),
        shell_quote(&format!("{script_sha256}  /tmp/brocade-install.sh")),
        shell_quote(value),
        shell_quote(&dist.agent_public_url),
    );
    if let Some(url) = &dist.agent_binary_url {
        command.push_str(" --agent-bin-url ");
        command.push_str(&shell_quote(url));
    }
    if let Some(sha256) = &dist.agent_binary_sha256 {
        command.push_str(" --agent-bin-sha256 ");
        command.push_str(&shell_quote(sha256));
    }
    if let Some(url) = &dist.xray_binary_url {
        command.push_str(" --xray-bin-url ");
        command.push_str(&shell_quote(url));
    }
    if let Some(sha256) = &dist.xray_binary_sha256 {
        command.push_str(" --xray-bin-sha256 ");
        command.push_str(&shell_quote(sha256));
    }
    // Written into the command as well as the manifest. A machine reinstalled from a copied
    // command has to land on the same version as one installed from the manifest; otherwise the
    // fleet diverges one manual repair at a time, and the divergence is not visible until a
    // tunnel stops carrying traffic.
    if let Some(version) = &dist.xray_version {
        command.push_str(" --xray-version ");
        command.push_str(&shell_quote(version));
    }
    command
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn install_script_sha256() -> String {
    sha256_hex(INSTALL_SCRIPT.as_bytes())
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
enum ApiError {
    Store(StoreError),
    Provider(String),
    Unauthorized,
    Forbidden,
    PreconditionRequired,
    Unavailable(&'static str),
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message, reference) = match self {
            ApiError::Provider(message) => (StatusCode::BAD_GATEWAY, message, None),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_owned(), None),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_owned(), None),
            ApiError::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                "If-Match with the settings revision is required".to_owned(),
                None,
            ),
            ApiError::Unavailable(message) => {
                (StatusCode::SERVICE_UNAVAILABLE, message.to_owned(), None)
            }
            ApiError::Store(StoreError::NotFound(message)) => {
                (StatusCode::NOT_FOUND, message, None)
            }
            ApiError::Store(StoreError::Unauthorized(_)) => {
                (StatusCode::UNAUTHORIZED, "unauthorized".to_owned(), None)
            }
            ApiError::Store(StoreError::Forbidden(message)) => {
                (StatusCode::FORBIDDEN, message, None)
            }
            ApiError::Store(StoreError::Conflict(message)) => (StatusCode::CONFLICT, message, None),
            ApiError::Store(StoreError::Unavailable(message)) => {
                (StatusCode::SERVICE_UNAVAILABLE, message, None)
            }
            ApiError::Store(StoreError::InvalidData(message)) => {
                (StatusCode::BAD_REQUEST, message, None)
            }
            ApiError::Store(StoreError::Unsupported(message)) => {
                (StatusCode::CONFLICT, message, None)
            }
            ApiError::Store(error @ StoreError::PublishBlocked(_)) => {
                (StatusCode::CONFLICT, error.to_string(), None)
            }
            ApiError::Store(error) => {
                let reference = internal_error_reference();
                eprintln!("console internal error [{reference}]: {error}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_owned(),
                    Some(reference),
                )
            }
        };

        (
            status,
            Json(ErrorBody {
                error: message,
                reference,
            }),
        )
            .into_response()
    }
}

fn internal_error_reference() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format!(
        "{}-{seconds}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
}

/// The node install / upgrade script.
///
/// Its own file rather than an embedded string literal. 480 lines of shell inside Rust requires
/// escaping every `"`, matching heredoc terminators manually, loses syntax highlighting, and puts
/// the script out of reach of `sh -n`, so correctness can only be established by running it on a
/// real machine. That produced three incidents in one day: a dropped heredoc line, an unescaped
/// quote, and the wrong number of sed backslashes.
///
/// Split out, `templates/install.sh` is an ordinary script that `sh -n` and `shellcheck` read
/// directly.
const INSTALL_SCRIPT: &str = include_str!("../templates/install.sh");

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use axum::response::IntoResponse;
    use brocade_store::StoreError;

    use super::{
        bearer_token, detail_load_windows, dist_json, expired_session_cookie, install_command,
        looks_like_uuid, public_may, route_from_headers, safe_filename_slug, session_cookie,
        AgentDistribution, ApiError, ArtifactContentQuery, InstallCredential, IpFamily,
        SubscriptionProtocol, EMBEDDED_AGENTS, INSTALL_SCRIPT, SUBSCRIPTION_CACHE_CONTROL,
    };

    #[test]
    fn machine_load_range_reaches_one_day_but_never_exceeds_it() {
        assert_eq!(detail_load_windows(None), 24);
        assert_eq!(detail_load_windows(Some(2_880)), 2_880);
        assert_eq!(detail_load_windows(Some(u32::MAX)), 2_880);
    }

    /// The wire form of both subscription filters is pinned here because it belongs to the
    /// console's URL. Absent stays absent: every other artifact reader must keep receiving the
    /// complete subscription.
    #[test]
    fn the_artifact_filters_are_read_from_the_query_string() {
        let parse = |uri: &str| {
            axum::extract::Query::<ArtifactContentQuery>::try_from_uri(&uri.parse().unwrap())
                .map(|query| query.0)
        };

        assert!(matches!(
            parse("/artifacts/content/user/t:u/clash?family=v4")
                .unwrap()
                .family,
            Some(IpFamily::V4)
        ));
        assert!(matches!(
            parse("/artifacts/content/user/t:u/clash?family=v6")
                .unwrap()
                .family,
            Some(IpFamily::V6)
        ));
        assert!(parse("/artifacts/content/user/t:u/clash")
            .unwrap()
            .family
            .is_none());
        assert_eq!(
            parse("/artifacts/content/user/t:u/clash?revision=7")
                .unwrap()
                .revision,
            Some(7)
        );
        assert!(matches!(
            parse("/artifacts/content/user/t:u/clash?protocol=vless")
                .unwrap()
                .protocol,
            Some(SubscriptionProtocol::Vless)
        ));
        assert!(matches!(
            parse("/artifacts/content/user/t:u/clash?family=v4&protocol=hysteria2")
                .unwrap()
                .protocol,
            Some(SubscriptionProtocol::Hysteria2)
        ));
        assert!(
            parse("/artifacts/content/user/t:u/uri?serving=true")
                .unwrap()
                .serving
        );
        assert!(!parse("/artifacts/content/user/t:u/uri").unwrap().serving);
        // A misspelling is refused rather than read as both: the operator requested one family,
        // and returning every entry would be the opposite result.
        assert!(parse("/artifacts/content/user/t:u/clash?family=ipv4").is_err());
        assert!(parse("/artifacts/content/user/t:u/clash?protocol=hy2").is_err());
    }

    /// Setting and clearing have to carry the same attributes, or signing out does not clear the
    /// existing cookie. Whether `Secure` is present is determined by the build profile, and both
    /// sites change together. The assertion compares against `cfg!`, so it runs in both release
    /// and debug.
    #[test]
    fn session_cookies_carry_the_same_attributes_in_both_profiles() {
        let set = session_cookie("token-abc");
        let cleared = expired_session_cookie();
        for cookie in [&set, &cleared] {
            assert!(cookie.contains("Path=/"), "{cookie}");
            assert!(cookie.contains("HttpOnly"), "{cookie}");
            assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        }
        assert_eq!(
            set.contains("Secure"),
            cleared.contains("Secure"),
            "置入与清除的 Secure 不一致：\n{set}\n{cleared}"
        );
        assert_eq!(
            set.contains("Secure"),
            !cfg!(debug_assertions),
            "release 构建必须带 Secure，debug 构建必须不带：{set}"
        );
        assert!(set.contains("token-abc"));
        assert!(cleared.contains("Max-Age=0"));
    }

    /// The pages the public account is opened for have to load, and the rest of the console has
    /// to stay closed. Both directions are asserted here because the failure modes are opposite
    /// and neither is reported: too narrow renders empty boxes on the machines page that are not
    /// traced back to authorization, and too wide exposes the users table to an anonymous
    /// visitor. The visitor gets the read-only machine, link, user and usage pages; deployments
    /// and settings stay closed because every write path sits behind them.
    #[test]
    fn the_public_account_reaches_the_pages_it_is_opened_for_and_nothing_else() {
        use axum::http::Method;
        for path in [
            "/branding",
            "/whoami",
            "/model/snapshot",
            "/nodes/agent-state",
            "/revisions",
            "/compile/77",
            "/load/nodes",
            "/load/nodes/hk-01",
            "/usage/node-series",
            "/links/quality",
            "/links/health",
            "/probes/e2e",
            "/users",
            "/tenants",
            "/quotas",
            "/usage/samples",
            "/usage/monthly-summary",
            "/users/platform.acme/alice/grant-probes",
        ] {
            assert!(public_may(&Method::GET, path), "should allow GET {path}");
        }
        for path in [
            "/deployments",
            "/settings",
            "/distribution",
            "/admin/operators",
            "/artifacts/index",
        ] {
            assert!(!public_may(&Method::GET, path), "should refuse GET {path}");
        }
        // Signing out is the visitor's way to the login form, and it is the one write allowed.
        assert!(public_may(&Method::POST, "/auth/logout"));
        // Every other write, including on a path whose GET is allowed.
        assert!(!public_may(&Method::POST, "/model/apply"));
        assert!(!public_may(&Method::PUT, "/branding"));
        assert!(!public_may(&Method::PUT, "/nodes/hk-01"));
        assert!(!public_may(&Method::POST, "/nodes/provision"));
        assert!(!public_may(&Method::POST, "/revisions"));
        assert!(!public_may(
            &Method::POST,
            "/users/platform.acme/alice/grant-probes"
        ));
        // Do not let the dynamic suffix accidentally open a broader subtree.
        assert!(!public_may(
            &Method::GET,
            "/users/platform.acme/alice/grant-probes/p1"
        ));
        assert!(!public_may(&Method::GET, "/users//alice/grant-probes"));
    }

    #[tokio::test]
    async fn unexpected_store_errors_return_only_a_reference() {
        let response = ApiError::Store(StoreError::Sqlx(sqlx::Error::Protocol(
            "password=super-secret host=db.internal".to_owned(),
        )))
        .into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("internal server error"), "{text}");
        assert!(text.contains("reference"), "{text}");
        assert!(!text.contains("super-secret"), "{text}");
        assert!(!text.contains("db.internal"), "{text}");
    }

    #[test]
    fn install_script_preserves_mode_and_installs_binaries_atomically() {
        assert!(INSTALL_SCRIPT.contains("BROCADE_AGENT_APPLY=//p"));
        assert!(INSTALL_SCRIPT.contains("install_binary_atomic"));
        assert!(INSTALL_SCRIPT.contains("mv -f \"$stage\" \"$dest\""));
        assert!(INSTALL_SCRIPT.contains("-H \"@$auth_header\""));
        assert!(!INSTALL_SCRIPT.contains("-H \"Authorization: Bearer $ENROLL_TOKEN\""));
    }

    /// The install script verifies the bytes it downloads against the sha256 `/enroll/dist`
    /// reports. Any divergence between them costs the whole fleet its agent, and the symptom (a
    /// sha256 mismatch) is a whole deployment chain away from the cause (a build-time
    /// miscalculation). So this invariant is pinned to the compiled artifact rather than left to
    /// build.rs's restraint.
    #[test]
    fn the_advertised_sha256_is_computed_from_the_bytes_we_actually_serve() {
        use sha2::{Digest, Sha256};
        for (arch, bytes, sha256) in EMBEDDED_AGENTS {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            assert_eq!(&format!("{:x}", hasher.finalize()), sha256, "{arch}");
        }
    }

    /// What is embedded must be a real Linux executable.
    ///
    /// This guards against a build that succeeds while what it installs does not run. The
    /// `fetch_binary` note in `install.sh` records a failed download overwriting xray with 0
    /// bytes, whose symptom was `Exec format error`, which is worse than taking no action. The
    /// build side can produce the same result.
    #[test]
    fn the_embedded_agents_are_real_elves_for_the_arch_they_claim() {
        // e_machine in the ELF header (bytes 18-19, little endian), rather than the ELF magic
        // alone. Building both copies for the host architecture is the most common
        // cross-compilation error, and the result passes the sha check, installs, and fails only
        // when run on an ARM machine.
        const EM_X86_64: u16 = 62;
        const EM_AARCH64: u16 = 183;

        for (arch, bytes, _) in EMBEDDED_AGENTS {
            assert!(
                bytes.len() > 100_000,
                "{arch} 的 agent 只有 {} 字节，不像是一个编出来的二进制",
                bytes.len()
            );
            assert_eq!(&bytes[..4], b"\x7fELF", "{arch}");
            let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
            let want = match *arch {
                "x86_64" => EM_X86_64,
                "aarch64" => EM_AARCH64,
                other => panic!("没给 {other} 写 e_machine 判据"),
            };
            assert_eq!(
                machine, want,
                "{arch} 那份实际是给 e_machine={machine} 编的"
            );
        }
    }

    /// The embedded agent must contain no absolute path from the build machine.
    ///
    /// cargo gives rustc relative paths only where the source sits beneath the current directory
    /// and absolute ones otherwise, and those paths enter the binary through `file!()`. Once they
    /// do:
    ///
    /// - the build machine's directory structure is distributed to every node;
    /// - artifacts stop being reproducible. An operator's own
    ///   `cargo build --release -p brocade-agent --target <triple>` produces a sha that disagrees
    ///   with what the control plane reports, so nobody can verify that what it distributes is
    ///   the agent in the source. This binary runs as root on every node.
    ///
    /// The nested cargo in build.rs has to run at the workspace root; starting it in its own
    /// directory produces this.
    #[test]
    fn the_embedded_agents_carry_no_absolute_path_from_this_build_machine() {
        // The workspace root is two levels above this crate's directory, known at compile time.
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crates/brocade-console 之上一定有工作区根")
            .to_string_lossy()
            .into_owned();

        for (arch, bytes, _) in EMBEDDED_AGENTS {
            assert!(
                !bytes
                    .windows(workspace.len())
                    .any(|window| window == workspace.as_bytes()),
                "{arch} 的 agent 里印着构建机路径 {workspace}——\
                 build.rs 的嵌套 cargo 得在工作区根目录下跑，否则产物不可复现"
            );
        }
    }

    /// Every architecture must appear in the manifest, with its URL pointing at that
    /// architecture's own path.
    ///
    /// A mistyped key makes the install script fall back to the binary already on the machine,
    /// with the console reporting nothing while the machine runs the previous agent. This test
    /// pins the key names.
    #[test]
    fn the_dist_manifest_lists_a_url_and_sha_for_every_embedded_arch() {
        let dist = super::dist_json(&AgentDistribution {
            agent_public_url: "https://console.example.net:8443".to_owned(),
            ..AgentDistribution::default()
        });

        for (arch, _, sha256) in EMBEDDED_AGENTS {
            assert_eq!(
                dist[format!("agent_bin_url_{arch}")],
                format!("https://console.example.net:8443/brocade-agent/{arch}"),
                "{arch}"
            );
            assert_eq!(dist[format!("agent_bin_sha256_{arch}")], *sha256, "{arch}");
        }
        // With no override configured these two are null, on which the script falls back to
        // selecting by architecture
        assert!(dist["agent_bin_url"].is_null());
        assert!(dist["agent_bin_sha256"].is_null());
    }

    /// Parse it with `install.sh`'s own `dist_field`, verbatim.
    ///
    /// That fragment is `tr ',' '\n' | sed -n 's/.*"key" *: *"\([^"]*\)".*/\1/p'`, a
    /// comma-splitting approach tightly coupled to the JSON's structure: any nested object makes
    /// it extract half a value, and the result still resembles a URL. This test therefore runs
    /// the real fragment under a real sh rather than an equivalent written here.
    #[test]
    fn the_install_script_parser_can_actually_read_the_manifest() {
        let dist = super::dist_json(&AgentDistribution {
            agent_public_url: "https://console.example.net:8443".to_owned(),
            ..AgentDistribution::default()
        });
        let json = serde_json::to_string(&dist).unwrap();

        for (arch, _, sha256) in EMBEDDED_AGENTS {
            for (key, want) in [
                (
                    format!("agent_bin_url_{arch}"),
                    format!("https://console.example.net:8443/brocade-agent/{arch}"),
                ),
                (format!("agent_bin_sha256_{arch}"), (*sha256).to_owned()),
            ] {
                let script = format!(
                    r#"printf '%s' "$DIST_JSON" | tr ',' '\n' \
                       | sed -n 's/.*"{key}" *: *"\([^"]*\)".*/\1/p' | head -1"#
                );
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&script)
                    .env("DIST_JSON", &json)
                    .output()
                    .expect("跑得了 sh");
                let got = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                assert_eq!(got, want, "install.sh 解不出 {key}");
            }
        }
    }

    /// Without `BROCADE_AGENT_BIN_URL` configured the install command is still complete: the URL
    /// is assembled by the control plane and the script fetches from `/enroll/dist`. A configured
    /// value takes precedence, which a multi-arch fleet depends on.
    #[test]
    fn a_configured_binary_url_still_wins_over_the_embedded_one() {
        let embedded = AgentDistribution {
            agent_public_url: "https://console.example.net:8443".to_owned(),
            ..AgentDistribution::default()
        };
        assert!(embedded.agent_binary_url.is_none());

        let overridden = AgentDistribution {
            agent_public_url: "https://console.example.net:8443".to_owned(),
            agent_binary_url: Some("https://cdn.example.net/agent-arm64".to_owned()),
            agent_binary_sha256: Some("deadbeef".to_owned()),
            ..AgentDistribution::default()
        };
        let command = install_command(
            &overridden,
            "https://console.example.net:8443/enroll/install.sh",
            "abc",
            InstallCredential::Enrollment("broc_enroll_x"),
        );
        assert!(
            command.contains("--agent-bin-url 'https://cdn.example.net/agent-arm64'"),
            "{command}"
        );
    }

    #[test]
    fn install_command_keeps_credentials_out_of_child_process_arguments() {
        let dist = AgentDistribution {
            agent_public_url: "https://console.example.net:8443".to_owned(),
            ..AgentDistribution::default()
        };

        let enroll = install_command(
            &dist,
            "https://console.example.net:8443/enroll/install.sh",
            "abc",
            InstallCredential::Enrollment("broc_enroll_x"),
        );
        assert!(
            enroll.contains("BROCADE_ENROLL_TOKEN='broc_enroll_x'"),
            "{enroll}"
        );
        assert!(
            enroll.contains("--preserve-env=BROCADE_ENROLL_TOKEN"),
            "{enroll}"
        );
        assert!(!enroll.contains("--enroll-token"), "{enroll}");

        // After re-signing, the new token has to reach the machine, so this command takes
        // the environment rather than a --node-token argv entry.
        let rotate = install_command(
            &dist,
            "https://console.example.net:8443/enroll/install.sh",
            "abc",
            InstallCredential::NodeToken("broc_node_y"),
        );
        assert!(
            rotate.contains("BROCADE_NODE_TOKEN='broc_node_y'"),
            "{rotate}"
        );
        assert!(
            rotate.contains("--preserve-env=BROCADE_NODE_TOKEN"),
            "{rotate}"
        );
        assert!(!rotate.contains("--node-token"), "{rotate}");
    }

    /// The xray pin has to travel both channels. The manifest alone is not sufficient: an
    /// operator repairing a machine runs the command, and a command that omits the pin puts that
    /// machine on whatever upstream marks as newest, which is the version the pin exists to
    /// avoid.
    #[test]
    fn the_xray_pin_travels_in_both_the_command_and_the_manifest() {
        let pinned = AgentDistribution {
            agent_public_url: "https://a.example.net".to_owned(),
            xray_version: Some("v26.4.25".to_owned()),
            ..AgentDistribution::default()
        };
        let command = install_command(
            &pinned,
            "https://a.example.net/enroll/install.sh",
            "abc",
            InstallCredential::Enrollment("broc_enroll_x"),
        );
        assert!(
            command.contains("--xray-version 'v26.4.25'"),
            "装机命令要带上钉的版本：{command}"
        );
        assert_eq!(dist_json(&pinned)["xray_version"], "v26.4.25");

        // Unpinned, it has to be absent rather than empty. An empty `--xray-version ''` reaches
        // the script as a tag, and the download URL becomes
        // .../releases/download//Xray-linux-64.zip, a 404 that presents as a network fault rather
        // than a misconfiguration.
        let loose = AgentDistribution {
            agent_public_url: "https://a.example.net".to_owned(),
            ..AgentDistribution::default()
        };
        let command = install_command(
            &loose,
            "https://a.example.net/enroll/install.sh",
            "abc",
            InstallCredential::Enrollment("broc_enroll_x"),
        );
        assert!(!command.contains("--xray-version"), "{command}");
        assert_eq!(dist_json(&loose)["xray_version"], serde_json::Value::Null);
    }

    #[test]
    fn bearer_token_accepts_nonempty_bearer_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer broc_node_abc"),
        );

        assert_eq!(bearer_token(&headers), Some("broc_node_abc"));
    }

    #[test]
    fn bearer_token_rejects_missing_or_empty_value() {
        assert_eq!(bearer_token(&HeaderMap::new()), None);

        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer "));
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn route_headers_accept_matching_ip_families() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-brocade-route-ipv4",
            HeaderValue::from_static("198.51.100.10"),
        );
        headers.insert(
            "x-brocade-route-ipv6",
            HeaderValue::from_static("2001:db8::10"),
        );

        let route = route_from_headers(&headers).unwrap();
        assert_eq!(route.ipv4.as_deref(), Some("198.51.100.10"));
        assert_eq!(route.ipv6.as_deref(), Some("2001:db8::10"));
    }

    #[test]
    fn subscription_credentials_and_filenames_are_strictly_normalized() {
        assert!(looks_like_uuid("2d2304da-f114-4574-8d44-625afdb1db5c"));
        assert!(!looks_like_uuid(
            "2d2304da-f114-4574-8d44-625afdb1db5c/extra"
        ));
        assert!(!looks_like_uuid("not-a-uuid"));
        assert_eq!(safe_filename_slug("alice@example"), "alice-example");
        assert_eq!(safe_filename_slug("用户"), "brocade");
    }

    #[test]
    fn store_unauthorized_maps_to_http_401() {
        let response = ApiError::from(StoreError::Unauthorized("node token revoked".to_owned()))
            .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn store_unavailable_maps_to_http_503() {
        let response = ApiError::from(StoreError::Unavailable("release in progress".to_owned()))
            .into_response();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn publish_blocked_maps_to_http_409() {
        let response = ApiError::from(StoreError::PublishBlocked(
            brocade_core::compile::PublishBlocked {
                summary: brocade_core::diagnostic::DiagnosticSummary {
                    errors: 1,
                    warnings: 0,
                    infos: 0,
                    can_publish: false,
                },
                diagnostics: Vec::new(),
            },
        ))
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }

    // Static serving does not touch store, so a PgStore built with connect_lazy that never really
    // connects suffices to test route precedence.
    fn offline_store() -> brocade_store::PgStore {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://brocade:unused@127.0.0.1:1/brocade")
            .unwrap();
        brocade_store::PgStore::from_pool(pool)
    }

    #[tokio::test]
    async fn console_static_serves_index_without_shadowing_api_routes() {
        use axum::body::Body;
        use axum::http::{header, Request, StatusCode};
        use tower::ServiceExt;

        let router = super::with_console_static(super::admin_router(offline_store()));

        let index = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(index.status(), StatusCode::OK);
        // The header as well as the status: `immutable` on index.html makes a deployment
        // invisible to every browser that already has the page open.
        assert_eq!(index.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(
            index.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );

        let health = router
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let missing = router
            .oneshot(
                Request::get("/no-such-asset.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn public_subscription_errors_are_no_store_and_never_etagged() {
        use axum::body::Body;
        use axum::http::{header, Request, StatusCode};
        use tower::ServiceExt;

        let response = super::agent_router(offline_store())
            .oneshot(
                Request::get("/sub/v1/not-a-uuid/clash.yaml")
                    .header(header::IF_NONE_MATCH, "anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            SUBSCRIPTION_CACHE_CONTROL
        );
        assert_eq!(response.headers()[header::PRAGMA], "no-cache");
        assert!(!response.headers().contains_key(header::ETAG));
    }

    /// The escape hatch has to keep working, and nothing else exercises this branch: the deployed
    /// binary never takes it, so a regression would go undetected until it was needed during an
    /// incident.
    #[tokio::test]
    async fn console_static_dir_serves_the_directory_it_was_given() {
        use axum::body::Body;
        use axum::http::{header, Request, StatusCode};
        use tower::ServiceExt;

        let dist =
            std::env::temp_dir().join(format!("brocade-console-static-{}", std::process::id()));
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><title>另一份前端</title>",
        )
        .unwrap();
        std::fs::write(dist.join("assets/index-deadbeef.js"), "console.log(1)").unwrap();

        let router = super::with_console_static_dir(
            super::admin_router(offline_store()),
            dist.to_str().unwrap(),
        );

        let index = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(index.status(), StatusCode::OK);
        let body = axum::body::to_bytes(index.into_body(), 64 * 1024)
            .await
            .unwrap();
        // The directory's copy rather than the embedded one, which is the purpose of this branch.
        assert!(String::from_utf8_lossy(&body).contains("另一份前端"));

        let asset = router
            .clone()
            .oneshot(
                Request::get("/assets/index-deadbeef.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            asset.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );

        // Absent from the directory means absent: this branch does not fall back to the embedded
        // copy, or serving from disk would not be accurate.
        let missing = router
            .oneshot(
                Request::get("/no-such-asset.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        std::fs::remove_dir_all(&dist).ok();
    }
}
