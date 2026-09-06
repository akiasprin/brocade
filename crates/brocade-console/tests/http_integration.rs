use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use brocade_console::http::{
    admin_router, admin_router_with_wakes_and_realtime, agent_router, agent_router_with_origin,
    agent_router_with_origin_and_realtime, merged_router, with_console_static, EMBEDDED_XRAYS,
};
use brocade_console::realtime::{RealtimeBroadcast, RealtimeService};
use brocade_core::model::{ExternalOutboundProtocol, ExternalOutboundSecurity};
use brocade_store::{
    AdminContext, AdminInitRequest, CreateChainRequest, IssuedAdminToken, IssuedNodeToken, ModelOp,
    PgStore, PingProbeReportRequest, PingProbeSample, PingProbeSettings, PingProbeTarget,
    RegisterWarpBindingRequest, UpsertExternalOutboundRequest, ENROLLMENT_TOKEN_PREFIX,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "correct horse battery staple";

struct TestPg {
    _container: testcontainers::ContainerAsync<Postgres>,
    store: PgStore,
}

impl TestPg {
    async fn start_if_enabled() -> Option<Self> {
        if std::env::var("BROCADE_RUN_PG_TESTS").as_deref() != Ok("1") {
            eprintln!("skipping Postgres integration test; set BROCADE_RUN_PG_TESTS=1 to run");
            return None;
        }

        let container = Postgres::default()
            .with_tag("16-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres mapped port");
        let database_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let store = PgStore::connect(&database_url)
            .await
            .expect("connect postgres");
        Some(Self {
            _container: container,
            store,
        })
    }

    fn pool(&self) -> &PgPool {
        self.store.pool()
    }
}

async fn seed_subscription_serving(db: &TestPg) -> u64 {
    let revision = refresh_current_model_snapshot(db).await;
    sqlx::query(
        "INSERT INTO subscription_serving_state (
             id, topology_revision_id, permissions_revision_id, client_snapshot_id, generation
         ) VALUES (
             TRUE, $1, $1,
             (SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE),
             1
         )
         ON CONFLICT (id) DO UPDATE SET
             topology_revision_id = EXCLUDED.topology_revision_id,
             permissions_revision_id = EXCLUDED.permissions_revision_id,
             client_snapshot_id = EXCLUDED.client_snapshot_id,
             topology_deployment_id = NULL,
             permissions_deployment_id = NULL,
             generation = subscription_serving_state.generation + 1,
             updated_at = now()",
    )
    .bind(i64::try_from(revision).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    revision
}

/// Direct SQL keeps these HTTP fixtures compact, but agent work-list routes deliberately read the
/// immutable revision snapshot. Refresh that boundary explicitly after a fixture mutates model
/// tables so the test exercises the same coherent view as production commits.
async fn refresh_current_model_snapshot(db: &TestPg) -> u64 {
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let revision = snapshot.revision;
    sqlx::query(
        "INSERT INTO model_snapshots (revision_id, snapshot)
         VALUES ($1, $2)
         ON CONFLICT (revision_id) DO UPDATE SET snapshot = EXCLUDED.snapshot",
    )
    .bind(i64::try_from(revision).unwrap())
    .bind(serde_json::to_value(snapshot).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    revision
}

async fn admin_token(db: &TestPg) -> String {
    let initialized = db.store.admin_auth_state().await.unwrap().initialized;
    if initialized {
        db.store
            .issue_admin_token(
                &brocade_store::AdminContext::system_admin("test-admin"),
                "test-admin",
            )
            .await
            .unwrap()
            .token
    } else {
        db.store
            .init_admin(AdminInitRequest {
                operator_id: "test-admin".to_owned(),
                display_name: "Test Admin".to_owned(),
                password: ADMIN_PASSWORD.to_owned(),
            })
            .await
            .unwrap();
        // Initialization creates only the person and their password, signing no API token
        // (admin.rs::init_admin). Whoever wants a token takes /admin/operators/{id}/token.
        db.store
            .issue_admin_token(
                &brocade_store::AdminContext::system_admin("test-admin"),
                "test-admin",
            )
            .await
            .unwrap()
            .token
    }
}

async fn admin_app(db: &TestPg) -> (Router, String) {
    (admin_router(db.store.clone()), admin_token(db).await)
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_warp_binding_route_updates_every_machine_runtime_override() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    db.store
        .apply_draft(
            &AdminContext::system_admin("fixture"),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "warp".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Cloudflare WARP".to_owned(),
                    address: "engage.cloudflareclient.com".to_owned(),
                    port: 2408,
                    protocol: ExternalOutboundProtocol::Warp {
                        mtu: 1280,
                        keep_alive: 25,
                        allowed_ips: vec!["0.0.0.0/0".to_owned(), "::/0".to_owned()],
                        no_kernel_tun: true,
                        domain_strategy: "ForceIP".to_owned(),
                        workers: 0,
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    db.store
        .register_warp_binding(
            &AdminContext::system_admin("fixture"),
            RegisterWarpBindingRequest {
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                device_id: "device-n1".to_owned(),
                account_id: "account-n1".to_owned(),
                access_token: "provider-token".to_owned(),
                private_key: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
                peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
                local_addresses: vec!["172.16.0.2/32".to_owned()],
                reserved: vec![1, 2, 3],
                note: None,
            },
        )
        .await
        .unwrap();

    let (app, token) = admin_app(&db).await;
    let uri = "/tenants/platform.acme/tunnels/warp/warp-bindings/n1";
    let unauthorized = app
        .clone()
        .oneshot(
            Request::put(uri)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let unauthorized_delete = app
        .clone()
        .oneshot(Request::delete(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized_delete.status(), StatusCode::UNAUTHORIZED);

    let updated = app
        .clone()
        .oneshot(
            Request::put(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "endpoint_address": "162.159.193.10",
                        "endpoint_port": 500,
                        "mtu": 1420,
                        "keep_alive": 40,
                        "allowed_ips": ["::/0"],
                        "no_kernel_tun": false,
                        "domain_strategy": "ForceIPv6",
                        "workers": 4
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = response_json(updated).await;
    assert_eq!(updated["binding"]["endpoint_address"], "162.159.193.10");
    assert_eq!(updated["binding"]["endpoint_port"], 500);
    assert_eq!(updated["binding"]["mtu"], 1420);
    assert_eq!(updated["binding"]["keep_alive"], 40);
    assert_eq!(updated["binding"]["allowed_ips"], json!(["::/0"]));
    assert_eq!(updated["binding"]["no_kernel_tun"], false);
    assert_eq!(updated["binding"]["domain_strategy"], "ForceIPv6");
    assert_eq!(updated["binding"]["workers"], 4);

    let incomplete_policy = app
        .clone()
        .oneshot(
            Request::put(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "allowed_ips": ["0.0.0.0/0"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(incomplete_policy.status(), StatusCode::BAD_REQUEST);

    let blank_address = app
        .clone()
        .oneshot(
            Request::put(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "endpoint_address": "   ",
                        "endpoint_port": 500,
                        "mtu": 1420
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blank_address.status(), StatusCode::BAD_REQUEST);

    let cleared = app
        .oneshot(
            Request::put(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "endpoint_address": null,
                        "endpoint_port": null,
                        "mtu": null,
                        "keep_alive": null,
                        "allowed_ips": null,
                        "no_kernel_tun": null,
                        "domain_strategy": null,
                        "workers": null
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared = response_json(cleared).await;
    assert!(cleared["binding"]["endpoint_address"].is_null());
    assert!(cleared["binding"]["endpoint_port"].is_null());
    assert!(cleared["binding"]["mtu"].is_null());
    assert!(cleared["binding"]["keep_alive"].is_null());
    assert!(cleared["binding"]["allowed_ips"].is_null());
    assert!(cleared["binding"]["no_kernel_tun"].is_null());
    assert!(cleared["binding"]["domain_strategy"].is_null());
    assert!(cleared["binding"]["workers"].is_null());
}

/// Asking for the split still gets two separate route tables, and the agent face still learns its
/// own origin. The console's paths must not be reachable from it: that separation is the only
/// reason to run two listeners, so it is worth a test rather than an assumption.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn split_agent_router_keeps_its_own_origin_and_refuses_console_paths() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let agent = agent_router_with_origin(db.store.clone(), "http://10.0.0.7:9091".to_owned());

    let dist = agent
        .clone()
        .oneshot(Request::get("/enroll/dist").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(dist.status(), StatusCode::OK);
    let body = axum::body::to_bytes(dist.into_body(), 64 * 1024)
        .await
        .unwrap();
    let dist: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        dist["agent_bin_url_aarch64"],
        "http://10.0.0.7:9091/brocade-agent/aarch64"
    );
    assert_eq!(
        dist["xray_bin_url_aarch64"],
        "http://10.0.0.7:9091/brocade-xray/aarch64"
    );

    let xray = agent
        .clone()
        .oneshot(
            Request::get("/brocade-xray/aarch64")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(xray.status(), StatusCode::OK);
    let xray = to_bytes(xray.into_body(), usize::MAX).await.unwrap();
    let embedded = EMBEDDED_XRAYS
        .iter()
        .find(|(arch, ..)| *arch == "aarch64")
        .unwrap()
        .1;
    assert_eq!(xray.as_ref(), embedded);

    let unknown = agent
        .clone()
        .oneshot(
            Request::get("/brocade-xray/riscv64")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // Split off, the agent face is exactly the agent face — and with no SPA fallback either, so an
    // unknown path is a plain 404 rather than the console's index.html.
    for path in [
        "/auth/state",
        "/revisions",
        "/nodes/agent-state",
        "/nowhere",
    ] {
        let response = agent
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path} leaked");
    }

    // Both faces answer it; that is exactly why merging needs one of the two dropped.
    let health = agent
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn realtime_websocket_authenticates_leases_and_forwards_a_sample() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = db.store.issue_node_token("n1").await.unwrap().token;
    let service = RealtimeService::new(Default::default());
    let app = agent_router_with_origin_and_realtime(
        db.store.clone(),
        "http://127.0.0.1:8080".to_owned(),
        service.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mut request = format!("ws://{address}/agent/v1/realtime")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let first = socket.next().await.unwrap().unwrap();
    let first = serde_json::from_str::<brocade_deployment::protocol::AgentRealtimeCommand>(
        first.into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(
        first,
        brocade_deployment::protocol::AgentRealtimeCommand::Stop
    );

    let mut subscription = service.subscribe(["n1".to_owned()]).await;
    let start = socket.next().await.unwrap().unwrap();
    let start = serde_json::from_str::<brocade_deployment::protocol::AgentRealtimeCommand>(
        start.into_text().unwrap().as_str(),
    )
    .unwrap();
    assert_eq!(
        start,
        brocade_deployment::protocol::AgentRealtimeCommand::Start {
            interval_millis: 1000
        }
    );

    let sample = brocade_deployment::protocol::AgentRealtimeSample {
        sequence: 1,
        sampled_at_unix_millis: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        elapsed_millis: 1000,
        interface: "eth0".to_owned(),
        rx_bytes_per_sec: 12_345,
        tx_bytes_per_sec: 678,
        has_gap: false,
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&sample).unwrap().into(),
        ))
        .await
        .unwrap();
    let forwarded = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let RealtimeBroadcast::Sample(event) = subscription.events.recv().await.unwrap() {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(forwarded.node_id, "n1");
    assert_eq!(forwarded.sample.rx_bytes_per_sec, 12_345);
    assert!(
        forwarded.sample.has_gap,
        "first connection sample marks a gap"
    );

    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn realtime_settings_and_sse_create_one_bounded_node_lease() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = admin_token(&db).await;
    let service = RealtimeService::new(Default::default());
    let mut agent = service.register_agent("n1".to_owned()).await;
    assert_eq!(
        *agent.commands.borrow(),
        brocade_deployment::protocol::AgentRealtimeCommand::Stop
    );
    let app = admin_router_with_wakes_and_realtime(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        brocade_console::geoip::GeoIpLookup::default(),
        service,
    );

    let settings = app
        .clone()
        .oneshot(
            Request::get("/realtime/settings")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);
    assert_eq!(
        response_json(settings).await,
        json!({ "enabled": true, "interval_secs": 1 })
    );

    let invalid = app
        .clone()
        .oneshot(
            Request::put("/realtime/settings")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "enabled": true, "interval_secs": 3 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let updated = app
        .clone()
        .oneshot(
            Request::put("/realtime/settings")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "enabled": true, "interval_secs": 2 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(
        response_json(updated).await,
        json!({ "enabled": true, "interval_secs": 2 })
    );
    assert_eq!(
        *agent.commands.borrow(),
        brocade_deployment::protocol::AgentRealtimeCommand::Stop,
        "an idle websocket stays idle when only the global interval changes"
    );

    let events = app
        .clone()
        .oneshot(
            Request::get("/realtime/nodes/n1/events")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    assert_eq!(
        events.headers()["cache-control"],
        "no-store, no-cache, max-age=0"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), agent.commands.changed())
        .await
        .expect("SSE subscription should lease the node")
        .unwrap();
    assert_eq!(
        *agent.commands.borrow(),
        brocade_deployment::protocol::AgentRealtimeCommand::Start {
            interval_millis: 2000
        }
    );

    let mut body = events.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
        .await
        .expect("SSE should immediately send its snapshot")
        .expect("SSE body should contain one frame")
        .unwrap();
    let first = std::str::from_utf8(&first).unwrap();
    assert!(first.contains("event: snapshot"), "{first}");
    assert!(first.contains("\"interval_secs\":2"), "{first}");
    assert!(first.contains("\"node_id\":\"n1\""), "{first}");
    drop(body);

    sqlx::query("UPDATE node_lifecycle_state SET phase = 'retired' WHERE node_id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let retired = app
        .oneshot(
            Request::get("/realtime/nodes/n1/events")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        retired.status(),
        StatusCode::CONFLICT,
        "retired nodes retain history but may not create new live sampling leases"
    );
}

/// One listener carries both faces, which is what a deployment gets unless it asks for the split.
///
/// Three things can break here and none of them shows up as a compile error. `/healthz` is declared
/// by both faces and axum panics when a merge finds a duplicate, so merely constructing the router
/// is half the test. The console's routes carry the `mask_assets` layer and the agent's must not
/// inherit it. And the SPA fallback is attached last, so an agent path that stopped being a real
/// route would come back as index.html with a 200 rather than a 404 — passing a naive status check
/// while every machine in the fleet silently failed to enrol.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn merged_router_serves_both_faces_on_one_listener() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let app = with_console_static(merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "http://127.0.0.1:8080".to_owned(),
    ));

    // The console's own route.
    let auth_state = app
        .clone()
        .oneshot(Request::get("/auth/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(auth_state.status(), StatusCode::OK);

    // The agent's, on the same listener. It answers with the distribution table rather than the
    // SPA — checked by reading the body, because the fallback would also have returned 200.
    let dist = app
        .clone()
        .oneshot(Request::get("/enroll/dist").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(dist.status(), StatusCode::OK);
    let body = axum::body::to_bytes(dist.into_body(), 64 * 1024)
        .await
        .unwrap();
    let dist: serde_json::Value = serde_json::from_slice(&body).expect("agent route, not the SPA");
    // The value, not merely the key. This string is what gets pasted onto a new machine, and
    // threading the origin through is the whole of what this router does differently — asserting
    // the key exists would pass just as well with the origin dropped on the floor and every
    // enrolment pointing at a port nothing listens on.
    assert_eq!(
        dist["agent_bin_url_x86_64"],
        "http://127.0.0.1:8080/brocade-agent/x86_64"
    );
    assert_eq!(
        dist["xray_bin_url_x86_64"],
        "http://127.0.0.1:8080/brocade-xray/x86_64"
    );

    insert_node(db.pool()).await;
    let (_, admin_token) = admin_app(&db).await;
    let node_token = db.store.issue_node_token("n1").await.unwrap();
    let admin_credential_on_agent_route = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("x-brocade-protocol-version", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        admin_credential_on_agent_route.status(),
        StatusCode::UNAUTHORIZED
    );
    let node_credential_on_admin_route = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("authorization", format!("Bearer {}", node_token.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        node_credential_on_admin_route.status(),
        StatusCode::UNAUTHORIZED
    );

    // The console's own front end, from inside the binary, on the same listener as both faces.
    let index = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);

    // Claimed by nobody — not a route on either face and not a file. The static fallback goes on
    // last and must still answer 404 here: were it to hand back index.html with a 200, an agent
    // path that stopped being a real route would look healthy to every check that reads a status.
    let nowhere = app
        .oneshot(Request::get("/nowhere").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(nowhere.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn clash_subscription_route_serves_only_stable_releases_without_http_caching() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    // A fresh database intentionally has no borrowed site. Seed one here because this test is
    // about readonly masking, not the factory configuration.
    sqlx::query(
        "UPDATE control_state
         SET reality_dest = 'borrowed.example.net:443',
             reality_server_names = '[\"borrowed.example.net\"]'::jsonb",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET public_ipv6 = '2001:db8::10' WHERE id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();

    let uuid = "2d2304da-f114-4574-8d44-625afdb1db5c";
    let agent = agent_router(db.store.clone());
    let before_first_release = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        before_first_release.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(before_first_release.headers()["retry-after"], "15");

    seed_subscription_serving(&db).await;
    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml"))
                .header("if-none-match", "a-value-that-must-be-ignored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["cache-control"],
        "no-store, no-cache, max-age=0, must-revalidate"
    );
    assert_eq!(response.headers()["pragma"], "no-cache");
    assert_eq!(
        response.headers()["content-type"],
        "text/yaml; charset=utf-8"
    );
    assert_eq!(response.headers()["profile-update-interval"], "1");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=alice"
    );
    assert!(response.headers().contains_key("subscription-userinfo"));
    assert!(response.headers().contains_key("x-brocade-quota-reset-at"));
    assert!(!response.headers().contains_key("etag"));
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("# Brocade · SubBoost 标准版"));
    assert!(body.contains("name: \"Main Chain\""));
    assert!(body.contains("server: n1.example.net"));
    assert!(body.contains("server: 2001:db8::10"));
    assert!(
        body.contains("  skip-domain:\n    - \"www.example.com\""),
        "标准 Clash 订阅必须保护它自己使用的 servername：{body}"
    );

    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml?protocol=vless"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("type: vless"), "{body}");
    assert!(!body.contains("type: hysteria2"), "{body}");

    // This fixture publishes VLESS only. A valid Hysteria-only view is therefore an empty
    // subscription rather than an accidental fallback to all protocols.
    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml?protocol=hysteria2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("type: vless"), "{body}");

    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml?protocol=hy2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml?family=v4"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=alice"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("server: n1.example.net"));
    assert!(!body.contains("server: 2001:db8::10"));

    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml?family=v6"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=alice"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("server: n1.example.net"));
    assert!(body.contains("server: 2001:db8::10"));

    // The rename crosses the real HTTP commit boundary and advances only client config.
    let (admin, token) = admin_app(&db).await;
    let committed = admin
        .clone()
        .oneshot(
            Request::post("/model/apply")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "ops": [{
                            "op": "upsert_chain",
                            "app_id": "app-a1b2",
                            "chain": CreateChainRequest {
                                id: "chn-b2c3-d4e5".to_owned(),
                                tenant_id: "platform.acme".to_owned(),
                                name: "Live Rename".to_owned(),
                                subscription_country: None,
                                note: None,
                            }
                        }],
                        "note": "rename live chain"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(committed.status(), StatusCode::OK);
    let committed = response_json(committed).await;
    let renamed_revision = committed["revision_id"].as_u64().unwrap();
    assert_eq!(committed["client_config"]["status"], "activated");
    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml"))
                .header("if-none-match", "anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("name: \"Main Chain\""));
    assert!(body.contains("name: \"Live Rename\""));

    let deployment_id: i64 = sqlx::query_scalar(
        "INSERT INTO deployments (revision_id, status, active, kind, note)
         VALUES ($1, 'planned', TRUE, 'config', 'HTTP serving gate')
         RETURNING id",
    )
    .bind(i64::try_from(renamed_revision).unwrap())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let during_planned = agent
        .clone()
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(during_planned.status(), StatusCode::OK);
    assert_eq!(
        during_planned.headers()["cache-control"],
        "no-store, no-cache, max-age=0, must-revalidate"
    );
    assert!(!during_planned.headers().contains_key("retry-after"));
    let during_planned_body = String::from_utf8(
        to_bytes(during_planned.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(during_planned_body.contains("name: \"Live Rename\""));

    sqlx::query(
        "UPDATE deployments
            SET status = 'canceled', active = NULL, finished_at = now()
          WHERE id = $1",
    )
    .bind(deployment_id)
    .execute(db.pool())
    .await
    .unwrap();
    let response = agent
        .oneshot(
            Request::get(format!("/sub/v1/{uuid}/clash.yaml"))
                .header("if-none-match", "anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("name: \"Live Rename\""));

    let response = admin
        .clone()
        .oneshot(
            Request::get("/users/platform.acme/alice/clash-subscription")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let info = response_json(response).await;
    assert_eq!(info["template"], "SubBoost 标准版");
    assert_eq!(info["haitun"]["template"], "koipy 测速");
    assert_eq!(info["haitun"]["status"], "not-created");
    assert!(info["haitun"]["urls"].is_null());
    assert!(info["url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/sub/v1/{uuid}/clash.yaml")));
    assert_eq!(info["urls"]["both"], info["url"]);
    assert!(info["urls"]["v4"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/sub/v1/{uuid}/clash.yaml?family=v4")));
    assert!(info["urls"]["v6"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/sub/v1/{uuid}/clash.yaml?family=v6")));

    let unauthorized = admin
        .clone()
        .oneshot(
            Request::post("/users/platform.acme/alice/clash-subscription/haitun")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let issued = admin
        .clone()
        .oneshot(
            Request::post("/users/platform.acme/alice/clash-subscription/haitun")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(issued.status(), StatusCode::OK);
    let issued = response_json(issued).await;
    assert_eq!(issued["status"], "active");
    let haitun_url = issued["urls"]["both"].as_str().unwrap();
    let haitun_path = haitun_url
        .split_once("/sub/")
        .map(|(_, tail)| format!("/sub/{tail}"))
        .unwrap();
    assert!(issued["urls"]["v4"]
        .as_str()
        .unwrap()
        .ends_with("?family=v4"));
    assert!(issued["urls"]["v6"]
        .as_str()
        .unwrap()
        .ends_with("?family=v6"));

    let agent = agent_router(db.store.clone());
    let response = agent
        .clone()
        .oneshot(Request::get(&haitun_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["cache-control"],
        "no-store, no-cache, max-age=0, must-revalidate"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("# Brocade · koipy 测速"), "{body}");
    assert!(body.contains("name: \"Live Rename\""), "{body}");
    assert!(body.contains("name: \"koipy 测速\""), "{body}");
    assert!(!body.contains("rule-providers:"), "{body}");
    assert!(!body.contains("SubBoost"), "{body}");
    let response = agent
        .clone()
        .oneshot(
            Request::get(format!("{haitun_path}?family=v4"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("server: n1.example.net"), "{body}");
    assert!(!body.contains("server: 2001:db8::10"), "{body}");

    // Issuing an already active link is idempotent and must not invalidate a URL a bot may be
    // fetching at the same moment.
    let issued_again = admin
        .clone()
        .oneshot(
            Request::post("/users/platform.acme/alice/clash-subscription/haitun")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let issued_again = response_json(issued_again).await;
    assert_eq!(issued_again["urls"]["both"], issued["urls"]["both"]);

    let revoked = admin
        .clone()
        .oneshot(
            Request::delete("/users/platform.acme/alice/clash-subscription/haitun")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let revoked = response_json(revoked).await;
    assert_eq!(revoked["status"], "revoked");
    assert!(revoked["urls"].is_null());
    let old = agent
        .clone()
        .oneshot(Request::get(&haitun_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::NOT_FOUND);

    let reissued = admin
        .oneshot(
            Request::post("/users/platform.acme/alice/clash-subscription/haitun")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reissued.status(), StatusCode::OK);
    let reissued = response_json(reissued).await;
    assert_eq!(reissued["status"], "active");
    assert_ne!(reissued["urls"]["both"], issued["urls"]["both"]);
    let replacement_path = reissued["urls"]["both"]
        .as_str()
        .unwrap()
        .split_once("/sub/")
        .map(|(_, tail)| format!("/sub/{tail}"))
        .unwrap();
    let replacement = agent
        .clone()
        .oneshot(Request::get(replacement_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(replacement.status(), StatusCode::OK);
    let old = agent
        .oneshot(Request::get(&haitun_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_admin_init_login_and_logout_use_session_cookie() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let app = admin_router(db.store.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/auth/state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["initialized"], false);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/init")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operator_id": "admin",
                        "display_name": "Admin",
                        "password": ADMIN_PASSWORD
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let init_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(init_cookie.starts_with("brocade_session=broc_session_"));
    assert!(init_cookie.contains("HttpOnly"));
    // `Secure` only appears in release builds — in debug the console is often bound to 0.0.0.0, and
    // plaintext HTTP over a LAN IP is not a secure context, so a `Secure` cookie would be dropped by
    // the browser. The mapping between this value and the build type is pinned separately by
    // `session_cookies_carry_the_same_attributes_in_both_profiles` in http.rs.
    assert_eq!(init_cookie.contains("Secure"), !cfg!(debug_assertions));
    assert!(init_cookie.contains("SameSite=Lax"));
    let init_cookie_pair = init_cookie.split(';').next().unwrap().to_owned();
    let body = response_json(response).await;
    assert_eq!(body["admin"]["operator_id"], "admin");
    assert_eq!(body["admin"]["role"], "system-admin");
    // Initialization signs no API token: a browser needs nothing beyond the session cookie, and a
    // script wanting one signs it through /admin/operators/{id}/token. One token fewer is one
    // long-lived credential fewer.
    assert!(body.get("api_token").is_none());
    assert_eq!(body["admin"]["token_prefix"], serde_json::Value::Null);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/auth/state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["initialized"], true);

    let duplicate = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/init")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operator_id": "admin2",
                        "display_name": "Admin Two",
                        "password": ADMIN_PASSWORD
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::FORBIDDEN);

    let cookie_whoami = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", &init_cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cookie_whoami.status(), StatusCode::OK);
    let body = response_json(cookie_whoami).await;
    assert_eq!(body["operator_id"], "admin");

    // The Bearer route must still work; the token is simply signed separately rather than handed
    // out by init.
    let api_token = db
        .store
        .issue_admin_token(&brocade_store::AdminContext::system_admin("admin"), "admin")
        .await
        .unwrap()
        .token;
    let bearer_whoami = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("authorization", format!("Bearer {api_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bearer_whoami.status(), StatusCode::OK);

    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header("cookie", &init_cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    let expired_cookie = logout
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(expired_cookie.starts_with("brocade_session=;"));
    assert!(expired_cookie.contains("Max-Age=0"));
    let body = response_json(logout).await;
    assert_eq!(body["revoked"], true);

    let revoked_cookie_whoami = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", &init_cookie_pair)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_cookie_whoami.status(), StatusCode::UNAUTHORIZED);

    let wrong_login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operator_id": "admin",
                        "password": "not the password"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_login.status(), StatusCode::UNAUTHORIZED);

    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operator_id": "admin",
                        "password": ADMIN_PASSWORD
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login_cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body = response_json(login).await;
    assert_eq!(body["admin"]["operator_id"], "admin");

    let relogin_whoami = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", login_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(relogin_whoami.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_agent_desired_authenticates_node_token_and_records_poll() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());

    let agent_admin_route = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(agent_admin_route.status(), StatusCode::NOT_FOUND);

    let admin_agent_route = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(admin_agent_route.status(), StatusCode::NOT_FOUND);

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let issued: IssuedNodeToken = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(issued.node_id, "n1");

    let legacy = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        legacy.headers().get("x-brocade-log-max-mib").unwrap(),
        "100"
    );
    assert_eq!(
        legacy
            .headers()
            .get("x-brocade-agent-journal-max-mib")
            .unwrap(),
        "100"
    );
    assert_eq!(
        legacy.headers().get("x-brocade-xray-log-max-mib").unwrap(),
        "100"
    );
    assert_eq!(
        legacy
            .headers()
            .get("x-brocade-phantun-log-max-mib")
            .unwrap(),
        "100"
    );
    assert_eq!(
        legacy
            .headers()
            .get("x-brocade-agent-upgrade-required")
            .unwrap(),
        "1"
    );

    let unauthorized = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let desired = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("user-agent", "brocade-agent-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(desired.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        desired.headers().get("x-brocade-log-max-mib").unwrap(),
        "100"
    );
    assert_eq!(
        desired
            .headers()
            .get("x-brocade-agent-journal-max-mib")
            .unwrap(),
        "100"
    );
    assert_eq!(
        desired.headers().get("x-brocade-xray-log-max-mib").unwrap(),
        "100"
    );
    assert_eq!(
        desired
            .headers()
            .get("x-brocade-phantun-log-max-mib")
            .unwrap(),
        "100"
    );
    let protocol: Option<i32> = sqlx::query_scalar(
        "SELECT agent_protocol_version FROM node_agent_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(protocol, Some(1));

    assert!(db.store.revoke_node_token("n1").await.unwrap());
    let revoked = agent
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    let row = sqlx::query(
        "SELECT token_last_used_at IS NOT NULL AS used,
                last_poll_at IS NOT NULL AS polled,
                agent_version
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(row.try_get::<bool, _>("used").unwrap());
    assert!(row.try_get::<bool, _>("polled").unwrap());
    assert_eq!(
        row.try_get::<Option<String>, _>("agent_version").unwrap(),
        Some("brocade-agent-test".to_owned())
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_node_provision_returns_install_command_and_agent_enrolls_once() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());

    let missing_on_agent = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/provision")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(provision_body("n-http").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_on_agent.status(), StatusCode::NOT_FOUND);

    let missing_on_admin = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/enroll/install.sh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_on_admin.status(), StatusCode::NOT_FOUND);

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/provision")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(provision_body("n-http").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["revision_id"], 2);
    assert_eq!(body["node"]["id"], "n-http");
    assert_eq!(body["node"]["overlay_addr"], "10.66.0.1");
    assert!(body["node"].get("wg_private_key").is_none());
    let enrollment_token = body["enrollment"]["token"].as_str().unwrap();
    assert!(enrollment_token.starts_with(ENROLLMENT_TOKEN_PREFIX));
    assert_eq!(
        body["enrollment"]["script_sha256"].as_str().unwrap().len(),
        64
    );
    assert!(body["enrollment"]["script_url"]
        .as_str()
        .unwrap()
        .ends_with("/enroll/install.sh"));
    assert!(body["enrollment"]["install_command"]
        .as_str()
        .unwrap()
        .contains("/tmp/brocade-install.sh"));

    let script = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/enroll/install.sh")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    let script_body = to_bytes(script.into_body(), 1024 * 1024).await.unwrap();
    assert!(std::str::from_utf8(&script_body)
        .unwrap()
        .contains("/agent/v1/enroll"));

    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/enroll")
                .header("authorization", format!("Bearer {enrollment_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued: IssuedNodeToken =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(issued.node_id, "n-http");

    let reused = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/enroll")
                .header("authorization", format!("Bearer {enrollment_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);

    let desired = agent
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(desired.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_deployment_detail_includes_raw_state_and_opt_in_content() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;

    let revision_id: i64 =
        sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("current_revision")
            .unwrap();
    let deployment_id: i64 = sqlx::query(
        "INSERT INTO deployments (revision_id, status, idempotency_key, warnings, note)
         VALUES ($1, 'succeeded', 'http-detail', '[]'::jsonb, 'detail test')
         RETURNING id",
    )
    .bind(revision_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("id")
    .unwrap();
    let xray_sha = "b".repeat(64);
    let wg_sha = "c".repeat(64);
    sqlx::query(
        "INSERT INTO artifact_blobs (sha256, content, byte_len)
         VALUES ($1, $2, $3), ($4, $5, $6)",
    )
    .bind(&xray_sha)
    .bind("xray-secret-content")
    .bind("xray-secret-content".len() as i32)
    .bind(&wg_sha)
    .bind("wg-secret-content")
    .bind("wg-secret-content".len() as i32)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO deployment_targets (deployment_id, node_id, status)
         VALUES ($1, 'n1', 'succeeded')",
    )
    .bind(deployment_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO deployment_target_state (
            deployment_id, node_id, wave, disruptive,
            desired_structure, observed_before, observed_after, verdict
         )
         VALUES ($1, 'n1', 0, FALSE, $2, $3, $4, $5)",
    )
    .bind(deployment_id)
    .bind(json!({
        "actions": ["apply-xray"],
        "xray": { "state": "present", "sha256": xray_sha, "byte_len": 19 },
        "wireguard": { "state": "present", "sha256": wg_sha, "byte_len": 17 }
    }))
    .bind(json!({
        "xray": { "state": "unknown" },
        "wireguard": { "state": "unknown" },
        "grants": { "state": "unknown" }
    }))
    .bind(json!({
        "xray": { "state": "present", "sha256": "b".repeat(64) },
        "wireguard": { "state": "present", "sha256": "c".repeat(64) },
        "grants": { "state": "present", "inbounds": [] }
    }))
    .bind(json!({
        "target_status": "succeeded",
        "desired_matched": true,
        "baseline_matched": null
    }))
    .execute(db.pool())
    .await
    .unwrap();

    let (app, admin_token) = admin_app(&db).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/deployments/{deployment_id}"))
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(
        body["targets"][0]["observed_after"]["xray"]["state"],
        "present"
    );
    assert!(body["targets"][0]["observed_after"]["xray"]
        .get("content")
        .is_none());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/deployments/{deployment_id}?include=content"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/deployments/{deployment_id}?include=content"))
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(
        body["targets"][0]["desired_structure"]["xray"]["content"],
        "xray-secret-content"
    );
    assert_eq!(
        body["targets"][0]["observed_after"]["wireguard"]["content"],
        "wg-secret-content"
    );
    assert!(body["targets"][0]["observed_before"]["xray"]
        .get("content")
        .is_none());
}

/// Probing over the full HTTP path: the agent POSTs with a node token and the admin side reads back
/// with an admin token.
///
/// The store layer is tested on its own, and the wiring of the two endpoints is not — a route
/// mounted on the wrong surface, a `node_id` taken from the body rather than the token, a wrong
/// permission test: not one of these shows up in store's tests.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_agent_link_probe_records_and_admin_reads_the_per_node_view() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            'n2', 'platform.acme', 'Node 2', 'n2.example.net', '10.66.0.2',
            'wg-private-n2', 'wg-public-n2', 51820,
            10085, TRUE, TRUE,
            'system', '[]'::jsonb
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued: IssuedNodeToken =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64;
    let body = json!({
        "probed_at_unix_secs": now,
        "links": [{
            "peer_node_id": "n2",
            "endpoint_host": "n2.example.net",
            "status": "ok",
            "path_mtu": 1358,
            "suggested_wg_mtu": 1298
        }]
    });
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/link-probe")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(result["node_id"], "n1", "节点身份取自 token，不是 body");
    assert_eq!(result["accepted_links"], 1);

    // No token, no reporting
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/link-probe")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/links/mtu")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let view: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(view["links"][0]["path_mtu"], 1358);
    let node = |id: &str| {
        view["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["node_id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(node("n1")["suggested_mtu"], 1298);
    assert_eq!(
        node("n2")["suggested_mtu"],
        1298,
        "路径两端共享，对端也该拿到建议值"
    );
    assert_eq!(node("n2")["tightest_peer"], "n1");
}

/// End-to-end probing over the full HTTP path: the agent fetches the work list, reports results, and
/// the admin side reads them back.
///
/// Three things show up only at this layer: whether the work list's probe credential really derives
/// from the ingress's private key, whether `node_id` is taken from the token, and whether "connected
/// but exited in the wrong place" is recorded faithfully.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_agent_e2e_probe_round_trips_through_both_faces() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    refresh_current_model_snapshot(&db).await;

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued: IssuedNodeToken =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();

    // (1) The work list
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/e2e-targets")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let list: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();

    let target = &list["targets"][0];
    assert_eq!(target["chain_id"], "chn-b2c3-d4e5");
    assert_eq!(target["port"], 443);
    // It dials the local machine rather than a public IP: going over the public internet also
    // measures inbound routing, which is not a property of the chain.
    assert_eq!(target["dial_host"], "127.0.0.1");
    // The REALITY parameters must match the ingress's field for field; one differing and a different
    // path is what gets measured.
    assert_eq!(target["reality"]["public_key"], "reality-public");
    assert_eq!(target["reality"]["server_name"], "www.example.com");
    assert_eq!(target["reality"]["flow"], "xtls-rprx-vision");
    // The credential derives from the ingress's private key and cannot be produced from the ingress
    // id alone — it is something that reaches the ingress.
    assert_eq!(
        target["uuid"],
        brocade_core::model::probe_uuid("reality-private", "ing-b2c3").as_str()
    );
    assert_ne!(
        target["uuid"],
        brocade_core::model::probe_uuid("", "ing-b2c3").as_str()
    );
    assert!(list["endpoint_url"]
        .as_str()
        .unwrap()
        .starts_with("http://"));

    // (2) Report a "connected but exited in the wrong place". That outcome is the whole reason this
    // feature exists, so it gets its own pass: it must be recorded as neither success nor
    // failure.
    let probe_base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
        - 120;
    let body = json!({
        "probed_at_unix_secs": probe_base,
        "chains": [{
            "app_id": "app-a1b2",
            "chain_id": "chn-b2c3-d4e5",
            "status": "ok",
            "ttfb_ms": 86,
            "exit_ip": "203.0.113.9",
            "exit_loc": "HK",
            "exit_verdict": "mismatch",
            "detail": "通了，但出口 IP 不在这条链的出口机器上"
        }]
    });
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/e2e-probe")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(result["node_id"], "n1", "节点身份取自 token，不是 body");
    assert_eq!(result["accepted_chains"], 1);

    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/e2e-probe")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // (3) The admin side reads them back
    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/probes/e2e")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let view: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    let chain = &view["chains"][0];
    assert_eq!(chain["chain_id"], "chn-b2c3-d4e5");
    assert_eq!(chain["chain_name"], "Main Chain");
    assert_eq!(chain["node_id"], "n1");
    assert_eq!(chain["status"], "ok");
    assert_eq!(chain["ttfb_ms"], 86);
    assert_eq!(chain["exit_verdict"], "mismatch", "这一档不能被抹平成成功");
    assert_eq!(chain["exit_ip"], "203.0.113.9");
    assert_eq!(chain["samples"].as_array().unwrap().len(), 1);

    // (4) A failure carries no timing: the timeout value says nothing about the chain's speed and
    // would be drawn into the trend line.
    let broken = json!({
        "probed_at_unix_secs": probe_base + 60,
        "chains": [{
            "app_id": "app-a1b2",
            "chain_id": "chn-b2c3-d4e5",
            "status": "timeout",
            "ttfb_ms": 10000,
            "exit_ip": null,
            "exit_loc": null,
            "exit_verdict": "match",
            "detail": "等不到回应"
        }]
    });
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/e2e-probe")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(broken.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/probes/e2e")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let view: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    let chain = &view["chains"][0];
    assert_eq!(chain["status"], "timeout");
    assert!(chain["ttfb_ms"].is_null(), "失败不该留下耗时：{chain}");
    assert_eq!(
        chain["exit_verdict"], "unknown",
        "没通就谈不上出口核对，agent 报的 match 要被纠正"
    );
    assert_eq!(chain["samples"].as_array().unwrap().len(), 2, "样本要累积");

    // (5) Rows whose chain is absent from the model are dropped rather than failing the batch —
    // right after a chain is deleted the agent's work list is still stale.
    let stale = json!({
        "probed_at_unix_secs": probe_base + 120,
        "chains": [{
            "app_id": "app-a1b2", "chain_id": "c-gone", "status": "ok", "ttfb_ms": 10,
            "exit_ip": null, "exit_loc": null, "exit_verdict": "unknown", "detail": null
        }]
    });
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/e2e-probe")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(stale.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(result["accepted_chains"], 0);
    assert_eq!(result["unknown_chains"], 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_agent_usage_records_samples_and_admin_lists_them() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let issued: IssuedNodeToken = serde_json::from_slice(&bytes).unwrap();

    // Retirement keeps usage open until teardown converges: disabling Xray takes one final sample
    // and losing it would permanently understate the machine. Other business observations remain
    // active-only.
    sqlx::query("UPDATE node_lifecycle_state SET phase = 'retiring' WHERE node_id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
        - 60;
    for (read_at, up, down) in [(base, 100_u64, 200_u64), (base + 60, 125, 260)] {
        let body = json!({
            "read_at_unix_secs": read_at,
            "xray_started_at_unix_secs": base - 1000,
            "counters": [{
                "label": "alice@platform.acme#ing-b2c3",
                "uplink_bytes": up,
                "downlink_bytes": down
            }]
        });
        let response = agent
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agent/v1/usage")
                    .header("authorization", format!("Bearer {}", issued.token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let future = json!({
        "read_at_unix_secs": base + 3600,
        "xray_started_at_unix_secs": base,
        "counters": []
    });
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/v1/usage")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(future.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "future clocks are retryable"
    );

    let response = admin
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/usage/samples?tenant_id=platform.acme&user_id=alice")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["samples"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["samples"][0]["grant_label"],
        "alice@platform.acme#ing-b2c3"
    );
    assert_eq!(body["samples"][0]["uplink_bytes"], 25);
    assert_eq!(body["samples"][0]["downlink_bytes"], 60);
    assert_eq!(body["samples"][0]["has_gap"], false);
}

/// The global flow control is the default an ingress takes when it writes none, not decoration — it
/// has to reach the model snapshot.
///
/// This watches that fallback: with an ingress's `reality.flow` blank, that ingress in the snapshot
/// must carry the global value. Asserting elsewhere (on `/settings` echoing back, say) verifies
/// nothing of it — the setting is stored and nobody reads it, and the symptom is an operator turning
/// Vision on in the settings while xray carries on unchanged.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_global_flow_flows_into_ingresses_that_do_not_override_it() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, admin_token) = admin_app(&db).await;

    let settings = get_json(&app, &admin_token, "/settings").await;
    assert_eq!(settings.0, StatusCode::OK);
    assert_eq!(
        settings.1["reality_site"]["flow"], "xtls-rprx-vision",
        "空库默认就开着 Vision"
    );

    // Allowlist: a value xray does not recognize is blocked as a 400 before reaching the
    // database
    let rejected = put_json(
        &app,
        &admin_token,
        "/settings",
        json!({ "reality_site": { "flow": "xtls-rprx-direct" } }),
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);

    let updated = put_json(
        &app,
        &admin_token,
        "/settings",
        json!({
            "reality_site": {
                "dest": "www.example.com:443",
                "server_names": ["www.example.com"],
                "fingerprint": "chrome",
                "flow": "xtls-rprx-vision"
            }
        }),
    )
    .await;
    assert_eq!(updated.0, StatusCode::OK);
    assert_eq!(
        updated.1["settings"]["reality_site"]["flow"],
        "xtls-rprx-vision"
    );

    post_json(
        &app,
        &admin_token,
        "/tenants",
        json!({ "id": "platform.acme", "name": "Platform Acme" }),
    )
    .await;
    post_json(
        &app,
        &admin_token,
        "/apps",
        json!({ "id": "app-a1b2", "label": "Main App" }),
    )
    .await;
    post_json(
        &app,
        &admin_token,
        "/nodes/provision",
        provision_body("n-api"),
    )
    .await;
    post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/chains",
        json!({ "id": "chn-b2c3-d4e5", "tenant_id": "platform.acme", "name": "Main Chain" }),
    )
    .await;

    // A wholly blank site follows the global setting
    let inherited = post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/ingresses",
        json!({
            "id": "ing-b2c3",
            "chain_id": "chn-b2c3-d4e5",
            "node_id": "n-api",
            "bind": "0.0.0.0",
            "port": 443,
            "reality": {}
        }),
    )
    .await;
    assert_eq!(inherited.0, StatusCode::OK);
    assert_eq!(
        inherited.1["ingress"]["wires"]["vless"]["flow"], "xtls-rprx-vision",
        "创建响应就该回显生效值"
    );

    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    assert_eq!(snapshot.0, StatusCode::OK);
    let ingresses = snapshot.1["snapshot"]["apps"][0]["ingresses"]
        .as_array()
        .unwrap();
    let inherited = ingresses
        .iter()
        .find(|v| v["id"] == "ing-b2c3")
        .expect("ing-b2c3 在快照里");
    assert_eq!(inherited["wires"]["vless"]["flow"], "xtls-rprx-vision");

    // Turning it off must really turn it off: on by default is not welded on. An ingress left blank
    // must follow back to shipping no flow, or "off" is a display effect.
    //
    // `null` and not an omitted key. This helper merges onto the settings it just read, so leaving
    // the field out asks for the value already there — and the endpoint refuses a body missing it
    // anyway (`require_complete_settings`), precisely so that absence never has to be interpreted.
    // Off is a value somebody sends, not a field somebody forgot.
    let disabled = put_json(
        &app,
        &admin_token,
        "/settings",
        json!({
            "reality_site": {
                "dest": "www.example.com:443",
                "server_names": ["www.example.com"],
                "fingerprint": "chrome",
                "flow": null
            }
        }),
    )
    .await;
    assert_eq!(disabled.0, StatusCode::OK);
    assert!(disabled.1["settings"]["reality_site"]["flow"].is_null());

    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    let ingresses = snapshot.1["snapshot"]["apps"][0]["ingresses"]
        .as_array()
        .unwrap();
    let inherited = ingresses
        .iter()
        .find(|v| v["id"] == "ing-b2c3")
        .expect("ing-b2c3 在快照里");
    assert!(
        inherited["wires"]["vless"]["flow"].is_null(),
        "全局关掉之后，跟随全局的接入面也不下发 flow"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_settings_exposes_and_updates_global_reality_client_policy() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let (app, admin_token) = admin_app(&db).await;

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let initial_etag = response.headers().get("etag").unwrap().clone();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    let initial_settings = body.clone();
    // The floor has a factory value (DEFAULT '1.0.0' in the table definition): this field is on by
    // default and tightening it is one number away. The ceiling and the clock tolerance have none —
    // blank is what means unbounded for them.
    assert_eq!(body["reality_client"]["min_client_ver"], "1.0.0");
    assert!(body["reality_client"]["max_client_ver"].is_null());
    assert!(body["reality_client"]["max_time_diff_ms"].is_null());
    assert_eq!(body["ports"]["anytls_base"], 18_443);
    assert_eq!(body["ports"]["hy2_base"], 30_000);

    let update_body = json!({
        "reality_client": {
            "min_client_ver": "1.8.0",
            "max_client_ver": "1.9.9",
            "max_time_diff_ms": 30000
        },
        "ports": {
            "anytls_base": 16123
        }
    });
    let (status, body) = put_json(&app, &admin_token, "/settings", update_body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revision_id"], 2);
    assert_eq!(
        body["settings"]["reality_client"]["min_client_ver"],
        "1.8.0"
    );
    assert_eq!(
        body["settings"]["reality_client"]["max_time_diff_ms"],
        30000
    );
    assert_eq!(body["settings"]["ports"]["anytls_base"], 16_123);

    let stale = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("if-match", initial_etag.clone())
                .header("content-type", "application/json")
                .body(Body::from(initial_settings.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let incomplete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("if-match", initial_etag)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "overlay": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(incomplete.status(), StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "tenant-admin-1",
                        "display_name": "Tenant Admin One",
                        "role": "tenant-admin",
                        "tenant_scope": "platform.acme",
                        "password": "tenant-admin-secret"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators/tenant-admin-1/token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let tenant_admin: IssuedAdminToken = serde_json::from_slice(&bytes).unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header("authorization", format!("Bearer {}", tenant_admin.token))
                .header("content-type", "application/json")
                .body(Body::from(update_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_user_login_keeps_general_views_masked_and_opens_only_self_service() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    sqlx::query(
        "UPDATE users
         SET account_type = 'test'
         WHERE tenant_id = 'platform.acme' AND id = 'alice'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    seed_subscription_serving(&db).await;

    let (app, admin_token) = admin_app(&db).await;
    let issued = post_json(
        &app,
        &admin_token,
        "/users/platform.acme/alice/login",
        json!({}),
    )
    .await;
    assert_eq!(issued.0, StatusCode::CREATED);
    assert_eq!(issued.1["operator_id"], "platform.acme/alice");
    let password = issued.1["password"].as_str().unwrap();
    let cookie = login_cookie(&app, "alice", password).await;

    let whoami = get_json_with_cookie(&app, "/whoami", &cookie).await;
    assert_eq!(whoami.0, StatusCode::OK);
    assert_eq!(whoami.1["role"], "user");
    assert_eq!(whoami.1["masked_assets"], true);
    assert_eq!(whoami.1["self_user"]["tenant_id"], "platform.acme");
    assert_eq!(whoami.1["self_user"]["user_id"], "alice");

    let users = get_json_with_cookie(&app, "/users?include_disabled=true", &cookie).await;
    assert_eq!(users.0, StatusCode::OK);
    let general = &users.1["users"][0];
    assert!(
        general.get("uuid").is_none(),
        "general list leaked UUID: {general}"
    );
    assert!(general.get("login_enabled").is_none());
    assert_eq!(general["account_type"], "test");

    let me = get_json_with_cookie(&app, "/me/user", &cookie).await;
    assert_eq!(me.0, StatusCode::OK);
    assert_eq!(me.1["uuid"], "2d2304da-f114-4574-8d44-625afdb1db5c");
    assert_eq!(me.1["login_enabled"], true);

    let artifact = get_json_with_cookie(&app, "/me/artifact", &cookie).await;
    assert_eq!(artifact.0, StatusCode::OK);
    assert!(artifact.1["content"]
        .as_str()
        .unwrap()
        .contains("2d2304da-f114-4574-8d44-625afdb1db5c"));
    let generic_artifact = get_json_with_cookie(
        &app,
        "/artifacts/content/user/platform.acme%3Aalice/uri?serving=true",
        &cookie,
    )
    .await;
    assert_eq!(generic_artifact.0, StatusCode::FORBIDDEN);

    let capability = get_json_with_cookie(&app, "/grant-probes/capability", &cookie).await;
    assert_eq!(capability.0, StatusCode::OK);
    let own_probe = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/platform.acme/alice/grant-probes")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                // An invalid frozen id stops before any network activity if the local probe
                // runtime is available; without Xray the capability gate returns unavailable.
                .body(Body::from(
                    json!({ "item_ids": ["not-in-plan"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            own_probe.status(),
            StatusCode::BAD_REQUEST | StatusCode::SERVICE_UNAVAILABLE
        ),
        "the user's own probe must pass authorization without starting network work"
    );
    let another_users_probe = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/platform.acme/bob/grant-probes")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "item_ids": [] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(another_users_probe.status(), StatusCode::FORBIDDEN);

    let forbidden_rotate = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/platform.acme/alice/rotate-uuid")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden_rotate.status(), StatusCode::FORBIDDEN);

    let self_rotate = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/me/rotate-uuid")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(self_rotate.status(), StatusCode::OK);
    let rotated = response_json(self_rotate).await;
    let rotated_uuid = rotated["user"]["uuid"].as_str().unwrap();
    assert_ne!(rotated_uuid, "2d2304da-f114-4574-8d44-625afdb1db5c");
    assert_eq!(
        get_json_with_cookie(&app, "/me/user", &cookie).await.1["uuid"],
        rotated_uuid
    );

    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/user")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::FORBIDDEN);

    let forbidden_password = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/users/platform.acme/alice/login")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "new_password": "user-chosen-password" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden_password.status(), StatusCode::FORBIDDEN);

    let changed_password = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/users/platform.acme/alice/login")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "new_password": "user-chosen-password" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(changed_password.status(), StatusCode::OK);
    assert_eq!(
        response_json(changed_password).await["operator_id"],
        "platform.acme/alice"
    );
    login_cookie(&app, "platform.acme/alice", "user-chosen-password").await;
}

/// A reviewing role reads the model to check it and must not walk away with the
/// addresses. What is asserted here is the property the masking layer exists for —
/// that no raw address reaches such a viewer through *any* read endpoint — rather
/// than the wording of one field: a per-field test passes happily while the next
/// endpoint added leaks everything.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_console_hides_asset_addresses_from_a_readonly_viewer() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    sqlx::query(
        "UPDATE control_state
         SET reality_dest = 'borrowed.example.net:443',
             reality_server_names = '[\"borrowed.example.net\"]'::jsonb",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cert_domains (id, domain, acme_directory)
         VALUES ('readonly-mask-domain', 'huacu.io', 'https://acme.test/directory')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cert_labels (id, domain_id, label, name)
         VALUES ('readonly-mask-label', 'readonly-mask-domain', 'a2335a6d', '只读脱敏测试')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO certificates (
            id, label_id, cert_pem, key_pem_sealed, issued_at, expires_at, status
         ) VALUES (
            'readonly-mask-cert', 'readonly-mask-label', 'cert', 'sealed',
            now(), now() + interval '30 days', 'serving'
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_cert_label (node_id, label_id)
         VALUES ('n1', 'readonly-mask-label')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let (app, admin_token) = admin_app(&db).await;
    let created = post_json(
        &app,
        &admin_token,
        "/admin/operators",
        json!({
            "id": "reviewer",
            "display_name": "Reviewer",
            "role": "readonly",
            "tenant_scope": "platform.acme",
            "password": "reviewer-secret"
        }),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED);
    let issued = post_json(
        &app,
        &admin_token,
        "/admin/operators/reviewer/token",
        json!({}),
    )
    .await;
    assert_eq!(issued.0, StatusCode::CREATED);
    let reviewer_token = issued.1["token"].as_str().unwrap().to_owned();

    let whoami = get_json_with_token(&app, "/whoami", &reviewer_token).await;
    assert_eq!(whoami.0, StatusCode::OK);
    assert_eq!(whoami.1["role"], "readonly");
    // The flag the front end uses to leave out what it would only be refused
    assert_eq!(whoami.1["masked_assets"], true);

    // The fixture's node carries a name in public_ipv4 and an address in the
    // overlay, so both halves of the rule are exercised
    let snapshot = get_json_with_token(&app, "/model/snapshot", &reviewer_token).await;
    assert_eq!(snapshot.0, StatusCode::OK);
    let node = &snapshot.1["snapshot"]["nodes"][0];
    assert_eq!(node["id"], "n1", "机器本身还得认得出来，遮的是地址不是身份");
    assert_eq!(node["public_ipv4"], "***.net");
    assert_eq!(node["overlay_addr"], "10.66.***.***");
    assert_eq!(node["wireguard"]["listen_port"], "***");
    assert_eq!(node["api_port"], "***");
    assert_eq!(
        node["certificate_name"], "***.io",
        "VLESS、AnyTLS 和 REALITY 共用的本机证书域名必须在服务端脱敏"
    );
    let user = &snapshot.1["snapshot"]["users"][0];
    assert_eq!(user["id"], "alice");
    assert!(
        user.get("uuid").is_none(),
        "readonly snapshot carried a usable UUID: {user}"
    );

    // `/users` is a separate query and response type from the model snapshot. The response
    // middleware is the security boundary, so both must have the same credential-free shape.
    let users = get_json_with_token(&app, "/users?include_disabled=true", &reviewer_token).await;
    assert_eq!(users.0, StatusCode::OK);
    let listed_user = &users.1["users"][0];
    assert_eq!(listed_user["id"], "alice");
    assert!(
        listed_user.get("uuid").is_none(),
        "readonly user list carried a usable UUID: {listed_user}"
    );

    // Certificate health is visible on the machine page for a reviewer. The status and trust
    // source survive, while the three pieces that could reconstruct its SNI (`domain`, `label`,
    // and `names`) cross the same masking boundary as the machine snapshot.
    let certs = get_json_with_token(&app, "/certs", &reviewer_token).await;
    assert_eq!(certs.0, StatusCode::OK);
    let cert_group = &certs.1["groups"][0];
    assert_eq!(cert_group["domain"], "***.io");
    assert_eq!(cert_group["label"], "***");
    assert_eq!(cert_group["names"][0], "***.io");
    assert_eq!(cert_group["certificates"][0]["status"], "serving");
    assert_eq!(cert_group["certificates"][0]["signing_method"], "public-ca");
    assert_eq!(certs.1["nodes"][0]["certificate_name"], "***.io");
    assert_eq!(certs.1["nodes"][0]["on_disk"], "unknown");
    assert!(
        !certs.1.to_string().contains("a2335a6d.huacu.io"),
        "readonly certificate view carried the real SNI: {}",
        certs.1
    );

    // The whole body, not the fields anybody thought to name: the fixture's
    // addresses must not survive anywhere in it, however deeply nested
    let text = snapshot.1.to_string();
    for raw in [
        "10.66.0.1",
        "n1.example.net",
        "a2335a6d.huacu.io",
        "1.1.1.1",
        "51820",
        "10085",
    ] {
        assert!(
            !text.contains(raw),
            "{raw} 漏在 /model/snapshot 里了：{text}"
        );
    }

    // REALITY's borrowed site reaches a reviewer through the settings page rather than
    // the snapshot, and it is the one host on that page: everything else there is a
    // number or a policy. It also does not look like an address, so only the key rule
    // catches it — which is exactly the kind of coverage that goes missing.
    let settings = get_json_with_token(&app, "/settings", &reviewer_token).await;
    assert_eq!(settings.0, StatusCode::OK);
    let site = &settings.1["reality_site"];
    // Read out rather than matched over. The fixture explicitly seeded this site, so null or an
    // empty list means these assertions stopped exercising the masking path.
    let dest = site["dest"]
        .as_str()
        .expect("夹具写入了借用站点；为空说明下面几条会静默失效");
    assert!(
        !dest.contains("borrowed") && dest.contains("***"),
        "借用站点漏了：{dest}"
    );
    let names = site["server_names"]
        .as_array()
        .filter(|names| !names.is_empty())
        .expect("夹具写入了一条 SNI；空列表同上");
    for name in names {
        let name = name.as_str().unwrap_or_default();
        assert!(name.contains("***"), "借用站点的 SNI 漏了：{name}");
    }

    // The same viewer through the admin token sees everything, or the assertions
    // above would prove nothing
    let full = get_json(&app, &admin_token, "/model/snapshot").await;
    assert_eq!(full.1["snapshot"]["nodes"][0]["overlay_addr"], "10.66.0.1");
    assert_eq!(
        full.1["snapshot"]["nodes"][0]["certificate_name"], "a2335a6d.huacu.io",
        "管理员仍应看到完整证书域名"
    );
    assert!(
        full.1["snapshot"]["users"][0]["uuid"].is_string(),
        "credential masking must apply only to readonly responses"
    );
    let full_certs = get_json(&app, &admin_token, "/certs").await;
    assert_eq!(full_certs.0, StatusCode::OK);
    assert_eq!(full_certs.1["groups"][0]["domain"], "huacu.io");
    assert_eq!(full_certs.1["groups"][0]["label"], "a2335a6d");
    assert_eq!(
        full_certs.1["nodes"][0]["certificate_name"],
        "a2335a6d.huacu.io"
    );

    // The compile view stays readable — it is the review surface: topology, chains,
    // diagnostics. Its addresses are masked like everything else.
    let compile = get_json_with_token(&app, "/compile/1", &reviewer_token).await;
    assert_eq!(compile.0, StatusCode::OK);
    let text = compile.1.to_string();
    assert!(
        !text.contains("\"uuid\""),
        "readonly compile view carried a UUID member: {text}"
    );
    for raw in ["10.66.0.1", "n1.example.net", "51820"] {
        assert!(!text.contains(raw), "{raw} 漏在 /compile 里了");
    }
    assert!(
        compile.1["diagnostics"].is_array(),
        "诊断还得看得见，脱敏不该把评审要看的东西一起端走"
    );

    // A rendered config file is the one thing refused outright: it is addresses
    // nearly end to end, and masked it is no longer a config anybody can review
    let artifact = get_json_with_token(
        &app,
        "/artifacts/content/node/n1/wireguard",
        &reviewer_token,
    )
    .await;
    assert_eq!(artifact.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_console_read_surface_returns_redacted_snapshot_compile_state_and_artifact_index() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;

    let (app, admin_token) = admin_app(&db).await;

    let whoami = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whoami.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(whoami.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["operator_id"], "test-admin");
    assert_eq!(body["role"], "system-admin");

    let snapshot = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/model/snapshot")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(snapshot.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["redacted"], true);
    let text = body.to_string();
    assert!(!text.contains("wg-private"));
    assert!(!text.contains("reality-private"));
    assert_eq!(
        body["snapshot"]["nodes"][0]["wireguard"]["public_key"],
        "wg-public"
    );
    assert!(body["snapshot"]["nodes"][0]["wireguard"]
        .get("private_key")
        .is_none());
    assert!(body["snapshot"]["apps"][0]["ingresses"][0]["transport"]
        .get("private_key")
        .is_none());

    let compile = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/compile/1")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(compile.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(compile.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["revision"], 1);
    assert_eq!(body["summary"]["can_publish"], true);
    assert!(!body.to_string().contains("private_key"));

    let issued = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued: IssuedNodeToken =
        serde_json::from_slice(&to_bytes(issued.into_body(), 1024 * 1024).await.unwrap()).unwrap();

    let agent = agent_router(db.store.clone());
    let desired = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/desired")
                .header("x-brocade-protocol-version", "1")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("user-agent", "brocade-agent/read-surface")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(desired.status(), StatusCode::NO_CONTENT);

    let state = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/nodes/agent-state")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(state.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["nodes"][0]["node_id"], "n1");
    assert_eq!(
        body["nodes"][0]["agent_version"],
        "brocade-agent/read-surface"
    );
    assert_eq!(body["nodes"][0]["token_prefix"], issued.token_prefix);

    let artifacts = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/artifacts/index?revision=1")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(artifacts.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(artifacts.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    let kinds = body["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["artifact_kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"phantun"));
    assert!(kinds.contains(&"wireguard"));
    assert!(kinds.contains(&"xray"));
    assert!(kinds.contains(&"hy2_port_hop"));
    assert!(kinds.contains(&"grants"));
    assert!(kinds.contains(&"uri"));
    assert!(kinds.contains(&"clash"));

    let hop = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/artifacts/content/node/n1/hy2_port_hop?revision=1")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hop.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(hop.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["state"], "disabled");

    let phantun = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/artifacts/content/node/n1/phantun?revision=1")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(phantun.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(phantun.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["state"], "disabled");

    let grants = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/artifacts/content/node/n1/grants?revision=1")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(grants.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(grants.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["state"], "present");
    let grants: Value = serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
    assert_eq!(grants["inbounds"][0]["tag"], "in:app-a1b2/ing-b2c3");
    assert_eq!(
        grants["inbounds"][0]["clients"][0]["id"],
        "2d2304da-f114-4574-8d44-625afdb1db5c"
    );

    let operators = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(operators.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_console_write_surface_updates_model_and_keeps_generated_secrets_server_side() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let (app, admin_token) = admin_app(&db).await;

    let tenant = post_json(
        &app,
        &admin_token,
        "/tenants",
        json!({ "id": "platform.acme", "name": "Platform Acme" }),
    )
    .await;
    assert_eq!(tenant.0, StatusCode::CREATED);
    assert_eq!(tenant.1["revision_id"], 2);

    let app_result = post_json(
        &app,
        &admin_token,
        "/apps",
        json!({ "id": "app-a1b2", "label": "Main App" }),
    )
    .await;
    assert_eq!(app_result.0, StatusCode::OK);
    assert_eq!(app_result.1["revision_id"], 3);

    let node = post_json(
        &app,
        &admin_token,
        "/nodes/provision",
        provision_body("n-api"),
    )
    .await;
    assert_eq!(node.0, StatusCode::CREATED);
    assert_eq!(node.1["revision_id"], 4);
    assert!(node.1["node"].get("wg_private_key").is_none());

    let user = post_json(
        &app,
        &admin_token,
        "/users",
        json!({ "tenant_id": "platform.acme", "id": "alice" }),
    )
    .await;
    assert_eq!(user.0, StatusCode::CREATED);
    assert_eq!(user.1["revision_id"], 5);
    let user_uuid = user.1["user"]["uuid"].as_str().unwrap();
    assert_eq!(user_uuid.len(), 36);
    assert_eq!(&user_uuid[14..15], "4");

    let chain = post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/chains",
        json!({
            "id": "chn-b2c3-d4e5",
            "tenant_id": "platform.acme",
            "name": "Main Chain"
        }),
    )
    .await;
    assert_eq!(chain.0, StatusCode::OK);
    assert_eq!(chain.1["revision_id"], 6);

    let ingress = post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/ingresses",
        json!({
            "id": "ing-b2c3",
            "chain_id": "chn-b2c3-d4e5",
            "node_id": "n-api",
            "bind": "0.0.0.0",
            "port": 443,
            "reality": {
                "dest": "www.example.com:443",
                "server_names": ["www.example.com"],
                "fingerprint": "chrome",
                "flow": "xtls-rprx-vision"
            }
        }),
    )
    .await;
    assert_eq!(ingress.0, StatusCode::OK);
    assert_eq!(ingress.1["revision_id"], 7);
    assert!(ingress.1["ingress"]["transport"]
        .get("private_key")
        .is_none());
    assert_eq!(
        ingress.1["ingress"]["identity"]["public_key"]
            .as_str()
            .unwrap()
            .len(),
        43
    );
    assert_eq!(
        ingress.1["ingress"]["identity"]["short_ids"][0]
            .as_str()
            .unwrap()
            .len(),
        16
    );

    let legacy_step = put_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/chains/chn-b2c3-d4e5/steps/n-api",
        json!({ "rules": [] }),
    )
    .await;
    assert_eq!(legacy_step.0, StatusCode::METHOD_NOT_ALLOWED);

    let step = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-api",
        json!({
            "rules": [{
                "m": { "t": "any" },
                "a": { "t": "egress", "send_through": null }
            }]
        }),
    )
    .await;
    assert_eq!(step.0, StatusCode::OK);
    assert_eq!(step.1["revision_id"], 8);

    let grant = post_json(
        &app,
        &admin_token,
        "/grants",
        json!({
            "app_id": "app-a1b2",
            "tenant_id": "platform.acme",
            "user_id": "alice",
            "ingress_id": "ing-b2c3",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(grant.0, StatusCode::OK);
    assert_eq!(grant.1["revision_id"], 9);

    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    assert_eq!(snapshot.0, StatusCode::OK);
    assert_eq!(snapshot.1["snapshot"]["revision"], 9);
    assert_eq!(
        snapshot.1["snapshot"]["apps"][0]["steps"][0]["rules"][0]["m"]["t"],
        "any"
    );
    assert!(!snapshot.1.to_string().contains("private_key"));

    let compile = get_json(&app, &admin_token, "/compile/9").await;
    assert_eq!(compile.0, StatusCode::OK);
    assert_eq!(compile.1["summary"]["can_publish"], true);

    let rotated = post_json(
        &app,
        &admin_token,
        "/users/platform.acme/alice/rotate-uuid",
        json!({}),
    )
    .await;
    assert_eq!(rotated.0, StatusCode::OK);
    assert_eq!(rotated.1["revision_id"], 10);
    assert_ne!(rotated.1["user"]["uuid"].as_str().unwrap(), user_uuid);

    let node_update = put_json(
        &app,
        &admin_token,
        "/nodes/n-api",
        json!({ "name": "Node API Updated" }),
    )
    .await;
    assert_eq!(node_update.0, StatusCode::OK);
    assert_eq!(node_update.1["revision_id"], 11);
    assert_eq!(node_update.1["node"]["name"], "Node API Updated");
    assert!(node_update.1["node"]["wireguard"]
        .get("private_key")
        .is_none());

    let stored = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(stored.revision, 11);
    assert_eq!(stored.users[0].id, "alice");
    assert_eq!(stored.apps[0].grants.len(), 1);
    assert_eq!(stored.apps[0].ingresses.len(), 1);

    let historical = get_json(&app, &admin_token, "/model/snapshot?revision=5").await;
    assert_eq!(historical.0, StatusCode::OK);
    assert_eq!(historical.1["snapshot"]["revision"], 5);
    assert_eq!(historical.1["snapshot"]["users"][0]["id"], "alice");
    assert!(historical.1["snapshot"]["apps"][0]["chains"]
        .as_array()
        .unwrap()
        .is_empty());

    let revisions = get_json(&app, &admin_token, "/revisions?limit=20").await;
    assert_eq!(revisions.0, StatusCode::OK);
    assert_eq!(revisions.1["current_revision"], 11);
    assert!(revisions.1["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|revision| revision["id"] == 5 && revision["has_snapshot"] == true));

    let artifact = get_json(
        &app,
        &admin_token,
        "/artifacts/content/node/n-api/wireguard",
    )
    .await;
    assert_eq!(artifact.0, StatusCode::OK);
    assert_eq!(artifact.1["state"], "present");
    assert_eq!(artifact.1["redacted"], false);
    assert!(artifact.1["content"]
        .as_str()
        .unwrap()
        .contains("PrivateKey = "));

    let readonly = post_json(
        &app,
        &admin_token,
        "/admin/operators",
        json!({
            "id": "reader-api",
            "display_name": "Reader API",
            "role": "readonly",
            "tenant_scope": "platform.acme",
            "password": "reader-secret"
        }),
    )
    .await;
    assert_eq!(readonly.0, StatusCode::CREATED);
    let readonly_token = post_json(
        &app,
        &admin_token,
        "/admin/operators/reader-api/token",
        json!({}),
    )
    .await;
    assert_eq!(readonly_token.0, StatusCode::CREATED);
    let readonly_token = readonly_token.1["token"].as_str().unwrap();
    // A reviewing role is refused the artifact outright. Redacting the private key
    // was never enough: every line around it is an address, and a wg config with
    // the endpoints masked is not a config anybody could review anyway. See
    // `AdminPermission::ViewArtifacts`.
    let refused_artifact = get_json_with_token(
        &app,
        "/artifacts/content/node/n-api/wireguard",
        readonly_token,
    )
    .await;
    assert_eq!(refused_artifact.0, StatusCode::FORBIDDEN);
    let refused_probe = get_json_with_token(&app, "/grant-probes/capability", readonly_token).await;
    assert_eq!(
        refused_probe.0,
        StatusCode::FORBIDDEN,
        "a role that cannot read credentials must not be allowed to execute them"
    );
    let admin_probe = get_json(&app, &admin_token, "/grant-probes/capability").await;
    assert_eq!(admin_probe.0, StatusCode::OK);
    assert_eq!(admin_probe.1["concurrency"], 30);

    let verify = post_json(
        &app,
        &admin_token,
        "/deployments/verify",
        json!({ "revision_id": 11 }),
    )
    .await;
    assert_eq!(verify.0, StatusCode::OK);
    assert_eq!(verify.1["converged"], false);
    assert_eq!(verify.1["summary"]["changed_targets"], 1);
    let target = &verify.1["targets"][0];
    let observed_grants = observed_grants_from_desired(&target["desired"]["grants"]);
    sqlx::query(
        "INSERT INTO node_applied_state (
            node_id,
            phantun_state, phantun_observed,
            wireguard_state, wireguard_sha256, wireguard_observed,
            xray_state, xray_sha256, xray_observed,
            -- 这台没有跳转段，所以机器上该是「关着」。留空会落到列默认的 unknown，而
            -- unknown 一律判成要动作 —— 下面那句 converged 就再也不可能成立。
            hy2_port_hop_state, hy2_port_hop_observed,
            grants_state, grants_observed,
            observed_at
         )
         VALUES (
            'n-api',
            'disabled', jsonb_build_object('state', 'disabled'),
            'present', $1, jsonb_build_object('state', 'present', 'sha256', $1::text),
            'present', $2, jsonb_build_object('state', 'present', 'sha256', $2::text),
            'disabled', jsonb_build_object('state', 'disabled'),
            'present', $3,
            now()
         )",
    )
    .bind(target["desired"]["wireguard"]["sha256"].as_str().unwrap())
    .bind(target["desired"]["xray"]["sha256"].as_str().unwrap())
    .bind(observed_grants)
    .execute(db.pool())
    .await
    .unwrap();
    let verify = post_json(
        &app,
        &admin_token,
        "/deployments/verify",
        json!({ "revision_id": 11 }),
    )
    .await;
    assert_eq!(verify.0, StatusCode::OK);
    assert_eq!(verify.1["converged"], true);
    assert_eq!(verify.1["summary"]["changed_targets"], 0);

    let disabled = put_json(
        &app,
        &admin_token,
        "/users/platform.acme/alice/status",
        json!({ "status": "disabled" }),
    )
    .await;
    assert_eq!(disabled.0, StatusCode::OK);
    assert_eq!(disabled.1["revision_id"], 12);
    assert_eq!(disabled.1["user"]["status"], "disabled");

    let users = get_json(&app, &admin_token, "/users?tenant_id=platform.acme").await;
    assert_eq!(users.0, StatusCode::OK);
    assert!(users.1["users"].as_array().unwrap().is_empty());
    let users = get_json(
        &app,
        &admin_token,
        "/users?tenant_id=platform.acme&include_disabled=true",
    )
    .await;
    assert_eq!(users.0, StatusCode::OK);
    assert_eq!(users.1["users"][0]["status"], "disabled");

    let current = get_json(&app, &admin_token, "/model/snapshot").await;
    assert_eq!(current.0, StatusCode::OK);
    assert_eq!(current.1["snapshot"]["revision"], 12);
    assert!(current.1["snapshot"]["users"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(current.1["snapshot"]["apps"][0]["grants"]
        .as_array()
        .unwrap()
        .is_empty());
    let historical = get_json(&app, &admin_token, "/model/snapshot?revision=11").await;
    assert_eq!(historical.0, StatusCode::OK);
    assert_eq!(historical.1["snapshot"]["users"][0]["id"], "alice");
    assert_eq!(
        historical.1["snapshot"]["apps"][0]["grants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_admin_rbac_requires_tokens_and_separates_system_admin() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query(
        "INSERT INTO tenants (id, name)
         VALUES
            ('platform.acme', 'Platform Acme'),
            ('platform.acme.child', 'Platform Acme Child'),
            ('platform.other', 'Platform Other')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let (app, admin_token) = admin_app(&db).await;
    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/deployments")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "publisher-1",
                        "display_name": "Publisher One",
                        "role": "publisher",
                        "tenant_scope": "platform.acme",
                        "password": "publisher-secret"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators/publisher-1/token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let issued: IssuedAdminToken = serde_json::from_slice(&bytes).unwrap();

    let readable = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/deployments")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(readable.status(), StatusCode::OK);

    let system_only = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(system_only.status(), StatusCode::FORBIDDEN);

    let provision_system_only = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/provision")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("content-type", "application/json")
                .body(Body::from(provision_body("n-forbidden").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(provision_system_only.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "tenant-admin-1",
                        "display_name": "Tenant Admin One",
                        "role": "tenant-admin",
                        "tenant_scope": "platform.acme",
                        "password": "tenant-admin-secret"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators/tenant-admin-1/token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let tenant_admin: IssuedAdminToken = serde_json::from_slice(&bytes).unwrap();

    let scoped_operator = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {}", tenant_admin.token))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "acme-child-reader",
                        "display_name": "Acme Child Reader",
                        "role": "readonly",
                        "tenant_scope": "platform.acme.child",
                        "password": "reader-secret"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(scoped_operator.status(), StatusCode::CREATED);

    let other_operator = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {}", tenant_admin.token))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "other-reader",
                        "display_name": "Other Reader",
                        "role": "readonly",
                        "tenant_scope": "platform.other"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(other_operator.status(), StatusCode::FORBIDDEN);

    let system_operator = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {}", tenant_admin.token))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "system-2",
                        "display_name": "System Two",
                        "role": "system-admin",
                        "tenant_scope": null
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(system_operator.status(), StatusCode::FORBIDDEN);

    let secret_content = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/deployments/1?include=content")
                .header("authorization", format!("Bearer {}", issued.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(secret_content.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_grant_automation_status_exposes_pre_deployment_retries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query(
        "INSERT INTO jobs (kind, status, payload, attempts, last_error)
         VALUES (
             'grants-deployment',
             'queued',
             '{\"revision_id\":42}'::jsonb,
             7,
             'historical snapshot could not be decoded'
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let (app, admin_token) = admin_app(&db).await;
    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/grants/automation")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/grants/automation")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["pending_jobs"], 1);
    assert_eq!(body["retrying_jobs"], 1);
    assert_eq!(body["max_attempts"], 7);
    assert_eq!(body["latest_revision_id"], 42);
    assert_eq!(
        body["last_error"],
        "historical snapshot could not be decoded"
    );
}

/// Submitting the same thing again must produce no new revision.
///
/// The rule is that a changed field produces a revision — a *changed* field. The console's pages are
/// mostly whole-form submissions: open a machine's detail view, change nothing, press save, and this
/// used to stamp a new number every time, filling the revision list with entries identical to their
/// predecessor while "how many machines did revision N move" answered none every time.
///
/// Two things are verified together: an empty commit consumes no number, and a number taken and
/// returned must be usable by the next real change — without winding the IDENTITY cursor back, the
/// revision numbers advance a step at a time and it looks as though history went missing.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_console_resubmitting_a_write_verbatim_does_not_burn_a_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, admin_token) = admin_app(&db).await;

    // Each step states which number it should be. The numbers run consecutively, so one extra empty
    // commit anywhere collapses every assertion after it — which is the point: a gap must be a
    // visible failure.
    let tenant_body = json!({ "id": "platform.acme", "name": "Platform Acme" });
    let tenant = post_json(&app, &admin_token, "/tenants", tenant_body.clone()).await;
    assert_eq!(tenant.0, StatusCode::CREATED);
    assert_eq!(tenant.1["revision_id"], 2);

    let again = post_json(&app, &admin_token, "/tenants", tenant_body).await;
    assert_eq!(again.0, StatusCode::CREATED);
    assert_eq!(again.1["revision_id"], 2, "同一个租户原样再存一遍");

    // A genuinely changed field: the number must follow 2 rather than jump to 4 — the 3 returned
    // last time must still be there.
    let renamed = post_json(
        &app,
        &admin_token,
        "/tenants",
        json!({ "id": "platform.acme", "name": "Platform ACME" }),
    )
    .await;
    assert_eq!(
        renamed.1["revision_id"], 3,
        "退掉的号要能被下一次真改动接着用"
    );

    let app_body = json!({ "id": "app-a1b2", "label": "Main App" });
    let created = post_json(&app, &admin_token, "/apps", app_body.clone()).await;
    assert_eq!(created.1["revision_id"], 4);
    let created_again = post_json(&app, &admin_token, "/apps", app_body).await;
    assert_eq!(created_again.1["revision_id"], 4, "同一个应用原样再存一遍");

    let node = post_json(
        &app,
        &admin_token,
        "/nodes/provision",
        provision_body("n-api"),
    )
    .await;
    assert_eq!(node.1["revision_id"], 5);

    // The machine detail view is where whole-form submission hurts most: it writes the values it
    // read straight back.
    let node_name = node.1["node"]["name"].as_str().unwrap().to_owned();
    let untouched = put_json(
        &app,
        &admin_token,
        "/nodes/n-api",
        json!({ "name": node_name }),
    )
    .await;
    assert_eq!(untouched.0, StatusCode::OK);
    assert_eq!(untouched.1["revision_id"], 5, "机器名写回原值");

    let empty_form = put_json(&app, &admin_token, "/nodes/n-api", json!({})).await;
    assert_eq!(empty_form.1["revision_id"], 5, "一个字段都没提的更新");

    let user = post_json(
        &app,
        &admin_token,
        "/users",
        json!({ "tenant_id": "platform.acme", "id": "alice" }),
    )
    .await;
    assert_eq!(user.1["revision_id"], 6);

    let chain_body = json!({
        "id": "chn-b2c3-d4e5",
        "tenant_id": "platform.acme",
        "name": "Main Chain"
    });
    let chain = post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/chains",
        chain_body.clone(),
    )
    .await;
    assert_eq!(chain.1["revision_id"], 7);
    let chain_again = post_json(&app, &admin_token, "/apps/app-a1b2/chains", chain_body).await;
    assert_eq!(chain_again.1["revision_id"], 7, "链和主干都原样");

    let ingress_body = json!({
        "id": "ing-b2c3",
        "chain_id": "chn-b2c3-d4e5",
        "node_id": "n-api",
        "bind": "0.0.0.0",
        "port": 443,
        "reality": {
            "dest": "www.example.com:443",
            "server_names": ["www.example.com"],
            "fingerprint": "chrome",
            "flow": "xtls-rprx-vision"
        }
    });
    let ingress = post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/ingresses",
        ingress_body.clone(),
    )
    .await;
    assert_eq!(ingress.1["revision_id"], 8);
    let public_key = ingress.1["ingress"]["identity"]["public_key"]
        .as_str()
        .unwrap()
        .to_owned();

    let ingress_again =
        post_json(&app, &admin_token, "/apps/app-a1b2/ingresses", ingress_body).await;
    assert_eq!(ingress_again.0, StatusCode::OK);
    assert_eq!(ingress_again.1["revision_id"], 8, "接入面原样再存一遍");
    // The empty-commit path no longer goes through RETURNING and reads the keys from the database —
    // reading the wrong one turns them into a different key on the spot.
    assert_eq!(
        ingress_again.1["ingress"]["identity"]["public_key"], public_key,
        "空提交不能把已有的 REALITY 密钥换掉"
    );

    let step_body = json!({
        "rules": [{ "m": { "t": "any" }, "a": { "t": "egress", "send_through": null } }]
    });
    let step = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-api",
        step_body.clone(),
    )
    .await;
    assert_eq!(step.1["revision_id"], 9);
    let step_again = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-api",
        step_body,
    )
    .await;
    assert_eq!(step_again.1["revision_id"], 9, "同一张规则表再存一遍");

    let grant_body = json!({
        "app_id": "app-a1b2",
        "tenant_id": "platform.acme",
        "user_id": "alice",
        "ingress_id": "ing-b2c3",
        "enabled": true
    });
    let grant = post_json(&app, &admin_token, "/grants", grant_body.clone()).await;
    assert_eq!(grant.1["revision_id"], 10);
    let grant_again = post_json(&app, &admin_token, "/grants", grant_body).await;
    assert_eq!(grant_again.1["revision_id"], 10, "已经授权了再授一次");

    // Revoking a grant that was never there: also a no-op.
    let revoke_absent = post_json(
        &app,
        &admin_token,
        "/grants",
        json!({
            "app_id": "app-a1b2",
            "tenant_id": "platform.acme",
            "user_id": "alice",
            "ingress_id": "ing-b2c3",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(revoke_absent.1["revision_id"], 10);

    let disabled = put_json(
        &app,
        &admin_token,
        "/users/platform.acme/alice/status",
        json!({ "status": "disabled" }),
    )
    .await;
    assert_eq!(disabled.1["revision_id"], 11);
    let disabled_again = put_json(
        &app,
        &admin_token,
        "/users/platform.acme/alice/status",
        json!({ "status": "disabled" }),
    )
    .await;
    assert_eq!(disabled_again.1["revision_id"], 11, "已经停用了再停一次");

    let retired = put_json(
        &app,
        &admin_token,
        "/nodes/n-api/status",
        json!({ "status": "retired" }),
    )
    .await;
    assert_eq!(retired.1["revision_id"], 12);
    assert_eq!(retired.1["lifecycle"]["phase"], "retiring");
    assert_eq!(retired.1["lifecycle"]["lifecycle_epoch"], 1);
    assert!(retired.1["deployment_id"].is_number());
    let retirement_deployment_id = retired.1["deployment_id"].clone();
    let retired_again = put_json(
        &app,
        &admin_token,
        "/nodes/n-api/status",
        json!({ "status": "retired" }),
    )
    .await;
    assert_eq!(retired_again.1["revision_id"], 12, "已经退役了再退一次");
    assert_eq!(retired_again.1["deployment_id"], retirement_deployment_id);

    // Global settings: read them and write them straight back, which is exactly "opened the settings
    // page and pressed save".
    let settings = get_json(&app, &admin_token, "/settings").await;
    assert_eq!(settings.0, StatusCode::OK);
    let settings_back = put_json(&app, &admin_token, "/settings", settings.1.clone()).await;
    assert_eq!(settings_back.0, StatusCode::OK);
    assert_eq!(settings_back.1["revision_id"], 12, "设置原样写回");

    let settings_changed = put_json(
        &app,
        &admin_token,
        "/settings",
        json!({ "reality_client": { "min_client_ver": "1.8.0" } }),
    )
    .await;
    assert_eq!(settings_changed.1["revision_id"], 13, "设置真改了才占号");

    // Closing the loop: the revision numbers must be a consecutive run of 1..=13. An empty commit
    // either took no number or took and returned one, and neither may leave a gap here;
    // current_revision must also rest on the last of them.
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM revisions ORDER BY id")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(ids, (1..=13).collect::<Vec<i64>>(), "修订号中间不许有洞");

    let revisions = get_json(&app, &admin_token, "/revisions?limit=50").await;
    assert_eq!(revisions.1["current_revision"], 13);
}

/// A stored relay port must read back, and it hangs off the chain.
///
/// The console's relay-port editor lives on the rule-table page (`rules.tsx`) and reads a step's
/// `hop_in` from the model snapshot. Break this round trip and the symptom is nothing changing after
/// a save. The body's shape is copied from what the front end really sends.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_hop_in_round_trips_through_the_model_snapshot() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, admin_token) = admin_app(&db).await;

    post_json(
        &app,
        &admin_token,
        "/tenants",
        json!({ "id": "platform.acme", "name": "Platform Acme" }),
    )
    .await;
    post_json(
        &app,
        &admin_token,
        "/apps",
        json!({ "id": "app-a1b2", "label": "Main App" }),
    )
    .await;
    for id in ["n-edge", "n-hop"] {
        let created = post_json(&app, &admin_token, "/nodes/provision", provision_body(id)).await;
        assert_eq!(created.0, StatusCode::CREATED);
    }
    post_json(
        &app,
        &admin_token,
        "/apps/app-a1b2/chains",
        json!({
            "id": "chn-b2c3-d4e5",
            "tenant_id": "platform.acme",
            "name": "主链路"
        }),
    )
    .await;

    let hop_in = |snapshot: &Value| -> Value {
        snapshot["snapshot"]["apps"][0]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["node"] == "n-hop")
            .map(|s| s["hop_in"].clone())
            .unwrap_or(Value::Null)
    };

    // The unencrypted variant: stored and read back.
    let step = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-hop",
        json!({
            "accept": {},
            "hop_in": { "port": 20000, "security": { "t": "none" } },
            "rules": [{ "m": { "t": "any" }, "a": { "t": "egress" } }]
        }),
    )
    .await;
    assert_eq!(step.0, StatusCode::OK, "{:?}", step.1);

    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    assert_eq!(hop_in(&snapshot.1)["port"], 20000);
    assert_eq!(hop_in(&snapshot.1)["security"]["t"], "none");

    // Switching to REALITY: the material is generated server-side and the private key does not leave
    // over HTTP.
    let step = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-hop",
        json!({
            "accept": {},
            "hop_in": {
                "port": 443,
                "security": {
                    "t": "reality",
                    "v": { "dest": "www.apple.com: 443", "server_names": ["www.apple.com"] }
                }
            },
            "rules": [{ "m": { "t": "any" }, "a": { "t": "egress" } }]
        }),
    )
    .await;
    assert_eq!(step.0, StatusCode::OK, "{:?}", step.1);
    let rendered = serde_json::to_string(&step.1).unwrap();
    assert!(
        !rendered.contains("private_key") || rendered.contains("<redacted>"),
        "私钥不该出 HTTP：{rendered}"
    );

    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    let security = hop_in(&snapshot.1)["security"].clone();
    assert_eq!(security["t"], "reality");
    assert_eq!(security["v"]["dest"], "www.apple.com:443", "站点要读得回来");
    assert_eq!(security["v"]["server_names"][0], "www.apple.com");
    assert_eq!(hop_in(&snapshot.1)["port"], 443);

    // port: 0 turns it off. It must be a different thing from not mentioning the field.
    let step = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-hop",
        json!({
            "accept": {},
            "hop_in": { "port": 0 },
            "rules": [{ "m": { "t": "any" }, "a": { "t": "egress" } }]
        }),
    )
    .await;
    assert_eq!(step.0, StatusCode::OK);
    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    assert!(hop_in(&snapshot.1).is_null(), "port 0 该把中转口关掉");

    // Not mentioning the field leaves it alone, so that the turning-off above is not undone by every
    // rule edit.
    apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-b2c3-d4e5",
        "n-hop",
        json!({
            "accept": {},
            "rules": [{ "m": { "t": "any" }, "a": { "t": "block" } }]
        }),
    )
    .await;
    let snapshot = get_json(&app, &admin_token, "/model/snapshot").await;
    assert!(hop_in(&snapshot.1).is_null(), "没提 hop_in 就不该变");
}

/// One relay serving two chains, each on its own transport layer — the entire reason relay ports hang
/// off the chain.
///
/// On the node it was one `hop_security` per machine and these two chains could only pick one; now
/// one takes REALITY through censorship while the other runs unencrypted on a datacenter network for
/// speed, both at once. Any segment of this dropping has the same symptom, configured and not in
/// effect, while the artifacts look entirely correct.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_two_chains_on_one_relay_keep_separate_hop_inbounds() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, admin_token) = admin_app(&db).await;

    post_json(
        &app,
        &admin_token,
        "/tenants",
        json!({ "id": "platform.acme", "name": "Platform Acme" }),
    )
    .await;
    post_json(
        &app,
        &admin_token,
        "/apps",
        json!({ "id": "app-a1b2", "label": "Main App" }),
    )
    .await;
    for id in ["n-lan", "n-wan", "n-relay"] {
        let created = post_json(&app, &admin_token, "/nodes/provision", provision_body(id)).await;
        assert_eq!(created.0, StatusCode::CREATED);
    }

    for (chain, head, ingress) in [
        ("chn-c3d4-e5f6", "n-lan", "ing-c3d4"),
        ("chn-d4e5-f607", "n-wan", "ing-d4e5"),
    ] {
        post_json(
            &app,
            &admin_token,
            "/apps/app-a1b2/chains",
            json!({
                "id": chain,
                "tenant_id": "platform.acme",
                "name": chain
            }),
        )
        .await;
        // Every chain needs an ingress, or compilation reports chain.no-ingress first and the
        // relay-port assertions are never reached
        post_json(
            &app,
            &admin_token,
            "/apps/app-a1b2/ingresses",
            json!({
                "id": ingress,
                "chain_id": chain,
                "node_id": head,
                "bind": "0.0.0.0",
                "port": 443,
                "reality": {
                    "dest": "apps.apple.com:443",
                    "server_names": ["apps.apple.com"],
                    "fingerprint": "chrome",
                    "flow": "xtls-rprx-vision"
                }
            }),
        )
        .await;
    }

    // The in-datacenter one: dials the internal address and enters the unencrypted port
    apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-c3d4-e5f6",
        "n-lan",
        json!({
            "rules": [{
                "m": { "t": "any" },
                "a": { "t": "forward", "to": "n-relay", "dial": { "t": "addr", "v": "10.0.0.9:8443" } }
            }]
        }),
    )
    .await;
    let lan = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-c3d4-e5f6",
        "n-relay",
        json!({
            "accept": {},
            "hop_in": { "port": 20000, "security": { "t": "none" } },
            "rules": [{ "m": { "t": "any" }, "a": { "t": "egress" } }]
        }),
    )
    .await;
    assert_eq!(lan.0, StatusCode::OK, "{:?}", lan.1);

    // The cross-border one: dials the public address, enters the REALITY port, and takes a port
    // distinct from the one above
    apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-d4e5-f607",
        "n-wan",
        json!({
            "rules": [{
                "m": { "t": "any" },
                "a": {
                    "t": "forward",
                    "to": "n-relay",
                    "dial": { "t": "addr", "v": "relay.sg.example:443" }
                }
            }]
        }),
    )
    .await;
    let wan = apply_step_json(
        &app,
        &admin_token,
        "app-a1b2",
        "chn-d4e5-f607",
        "n-relay",
        json!({
            "accept": {},
            "hop_in": {
                "port": 20001,
                "security": {
                    "t": "reality",
                    "v": { "dest": "apps.apple.com:443", "server_names": ["apps.apple.com"] }
                }
            },
            "rules": [{ "m": { "t": "any" }, "a": { "t": "egress" } }]
        }),
    )
    .await;
    assert_eq!(wan.0, StatusCode::OK, "{:?}", wan.1);

    // Compilation must be clean, and the two chains' routing must resolve to their own
    // addresses.
    let revisions = get_json(&app, &admin_token, "/revisions?limit=1").await;
    let current = revisions.1["current_revision"].as_u64().unwrap();
    let compile = get_json(&app, &admin_token, &format!("/compile/{current}")).await;
    assert_eq!(compile.0, StatusCode::OK);
    let errors: Vec<&str> = compile.1["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["level"] == "error")
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(errors.is_empty(), "不该有错：{errors:?}");

    let hops: Vec<&Value> = compile.1["apps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["hops"].as_array().unwrap())
        .collect();
    let lan_hop = hops.iter().find(|h| h["chain"] == "chn-c3d4-e5f6").unwrap();
    assert_eq!(lan_hop["address"], "10.0.0.9");
    assert_eq!(lan_hop["security"]["t"], "none");
    let wan_hop = hops.iter().find(|h| h["chain"] == "chn-d4e5-f607").unwrap();
    assert_eq!(wan_hop["address"], "relay.sg.example");
    assert_eq!(wan_hop["security"]["t"], "reality");

    // On the artifact side: the relay carries two inbounds with independent ports and material.
    let artifact = get_json(
        &app,
        &admin_token,
        &format!("/artifacts/content/node/n-relay/xray?revision={current}"),
    )
    .await;
    assert_eq!(artifact.0, StatusCode::OK, "{:?}", artifact.1);
    let config: Value =
        serde_json::from_str(artifact.1["content"].as_str().expect("xray 产物是文本")).unwrap();
    let hop_inbounds: Vec<&Value> = config["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["tag"].as_str().is_some_and(|t| t.starts_with("in:hop:")))
        .collect();
    assert_eq!(hop_inbounds.len(), 2, "两条链两个中转口：{hop_inbounds:#?}");
    let ports: Vec<u64> = hop_inbounds
        .iter()
        .filter_map(|i| i["port"].as_u64())
        .collect();
    assert!(
        ports.contains(&20000) && ports.contains(&20001),
        "{ports:?}"
    );
    let securities: Vec<&str> = hop_inbounds
        .iter()
        .map(|i| i["streamSettings"]["security"].as_str().unwrap_or("none"))
        .collect();
    assert!(
        securities.contains(&"none") && securities.contains(&"reality"),
        "两条链各用各的传输层：{securities:?}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn certificate_settings_issue_a_direct_self_signed_certificate() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let (app, token) = admin_app(&db).await;

    let response = app
        .oneshot(
            Request::put("/certs/domain")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "domain": "private.apple.com",
                        "signing_method": "self-signed",
                        "renew_before_days": 30
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["domain"]["signing_method"], "self-signed");
    assert_eq!(body["domain"]["acme_directory"], "self-signed");
    assert_eq!(body["domain"]["has_credential"], false);
    let text = body.to_string();
    assert!(!text.contains("BEGIN PRIVATE KEY"));
    assert!(!text.contains("BEGIN CERTIFICATE"));

    let domain_id = body["domain"]["id"].as_str().unwrap();
    let label_id = db
        .store
        .create_cert_label(
            &AdminContext::system_admin("test-admin"),
            domain_id,
            "Self-signed SNI",
            None,
        )
        .await
        .unwrap();
    db.store
        .set_node_cert_label(
            &AdminContext::system_admin("test-admin"),
            "n1",
            Some(&label_id),
        )
        .await
        .unwrap();
    assert_eq!(brocade_console::certs::scan_once(&db.store).await, (1, 0));
    let groups = db
        .store
        .cert_groups(&AdminContext::system_admin("test-admin"))
        .await
        .unwrap();
    let serving = groups[0]
        .certificates
        .iter()
        .find(|certificate| certificate.status == "serving")
        .unwrap();
    assert!(
        serving
            .issuer
            .as_deref()
            .is_some_and(|issuer| !issuer.trim().is_empty() && issuer != "Let's Encrypt"),
        "a self-signed leaf should identify its generated private root"
    );
    let row = sqlx::query("SELECT key_pem_sealed, peer_sha256 FROM certificates WHERE id = $1")
        .bind(&serving.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let sealed_key: String = row.try_get("key_pem_sealed").unwrap();
    let peer_sha256: String = row.try_get("peer_sha256").unwrap();
    assert!(!sealed_key.contains("BEGIN PRIVATE KEY"));
    assert_eq!(peer_sha256.len(), 64);
    let material = db.store.cert_delta_for_node("n1").await.unwrap().unwrap();
    assert_eq!(material.cert_pem.matches("BEGIN CERTIFICATE").count(), 1);
    assert!(material.key_pem.contains("BEGIN PRIVATE KEY"));
}

async fn apply_step_json(
    app: &Router,
    token: &str,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
    step: Value,
) -> (StatusCode, Value) {
    post_json(
        app,
        token,
        "/model/apply",
        json!({
            "ops": [{
                "op": "put_step",
                "app_id": app_id,
                "chain_id": chain_id,
                "node_id": node_id,
                "step": step,
            }],
        }),
    )
    .await
}

async fn get_json(app: &Router, token: &str, uri: &str) -> (StatusCode, Value) {
    request_json_with_token(app, "GET", uri, None, token).await
}

async fn get_json_with_token(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
    request_json_with_token(app, "GET", uri, None, token).await
}

async fn post_json(app: &Router, token: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    request_json_with_token(app, "POST", uri, Some(body), token).await
}

async fn put_json(app: &Router, token: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    if uri == "/settings" {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = response.headers().get("etag").unwrap().clone();
        let mut complete: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        merge_json(&mut complete, body);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .header("if-match", etag)
                    .header("content-type", "application/json")
                    .body(Body::from(complete.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value = if bytes.is_empty() {
            json!(null)
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        return (status, value);
    }
    request_json_with_token(app, "PUT", uri, Some(body), token).await
}

fn merge_json(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if let Some(existing) = target.get_mut(&key) {
                    merge_json(existing, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, value) => *target = value,
    }
}

async fn request_json_with_token(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_operator_password_lifecycle() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, admin_token) = admin_app(&db).await;
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    // A password given at creation
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "editor-1",
                        "display_name": "Editor One",
                        "role": "editor",
                        "tenant_scope": "platform.acme",
                        "password": "first-password"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response_json(response).await["passwordless"], false);

    // Arbitrary privileged accounts cannot be created passwordless.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "editor-2",
                        "display_name": "Editor Two",
                        "role": "editor",
                        "tenant_scope": "platform.acme"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // The one deliberate exception is the fixed public readonly operator.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": "public",
                        "display_name": "Public",
                        "role": "readonly",
                        "tenant_scope": "platform.acme"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response_json(response).await["passwordless"], true);

    let guest_cookie = login_cookie(&app, "public", "").await;
    let guest = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", &guest_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(guest.status(), StatusCode::OK);
    assert_eq!(response_json(guest).await["operator_id"], "public");

    let cookie = login_cookie(&app, "editor-1", "first-password").await;

    // Setting a password on someone's behalf: the response carries a one-time password, the old one
    // stops working at once, and the old sessions are void
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/operators/editor-1/password")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let reset = response_json(response).await;
    let reset_password = reset["password"].as_str().unwrap().to_owned();
    assert_eq!(reset["operator_id"], "editor-1");
    assert!(reset["sessions_revoked"].as_u64().unwrap() >= 1);

    let stale = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    // A self-service change: editor holds only Edit and must still be able to take this route
    let cookie = login_cookie(&app, "editor-1", &reset_password).await;
    let wrong = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/password")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "current_password": "not-the-password",
                        "new_password": "chosen-by-me"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let changed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/password")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "current_password": reset_password,
                        "new_password": "chosen-by-me"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    assert_eq!(response_json(changed).await["operator_id"], "editor-1");

    // This session survives the change — otherwise changing a password logs one out on the spot
    let still_live = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/whoami")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(still_live.status(), StatusCode::OK);
    login_cookie(&app, "editor-1", "chosen-by-me").await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn public_guest_can_read_masked_ping_probe_history_but_not_settings() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    db.store
        .update_ping_probe_settings(
            &AdminContext::system_admin("fixture"),
            PingProbeSettings {
                targets: vec![PingProbeTarget {
                    name: "TCP".to_owned(),
                    address: "tcp://192.0.2.1:443".to_owned(),
                }],
                interval_secs: 60,
                timeout_ms: 420,
            },
        )
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    db.store
        .record_ping_probe(
            "n1",
            PingProbeReportRequest {
                probed_at_unix_secs: now,
                samples: vec![PingProbeSample {
                    target: "tcp://192.0.2.1:443".to_owned(),
                    attempted: true,
                    latency_us: Some(37_250),
                }],
            },
        )
        .await
        .unwrap();

    let (app, admin_token) = admin_app(&db).await;
    let enabled = put_json(
        &app,
        &admin_token,
        "/visitor-access",
        json!({ "enabled": true }),
    )
    .await;
    assert_eq!(enabled.0, StatusCode::OK);
    assert_eq!(enabled.1["public_open"], true);
    let cookie = login_cookie(&app, "public", "").await;

    // Public access includes the system's masked user and usage views. The same response layer
    // that masks machine addresses must keep UUID credentials and login state out of both the
    // dedicated list and the model snapshot.
    for path in [
        "/users?include_disabled=true",
        "/quotas",
        "/usage/samples",
        "/usage/monthly-summary",
        "/certs",
    ] {
        assert_eq!(
            get_json_with_cookie(&app, path, &cookie).await.0,
            StatusCode::OK,
            "public read-only view stayed closed: {path}"
        );
    }
    let probe_plan =
        get_json_with_cookie(&app, "/users/platform.acme/alice/grant-probes", &cookie).await;
    assert!(
        matches!(
            probe_plan.0,
            StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
        ),
        "public probe plan was refused by authorization: {}",
        probe_plan.0
    );
    let users = get_json_with_cookie(&app, "/users?include_disabled=true", &cookie).await;
    let listed_user = &users.1["users"][0];
    assert_eq!(listed_user["id"], "alice");
    assert!(listed_user.get("uuid").is_none());
    assert!(listed_user.get("login_enabled").is_none());

    let snapshot = get_json_with_cookie(&app, "/model/snapshot", &cookie).await;
    assert_eq!(snapshot.0, StatusCode::OK);
    assert_eq!(snapshot.1["snapshot"]["users"][0]["id"], "alice");
    assert!(snapshot.1["snapshot"]["users"][0].get("uuid").is_none());
    assert_eq!(
        snapshot.1["snapshot"]["apps"][0]["grants"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        snapshot.1["snapshot"]["apps"][0]["chains"][0]["id"],
        "chn-b2c3-d4e5"
    );

    let list = get_json_with_cookie(&app, "/ping-probe/nodes?window_secs=3600", &cookie).await;
    assert_eq!(list.0, StatusCode::OK);
    assert_eq!(list.1["nodes"][0]["node_id"], "n1");
    assert_eq!(
        list.1["nodes"][0]["targets"][0]["samples"][0]["latency_us"],
        37_250
    );
    let list_address = list.1["nodes"][0]["targets"][0]["address"]
        .as_str()
        .unwrap();
    assert!(
        list_address.starts_with("tcp://"),
        "probe kind was lost: {list_address}"
    );
    assert!(
        !list_address.contains("192.0.2.1"),
        "probe address leaked: {list_address}"
    );

    let detail = get_json_with_cookie(&app, "/ping-probe/nodes/n1?window_secs=3600", &cookie).await;
    assert_eq!(detail.0, StatusCode::OK);
    assert_eq!(detail.1["targets"][0]["samples"][0]["latency_us"], 37_250);
    assert!(detail.1["targets"][0]["address"]
        .as_str()
        .unwrap()
        .starts_with("tcp://"));

    let settings = get_json_with_cookie(&app, "/ping-probe/settings", &cookie).await;
    assert_eq!(settings.0, StatusCode::FORBIDDEN);

    let disabled = put_json(
        &app,
        &admin_token,
        "/visitor-access",
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(disabled.0, StatusCode::OK);
    assert_eq!(disabled.1["public_open"], false);
    assert_eq!(
        get_json_with_cookie(&app, "/whoami", &cookie).await.0,
        StatusCode::UNAUTHORIZED,
        "disabling visitor access must close existing visitor sessions"
    );
}

/// Sign in once and extract the session cookie for the requests that follow.
async fn login_cookie(app: &Router, operator_id: &str, password: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "operator_id": operator_id, "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "login should succeed for {operator_id}"
    );
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

async fn get_json_with_cookie(app: &Router, uri: &str, cookie: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, response_json(response).await)
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn observed_grants_from_desired(desired: &Value) -> Value {
    let inbounds = desired["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|inbound| {
            let clients = inbound["clients"]
                .as_array()
                .unwrap()
                .iter()
                .map(|client| {
                    json!({
                        "email": client["email"],
                        "uuid": client["uuid"],
                        "flow": client["flow"]
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "tag": inbound["tag"],
                "clients": clients
            })
        })
        .collect::<Vec<_>>();
    json!({ "inbounds": inbounds })
}

/// Releasing an agent, over the full HTTP path: an admin records the clearance, and a node with a
/// token asks what it should be running.
///
/// Worth testing here rather than only in store, because everything that decides the answer is
/// wiring: the clearance is compared against a build id computed from the *binaries compiled into
/// this control plane*, the architecture arrives in a header only the node can set, and the URL is
/// assembled from the agent-facing origin. None of that exists at the store layer, and each of
/// them fails silently — a node that is told 204 forever looks exactly like a node with nothing to
/// do.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_agent_release_is_offered_only_to_nodes_in_scope() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;

    let (admin, admin_token) = admin_app(&db).await;
    let agent = agent_router_with_origin(db.store.clone(), "http://10.0.0.7:9091".to_owned());

    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nodes/n1/agent-token")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let issued: IssuedNodeToken =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();

    // The node's own question, asked the way the agent asks it.
    let ask = |token: String, arch: Option<&'static str>| {
        let agent = agent.clone();
        async move {
            let mut request = Request::builder()
                .method("GET")
                .uri("/agent/v1/agent-release")
                .header("authorization", format!("Bearer {token}"))
                .header(
                    "x-brocade-protocol-version",
                    brocade_deployment::protocol::MIN_AGENT_PROTOCOL_VERSION.to_string(),
                );
            if let Some(arch) = arch {
                request = request.header("x-brocade-arch", arch);
            }
            agent
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };

    // Nothing released yet. This is the answer for the entire life of a fleet nobody has released
    // to, so it must not be an error.
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // What this control plane can actually serve. The admin side has to publish it, because
    // nothing else knows it — it comes from the binaries compiled in.
    let response = admin
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent-release")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let view: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    let available = view["available_release_id"].as_str().unwrap().to_owned();
    assert_eq!(available.len(), 64, "构建号是 sha256");
    assert_eq!(view["released"]["scope"], "off");

    let release = |body: Value| {
        let admin = admin.clone();
        let admin_token = admin_token.clone();
        async move {
            admin
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/agent-release")
                        .header("authorization", format!("Bearer {admin_token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    // Staged to a node that is not this one.
    let response = release(json!({
        "release_id": available, "scope": "nodes", "nodes": ["n2"]
    }))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "不在范围里的节点不该拿到"
    );

    // Staged to this one.
    let response = release(json!({
        "release_id": available, "scope": "nodes", "nodes": ["n1"]
    }))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let offer: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    // The value, not merely the key: the URL is what a root process will fetch and run, and an
    // origin dropped on the floor points every node at a port nothing listens on.
    assert_eq!(
        offer["url"], "http://10.0.0.7:9091/brocade-agent/x86_64",
        "下载地址要是节点那侧够得着的"
    );
    let sha = offer["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    // The sha must belong to the architecture that was asked for, not to whichever agent happens
    // to be listed first. Getting this wrong installs a binary that verifies and cannot run.
    let x86 = view["available_agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["arch"] == "x86_64")
        .unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(sha, x86);

    // A different architecture gets that architecture's bytes.
    let response = ask(issued.token.clone(), Some("aarch64")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let arm: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_ne!(arm["sha256"], offer["sha256"], "两个架构不可能同一个 sha");

    // An architecture this control plane does not carry, and a request that names none. Both are
    // 204 rather than an error: nothing is wrong with such a machine, there is simply nothing here
    // for it, and an error would show up on the console as a node failing.
    for arch in [Some("riscv64"), None] {
        let response = ask(issued.token.clone(), arch).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{arch:?}");
    }

    // Widening to the whole fleet.
    let response = release(json!({ "release_id": available, "scope": "all", "nodes": [] })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(response.status(), StatusCode::OK);

    // A clearance naming a build this control plane does not have — which is exactly what a
    // redeployed control plane looks like. It must serve nothing, or deploying the control plane
    // would double as releasing whatever agent it happens to embed.
    let response = release(json!({
        "release_id": "0".repeat(64), "scope": "all", "nodes": []
    }))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "批准的不是这台控制面带的那一批，就该谁也不给"
    );

    // A legacy agent cannot consume desired state, so the same staged scope is allowed to rescue
    // it with this control plane's embedded build even though the stored build id is stale.
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/agent-release")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("x-brocade-arch", "x86_64")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Pausing keeps the build id, so resuming does not mean choosing it again.
    let response = release(json!({ "release_id": available, "scope": "off", "nodes": [] })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let paused: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(paused["released"]["release_id"], available);
    let response = ask(issued.token.clone(), Some("x86_64")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // No token, no answer. This endpoint says which bytes a machine should run as root.
    let response = agent
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/agent/v1/agent-release")
                .header("x-brocade-arch", "x86_64")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn http_grant_probe_plan_is_frozen_and_contains_no_connection_material() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let revision = seed_subscription_serving(&db).await;
    let (app, token) = admin_app(&db).await;

    let response = get_json(&app, &token, "/users/platform.acme/alice/grant-probes").await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["serving_revision"], revision);
    assert_eq!(response.1["serving_generation"], 1);
    assert!(!response.1["items"].as_array().unwrap().is_empty());

    let created = post_json(
        &app,
        &token,
        "/admin/operators",
        json!({
            "id": "probe-reader",
            "display_name": "Probe Reader",
            "role": "readonly",
            "tenant_scope": "platform.acme",
            "password": "reader-secret"
        }),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED);
    let reader_token = post_json(
        &app,
        &token,
        "/admin/operators/probe-reader/token",
        json!({}),
    )
    .await;
    assert_eq!(reader_token.0, StatusCode::CREATED);
    let reader_token = reader_token.1["token"].as_str().unwrap();
    let readonly_plan = get_json_with_token(
        &app,
        "/users/platform.acme/alice/grant-probes",
        reader_token,
    )
    .await;
    assert_eq!(readonly_plan.0, StatusCode::OK);
    assert_eq!(readonly_plan.1["serving_revision"], revision);
    assert_eq!(
        post_json(
            &app,
            reader_token,
            "/users/platform.acme/alice/grant-probes",
            json!({ "item_ids": [] }),
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "readonly may inspect the plan but must never execute its credentials"
    );

    let serialized = response.1.to_string();
    let readonly_serialized = readonly_plan.1.to_string();
    for secret in [
        "2d2304da-f114-4574-8d44-625afdb1db5c",
        "n1.example.net",
        "reality-public",
        "8337a0bf",
        "www.example.com",
    ] {
        assert!(
            !serialized.contains(secret),
            "grant probe plan leaked connection material {secret}: {serialized}"
        );
        assert!(
            !readonly_serialized.contains(secret),
            "readonly grant probe plan leaked connection material {secret}: {readonly_serialized}"
        );
    }
}

async fn insert_node(pool: &PgPool) {
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            'n1', 'platform.acme', 'Node 1', 'n1.example.net', '10.66.0.1',
            'wg-private', 'wg-public', 51820,
            10085, TRUE, TRUE,
            'servers', '[\"1.1.1.1\"]'::jsonb
         )",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn provision_body(id: &str) -> Value {
    json!({
        "id": id,
        "tenant_id": "platform.acme",
        "name": format!("Node {id}"),
        "public_ipv4": format!("{id}.example.net"),
        "wg_listen_port": 51820,
        "api_port": 10085,
        "overlay": true,
        "egress_allowed": true,
        "dns": { "t": "servers", "v": ["1.1.1.1"] }
    })
}

async fn insert_usage_model(pool: &PgPool) {
    insert_node(pool).await;
    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'alice', '2d2304da-f114-4574-8d44-625afdb1db5c')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO apps (id, label, position) VALUES ('app-a1b2', 'Main App', 0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('chn-b2c3-d4e5', 'app-a1b2', 'platform.acme', 'Main Chain', 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        // `transport_kind` is not decoration here: `ingresses_has_a_wire` refuses a row with
        // neither a TCP wire nor Hysteria 2, because an ingress listening on nothing compiles to a
        // machine with no inbound.
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'ing-b2c3', 'app-a1b2', 'chn-b2c3-d4e5', 'n1', '0.0.0.0', 443, NULL, 'vless-reality',
            'reality-private', 'reality-public', '[\"8337a0bf\"]'::jsonb,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'xtls-rprx-vision',
            'custom-site'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ingress_client_settings (ingress_id, reality_fingerprint)
         VALUES ('ing-b2c3', 'chrome')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules)
         VALUES ('chn-b2c3-d4e5', 'n1', '[{\"match\":{\"t\":\"any\"},\"action\":{\"t\":\"egress\",\"send_through\":null}}]'::jsonb)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-a1b2', 'platform.acme', 'alice', 'ing-b2c3')",
    )
    .execute(pool)
    .await
    .unwrap();
}
