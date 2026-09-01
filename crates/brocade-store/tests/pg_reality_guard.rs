//! The REALITY fallback guard, from the console request down to what the compiler will read back.
//!
//! Worth a database rather than a unit test because nothing here is Rust: the flag crosses an
//! `INSERT` whose parameters are positional, and a column added in the wrong place shifts every
//! bind after it onto its neighbour. That failure produces no compile error and no test failure
//! anywhere else — the ingress simply comes back with somebody else's value in it.
//!
//! Its own fixture rather than `pg_integration`'s: this needs an ingress written through the
//! console path, which is exactly the path under test, so seeding one by hand would prove nothing.

use brocade_core::model::Projection;
use brocade_store::{
    AdminContext, CreateIngressRequest, CreateRealityIngressRequest, PgStore, TransportRequest,
    WiresRequest,
};
use sqlx::PgPool;
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn fallback_guard_round_trips() {
    if std::env::var("BROCADE_RUN_PG_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping Postgres integration test; set BROCADE_RUN_PG_TESTS=1 to run");
        return;
    }
    let container = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let store = PgStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let pool = PgPool::connect(&url).await.unwrap();

    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed, dns_kind, dns_servers
         ) VALUES (
            'n1', 'platform.acme', 'Node 1', 'n1.example.net', '10.66.0.1',
            'wg-private', 'wg-public', 51820, 10085, TRUE, TRUE, 'servers', '[\"1.1.1.1\"]'::jsonb
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO apps (id, label, position) VALUES ('app-main', 'Main App', 0)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-main', 'app-main', 'platform.acme', 'Main Chain', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let face = |guard: Option<bool>| CreateIngressRequest {
        wires: WiresRequest {
            vless: Some(TransportRequest::VlessReality),
            hysteria2: None,
        },
        id: "i-main".to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port: 443,
        front_id: None,
        guard: brocade_core::model::IngressGuard::OPEN,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: guard,
            dest: Some("www.example.com:443".to_owned()),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: Some("chrome".to_owned()),
            flow: None,
        },
        projection: Projection::default(),
        note: None,
    };
    let admin = AdminContext::system_admin("test-system");

    // Absent means on.
    store
        .upsert_ingress(&admin, "app-main", face(None))
        .await
        .unwrap();
    let snapshot = store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot.apps[0].ingresses[0]
        .wires
        .reality()
        .unwrap()
        .clone();
    assert!(stored.fallback_guard, "缺省应当是开");

    // Off survives the write and comes back off.
    store
        .upsert_ingress(&admin, "app-main", face(Some(false)))
        .await
        .unwrap();
    let snapshot = store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot.apps[0].ingresses[0]
        .wires
        .reality()
        .unwrap()
        .clone();
    assert!(!stored.fallback_guard, "关掉之后读回来还是关");

    // And back on again, so that the column is not merely stuck at whatever landed first.
    let echo = store
        .upsert_ingress(&admin, "app-main", face(Some(true)))
        .await
        .unwrap();
    // The echo is what the console reads back after a save, so the flag has to be in it — a UI
    // toggle reading an absent field would show every ingress as unguarded.
    assert_eq!(echo.ingress["wires"]["vless"]["fallback_guard"], true);
    let snapshot = store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot.apps[0].ingresses[0]
        .wires
        .reality()
        .unwrap()
        .clone();
    assert!(stored.fallback_guard);
}
