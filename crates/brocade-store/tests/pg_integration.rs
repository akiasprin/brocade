use brocade_core::{
    client_config::{SubscriptionClientConfig, SUBSCRIPTION_CLIENT_CONFIG_SCHEMA},
    compile::compile,
    model::{
        Action, AnyTls, ConnectionSettings, DestMatch, Dns, DomainStrategy,
        EgressDnsAddressStrategy, EgressDnsFallback, EgressDnsResolution, EgressDnsTransport,
        ExternalOutboundProtocol, ExternalOutboundSecurity, ExternalVlessTransport,
        ExternalVlessXhttp, ExternalVlessXhttpDownload, HopDial, HopPool, Hysteria2,
        HysteriaBandwidth, HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, IngressWires,
        ModelSettings, NodeConnection, OverlaySettings, PortSettings, Projection,
        ProjectionDownloadEndpoint, ProjectionEndpoint, RealityClientPolicy, RealityFallbackLimits,
        RealityFallbackMode, RealityFallbackRateLimit, RealitySite, Rule, Transport, WgTransport,
        Xhttp, XhttpDownload, XhttpMode, XhttpTuning, XhttpXmux, XhttpXmuxRange,
    },
};
use brocade_deployment::plan::{
    AppliedArtifactState, AppliedGrantsState, DeploymentKind, DeploymentPlan, DesiredArtifact,
    DesiredGrants, ObservedClient, ObservedInbound, PlannedAction, PlannedTarget,
    PlannedTargetStatus,
};
use brocade_deployment::protocol::{
    DiskDetailSample, GeodataFileState, GeodataObservation, HostFacts, LoadReportRequest,
    LoadSample, LocalReconcileReport, NetworkDetailSample, NodeRuntimeReport, NodeVersions,
    SpoolBacklog, WireGuardHealth, WireGuardPeerHealth, WireGuardPeerStatus,
};
use brocade_store::{
    generate_reality_short_id, is_reality_short_id, node_token_display_prefix, node_token_hash,
    AbandonNodeRequest, AdminContext, AdminLoginRequest, AdminRole, ApplyDraftResult,
    CertDomainInput, ChangeAdminPasswordRequest, CreateAdminOperatorRequest, CreateAppRequest,
    CreateChainRequest, CreateDeploymentRequest, CreateGrantRequest, CreateIngressRequest,
    CreateRealityIngressRequest, CreateRollbackRequest, CreateTenantRequest, CreateUserRequest,
    HopInRequest, HopWireRequest, IsolateDeploymentTargetRequest, LinkProbe, LinkProbeRequest,
    LinkProbeStatus, ModelOp, NodeDesiredDeployment, PgStore, PingProbeReportRequest,
    PingProbeSample, PingProbeSettings, PingProbeTarget, ProbeTransport, ProvisionNodeRequest,
    PutStepRequest, RegisterWarpBindingRequest, RemoveWarpBindingRequest, ReportedNodeState,
    RestoreNodeServiceRequest, SetUserAppQuotaRequest, StepAcceptRequest, StoreError,
    TargetApplyResult, TargetConvergenceReport, TransportRequest, UpdateAgentLogDefaultRequest,
    UpdateNodeLogPolicyRequest, UpdateNodeRequest, UpdateRealtimeTelemetryPolicyRequest,
    UpdateUserStatusRequest, UpdateWarpBindingRequest, UpsertExternalOutboundRequest, UsageCounter,
    UsageReportRequest, VerifyDeploymentRequest, WiresRequest, ENROLLMENT_TOKEN_PREFIX,
    NODE_TOKEN_PREFIX,
};
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::time::Duration;
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// A User-Agent in the shape the agent actually sends: `brocade-agent/<sha256 of its own binary>`.
/// Not a version number — nobody bumps one on the way to a node, so it identified nothing.
const AGENT_BUILD: &str =
    "brocade-agent/9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

struct TestPg {
    _container: testcontainers::ContainerAsync<Postgres>,
    store: PgStore,
    /// The URL the store was opened with. Kept so that a test can point at a *different* database
    /// on the same server without paying for a second container.
    url: String,
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
            url: database_url,
        })
    }

    /// The same server, a different database name.
    fn url_for(&self, database: &str) -> String {
        let (prefix, _) = self.url.rsplit_once('/').expect("URL 末尾是 /<库名>");
        format!("{prefix}/{database}")
    }

    fn pool(&self) -> &PgPool {
        self.store.pool()
    }
}

fn system_admin() -> AdminContext {
    AdminContext::system_admin("test-system")
}

/// Direct SQL fixtures do not pass through a revision commit, so freeze their current shape as
/// the initial deployed model before exercising subscription semantics.
async fn seed_subscription_serving(db: &TestPg) -> u64 {
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let revision = snapshot.revision;
    sqlx::query(
        "INSERT INTO model_snapshots (revision_id, snapshot)
         VALUES ($1, $2)
         ON CONFLICT (revision_id) DO UPDATE SET snapshot = EXCLUDED.snapshot",
    )
    .bind(i64::try_from(revision).unwrap())
    .bind(serde_json::to_value(&snapshot).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    let client = SubscriptionClientConfig::from_snapshot(&snapshot);
    // Hash the persisted JSON document, just like the production checkpoint writer. Hashing the
    // typed struct directly would preserve struct-field order, while a JSON value (and JSONB)
    // canonicalizes object keys; those are semantically equal documents but different byte
    // sequences.
    let client_document = serde_json::to_value(client).unwrap();
    let client_sha = brocade_core::hash::sha256_hex(&serde_json::to_vec(&client_document).unwrap());
    let client_snapshot_id: i64 = sqlx::query_scalar(
        "INSERT INTO subscription_client_snapshots (
             source_revision_id, schema_version, document, content_sha256, reason
         ) VALUES ($1, $2, $3, $4, 'test-serving-seed')
         RETURNING id",
    )
    .bind(i64::try_from(revision).unwrap())
    .bind(i32::try_from(SUBSCRIPTION_CLIENT_CONFIG_SCHEMA).unwrap())
    .bind(client_document)
    .bind(client_sha)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE subscription_client_state
            SET head_snapshot_id = $1, updated_at = now()
          WHERE id = TRUE",
    )
    .bind(client_snapshot_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO subscription_serving_state (
             id, topology_revision_id, permissions_revision_id, client_snapshot_id, generation
         ) VALUES (TRUE, $1, $1, $2, 1)
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
    .bind(client_snapshot_id)
    .execute(db.pool())
    .await
    .unwrap();
    revision
}

/// Commit direct fixture mutations as a real immutable revision. Production writes do this in
/// `commit_revision`; keeping the helper local avoids exposing a test-only store API.
async fn commit_direct_fixture_revision(db: &TestPg, note: &str) -> u64 {
    let revision: i64 = sqlx::query_scalar(
        "INSERT INTO revisions (author, note) VALUES ('test:fixture', $1) RETURNING id",
    )
    .bind(note)
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE control_state SET current_revision = $1 WHERE id = TRUE")
        .bind(revision)
        .execute(db.pool())
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, u64::try_from(revision).unwrap());
    sqlx::query("INSERT INTO model_snapshots (revision_id, snapshot) VALUES ($1, $2)")
        .bind(revision)
        .bind(serde_json::to_value(snapshot).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    u64::try_from(revision).unwrap()
}

async fn put_step_draft(
    db: &TestPg,
    app_id: &str,
    chain_id: &str,
    node_id: &str,
    step: PutStepRequest,
) -> ApplyDraftResult {
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::PutStep {
                app_id: app_id.to_owned(),
                chain_id: chain_id.to_owned(),
                node_id: node_id.to_owned(),
                step,
            }],
            None,
        )
        .await
        .unwrap()
}

fn tenant_admin(scope: &str) -> AdminContext {
    AdminContext::new(
        format!("tenant-admin:{scope}"),
        AdminRole::TenantAdmin,
        Some(scope.to_owned()),
    )
}

fn publisher(scope: &str) -> AdminContext {
    AdminContext::new(
        format!("publisher:{scope}"),
        AdminRole::Publisher,
        Some(scope.to_owned()),
    )
}

/// Issues a certificate for whatever group this machine draws from, the way a scan would: ask for
/// a row, then record a result against it. Returns the certificate id.
///
/// Per group rather than per machine, which is the whole point of the pool — two machines in one
/// group are served by the one call.
async fn issue_certificate_for(db: &TestPg, node_id: &str, issuer: &str) -> String {
    let cert_id = db
        .store
        .request_spare_certificate(&system_admin(), &label_of_node(db, node_id).await)
        .await
        .unwrap();
    db.store
        .record_certificate(
            &cert_id,
            "-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\ny\n-----END PRIVATE KEY-----\n",
            "2099-01-01T00:00:00Z",
            issuer,
        )
        .await
        .unwrap();
    cert_id
}

/// Which group a machine draws from, creating one for it if the fixture did not.
///
/// A machine has a group only because somebody chose one — there is no default — so a fixture that
/// inserts nodes with raw SQL has to make that choice too. This is that choice, made once for the
/// tests that need a certificate to exist.
async fn label_of_node(db: &TestPg, node_id: &str) -> String {
    let existing: Option<String> =
        sqlx::query("SELECT label_id FROM node_cert_label WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(db.pool())
            .await
            .unwrap()
            .map(|row| row.try_get("label_id").unwrap());
    if let Some(label_id) = existing {
        return label_id;
    }
    let domain_id: String = sqlx::query("SELECT id FROM cert_domains ORDER BY id LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("id")
        .unwrap();
    let label_id = db
        .store
        .create_cert_label(&system_admin(), &domain_id, &format!("组 {node_id}"), None)
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_cert_label (node_id, label_id) VALUES ($1, $2)")
        .bind(node_id)
        .bind(&label_id)
        .execute(db.pool())
        .await
        .unwrap();
    label_id
}

fn provision_node_request(id: &str) -> ProvisionNodeRequest {
    ProvisionNodeRequest {
        id: id.to_owned(),
        tenant_id: "platform.acme".to_owned(),
        name: format!("Node {id}"),
        public_ipv4: Some(format!("{id}.example.net")),
        public_ipv6: None,
        public_ipv4_nat: false,
        public_ipv6_nat: false,
        wg_listen_port: 51820,
        api_port: Some(10085),
        overlay: true,
        egress_allowed: true,
        dns: Dns::Servers(vec!["1.1.1.1".to_owned()]),
        domain_strategy: DomainStrategy::default(),
        note: None,
        cert_label_id: None,
        enrollment_ttl_seconds: None,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn certificate_scan_lock_is_shared_by_console_instances() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let first = db
        .store
        .try_certificate_scan_lock()
        .await
        .unwrap()
        .expect("first worker acquires the lock");
    assert!(
        db.store
            .try_certificate_scan_lock()
            .await
            .unwrap()
            .is_none(),
        "a second worker must skip while issuance is active"
    );
    drop(first);
    assert!(
        db.store
            .try_certificate_scan_lock()
            .await
            .unwrap()
            .is_some(),
        "dropping the transaction releases the lock"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn migrations_bootstrap_empty_snapshot() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let table_count: i64 = sqlx::query(
        "SELECT count(*) AS n FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert!(table_count >= 20);

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.overlay_cidr.to_string(), "10.66.0.0/16");
    assert!(snapshot.nodes.is_empty());
    assert!(snapshot.users.is_empty());
    assert!(snapshot.apps.is_empty());

    let serving_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name
           FROM information_schema.columns
          WHERE table_schema = 'public'
            AND table_name = 'subscription_serving_state'
          ORDER BY ordinal_position",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        serving_columns,
        vec![
            "id",
            "topology_revision_id",
            "permissions_revision_id",
            "client_snapshot_id",
            "topology_deployment_id",
            "permissions_deployment_id",
            "isolated_node_ids",
            "generation",
            "updated_at",
        ]
    );
    let serving_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM subscription_serving_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(serving_rows, 0, "a fresh database has never been deployed");
    let client_head: Option<i64> = sqlx::query_scalar(
        "SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        client_head.is_some(),
        "migration initializes the committed client head"
    );

    let reality_defaults =
        sqlx::query("SELECT reality_dest, reality_server_names FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        reality_defaults
            .try_get::<Option<String>, _>("reality_dest")
            .unwrap(),
        None
    );
    assert_eq!(
        reality_defaults
            .try_get::<serde_json::Value, _>("reality_server_names")
            .unwrap(),
        json!([])
    );

    let branding =
        sqlx::query("SELECT site_name, site_icon_data_url FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        branding.try_get::<String, _>("site_name").unwrap(),
        "Brocade"
    );
    assert_eq!(
        branding
            .try_get::<Option<String>, _>("site_icon_data_url")
            .unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn migration_0001_replays_after_its_checksum_row_is_cleared() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    sqlx::query(
        "INSERT INTO apps (id, label, position)
         VALUES ('app-secondary', 'Secondary App', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // Simulate the pre-position chain schema with more than one row in the same app. The replay
    // must rank each app independently and use stable chain IDs as the legacy/default order.
    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES
            ('c-zulu', 'app-main', 'platform.acme', 'Zulu Chain', 1),
            ('c-alpha', 'app-main', 'platform.acme', 'Alpha Chain', 2)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::raw_sql(
        "INSERT INTO fronts (id, app_id, tenant_id, name, strategy)
         VALUES ('f-main', 'app-main', 'platform.acme', 'Main Front', 'select');
         UPDATE ingresses SET front_id = 'f-main' WHERE id = 'i-main';
         INSERT INTO front_vias (front_id, ingress_id, ordinal) VALUES ('f-main', 'i-main', 0);
         UPDATE steps
            SET accept_uuid = '87a10c5e-c3a4-4f19-95a5-d56fef82cd8a',
                accept_label = 'c-main@n1'
          WHERE chain_id = 'c-main' AND node_id = 'n1';
         INSERT INTO e2e_probes
             (chain_id, app_id, node_id, status, ttfb_ms, exit_verdict, probed_at)
         VALUES ('c-main', 'app-main', 'n1', 'ok', 12, 'unknown', now());
         INSERT INTO e2e_probe_samples (chain_id, probed_at, status, ttfb_ms)
         VALUES ('c-main', now(), 'ok', 12);
         INSERT INTO user_app_quotas (tenant_id, user_id, app_id, limit_bytes)
         VALUES ('platform.acme', 'alice', 'app-main', 1000);
         INSERT INTO quota_suspensions (tenant_id, user_id, app_id, ingress_id)
         VALUES ('platform.acme', 'alice', 'app-main', 'i-main');
         INSERT INTO usage_readings
             (node_id, label, read_at, xray_started_at, uplink_bytes, downlink_bytes)
         VALUES
             ('n1', 'alice@platform.acme#i-main', now() - interval '2 minutes', now() - interval '1 hour', 10, 20),
             ('n1', 'c-main@n1', now() - interval '2 minutes', now() - interval '1 hour', 30, 40);
         INSERT INTO usage_samples
             (window_start, window_end, node_id, tenant_id, user_id, ingress_id, grant_label,
              uplink_bytes, downlink_bytes, app_id)
         VALUES
             (now() - interval '2 minutes', now() - interval '1 minute', 'n1', 'platform.acme',
              'alice', 'i-main', 'alice@platform.acme#i-main', 10, 20, 'app-main');
         INSERT INTO usage_chain_samples
             (window_start, window_end, node_id, tenant_id, app_id, chain_id, hop_label,
              uplink_bytes, downlink_bytes)
         VALUES
             (now() - interval '2 minutes', now() - interval '1 minute', 'n1', 'platform.acme',
              'app-main', 'c-main', 'c-main@n1', 30, 40);
         INSERT INTO link_health
             (node_id, chain_id, peer_node_id, alive, downlink_bytes, window_secs, checked_at)
         VALUES ('n1', 'app-main/c-main', 'n1', TRUE, 1, 30, now());
         INSERT INTO node_hop_link_samples
             (node_id, chain_id, peer_node_id, window_start, window_end, conns,
              conns_measured, btlbw_p50_bps, btlbw_p90_bps, min_rtt_us, rtt_p50_us,
              rtt_p90_us, retrans_pct, busy_pct, rwnd_limited_pct, sndbuf_limited_pct)
         VALUES
             ('n1', 'app-main/c-main', 'n1', now() - interval '2 minutes',
              now() - interval '1 minute', 1, 1, 100, 200, 100, 200, 300, 0, 50, 0, 0)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    store_current_model_snapshot(db.pool(), &db.store).await;

    // Simulate the development schema which introduced machine DNS before its independent
    // priority column. Replaying 0001 must retain both policies and assign deterministic slots.
    for (position, host) in ["b.example", "a.example"].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO node_egress_dns (node_id, position, selector, resolution)
             VALUES ('n1', $1, $2, $3)",
        )
        .bind(i32::try_from(position).unwrap())
        .bind(json!({ "t": "domain_suffix", "v": [host] }))
        .bind(json!({
            "address": "192.0.2.53",
            "port": 53,
            "transport": "tcp",
            "address_strategy": "use_ip",
            "fallback": "stop"
        }))
        .execute(db.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "UPDATE model_snapshots
         SET snapshot = jsonb_set(snapshot, '{node_egress_dns}', $1, true)",
    )
    .bind(json!([
        {
            "node": "n1",
            "selector": { "t": "domain_suffix", "v": ["b.example"] },
            "resolution": {
                "address": "192.0.2.53",
                "port": 53,
                "transport": "tcp",
                "address_strategy": "use_ip",
                "fallback": "stop"
            }
        },
        {
            "node": "n1",
            "selector": { "t": "domain_suffix", "v": ["a.example"] },
            "resolution": {
                "address": "192.0.2.53",
                "port": 53,
                "transport": "tcp",
                "address_strategy": "use_ip",
                "fallback": "stop"
            }
        }
    ]))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("ALTER TABLE node_egress_dns DROP COLUMN position")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE apps DROP COLUMN position")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE chains DROP COLUMN position")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE control_state DROP COLUMN port_anytls_base")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::raw_sql(
        "ALTER TABLE ingresses ADD COLUMN reality_fingerprint TEXT;
         ALTER TABLE ingresses ADD COLUMN xhttp_host TEXT;
         ALTER TABLE ingresses ADD COLUMN xhttp_xmux JSONB;
         UPDATE ingresses AS ingress
            SET reality_fingerprint = client.reality_fingerprint,
                xhttp_host = client.xhttp_host,
                xhttp_xmux = client.xhttp_xmux
           FROM ingress_client_settings AS client
          WHERE client.ingress_id = ingress.id;
         DROP TABLE ingress_client_settings;
         ALTER TABLE ingresses ADD COLUMN xhttp_mux INTEGER;
         UPDATE ingresses SET xhttp_mux = 16 WHERE id = 'i-main';
         ALTER TABLE ingresses DROP COLUMN xhttp_xmux",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // Development deployments deliberately keep schema evolution in 0001. Clearing its sqlx
    // record makes the migrator execute the complete file again against an existing schema,
    // which is the same compatibility path used by those deployments.
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 1")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate().await.unwrap();

    let realtime_policy = db.store.realtime_telemetry_policy().await.unwrap();
    assert!(realtime_policy.enabled);
    assert_eq!(realtime_policy.interval_secs, 1);
    assert_eq!(db.store.settings().await.unwrap().ports.anytls_base, 16_000);

    let app_main = "app-main".to_owned();
    let app_secondary = "app-secondary".to_owned();
    let chain_alpha: String =
        sqlx::query_scalar("SELECT id FROM chains WHERE name = 'Alpha Chain'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let chain_main: String = sqlx::query_scalar("SELECT id FROM chains WHERE name = 'Main Chain'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let chain_zulu: String = sqlx::query_scalar("SELECT id FROM chains WHERE name = 'Zulu Chain'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let ingress_main: String =
        sqlx::query_scalar("SELECT id FROM ingresses WHERE chain_id = $1 AND node_id = 'n1'")
            .bind(&chain_main)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(chain_main.starts_with("c-") && chain_main.len() == 8);
    assert!(ingress_main.starts_with("i-") && ingress_main.len() == 8);
    assert_ne!(
        &chain_main[2..],
        &ingress_main[2..],
        "Chain and Ingress must share one six-letter namespace"
    );
    let migrated_xmux: serde_json::Value =
        sqlx::query_scalar("SELECT xhttp_xmux FROM ingress_client_settings WHERE ingress_id = $1")
            .bind(&ingress_main)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        migrated_xmux,
        json!({
            "max_concurrency": 16,
            "h_max_request_times": { "from": 600, "to": 900 },
            "h_max_reusable_secs": { "from": 1800, "to": 3000 }
        })
    );
    let legacy_xhttp_mux: Option<String> = sqlx::query_scalar(
        "SELECT column_name
           FROM information_schema.columns
          WHERE table_schema = 'public'
            AND table_name = 'ingresses'
            AND column_name = 'xhttp_mux'",
    )
    .fetch_optional(db.pool())
    .await
    .unwrap();
    assert_eq!(legacy_xhttp_mux, None);
    let removed_client_columns: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM information_schema.columns
          WHERE table_schema = 'public'
            AND table_name = 'ingresses'
            AND column_name = ANY(ARRAY['reality_fingerprint', 'xhttp_host', 'xhttp_xmux'])",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(removed_client_columns, 0);
    let reserved_bodies: (i64, i64) =
        sqlx::query_as("SELECT count(*), count(DISTINCT body) FROM friendly_model_id_bodies")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(reserved_bodies, (4, 4));
    let alias_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('model_id_aliases')::text")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        alias_table, None,
        "rolling compatibility data must be removed"
    );

    let live_refs: (String, String, String, String, String) = sqlx::query_as(
        "SELECT c.app_id, i.app_id, i.chain_id, g.app_id, g.ingress_id
           FROM chains c
           JOIN ingresses i ON i.chain_id = c.id
           JOIN grants g ON g.ingress_id = i.id
          WHERE c.id = $1",
    )
    .bind(&chain_main)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        live_refs,
        (
            app_main.clone(),
            app_main.clone(),
            chain_main.clone(),
            app_main.clone(),
            ingress_main.clone(),
        )
    );
    let structural_refs: (String, String, String, String, String) = sqlx::query_as(
        "SELECT f.app_id, fv.ingress_id, s.chain_id, p.app_id, p.chain_id
           FROM fronts f
           JOIN front_vias fv ON fv.front_id = f.id
           JOIN steps s ON s.node_id = 'n1'
           JOIN e2e_probes p ON p.node_id = 'n1'
          WHERE f.id = 'f-main'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        structural_refs,
        (
            app_main.clone(),
            ingress_main.clone(),
            chain_main.clone(),
            app_main.clone(),
            chain_main.clone(),
        )
    );
    let probe_sample_chain: String =
        sqlx::query_scalar("SELECT chain_id FROM e2e_probe_samples LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(probe_sample_chain, chain_main);

    let current_snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(current_snapshot.apps[0].id, app_main);
    assert!(current_snapshot.apps[0]
        .chains
        .iter()
        .any(|chain| chain.id == chain_main));
    assert_eq!(current_snapshot.apps[0].ingresses[0].id, ingress_main);

    let historical_old_ids: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM model_snapshots
          WHERE snapshot::text LIKE '%\"c-main\"%'
             OR snapshot::text LIKE '%\"i-main\"%'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        historical_old_ids, 0,
        "rollback snapshots retain legacy ids"
    );

    let user_usage: (String, String, String) =
        sqlx::query_as("SELECT app_id, ingress_id, grant_label FROM usage_samples LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        user_usage,
        (
            app_main.clone(),
            ingress_main.clone(),
            format!("alice@platform.acme#{ingress_main}"),
        )
    );
    let chain_usage: (String, String, String) =
        sqlx::query_as("SELECT app_id, chain_id, hop_label FROM usage_chain_samples LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        chain_usage,
        (
            app_main.clone(),
            chain_main.clone(),
            format!("{chain_main}@n1"),
        )
    );
    let health_chain: String = sqlx::query_scalar("SELECT chain_id FROM link_health LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(health_chain, format!("{app_main}/{chain_main}"));
    let hop_sample_chain: String =
        sqlx::query_scalar("SELECT chain_id FROM node_hop_link_samples LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(hop_sample_chain, format!("{app_main}/{chain_main}"));
    let quota_refs: (String, String, String) = sqlx::query_as(
        "SELECT q.app_id, s.app_id, s.ingress_id
           FROM user_app_quotas q
           JOIN quota_suspensions s USING (tenant_id, user_id)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        quota_refs,
        (app_main.clone(), app_main.clone(), ingress_main.clone())
    );
    let reading_labels: Vec<String> =
        sqlx::query_scalar("SELECT label FROM usage_readings ORDER BY label")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        reading_labels,
        [
            format!("alice@platform.acme#{ingress_main}"),
            format!("{chain_main}@n1"),
        ]
    );

    // Once the fleet has converged there is no second identity namespace. A stale pre-migration
    // counter is unknown rather than being silently attributed through a permanent alias.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let transition = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: now,
                xray_started_at_unix_secs: now - 3600,
                route: None,
                counters: vec![
                    UsageCounter {
                        label: "alice@platform.acme#i-main".to_owned(),
                        uplink_bytes: 50,
                        downlink_bytes: 70,
                    },
                    UsageCounter {
                        label: "c-main@n1".to_owned(),
                        uplink_bytes: 80,
                        downlink_bytes: 100,
                    },
                ],
            },
        )
        .await
        .unwrap();
    assert_eq!(transition.accepted_readings, 0);
    assert_eq!(transition.skipped_counters, 2);
    let legacy_readings: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM usage_readings WHERE label IN ('c-main@n1', 'alice@platform.acme#i-main')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(legacy_readings, 0);

    let collision = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::CreateApp {
                app: CreateAppRequest {
                    id: app_main.clone(),
                    label: "must not rename".to_owned(),
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(collision, StoreError::Conflict(_)));

    let resurrect = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertChain {
                app_id: app_main.clone(),
                chain: CreateChainRequest {
                    id: "c-main".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "legacy draft".to_owned(),
                    subscription_country: None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(resurrect, StoreError::InvalidData(_)));
    let legacy_chain_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chains WHERE id = 'c-main'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        legacy_chain_count, 0,
        "a stale draft resurrected a migrated chain id"
    );

    let cross_kind_id = format!("c-{}", &ingress_main[2..]);
    let cross_kind = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertChain {
                app_id: app_main.clone(),
                chain: CreateChainRequest {
                    id: cross_kind_id,
                    tenant_id: "platform.acme".to_owned(),
                    name: "must not share an ingress body".to_owned(),
                    subscription_country: None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(cross_kind, StoreError::Conflict(_)));

    // The trigger is the final backstop for maintenance SQL and concurrent writers which bypass
    // the Store helper. A prefix change must not make the same six-letter body legal.
    let duplicate_ingress_id = format!("i-{}", &chain_main[2..]);
    let direct_collision = sqlx::query("UPDATE ingresses SET id = $1 WHERE id = $2")
        .bind(&duplicate_ingress_id)
        .bind(&ingress_main)
        .execute(db.pool())
        .await
        .unwrap_err();
    assert_eq!(
        direct_collision
            .as_database_error()
            .and_then(|error| error.code()),
        Some(std::borrow::Cow::Borrowed("23505"))
    );

    let app_positions: Vec<(String, i32)> =
        sqlx::query_as("SELECT id, position FROM apps ORDER BY position")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        app_positions,
        [(app_main.clone(), 0), (app_secondary.clone(), 1)]
    );

    let dns_positions: Vec<i32> = sqlx::query_scalar(
        "SELECT position FROM node_egress_dns WHERE node_id = 'n1' ORDER BY position",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(dns_positions, [0, 1]);

    let chain_positions: Vec<(String, i32)> = sqlx::query_as(
        "SELECT id, position
         FROM chains
         WHERE app_id = $1
         ORDER BY position, id",
    )
    .bind(&app_main)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        chain_positions,
        [
            (chain_alpha.clone(), 0),
            (chain_main.clone(), 1),
            (chain_zulu.clone(), 2),
        ]
    );

    let snapshot_dns_positions: Vec<i32> = sqlx::query_scalar(
        "SELECT (policy->>'position')::integer
         FROM model_snapshots
         JOIN control_state ON control_state.current_revision = model_snapshots.revision_id
         CROSS JOIN LATERAL jsonb_array_elements(snapshot->'node_egress_dns')
              WITH ORDINALITY AS item(policy, ordinality)
         ORDER BY ordinality",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(snapshot_dns_positions, [0, 1]);

    // A second replay is the normal development deployment path. Existing operator order is
    // data, not a derived ID sort, and therefore must survive an already-compatible 0001 replay.
    sqlx::query(
        "UPDATE apps
         SET position = CASE id WHEN $1 THEN 1 WHEN $2 THEN 0 END
         WHERE id IN ($1, $2)",
    )
    .bind(&app_main)
    .bind(&app_secondary)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE chains
         SET position = CASE id WHEN $1 THEN 2 WHEN $2 THEN 0 WHEN $3 THEN 1 END
         WHERE app_id = $4",
    )
    .bind(&chain_alpha)
    .bind(&chain_main)
    .bind(&chain_zulu)
    .bind(&app_main)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 1")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate().await.unwrap();
    let replayed_app_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM apps ORDER BY position")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(replayed_app_order, [app_secondary, app_main.clone()]);
    let replayed_chain_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM chains WHERE app_id = $1 ORDER BY position, id")
            .bind(&app_main)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(replayed_chain_order, [chain_main, chain_zulu, chain_alpha]);

    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name = 'external_outbound_bindings'
           AND column_name IN (
               'endpoint_address', 'endpoint_port', 'mtu', 'keep_alive',
               'allowed_ips', 'no_kernel_tun', 'domain_strategy', 'workers'
           )
         ORDER BY column_name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        columns,
        [
            "allowed_ips",
            "domain_strategy",
            "endpoint_address",
            "endpoint_port",
            "keep_alive",
            "mtu",
            "no_kernel_tun",
            "workers",
        ]
    );

    let migration_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(migration_count, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn reordering_app_positions_keeps_ids_and_all_usage_ownership_stable() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO apps (id, label, position)
         VALUES ('app-secondary', 'Secondary App', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    insert_usage_history_for_main_fixture(db.pool()).await;
    sqlx::query("UPDATE usage_samples SET app_id = 'app-main'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_app_quotas (tenant_id, user_id, app_id, limit_bytes)
         VALUES ('platform.acme', 'alice', 'app-main', 1048576)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO e2e_probes (
            chain_id, app_id, node_id, status, ttfb_ms, exit_ip,
            exit_loc, exit_verdict, probed_at
         ) VALUES (
            'c-main', 'app-main', 'n1', 'ok', 25, '192.0.2.10',
            'test-region', 'match', now()
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let identity_references = || async {
        sqlx::query_as::<_, (String, String, String, String, String, String, String)>(
            "SELECT
                (SELECT app_id FROM chains WHERE id = 'c-main'),
                (SELECT app_id FROM grants WHERE ingress_id = 'i-main'),
                (SELECT app_id FROM e2e_probes WHERE chain_id = 'c-main'),
                (SELECT app_id FROM usage_samples LIMIT 1),
                (SELECT app_id FROM usage_chain_samples LIMIT 1),
                (SELECT chain_id FROM usage_chain_samples LIMIT 1),
                (SELECT app_id FROM user_app_quotas WHERE user_id = 'alice')",
        )
        .fetch_one(db.pool())
        .await
        .unwrap()
    };
    let before = identity_references().await;

    let result = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::ReorderApps {
                ids: vec!["app-secondary".to_owned(), "app-main".to_owned()],
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.changed, 1);

    let positions: Vec<(String, i32)> =
        sqlx::query_as("SELECT id, position FROM apps ORDER BY position")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        positions,
        [("app-secondary".to_owned(), 0), ("app-main".to_owned(), 1)]
    );
    assert_eq!(identity_references().await, before);
    assert_eq!(
        before,
        (
            "app-main".to_owned(),
            "app-main".to_owned(),
            "app-main".to_owned(),
            "app-main".to_owned(),
            "app-main".to_owned(),
            "c-main".to_owned(),
            "app-main".to_owned(),
        )
    );

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let app_ids: Vec<&str> = snapshot.apps.iter().map(|app| app.id.as_str()).collect();
    assert_eq!(app_ids, ["app-secondary", "app-main"]);

    db.store
        .upsert_app(
            &system_admin(),
            CreateAppRequest {
                id: "app-new".to_owned(),
                label: "New App".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let order_after_create: Vec<String> =
        sqlx::query_scalar("SELECT id FROM apps ORDER BY position")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(order_after_create, ["app-secondary", "app-main", "app-new"]);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_order_is_app_local_and_keeps_ids_statistics_and_projections_stable() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_usage_history_for_main_fixture(db.pool()).await;

    // The untouched/default sequence follows stable IDs even when creation happens in another
    // order. Both chains belong to app-main; positions are deliberately not global.
    for (id, name) in [("c-zovuru", "Zulu Chain"), ("c-bacemu", "Alpha Chain")] {
        db.store
            .upsert_chain(
                &system_admin(),
                "app-main",
                CreateChainRequest {
                    id: id.to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: name.to_owned(),
                    subscription_country: None,
                    note: None,
                },
            )
            .await
            .unwrap();
    }
    let default_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM chains WHERE app_id = 'app-main' ORDER BY position, id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(default_order, ["c-bacemu", "c-main", "c-zovuru"]);

    // A complete ordering document is also the stale-page guard. Missing or duplicated IDs must
    // abort the whole draft rather than assigning a partial set of positions.
    for ids in [
        vec!["c-main".to_owned(), "c-zovuru".to_owned()],
        vec![
            "c-bacemu".to_owned(),
            "c-bacemu".to_owned(),
            "c-zovuru".to_owned(),
        ],
    ] {
        let error = db
            .store
            .apply_draft(
                &system_admin(),
                vec![ModelOp::ReorderChains {
                    app_id: "app-main".to_owned(),
                    ids,
                }],
                None,
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("order"),
            "unexpected complete-order error: {error}"
        );
    }
    let after_rejected_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM chains WHERE app_id = 'app-main' ORDER BY position, id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(after_rejected_order, default_order);

    // Give every chain an ingress so materialization's authorization-column projection is tested
    // as well as the chain declarations themselves. These are fixture rows, not the write path
    // under test, so cloning the known-good ingress keeps this test about ordering.
    for (chain_id, ingress_id, port) in [
        ("c-bacemu", "i-dafino", 444_i32),
        ("c-zovuru", "i-gurelo", 445_i32),
    ] {
        sqlx::query(
            "INSERT INTO ingresses (
                id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
                reality_private_key, reality_public_key, reality_short_ids,
                reality_dest, reality_server_names, reality_flow,
                reality_fallback_mode
             )
             SELECT $1, app_id, $2, node_id, bind, $3, front_id, transport_kind,
                    reality_private_key || '-' || $1,
                    reality_public_key || '-' || $1,
                    reality_short_ids,
                    reality_dest, reality_server_names, reality_flow,
                    reality_fallback_mode
             FROM ingresses
             WHERE id = 'i-main'",
        )
        .bind(ingress_id)
        .bind(chain_id)
        .bind(port)
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
             VALUES ('app-main', 'platform.acme', 'alice', $1)",
        )
        .bind(ingress_id)
        .execute(db.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO e2e_probes (
            chain_id, app_id, node_id, status, ttfb_ms, exit_verdict, probed_at
         ) VALUES
            ('c-bacemu', 'app-main', 'n1', 'ok', 21, 'match', now()),
            ('c-main', 'app-main', 'n1', 'ok', 22, 'match', now()),
            ('c-zovuru', 'app-main', 'n1', 'ok', 23, 'match', now())",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let references_before: (i64, i64, i64, String, String) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM grants WHERE app_id = 'app-main'),
            (SELECT count(*) FROM e2e_probes WHERE app_id = 'app-main'),
            (SELECT count(*) FROM usage_chain_samples WHERE app_id = 'app-main'),
            (SELECT chain_id FROM usage_chain_samples WHERE app_id = 'app-main' LIMIT 1),
            (SELECT app_id FROM chains WHERE id = 'c-main')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    let changed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::ReorderChains {
                app_id: "app-main".to_owned(),
                ids: vec![
                    "c-zovuru".to_owned(),
                    "c-main".to_owned(),
                    "c-bacemu".to_owned(),
                ],
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(changed.changed, 1);

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let app = snapshot
        .apps
        .iter()
        .find(|app| app.id == "app-main")
        .unwrap();
    assert_eq!(
        app.chains
            .iter()
            .map(|chain| chain.id.as_str())
            .collect::<Vec<_>>(),
        ["c-zovuru", "c-main", "c-bacemu"]
    );
    assert_eq!(
        app.ingresses
            .iter()
            .map(|ingress| ingress.chain.as_str())
            .collect::<Vec<_>>(),
        ["c-zovuru", "c-main", "c-bacemu"]
    );
    assert_eq!(
        db.store
            .e2e_probes(&system_admin())
            .await
            .unwrap()
            .iter()
            .map(|probe| probe.chain_id.as_str())
            .collect::<Vec<_>>(),
        ["c-zovuru", "c-main", "c-bacemu"]
    );
    let references_after: (i64, i64, i64, String, String) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM grants WHERE app_id = 'app-main'),
            (SELECT count(*) FROM e2e_probes WHERE app_id = 'app-main'),
            (SELECT count(*) FROM usage_chain_samples WHERE app_id = 'app-main'),
            (SELECT chain_id FROM usage_chain_samples WHERE app_id = 'app-main' LIMIT 1),
            (SELECT app_id FROM chains WHERE id = 'c-main')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(references_after, references_before);
    assert_eq!(references_after.3, "c-main");
    assert_eq!(references_after.4, "app-main");

    // Once an operator has established a custom order, a new chain appends even if its ID would
    // sort into the middle. This preserves the explicit sequence instead of partially resetting it.
    db.store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-betalo".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Beta Chain".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();
    let order_after_create: Vec<String> =
        sqlx::query_scalar("SELECT id FROM chains WHERE app_id = 'app-main' ORDER BY position, id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        order_after_create,
        ["c-zovuru", "c-main", "c-bacemu", "c-betalo"]
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_subscription_country_is_normalized_persisted_and_clearable() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let updated = db
        .store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Main".to_owned(),
                subscription_country: Some(" tw ".to_owned()),
                note: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.chain.subscription_country.as_deref(), Some("TW"));
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot.apps[0].chains[0].subscription_country.as_deref(),
        Some("TW")
    );

    let invalid = db
        .store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Main".to_owned(),
                subscription_country: Some("TWN".to_owned()),
                note: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(invalid, StoreError::InvalidData(_)));

    let cleared = db
        .store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Main".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.chain.subscription_country, None);
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.apps[0].chains[0].subscription_country, None);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn line_order_drives_every_operator_facing_projection_with_id_as_the_default() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // With no custom ordering, a newly-created line keeps the legacy ID order rather than merely
    // appending. This is the fallback contract for databases/operators that never reorder lines.
    db.store
        .upsert_app(
            &system_admin(),
            CreateAppRequest {
                id: "app-alpha".to_owned(),
                label: "Alpha App".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let default_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM apps ORDER BY position, id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(default_order, ["app-alpha", "app-main"]);

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-alpha', 'app-alpha', 'platform.acme', 'Alpha Chain', 0)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO e2e_probes (
            chain_id, app_id, node_id, status, ttfb_ms, exit_verdict, probed_at
         ) VALUES
            ('c-alpha', 'app-alpha', 'n1', 'ok', 20, 'match', now()),
            ('c-main', 'app-main', 'n1', 'ok', 30, 'match', now())",
    )
    .execute(db.pool())
    .await
    .unwrap();
    for app_id in ["app-alpha", "app-main"] {
        db.store
            .set_user_app_quota(
                &system_admin(),
                SetUserAppQuotaRequest {
                    tenant_id: "platform.acme".to_owned(),
                    user_id: "alice".to_owned(),
                    app_id: app_id.to_owned(),
                    limit_bytes: Some(1024),
                },
            )
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            app_id, grant_label, uplink_bytes, downlink_bytes
         ) VALUES
            (now() - interval '60 seconds', now() - interval '30 seconds',
             'n1', 'platform.acme', 'alice', 'i-main',
             'app-alpha', 'test-user#alpha', 10, 20),
            (now() - interval '60 seconds', now() - interval '30 seconds',
             'n1', 'platform.acme', 'alice', 'i-main',
             'app-main', 'test-user#main', 30, 40)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // Reverse the visible order without touching either stable ID. Every API whose rows are
    // displayed as ordinary line groups must now follow position rather than re-sorting by ID.
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::ReorderApps {
                ids: vec!["app-main".to_owned(), "app-alpha".to_owned()],
            }],
            None,
        )
        .await
        .unwrap();

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot
            .apps
            .iter()
            .map(|app| app.id.as_str())
            .collect::<Vec<_>>(),
        ["app-main", "app-alpha"]
    );
    assert_eq!(
        db.store
            .e2e_probes(&system_admin())
            .await
            .unwrap()
            .iter()
            .map(|probe| probe.app_id.as_str())
            .collect::<Vec<_>>(),
        ["app-main", "app-alpha"]
    );
    assert_eq!(
        db.store
            .list_user_app_quotas(&system_admin(), None)
            .await
            .unwrap()
            .quotas
            .iter()
            .map(|quota| quota.app_id.as_str())
            .collect::<Vec<_>>(),
        ["app-main", "app-alpha"]
    );
    assert_eq!(
        db.store
            .list_monthly_usage_summary(&system_admin())
            .await
            .unwrap()
            .views
            .iter()
            .map(|view| view.app_id.as_str())
            .collect::<Vec<_>>(),
        ["app-main", "app-alpha"]
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_creation_adds_exactly_one_default_warp_in_the_same_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let created = db
        .store
        .create_tenant(
            &system_admin(),
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform Acme".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, created.revision_id);
    assert_eq!(snapshot.external_outbounds.len(), 1);
    let warp = &snapshot.external_outbounds[0];
    assert_eq!(warp.id, "warp.platform.acme");
    assert_eq!(warp.tenant, "platform.acme");
    assert_eq!(warp.name, "Cloudflare WARP");
    assert_eq!(warp.address, "engage.cloudflareclient.com");
    assert_eq!(warp.port, 2408);
    assert!(matches!(
        &warp.protocol,
        ExternalOutboundProtocol::Warp {
            mtu: 1280,
            keep_alive: 25,
            allowed_ips,
            no_kernel_tun: false,
            domain_strategy,
            workers: 0,
        } if allowed_ips == &["0.0.0.0/0", "::/0"] && domain_strategy == "ForceIP"
    ));
    let credential: String = sqlx::query_scalar(
        "SELECT credential_sealed FROM external_outbounds WHERE id = 'warp.platform.acme'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        credential.is_empty(),
        "WARP has no resource-level credential"
    );

    let repeated = db
        .store
        .create_tenant(
            &system_admin(),
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform Acme".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(repeated.revision_id, created.revision_id);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM external_outbounds WHERE tenant_id = 'platform.acme' AND protocol = 'warp'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn startup_backfills_missing_tenant_warp_idempotently_and_avoids_global_id_collision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    sqlx::query(
        "INSERT INTO tenants (id, name)
         VALUES ('alpha', 'Alpha'), ('beta', 'Beta')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // Occupy alpha's readable id with beta's already-existing WARP. The reconciler must preserve
    // this resource, count it as beta's default, and allocate alpha a checked fallback id.
    sqlx::query(
        "INSERT INTO external_outbounds
            (id, tenant_id, name, address, port, protocol, credential_sealed,
             protocol_options, security)
         VALUES
            ('warp.alpha', 'beta', 'Existing WARP', 'engage.cloudflareclient.com', 2408,
             'warp', '', $1, $2)",
    )
    .bind(json!({
        "mtu": 1280,
        "keep_alive": 25,
        "allowed_ips": ["0.0.0.0/0", "::/0"],
        "no_kernel_tun": false,
        "domain_strategy": "ForceIP",
        "workers": 0,
    }))
    .bind(json!({ "t": "none" }))
    .execute(db.pool())
    .await
    .unwrap();

    let before: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let added = db.store.migrate().await.unwrap();
    assert_eq!(added, 1);
    let after: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before + 1);

    let rows = sqlx::query(
        "SELECT id, tenant_id FROM external_outbounds WHERE protocol = 'warp' ORDER BY tenant_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].try_get::<String, _>("tenant_id").unwrap(), "alpha");
    let alpha_id = rows[0].try_get::<String, _>("id").unwrap();
    assert!(alpha_id.starts_with("warp-"), "fallback id was {alpha_id}");
    assert_eq!(alpha_id.len(), 17);
    assert_eq!(rows[1].try_get::<String, _>("tenant_id").unwrap(), "beta");
    assert_eq!(rows[1].try_get::<String, _>("id").unwrap(), "warp.alpha");

    assert_eq!(db.store.migrate().await.unwrap(), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        after
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn external_outbound_round_trips_sealed_and_redacted() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let request = UpsertExternalOutboundRequest {
        id: "vendor-edge".to_owned(),
        tenant_id: "platform.acme".to_owned(),
        name: "Vendor edge".to_owned(),
        address: "edge.vendor.example".to_owned(),
        port: 2408,
        protocol: ExternalOutboundProtocol::Wireguard {
            credential: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
            peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
            local_addresses: vec!["172.16.0.2/32".to_owned()],
            mtu: 1420,
            reserved: vec![0, 0, 0],
            keep_alive: 25,
            allowed_ips: vec!["0.0.0.0/0".to_owned()],
            no_kernel_tun: true,
            domain_strategy: "ForceIPv4".to_owned(),
        },
        security: ExternalOutboundSecurity::None,
        note: None,
    };
    let committed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: request.clone(),
            }],
            None,
        )
        .await
        .unwrap();

    let sealed: String =
        sqlx::query("SELECT credential_sealed FROM external_outbounds WHERE id = 'vendor-edge'")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("credential_sealed")
            .unwrap();
    assert!(sealed.starts_with("v1."));
    assert!(!sealed.contains(request.protocol.credential()));
    let first_client_sealed: String = sqlx::query_scalar(
        "SELECT snapshot.document->'external_outbounds'->0->'protocol'->'v'->>'credential'
           FROM subscription_client_state state
           JOIN subscription_client_snapshots snapshot ON snapshot.id = state.head_snapshot_id
          WHERE state.id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(first_client_sealed.starts_with("v1."));
    assert_ne!(first_client_sealed, request.protocol.credential());
    let (persisted_document, persisted_sha256): (serde_json::Value, String) = sqlx::query_as(
        "SELECT snapshot.document, snapshot.content_sha256
           FROM subscription_client_state state
           JOIN subscription_client_snapshots snapshot ON snapshot.id = state.head_snapshot_id
          WHERE state.id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        persisted_sha256,
        brocade_core::hash::sha256_hex(&serde_json::to_vec(&persisted_document).unwrap()),
        "the one client checksum is over the sealed persisted JSON document"
    );

    for revision in [None, Some(committed.revision_id)] {
        let snapshot = db.store.materialize_snapshot(revision).await.unwrap();
        assert_eq!(snapshot.external_outbounds.len(), 1);
        assert_eq!(
            snapshot.external_outbounds[0].protocol.credential(),
            request.protocol.credential()
        );
    }
    let redacted = db
        .store
        .redacted_snapshot(&system_admin(), None)
        .await
        .unwrap();
    assert_eq!(
        redacted.snapshot["external_outbounds"][0]["protocol"]["v"]["credential"],
        "<redacted>"
    );

    let mut update = request;
    update.name = "Renamed vendor edge".to_owned();
    update.protocol.set_credential("<redacted>".to_owned());
    let mut changed_protocol = update.clone();
    changed_protocol.protocol = ExternalOutboundProtocol::Socks5 {
        username: None,
        credential: "<redacted>".to_owned(),
    };
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound { outbound: update }],
            None,
        )
        .await
        .unwrap();
    let second_client_sealed: String = sqlx::query_scalar(
        "SELECT snapshot.document->'external_outbounds'->0->'protocol'->'v'->>'credential'
           FROM subscription_client_state state
           JOIN subscription_client_snapshots snapshot ON snapshot.id = state.head_snapshot_id
          WHERE state.id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        second_client_sealed, first_client_sealed,
        "renaming a client proxy must reuse its sealed envelope"
    );
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.external_outbounds[0].name, "Renamed vendor edge");
    let ExternalOutboundProtocol::Wireguard {
        peer_public_key,
        local_addresses,
        keep_alive,
        ..
    } = &snapshot.external_outbounds[0].protocol
    else {
        panic!("expected WireGuard external outbound");
    };
    assert_eq!(
        peer_public_key,
        "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk="
    );
    assert_eq!(local_addresses, &["172.16.0.2/32"]);
    assert_eq!(*keep_alive, 25);
    assert_eq!(
        snapshot.external_outbounds[0].protocol.credential(),
        "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY="
    );

    let error = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: changed_protocol.clone(),
            }],
            None,
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("changing an external outbound protocol requires a new credential"));

    changed_protocol.protocol = ExternalOutboundProtocol::Socks5 {
        username: None,
        credential: String::new(),
    };
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: changed_protocol,
            }],
            None,
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(matches!(
        &snapshot.external_outbounds[0].protocol,
        ExternalOutboundProtocol::Socks5 {
            username: None,
            credential
        } if credential.is_empty()
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn managed_warp_binding_round_trips_sealed_and_redacted() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    db.store
        .apply_draft(
            &system_admin(),
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

    let private_key = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
    let access_token = "provider-control-token";
    let committed = db
        .store
        .register_warp_binding(
            &system_admin(),
            RegisterWarpBindingRequest {
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                device_id: "device-1".to_owned(),
                account_id: "account-1".to_owned(),
                access_token: access_token.to_owned(),
                private_key: private_key.to_owned(),
                peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
                local_addresses: vec![
                    "172.16.0.2/32".to_owned(),
                    "2606:4700:110:8::2/128".to_owned(),
                ],
                reserved: vec![1, 2, 3],
                note: None,
            },
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT access_token_sealed, private_key_sealed
         FROM external_outbound_bindings
         WHERE outbound_id = 'warp' AND node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let token_sealed = row.try_get::<String, _>("access_token_sealed").unwrap();
    let key_sealed = row.try_get::<String, _>("private_key_sealed").unwrap();
    assert!(token_sealed.starts_with("v1."));
    assert!(key_sealed.starts_with("v1."));
    assert!(!token_sealed.contains(access_token));
    assert!(!key_sealed.contains(private_key));

    let customized = db
        .store
        .update_warp_binding(
            &system_admin(),
            UpdateWarpBindingRequest {
                tenant_id: "platform.acme".to_owned(),
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                endpoint_address: Some("162.159.193.10".to_owned()),
                endpoint_port: Some(500),
                mtu: Some(1420),
                keep_alive: Some(40),
                allowed_ips: Some(vec!["::/0".to_owned()]),
                no_kernel_tun: Some(false),
                domain_strategy: Some("ForceIPv6".to_owned()),
                workers: Some(4),
                note: None,
            },
        )
        .await
        .unwrap();
    assert!(customized.revision_id > committed.revision_id);

    let customized_snapshot = db
        .store
        .materialize_snapshot(Some(customized.revision_id))
        .await
        .unwrap();
    let customized_binding = &customized_snapshot.external_outbounds[0].bindings[0];
    assert_eq!(
        customized_binding.endpoint_address.as_deref(),
        Some("162.159.193.10")
    );
    assert_eq!(customized_binding.endpoint_port, Some(500));
    assert_eq!(customized_binding.mtu, Some(1420));
    assert_eq!(customized_binding.keep_alive, Some(40));
    assert_eq!(
        customized_binding.allowed_ips,
        Some(vec!["::/0".to_owned()])
    );
    assert_eq!(customized_binding.no_kernel_tun, Some(false));
    assert_eq!(
        customized_binding.domain_strategy.as_deref(),
        Some("ForceIPv6")
    );
    assert_eq!(customized_binding.workers, Some(4));

    let customized_deployment = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(customized.revision_id, "warp-customized"),
        )
        .await
        .unwrap();
    mark_succeeded(db.pool(), customized_deployment.deployment_id).await;

    let no_op = db
        .store
        .update_warp_binding(
            &system_admin(),
            UpdateWarpBindingRequest {
                tenant_id: "platform.acme".to_owned(),
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                endpoint_address: Some("162.159.193.10".to_owned()),
                endpoint_port: Some(500),
                mtu: Some(1420),
                keep_alive: Some(40),
                allowed_ips: Some(vec!["::/0".to_owned()]),
                no_kernel_tun: Some(false),
                domain_strategy: Some("ForceIPv6".to_owned()),
                workers: Some(4),
                note: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(no_op.revision_id, customized.revision_id);

    let inherited = db
        .store
        .update_warp_binding(
            &system_admin(),
            UpdateWarpBindingRequest {
                tenant_id: "platform.acme".to_owned(),
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                endpoint_address: None,
                endpoint_port: None,
                mtu: None,
                keep_alive: None,
                allowed_ips: None,
                no_kernel_tun: None,
                domain_strategy: None,
                workers: None,
                note: None,
            },
        )
        .await
        .unwrap();
    assert!(inherited.revision_id > customized.revision_id);
    let current_snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let current_binding = &current_snapshot.external_outbounds[0].bindings[0];
    assert_eq!(current_binding.endpoint_address, None);
    assert_eq!(current_binding.endpoint_port, None);
    assert_eq!(current_binding.mtu, None);
    assert_eq!(current_binding.keep_alive, None);
    assert_eq!(current_binding.allowed_ips, None);
    assert_eq!(current_binding.no_kernel_tun, None);
    assert_eq!(current_binding.domain_strategy, None);
    assert_eq!(current_binding.workers, None);

    db.store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(
                customized_deployment.deployment_id,
                "warp-restore-customized",
            ),
        )
        .await
        .unwrap();
    let rolled_back = db.store.materialize_snapshot(None).await.unwrap();
    let rolled_back_binding = &rolled_back.external_outbounds[0].bindings[0];
    assert_eq!(
        rolled_back_binding.endpoint_address.as_deref(),
        Some("162.159.193.10")
    );
    assert_eq!(rolled_back_binding.endpoint_port, Some(500));
    assert_eq!(rolled_back_binding.mtu, Some(1420));
    assert_eq!(rolled_back_binding.keep_alive, Some(40));
    assert_eq!(
        rolled_back_binding.allowed_ips,
        Some(vec!["::/0".to_owned()])
    );
    assert_eq!(rolled_back_binding.no_kernel_tun, Some(false));
    assert_eq!(
        rolled_back_binding.domain_strategy.as_deref(),
        Some("ForceIPv6")
    );
    assert_eq!(rolled_back_binding.workers, Some(4));

    for revision in [
        None,
        Some(committed.revision_id),
        Some(customized.revision_id),
    ] {
        let snapshot = db.store.materialize_snapshot(revision).await.unwrap();
        let tunnel = snapshot
            .external_outbounds
            .iter()
            .find(|outbound| outbound.id == "warp")
            .unwrap();
        assert_eq!(tunnel.bindings.len(), 1);
        assert_eq!(tunnel.bindings[0].node, "n1");
        assert_eq!(tunnel.bindings[0].private_key, private_key);
        assert_eq!(tunnel.bindings[0].reserved, vec![1, 2, 3]);
    }

    let redacted = db
        .store
        .redacted_snapshot(&system_admin(), Some(committed.revision_id))
        .await
        .unwrap();
    let binding = &redacted.snapshot["external_outbounds"][0]["bindings"][0];
    assert_eq!(binding["device_id"], "device-1");
    assert!(binding.get("private_key").is_none());
    assert!(!redacted.snapshot.to_string().contains(access_token));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn managed_warp_removal_waits_until_current_and_published_routes_are_clear() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .apply_draft(
            &system_admin(),
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
                        no_kernel_tun: false,
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
            &system_admin(),
            RegisterWarpBindingRequest {
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                device_id: "device-remove".to_owned(),
                account_id: "account-remove".to_owned(),
                access_token: "provider-delete-token".to_owned(),
                private_key: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=".to_owned(),
                peer_public_key: "YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODk=".to_owned(),
                local_addresses: vec![
                    "172.16.0.2/32".to_owned(),
                    "2606:4700:110:8::2/128".to_owned(),
                ],
                reserved: vec![1, 2, 3],
                note: None,
            },
        )
        .await
        .unwrap();

    let proxy = put_step_draft(
        &db,
        "app-main",
        "c-main",
        "n1",
        PutStepRequest {
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "warp".to_owned(),
                },
            }],
            note: None,
        },
    )
    .await;
    let current_error = db
        .store
        .prepare_warp_binding_removal(&system_admin(), "platform.acme", "warp", "n1")
        .await
        .unwrap_err();
    assert!(
        current_error.to_string().contains("仍被当前规则引用"),
        "{current_error}"
    );

    let published_proxy = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(proxy.revision_id, "warp-remove-proxy"),
        )
        .await
        .unwrap();
    mark_succeeded(db.pool(), published_proxy.deployment_id).await;

    let direct = put_step_draft(
        &db,
        "app-main",
        "c-main",
        "n1",
        PutStepRequest {
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Egress { send_through: None },
            }],
            note: None,
        },
    )
    .await;
    let published_error = db
        .store
        .prepare_warp_binding_removal(&system_admin(), "platform.acme", "warp", "n1")
        .await
        .unwrap_err();
    assert!(
        published_error
            .to_string()
            .contains("最后成功发布的版本仍在使用"),
        "{published_error}"
    );

    let published_direct = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(direct.revision_id, "warp-remove-direct"),
        )
        .await
        .unwrap();
    mark_succeeded(db.pool(), published_direct.deployment_id).await;

    let prepared = db
        .store
        .prepare_warp_binding_removal(&system_admin(), "platform.acme", "warp", "n1")
        .await
        .unwrap();
    assert_eq!(prepared.device_id, "device-remove");
    assert_eq!(prepared.access_token, "provider-delete-token");

    let stale = db
        .store
        .remove_warp_binding(
            &system_admin(),
            RemoveWarpBindingRequest {
                tenant_id: "platform.acme".to_owned(),
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                expected_device_id: "different-device".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("identity on n1 changed"),
        "{stale}"
    );

    let removed = db
        .store
        .remove_warp_binding(
            &system_admin(),
            RemoveWarpBindingRequest {
                tenant_id: "platform.acme".to_owned(),
                outbound_id: "warp".to_owned(),
                node_id: "n1".to_owned(),
                expected_device_id: prepared.device_id,
                note: None,
            },
        )
        .await
        .unwrap();
    assert!(removed.removed);
    assert_eq!(removed.node_id, "n1");
    assert_eq!(removed.device_id, "device-remove");
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let warp = snapshot
        .external_outbounds
        .iter()
        .find(|outbound| outbound.id == "warp")
        .unwrap();
    assert!(warp.bindings.is_empty());
    let rows = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM external_outbound_bindings WHERE outbound_id = 'warp'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn external_vless_xhttp_round_trips_and_legacy_rows_default_to_raw() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let request = UpsertExternalOutboundRequest {
        id: "xhttp-edge".to_owned(),
        tenant_id: "platform.acme".to_owned(),
        name: "XHTTP edge".to_owned(),
        address: "upload.vendor.example".to_owned(),
        port: 443,
        protocol: ExternalOutboundProtocol::Vless {
            credential: "6f9d1a8e-2b3c-4d5e-8f70-1a2b3c4d5e6f".to_owned(),
            encryption: "none".to_owned(),
            flow: None,
            transport: ExternalVlessTransport::Xhttp(ExternalVlessXhttp {
                path: "/upload".to_owned(),
                host: Some("upload.vendor.example".to_owned()),
                mux: Some(4),
                mode: XhttpMode::StreamUp,
                download: Some(ExternalVlessXhttpDownload {
                    address: "download.vendor.example".to_owned(),
                    port: 8443,
                    security: ExternalOutboundSecurity::Tls {
                        server_name: "download.vendor.example".to_owned(),
                        fingerprint: "chrome".to_owned(),
                    },
                    path: "/download".to_owned(),
                    host: None,
                    mux: Some(2),
                    mode: XhttpMode::Auto,
                }),
            }),
        },
        security: ExternalOutboundSecurity::Reality {
            server_name: "www.example.com".to_owned(),
            public_key: "reality-public-key".to_owned(),
            short_id: "0123abcd".to_owned(),
            fingerprint: "chrome".to_owned(),
        },
        note: None,
    };
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: request.clone(),
            }],
            None,
        )
        .await
        .unwrap();

    let options: serde_json::Value =
        sqlx::query("SELECT protocol_options FROM external_outbounds WHERE id = 'xhttp-edge'")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("protocol_options")
            .unwrap();
    assert_eq!(options["transport"]["t"], "xhttp");
    assert_eq!(options["transport"]["v"]["path"], "/upload");
    assert_eq!(
        options["transport"]["v"]["download"]["address"],
        "download.vendor.example"
    );

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.external_outbounds[0].protocol, request.protocol);

    // Simulate an existing row written before transport was persisted. Runtime compatibility is
    // deliberately in the decoder; deployment also normalizes such rows to an explicit RAW value.
    sqlx::query(
        "UPDATE external_outbounds SET protocol_options = protocol_options - 'transport' WHERE id = 'xhttp-edge'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(matches!(
        snapshot.external_outbounds[0].protocol,
        ExternalOutboundProtocol::Vless {
            transport: ExternalVlessTransport::Raw,
            ..
        }
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn global_settings_update_materializes_reality_client_policy() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    assert_eq!(db.store.settings().await.unwrap().ports.anytls_base, 16_000);

    let result = db
        .store
        .update_settings(
            &system_admin(),
            ModelSettings {
                connection: Default::default(),
                stats_user_online: false,
                reality_client: RealityClientPolicy {
                    min_client_ver: Some("1.8.0".to_owned()),
                    max_client_ver: Some("1.9.9".to_owned()),
                    max_time_diff_ms: Some(30_000),
                },
                reality_site: RealitySite::default(),
                overlay: OverlaySettings::default(),
                ports: PortSettings {
                    anytls_base: 16_123,
                    ..Default::default()
                },
                probe: Default::default(),
                geodata: Default::default(),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.revision_id, 2);
    assert_eq!(
        result.settings.reality_client.min_client_ver.as_deref(),
        Some("1.8.0")
    );
    assert_eq!(result.settings.ports.anytls_base, 16_123);

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, result.revision_id);
    assert_eq!(
        snapshot.settings.reality_client.max_client_ver.as_deref(),
        Some("1.9.9")
    );
    assert_eq!(snapshot.settings.ports.anytls_base, 16_123);
    assert_eq!(
        snapshot.settings.reality_client.max_time_diff_ms,
        Some(30_000)
    );

    let forbidden = db
        .store
        .update_settings(&tenant_admin("platform.acme"), Default::default())
        .await
        .unwrap_err();
    assert!(
        matches!(forbidden, StoreError::Forbidden(_)),
        "expected tenant-admin settings update to be forbidden: {forbidden:?}"
    );

    let invalid = db
        .store
        .update_settings(
            &system_admin(),
            ModelSettings {
                connection: Default::default(),
                stats_user_online: false,
                reality_client: RealityClientPolicy {
                    min_client_ver: Some("1.x.0".to_owned()),
                    max_client_ver: None,
                    max_time_diff_ms: None,
                },
                reality_site: RealitySite::default(),
                overlay: OverlaySettings::default(),
                ports: Default::default(),
                probe: Default::default(),
                geodata: Default::default(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(invalid, StoreError::InvalidData(_)),
        "expected invalid settings to fail: {invalid:?}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn agent_log_policy_resolves_global_node_override_and_clear_without_a_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    for id in ["n1", "n2"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }
    let revision_before: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();

    assert_eq!(
        db.store.effective_node_log_max_mib("n1").await.unwrap(),
        100
    );
    db.store
        .update_agent_log_default(
            &system_admin(),
            UpdateAgentLogDefaultRequest { max_mib: 192 },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.effective_node_log_max_mib("n1").await.unwrap(),
        192
    );
    assert_eq!(
        db.store.effective_node_log_max_mib("n2").await.unwrap(),
        192
    );

    db.store
        .update_node_log_policy(
            &system_admin(),
            "n1",
            UpdateNodeLogPolicyRequest { max_mib: Some(64) },
        )
        .await
        .unwrap();
    db.store
        .update_agent_log_default(
            &system_admin(),
            UpdateAgentLogDefaultRequest { max_mib: 256 },
        )
        .await
        .unwrap();
    assert_eq!(db.store.effective_node_log_max_mib("n1").await.unwrap(), 64);
    assert_eq!(
        db.store.effective_node_log_max_mib("n2").await.unwrap(),
        256
    );

    db.store
        .update_node_log_policy(
            &system_admin(),
            "n1",
            UpdateNodeLogPolicyRequest { max_mib: None },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.effective_node_log_max_mib("n1").await.unwrap(),
        256
    );
    let revision_after: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        revision_after, revision_before,
        "运行时策略不应创建模型修订"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn realtime_policy_is_global_validated_and_does_not_create_a_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let initial = db.store.realtime_telemetry_policy().await.unwrap();
    assert!(initial.enabled);
    assert_eq!(initial.interval_secs, 1);

    let changed = db
        .store
        .update_realtime_telemetry_policy(
            &system_admin(),
            UpdateRealtimeTelemetryPolicyRequest {
                enabled: true,
                interval_secs: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.interval_secs, 2);
    assert_eq!(db.store.realtime_telemetry_policy().await.unwrap(), changed);
    assert!(db
        .store
        .update_realtime_telemetry_policy(
            &system_admin(),
            UpdateRealtimeTelemetryPolicyRequest {
                enabled: true,
                interval_secs: 3,
            },
        )
        .await
        .is_err());
    let after: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before, "实时遥测策略不应产生模型修订");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn deployment_schema_matches_convergence_design() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    assert_column_exists(db.pool(), "control_state", "agent_log_max_mib").await;
    assert_column_exists(db.pool(), "control_state", "ping_probe_targets").await;
    assert_column_exists(db.pool(), "control_state", "ping_probe_interval_secs").await;
    assert_column_exists(db.pool(), "control_state", "ping_probe_timeout_ms").await;
    assert_column_missing(db.pool(), "control_state", "tcp_probe_targets").await;
    assert_column_missing(db.pool(), "control_state", "tcp_probe_interval_secs").await;
    assert_column_missing(db.pool(), "control_state", "tcp_probe_timeout_ms").await;
    assert_column_exists(db.pool(), "control_state", "realtime_enabled").await;
    assert_column_exists(db.pool(), "control_state", "realtime_interval_secs").await;
    assert_column_exists(db.pool(), "nodes", "agent_log_max_mib").await;
    assert_table_exists(db.pool(), "node_ping_probe_samples").await;
    assert_column_exists(db.pool(), "node_ping_probe_samples", "node_id").await;
    assert_column_exists(db.pool(), "node_ping_probe_samples", "target").await;
    assert_column_exists(db.pool(), "node_ping_probe_samples", "probed_at").await;
    assert_column_exists(db.pool(), "node_ping_probe_samples", "attempted").await;
    assert_column_exists(db.pool(), "node_ping_probe_samples", "latency_us").await;
    assert_table_missing(db.pool(), "node_tcp_probe_samples").await;
    // Pin the deliberately minimal collection contract. A future chart can add a measurement only
    // together with a visible operator use; kernel/DNS diagnostics must not quietly accumulate.
    for column in [
        "connect_ms",
        "dns_ms",
        "resolved_ip",
        "ttl",
        "kernel_rtt_us",
        "rto_ms",
        "syn_retrans",
        "error",
    ] {
        assert_column_missing(db.pool(), "node_ping_probe_samples", column).await;
    }
    assert_table_exists(db.pool(), "artifact_blobs").await;
    assert_column_exists(db.pool(), "artifact_snapshots", "content_byte_len").await;
    assert_column_missing(db.pool(), "artifact_snapshots", "content").await;
    assert_column_missing(db.pool(), "node_applied_state", "grants_sha256").await;
    assert_column_exists(db.pool(), "node_agent_state", "token_hash").await;
    assert_column_exists(db.pool(), "node_agent_state", "token_prefix").await;
    assert_column_exists(db.pool(), "node_agent_state", "token_created_at").await;
    assert_column_exists(db.pool(), "node_agent_state", "token_last_used_at").await;
    assert_column_exists(db.pool(), "node_agent_state", "token_revoked_at").await;
    assert_column_missing(db.pool(), "node_agent_state", "cert_serial").await;
    assert_column_missing(db.pool(), "node_agent_state", "revoked_at").await;
    assert_table_exists(db.pool(), "node_enrollments").await;
    assert_column_exists(db.pool(), "node_enrollments", "token_hash").await;
    assert_column_exists(db.pool(), "node_enrollments", "expires_at").await;
    assert_column_exists(db.pool(), "node_enrollments", "used_at").await;
    assert_table_exists(db.pool(), "admin_operators").await;
    assert_table_exists(db.pool(), "clash_haitun_links").await;
    assert_column_exists(db.pool(), "clash_haitun_links", "token").await;
    assert_column_exists(db.pool(), "clash_haitun_links", "revoked_at").await;
    assert_column_exists(db.pool(), "deployment_target_state", "desired_grants").await;
    assert_column_exists(db.pool(), "deployment_target_state", "dispatched_grants").await;
    assert_column_exists(db.pool(), "deployment_target_state", "lifecycle_epoch").await;
    assert_column_exists(db.pool(), "deployment_target_state", "usage_generation_id").await;
    assert_table_exists(db.pool(), "node_lifecycle_state").await;
    assert_column_exists(db.pool(), "node_lifecycle_state", "lifecycle_epoch").await;
    assert_column_exists(db.pool(), "node_lifecycle_state", "phase").await;
    assert_table_exists(db.pool(), "node_lifecycle_events").await;
    assert_table_exists(db.pool(), "deployment_wave_confirmations").await;
    assert_table_exists(db.pool(), "subscription_serving_state").await;
    assert_table_exists(db.pool(), "subscription_client_snapshots").await;
    assert_table_exists(db.pool(), "subscription_client_state").await;
    assert_column_exists(
        db.pool(),
        "subscription_serving_state",
        "topology_revision_id",
    )
    .await;
    assert_column_exists(
        db.pool(),
        "subscription_serving_state",
        "permissions_revision_id",
    )
    .await;
    assert_column_exists(
        db.pool(),
        "subscription_serving_state",
        "client_snapshot_id",
    )
    .await;
    assert_column_exists(db.pool(), "subscription_serving_state", "generation").await;
    assert_column_exists(db.pool(), "deployments", "sync_of_deployment_id").await;
    assert_column_exists(db.pool(), "usage_samples", "has_gap").await;
    assert_column_exists(db.pool(), "usage_samples", "generation_id").await;
    assert_column_exists(db.pool(), "usage_chain_samples", "generation_id").await;
    assert_column_exists(db.pool(), "usage_readings", "agent_instance_id").await;
    assert_column_exists(db.pool(), "usage_readings", "sequence").await;
    assert_column_exists(db.pool(), "node_agent_state", "usage_last_result").await;
    assert_table_exists(db.pool(), "usage_generations").await;
    assert_table_exists(db.pool(), "usage_generation_activations").await;
    assert_table_exists(db.pool(), "usage_agent_cursors").await;
    assert_table_exists(db.pool(), "usage_report_receipts").await;
    assert_table_exists(db.pool(), "usage_counter_heads").await;
    assert_constraint_missing(db.pool(), "usage_samples_ingress_id_fkey").await;
    assert_constraint_missing(db.pool(), "usage_chain_samples_app_id_fkey").await;
    assert_constraint_missing(db.pool(), "usage_chain_samples_chain_id_fkey").await;
    assert_column_exists(db.pool(), "control_state", "reality_min_client_ver").await;
    assert_column_exists(db.pool(), "control_state", "reality_max_client_ver").await;
    assert_column_exists(db.pool(), "control_state", "reality_max_time_diff_ms").await;
    assert_column_exists(db.pool(), "control_state", "site_name").await;
    assert_column_exists(db.pool(), "control_state", "site_icon_data_url").await;

    let revision_id: i64 =
        sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("current_revision")
            .unwrap();

    let deployment_id: i64 = sqlx::query(
        "INSERT INTO deployments (revision_id, status, active, idempotency_key, warnings)
         VALUES ($1, 'halted', TRUE, 'deploy-1', '[]'::jsonb)
         RETURNING id",
    )
    .bind(revision_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("id")
    .unwrap();

    sqlx::query(
        "INSERT INTO deployment_targets (deployment_id, node_id, status)
         VALUES ($1, 'n1', 'canceled')",
    )
    .bind(deployment_id)
    .execute(db.pool())
    .await
    .unwrap();

    let second_active = sqlx::query(
        "INSERT INTO deployments (revision_id, status, active, idempotency_key, warnings)
         VALUES ($1, 'planned', TRUE, 'deploy-2', '[]'::jsonb)",
    )
    .bind(revision_id)
    .execute(db.pool())
    .await;
    assert!(
        second_active.is_err(),
        "single-flight index must reject a second active deployment"
    );

    let sha = "a".repeat(64);
    sqlx::query(
        "INSERT INTO artifact_snapshots (
            revision_id, target_kind, target_id, artifact_kind, content_sha256, content_byte_len
         ) VALUES ($1, 'user', 'alice', 'uri', $2, 512)",
    )
    .bind(revision_id)
    .bind(&sha)
    .execute(db.pool())
    .await
    .unwrap();

    let grant_snapshot = sqlx::query(
        "INSERT INTO artifact_snapshots (
            revision_id, target_kind, target_id, artifact_kind, content_sha256
         ) VALUES ($1, 'node', 'n1', 'grant-sync', $2)",
    )
    .bind(revision_id)
    .bind(sha)
    .execute(db.pool())
    .await;
    assert!(
        grant_snapshot.is_err(),
        "grant-sync must not be a historical artifact kind"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tcp_probe_schema_is_discarded_when_0001_is_replayed() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    // Shape the two probe objects back into the TCP-only development schema. This data is
    // intentionally disposable: the new wire contract distinguishes unattempted samples and uses
    // microseconds, so silently guessing those facts for old rows would manufacture history.
    sqlx::raw_sql(
        "ALTER TABLE control_state DROP COLUMN ping_probe_targets;
         ALTER TABLE control_state DROP COLUMN ping_probe_interval_secs;
         ALTER TABLE control_state DROP COLUMN ping_probe_timeout_ms;
         ALTER TABLE control_state
             ADD COLUMN tcp_probe_targets JSONB DEFAULT '[]'::jsonb NOT NULL;
         ALTER TABLE control_state
             ADD COLUMN tcp_probe_interval_secs INTEGER DEFAULT 60 NOT NULL;
         ALTER TABLE control_state
             ADD COLUMN tcp_probe_timeout_ms INTEGER DEFAULT 420 NOT NULL;
         UPDATE control_state
            SET tcp_probe_targets = '[{\"name\":\"old\",\"address\":\"tcp://192.0.2.1:443\"}]',
                tcp_probe_interval_secs = 75;

         DROP TABLE node_ping_probe_samples;
         CREATE TABLE node_tcp_probe_samples (
             node_id TEXT NOT NULL,
             target TEXT NOT NULL,
             probed_at TIMESTAMPTZ NOT NULL,
             connect_ms INTEGER
         );
         INSERT INTO node_tcp_probe_samples
         VALUES ('old-node', 'tcp://192.0.2.1:443', now(), 37);

         DELETE FROM _sqlx_migrations WHERE version = 1",
    )
    .execute(db.pool())
    .await
    .unwrap();

    db.store.migrate().await.unwrap();
    assert_eq!(
        db.store.ping_probe_settings().await.unwrap(),
        PingProbeSettings::default()
    );
    assert_column_missing(db.pool(), "control_state", "tcp_probe_targets").await;
    assert_table_missing(db.pool(), "node_tcp_probe_samples").await;
    let sample_count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_ping_probe_samples")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sample_count, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tcp_and_icmp_probe_samples_share_one_round_without_fabricating_loss() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .update_ping_probe_settings(
            &system_admin(),
            PingProbeSettings {
                targets: vec![
                    PingProbeTarget {
                        name: "TCP".to_owned(),
                        address: "tcp://192.0.2.1:443".to_owned(),
                    },
                    PingProbeTarget {
                        name: "ICMP".to_owned(),
                        address: "icmp://[2001:db8::1]".to_owned(),
                    },
                ],
                interval_secs: 5,
                timeout_ms: 420,
            },
        )
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let report = db
        .store
        .record_ping_probe(
            "n1",
            PingProbeReportRequest {
                probed_at_unix_secs: now,
                samples: vec![
                    PingProbeSample {
                        target: "tcp://192.0.2.1:443".to_owned(),
                        attempted: true,
                        latency_us: Some(37_250),
                    },
                    // No usable IPv6 route/socket is an observation gap, not an attempted timeout.
                    PingProbeSample {
                        target: "icmp://[2001:db8::1]".to_owned(),
                        attempted: false,
                        latency_us: None,
                    },
                ],
            },
        )
        .await
        .unwrap();
    assert_eq!(report.accepted_samples, 2);

    let view = db
        .store
        .node_ping_probe_view(&system_admin(), "n1", 3_600)
        .await
        .unwrap();
    assert_eq!(view.targets.len(), 2);
    assert_eq!(view.targets[0].samples[0].latency_us, Some(37_250));
    assert!(view.targets[0].samples[0].attempted);
    assert_eq!(view.targets[1].samples[0].latency_us, None);
    assert!(!view.targets[1].samples[0].attempted);

    let invalid = db
        .store
        .record_ping_probe(
            "n1",
            PingProbeReportRequest {
                probed_at_unix_secs: now + 1,
                samples: vec![PingProbeSample {
                    target: "icmp://[2001:db8::1]".to_owned(),
                    attempted: false,
                    latency_us: Some(1),
                }],
            },
        )
        .await;
    assert!(
        matches!(invalid, Err(StoreError::InvalidData(_))),
        "an unattempted sample must not smuggle in a latency"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn clash_subscription_uses_only_the_stable_serving_projection() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    set_quota(&db, Some(10_000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 1_200, 800).await;
    let initial_revision = seed_subscription_serving(&db).await;

    let uuid = "2d2304da-f114-4574-8d44-625afdb1db5c";
    let first = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert_eq!(first.user_id, "alice");
    assert_eq!(first.usage.upload_bytes, 1_200);
    assert_eq!(first.usage.download_bytes, 800);
    assert_eq!(first.usage.total_bytes, Some(10_000));
    assert_eq!(first.usage.remaining_bytes, Some(8_000));
    assert!(first.usage.reset_at.ends_with("+08:00"));
    assert!(first.content.contains("name: \"Main Chain\""));
    assert!(first.content.contains("# Brocade · SubBoost 标准版"));

    let before_client: (i64, i64) = sqlx::query_as(
        "SELECT client_snapshot_id, generation
           FROM subscription_serving_state
          WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    // A committed client-only rename advances its own checkpoint and creates no deployment.
    let rename = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertChain {
                app_id: "app-main".to_owned(),
                chain: CreateChainRequest {
                    id: "c-main".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Renamed Chain".to_owned(),
                    subscription_country: None,
                    note: None,
                },
            }],
            Some("rename chain".to_owned()),
        )
        .await
        .unwrap();
    let renamed_revision = rename.revision_id;
    assert!(matches!(
        rename.client_config.status,
        brocade_store::ClientConfigCommitStatus::Activated
    ));
    assert_ne!(
        rename.client_config.snapshot_id,
        u64::try_from(before_client.0).unwrap(),
        "a semantic client change must create a new immutable snapshot"
    );
    assert_eq!(
        rename.client_config.serving_generation,
        Some(u64::try_from(before_client.1 + 1).unwrap())
    );
    let after_client: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT serving.client_snapshot_id,
                client.head_snapshot_id,
                serving.generation,
                snapshot.source_revision_id
           FROM subscription_serving_state serving
           JOIN subscription_client_state client ON client.id = TRUE
           JOIN subscription_client_snapshots snapshot
             ON snapshot.id = serving.client_snapshot_id
          WHERE serving.id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(after_client.0, after_client.1);
    assert_eq!(
        u64::try_from(after_client.0).unwrap(),
        rename.client_config.snapshot_id
    );
    assert_eq!(after_client.2, before_client.1 + 1);
    assert_eq!(
        u64::try_from(after_client.3).unwrap(),
        renamed_revision,
        "the client snapshot retains its committed source revision"
    );
    let committed = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(!committed.content.contains("name: \"Main Chain\""));
    assert!(committed.content.contains("name: \"Renamed Chain\""));
    let deployment_count: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(deployment_count, 0, "rename must not forge a deployment");

    let deployment_id: i64 = sqlx::query_scalar(
        "INSERT INTO deployments (revision_id, status, active, kind, note)
         VALUES ($1, 'planned', TRUE, 'config', 'test serving gate')
         RETURNING id",
    )
    .bind(i64::try_from(renamed_revision).unwrap())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let during_planned = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(during_planned.content.contains("name: \"Renamed Chain\""));
    sqlx::query("UPDATE deployments SET status = 'halted' WHERE id = $1")
        .bind(deployment_id)
        .execute(db.pool())
        .await
        .unwrap();
    let during_halt = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(during_halt.content.contains("name: \"Renamed Chain\""));
    sqlx::query(
        "UPDATE deployments
            SET status = 'canceled', active = NULL, finished_at = now()
          WHERE id = $1",
    )
    .bind(deployment_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_applied_state (
             node_id, wireguard_state, xray_state, grants_state, observed_at
         ) VALUES ('n1', 'unknown', 'dirty', 'unknown', now())
         ON CONFLICT (node_id) DO UPDATE SET
             xray_state = 'dirty', xray_sha256 = NULL, observed_at = now()",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.store.clash_subscription_by_uuid(uuid).await,
        Err(StoreError::Unavailable(_))
    ));
    sqlx::query("DELETE FROM node_applied_state WHERE node_id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let after_cancel = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(after_cancel.content.contains("name: \"Renamed Chain\""));

    let renamed = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert_eq!(renamed.revision, initial_revision);
    assert!(renamed.content.contains("name: \"Renamed Chain\""));
    assert!(!renamed.content.contains("name: \"Main Chain\""));

    let partial_id: i64 = sqlx::query_scalar(
        "INSERT INTO deployments (revision_id, status, active, kind, note)
         VALUES ($1, 'canceled', NULL, 'config', 'partially applied test release')
         RETURNING id",
    )
    .bind(i64::try_from(renamed_revision).unwrap())
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO deployment_targets (deployment_id, node_id, status)
         VALUES ($1, 'n1', 'succeeded')",
    )
    .bind(partial_id)
    .execute(db.pool())
    .await
    .unwrap();
    let after_partial_cancel = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(after_partial_cancel
        .content
        .contains("name: \"Renamed Chain\""));
    // The unfinished release does not move the serving checkpoint. Remove the synthetic fixture
    // so the remaining assertions can exercise independent credential transitions.
    sqlx::query("DELETE FROM deployments WHERE id = $1")
        .bind(partial_id)
        .execute(db.pool())
        .await
        .unwrap();

    let grant_job_id: i64 = sqlx::query_scalar(
        "INSERT INTO jobs (kind, status, payload)
         VALUES ('grants-deployment', 'queued', jsonb_build_object('revision_id', $1))
         RETURNING id",
    )
    .bind(i64::try_from(renamed_revision).unwrap())
    .fetch_one(db.pool())
    .await
    .unwrap();
    let while_grants_wait = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(while_grants_wait
        .content
        .contains("name: \"Renamed Chain\""));
    sqlx::query("UPDATE jobs SET status = 'succeeded' WHERE id = $1")
        .bind(grant_job_id)
        .execute(db.pool())
        .await
        .unwrap();

    let link = db
        .store
        .issue_clash_haitun_link(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    let same_link = db
        .store
        .issue_clash_haitun_link(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    assert_eq!(
        same_link.token, link.token,
        "active issue must be idempotent"
    );
    let haitun = db
        .store
        .clash_subscription_by_haitun_token_for_family(&link.token, None)
        .await
        .unwrap();
    assert!(haitun.content.contains("# Brocade · koipy 测速"));
    assert!(!haitun.content.contains("rule-providers:"));
    let revoked = db
        .store
        .revoke_clash_haitun_link(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(matches!(
        db.store
            .clash_subscription_by_haitun_token_for_family(&link.token, None)
            .await,
        Err(StoreError::NotFound(_))
    ));
    let replacement = db
        .store
        .issue_clash_haitun_link(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    assert_ne!(replacement.token, link.token);

    // Credential rotation follows the independently converged permission line. Before it switches,
    // the old bearer remains the truthful deployed credential and the committed new one is absent.
    let next_uuid = "f98b74ba-58f1-41d0-aaad-8fa5724c6d2d";
    sqlx::query(
        "UPDATE users SET uuid = $1::uuid WHERE tenant_id = 'platform.acme' AND id = 'alice'",
    )
    .bind(next_uuid)
    .execute(db.pool())
    .await
    .unwrap();
    let rotated_revision = commit_direct_fixture_revision(&db, "rotate user UUID").await;
    assert_eq!(
        db.store
            .clash_subscription_by_uuid(uuid)
            .await
            .unwrap()
            .uuid,
        uuid
    );
    assert!(matches!(
        db.store.clash_subscription_by_uuid(next_uuid).await,
        Err(StoreError::NotFound(_))
    ));
    sqlx::query(
        "UPDATE subscription_serving_state
            SET permissions_revision_id = $1,
                permissions_deployment_id = NULL,
                generation = generation + 1,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(i64::try_from(rotated_revision).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.store.clash_subscription_by_uuid(uuid).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        db.store
            .clash_subscription_by_uuid(next_uuid)
            .await
            .unwrap()
            .uuid,
        next_uuid
    );

    sqlx::query(
        "UPDATE users SET status = 'disabled' WHERE tenant_id = 'platform.acme' AND id = 'alice'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let disabled_revision = commit_direct_fixture_revision(&db, "disable user").await;
    assert_eq!(
        db.store
            .clash_subscription_by_uuid(next_uuid)
            .await
            .unwrap()
            .uuid,
        next_uuid
    );
    sqlx::query(
        "UPDATE subscription_serving_state
            SET permissions_revision_id = $1,
                permissions_deployment_id = NULL,
                generation = generation + 1,
                updated_at = now()
          WHERE id = TRUE",
    )
    .bind(i64::try_from(disabled_revision).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.store.clash_subscription_by_uuid(next_uuid).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(initial_revision < renamed_revision);
}

/// A runtime reconcile must land, and an empty report must not erase the previous local
/// reconcile.
///
/// This watches the `COALESCE(last_local_reconcile, ...)` in `record_node_runtime`. It is a
/// textbook silent failure point: erased, everything proceeds as usual, the endpoint returns
/// 204, and the UI shows "never repaired" — while the truth is the machine did repair itself and
/// merely ran no reconcile this round. Writing the geodata SQL already went wrong twice in this
/// same way (shifted parameter numbering, a null hitting NOT NULL), and both times another test
/// caught it in passing because they were large enough to make the whole table unwritable. What
/// this one catches is the case where only this single field is wrong.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_runtime_report_round_trips_and_keeps_the_last_local_reconcile() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();

    let versions = NodeVersions {
        agent: "0.1.0".to_owned(),
        xray: Some("Xray 26.3.27 (Xray, Penetrates Everything.)".to_owned()),
        phantun: Some("phantun 0.7.0".to_owned()),
        wg_tools: Some("wireguard-tools v1.0.20210914".to_owned()),
        wg_backend: Some("kernel".to_owned()),
    };
    let reconcile = LocalReconcileReport {
        at: 1_767_225_600,
        actions: vec!["wireguard".to_owned()],
        error: None,
    };
    let geodata = GeodataObservation {
        geoip: Some(GeodataFileState {
            sha256: "c".repeat(64),
            bytes: 19_768_301,
            modified_at: 1_767_225_600,
        }),
        // One present and one absent really does happen: xray replaces files with individual
        // `os.Rename` calls, and a failure midway looks exactly like this. Hence an Option per
        // field rather than presence for the pair.
        geosite: None,
        asset_dir: "/usr/local/share/xray".to_owned(),
    };
    let wireguard_health = WireGuardHealth {
        enabled: true,
        error: None,
        peers: vec![WireGuardPeerHealth {
            peer_node_id: "n2".to_owned(),
            overlay_ip: Some("10.66.0.2".to_owned()),
            handshake_age_secs: Some(302_867),
            status: WireGuardPeerStatus::Down,
            detail: Some("overlay 探不通".to_owned()),
        }],
    };

    db.store
        .record_node_runtime(
            "n1",
            &NodeRuntimeReport {
                observed_at_unix_secs: None,
                certificate: Default::default(),
                versions: versions.clone(),
                geodata: Some(geodata.clone()),
                local_reconcile: Some(reconcile.clone()),
                wireguard_health: Some(wireguard_health.clone()),
                spool: SpoolBacklog {
                    observation: 3,
                    usage: 41,
                    dropped: 1_480,
                },
            },
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT agent_version, runtime_versions, spool_backlog, last_local_reconcile,
                wireguard_health, geodata_observed,
                runtime_reported_at IS NOT NULL AS stamped
         FROM node_agent_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    // The `agent_version` column is written only by `record_node_poll` (taking the User-Agent
    // header). A version that wrote it here too existed, with two paths writing one column in two
    // formats (wrapped in `brocade-agent/…` versus bare) while poll runs far more often and always
    // writes last, making this write pure waste. Now that there is one writer, this assertion
    // watches that it does not come back — what the agent reports about itself lives in
    // `runtime_versions.agent`.
    assert_eq!(
        row.try_get::<Option<String>, _>("agent_version").unwrap(),
        None,
        "record_node_runtime 不该碰 agent_version"
    );
    assert!(row.try_get::<bool, _>("stamped").unwrap());
    let stored: NodeVersions =
        serde_json::from_value(row.try_get("runtime_versions").unwrap()).unwrap();
    assert_eq!(stored, versions);
    let spool: SpoolBacklog =
        serde_json::from_value(row.try_get("spool_backlog").unwrap()).unwrap();
    assert_eq!(
        spool.dropped, 1_480,
        "累计丢弃是唯一一个「非零即有账永久丢了」的量"
    );
    let kept: LocalReconcileReport =
        serde_json::from_value(row.try_get("last_local_reconcile").unwrap()).unwrap();
    assert_eq!(kept, reconcile);
    let stored_wireguard: WireGuardHealth =
        serde_json::from_value(row.try_get("wireguard_health").unwrap()).unwrap();
    assert_eq!(stored_wireguard, wireguard_health);
    // The rule database shares this row and this channel — it is not an artifact of a release
    // but an observation every 30 minutes. A version on node_applied_state existed, and that path
    // is taken only on a release: a machine that does not ship for a month leaves its .dat state
    // unknown for a month, while the .dat files change daily.
    let dat: GeodataObservation =
        serde_json::from_value(row.try_get("geodata_observed").unwrap()).unwrap();
    assert_eq!(dat, geodata);

    // The second round carries no local reconcile — either the agent did not run one (it does
    // not while the control plane has work) or it is an older agent. The previous one must
    // survive.
    db.store
        .record_node_runtime(
            "n1",
            &NodeRuntimeReport {
                observed_at_unix_secs: None,
                certificate: Default::default(),
                versions: NodeVersions {
                    xray: Some("Xray 26.7.28".to_owned()),
                    ..versions.clone()
                },
                // The rule database is of a kind with versions and backlog: the fact of this
                // moment, overwritten every round. This round reports None (the asset directory
                // vanished, say) and the assertion below confirms it really was overwritten to
                // empty — the exact inverse of `last_local_reconcile`'s "must not be
                // erased".
                geodata: None,
                local_reconcile: None,
                wireguard_health: None,
                spool: SpoolBacklog {
                    observation: 0,
                    usage: 0,
                    dropped: 1_480,
                },
            },
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT runtime_versions, spool_backlog, last_local_reconcile, wireguard_health,
                geodata_observed
         FROM node_agent_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    let kept: LocalReconcileReport =
        serde_json::from_value(row.try_get("last_local_reconcile").unwrap()).unwrap();
    assert_eq!(kept, reconcile, "空报告把上一次的本地对账抹掉了");
    let kept_wireguard: WireGuardHealth =
        serde_json::from_value(row.try_get("wireguard_health").unwrap()).unwrap();
    assert_eq!(
        kept_wireguard, wireguard_health,
        "旧 agent 没有该字段时不该清掉最后一次 WG 判定"
    );
    // Versions and backlog are the inverse — they are the fact of this moment and are
    // overwritten every round.
    let stored: NodeVersions =
        serde_json::from_value(row.try_get("runtime_versions").unwrap()).unwrap();
    assert_eq!(
        stored.xray.as_deref(),
        Some("Xray 26.7.28"),
        "版本没跟着更新"
    );
    let spool: SpoolBacklog =
        serde_json::from_value(row.try_get("spool_backlog").unwrap()).unwrap();
    assert_eq!(spool.observation, 0, "积压没跟着更新");
    assert_eq!(
        row.try_get::<serde_json::Value, _>("geodata_observed")
            .unwrap(),
        serde_json::json!({}),
        "规则库是此刻的事实，报 None 就该被覆盖成空——它跟本地对账的语义相反"
    );
}

/// Delivery is asynchronous, so an older snapshot may finish its POST after a newer one. The
/// server must acknowledge that retry without moving the current runtime state backwards.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn delayed_runtime_report_cannot_overwrite_a_newer_snapshot() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let report = |observed_at_unix_secs, agent: &str| NodeRuntimeReport {
        observed_at_unix_secs: Some(observed_at_unix_secs),
        certificate: Default::default(),
        versions: NodeVersions {
            agent: agent.to_owned(),
            xray: None,
            phantun: None,
            wg_tools: None,
            wg_backend: None,
        },
        geodata: None,
        local_reconcile: None,
        wireguard_health: None,
        spool: SpoolBacklog {
            observation: 0,
            usage: 0,
            dropped: 0,
        },
    };

    db.store
        .record_node_runtime("n1", &report(now + 1, "new"))
        .await
        .unwrap();
    db.store
        .record_node_runtime("n1", &report(now, "old"))
        .await
        .unwrap();

    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT runtime_versions FROM node_agent_state WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored["agent"], "new");
}

/// A machine with no valid token may not write runtime state. The same gate as
/// `record_node_poll` — both endpoints are called unattended from the agent side, and one gate
/// missing is as good as none.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_runtime_report_is_rejected_without_an_active_token() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let report = NodeRuntimeReport {
        observed_at_unix_secs: None,
        certificate: Default::default(),
        versions: NodeVersions {
            agent: "0.1.0".to_owned(),
            xray: None,
            phantun: None,
            wg_tools: None,
            wg_backend: None,
        },
        geodata: None,
        local_reconcile: None,
        wireguard_health: None,
        spool: SpoolBacklog {
            observation: 0,
            usage: 0,
            dropped: 0,
        },
    };

    // No token has been signed yet
    let error = db
        .store
        .record_node_runtime("n1", &report)
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Unauthorized(_)), "{error:?}");

    // Signed and then revoked
    db.store.issue_node_token("n1").await.unwrap();
    db.store.record_node_runtime("n1", &report).await.unwrap();
    db.store.revoke_node_token("n1").await.unwrap();
    let error = db
        .store
        .record_node_runtime("n1", &report)
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::Unauthorized(_)), "{error:?}");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_token_issue_authenticate_and_record_poll() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let issued = db.store.issue_node_token("n1").await.unwrap();
    assert_eq!(issued.node_id, "n1");
    assert!(issued.token.starts_with(NODE_TOKEN_PREFIX));
    assert_eq!(
        issued.token_prefix,
        node_token_display_prefix(&issued.token)
    );

    let stored = sqlx::query(
        "SELECT token_hash, token_prefix,
                token_created_at IS NOT NULL AS created,
                token_last_used_at IS NULL AS never_used,
                token_revoked_at IS NULL AS active
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        stored.try_get::<String, _>("token_hash").unwrap(),
        node_token_hash(&issued.token)
    );
    assert_ne!(
        stored.try_get::<String, _>("token_hash").unwrap(),
        issued.token
    );
    assert_eq!(
        stored.try_get::<String, _>("token_prefix").unwrap(),
        issued.token_prefix
    );
    assert!(stored.try_get::<bool, _>("created").unwrap());
    assert!(stored.try_get::<bool, _>("never_used").unwrap());
    assert!(stored.try_get::<bool, _>("active").unwrap());

    assert!(db
        .store
        .authenticate_node_token("broc_node_wrong")
        .await
        .unwrap()
        .is_none());

    let authenticated = db
        .store
        .authenticate_node_token(&issued.token)
        .await
        .unwrap()
        .expect("issued token should authenticate");
    assert_eq!(authenticated.node_id, "n1");
    assert_eq!(authenticated.token_prefix, Some(issued.token_prefix));

    let token_used: bool = sqlx::query(
        "SELECT token_last_used_at IS NOT NULL AS used
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("used")
    .unwrap();
    assert!(token_used);

    db.store
        .record_node_poll("n1", Some(AGENT_BUILD))
        .await
        .unwrap();
    let poll = sqlx::query(
        "SELECT agent_version, last_poll_at IS NOT NULL AS polled
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        poll.try_get::<Option<String>, _>("agent_version").unwrap(),
        Some(AGENT_BUILD.to_owned())
    );
    assert!(poll.try_get::<bool, _>("polled").unwrap());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_token_rotation_invalidates_previous_token() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let first = db.store.issue_node_token("n1").await.unwrap();
    let second = db.store.issue_node_token("n1").await.unwrap();
    assert_ne!(first.token, second.token);

    assert!(db
        .store
        .authenticate_node_token(&first.token)
        .await
        .unwrap()
        .is_none());
    let authenticated = db
        .store
        .authenticate_node_token(&second.token)
        .await
        .unwrap()
        .expect("rotated token should authenticate");
    assert_eq!(authenticated.node_id, "n1");

    let stored_hash: String = sqlx::query(
        "SELECT token_hash
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("token_hash")
    .unwrap();
    assert_eq!(stored_hash, node_token_hash(&second.token));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_token_revoke_blocks_authentication_and_poll_updates() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let issued = db.store.issue_node_token("n1").await.unwrap();
    assert!(db.store.revoke_node_token("n1").await.unwrap());
    assert!(db.store.revoke_node_token("n1").await.unwrap());
    assert!(db
        .store
        .authenticate_node_token(&issued.token)
        .await
        .unwrap()
        .is_none());
    let poll_error = db
        .store
        .record_node_poll("n1", Some(AGENT_BUILD))
        .await
        .unwrap_err();
    assert!(matches!(poll_error, StoreError::Unauthorized(_)));

    let revoked: bool = sqlx::query(
        "SELECT token_revoked_at IS NOT NULL AS revoked
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("revoked")
    .unwrap();
    assert!(revoked);
}

/// A relay port's keys are regenerated only when the kind changes.
///
/// Changing REALITY's borrowed site has nothing to do with the keys, yet replacing the pair on
/// every save costs every relay dialing this machine its handshake until the next release lands —
/// while the operator merely changed a domain name.
///
/// Once relay ports moved onto the chain the test moved with them: not "what kind this machine is
/// now" but "what kind this chain on this machine is now".
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn hop_security_keeps_its_keys_until_the_kind_changes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Main".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();

    async fn set(db: &TestPg, security: HopWireRequest) {
        put_step_draft(
            db,
            "app-main",
            "c-main",
            "n1",
            PutStepRequest {
                accept: Some(StepAcceptRequest {
                    uuid: None,
                    label: None,
                }),
                hop_in: Some(HopInRequest {
                    port: 20000,
                    security: Some(security),
                }),
                rules: vec![Rule {
                    dest_match: DestMatch::Any,
                    action: Action::Egress { send_through: None },
                }],
                note: None,
            },
        )
        .await;
    }

    set(&db, HopWireRequest::Encryption).await;
    let first = stored_hop_security(db.pool()).await;
    assert_eq!(first["t"], "encryption");
    let private_key = first["v"]["private_key"].as_str().unwrap().to_owned();
    assert_eq!(
        private_key.len(),
        43,
        "X25519 私钥是 base64url 无填充的 32 字节"
    );

    // Storing the same kind again: the keys must be untouched.
    set(&db, HopWireRequest::Encryption).await;
    assert_eq!(
        stored_hop_security(db.pool()).await["v"]["private_key"],
        private_key.as_str()
    );

    // Switching to REALITY: this is what should produce new material.
    set(
        &db,
        HopWireRequest::Reality {
            dest: "apps.apple.com:443".to_owned(),
            server_names: vec!["apps.apple.com".to_owned()],
            fingerprint: None,
        },
    )
    .await;
    let reality = stored_hop_security(db.pool()).await;
    assert_eq!(reality["t"], "reality");
    assert_ne!(reality["v"]["private_key"], private_key.as_str());
    assert_eq!(reality["v"]["fingerprint"], "chrome");
    let reality_key = reality["v"]["private_key"].as_str().unwrap().to_owned();

    // Changing only the site keeps the keys.
    set(
        &db,
        HopWireRequest::Reality {
            dest: "www.microsoft.com:443".to_owned(),
            server_names: vec!["www.microsoft.com".to_owned()],
            fingerprint: None,
        },
    )
    .await;
    let moved = stored_hop_security(db.pool()).await;
    assert_eq!(moved["v"]["dest"], "www.microsoft.com:443");
    assert_eq!(moved["v"]["private_key"], reality_key.as_str());
}

/// Suggestions are per node: one machine has one wg0 and one MTU, and its suggestion is the
/// smallest across all its paths. The two unreachable outcomes do not participate — recording
/// blocked ICMP as a very small number has the operator follow it and drop the MTU to the floor,
/// on a link with nothing wrong with it.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn link_mtu_suggestion_is_per_node_and_ignores_inconclusive_probes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    for id in ["hk-01", "sg-01", "au-01", "nat-01"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();

    let result = db
        .store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: now,
                links: vec![
                    LinkProbe {
                        peer_node_id: "sg-01".to_owned(),
                        endpoint_host: "sg1.example.net".to_owned(),
                        status: LinkProbeStatus::Ok,
                        path_mtu: Some(1500),
                        suggested_wg_mtu: Some(1440),
                    },
                    LinkProbe {
                        peer_node_id: "au-01".to_owned(),
                        endpoint_host: "au1.example.net".to_owned(),
                        status: LinkProbeStatus::Ok,
                        path_mtu: Some(1258),
                        suggested_wg_mtu: Some(1198),
                    },
                    LinkProbe {
                        peer_node_id: "nat-01".to_owned(),
                        endpoint_host: "nat1.example.net".to_owned(),
                        status: LinkProbeStatus::Blocked,
                        path_mtu: None,
                        suggested_wg_mtu: None,
                    },
                    LinkProbe {
                        peer_node_id: "ghost-01".to_owned(),
                        endpoint_host: "ghost.example.net".to_owned(),
                        status: LinkProbeStatus::Ok,
                        path_mtu: Some(600),
                        suggested_wg_mtu: Some(540),
                    },
                ],
            },
        )
        .await
        .unwrap();

    assert_eq!(result.accepted_links, 3);
    assert_eq!(
        result.unknown_peers, 1,
        "ghost-01 不在模型里，这一行该丢掉而不是把建议值拉到 540"
    );

    let view = db.store.link_mtu_view(&system_admin()).await.unwrap();
    assert_eq!(view.links.len(), 3);
    assert_eq!(view.default_mtu, 1420);
    let node = |id: &str| {
        view.nodes
            .iter()
            .find(|n| n.node_id == id)
            .unwrap_or_else(|| panic!("没有 {id}"))
            .clone()
    };

    let hk = node("hk-01");
    assert_eq!(hk.suggested_mtu, Some(1198), "hk-01 到 au-01 那条最窄");
    assert_eq!(hk.tightest_peer.as_deref(), Some("au-01"));
    assert_eq!(hk.inconclusive, 1, "nat-01 那条 ICMP 被挡");
    assert_eq!(hk.current_mtu, 1420);
    assert!(!hk.overridden, "还没单独设过，用的是全局默认");

    // A path is shared by its two ends, so the peer gets a suggestion too — even though hk-01
    // originated the probe. Looking only at the originator leaves the machine behind NAT without
    // one forever, and it is the likeliest to need its MTU adjusted.
    assert_eq!(node("au-01").suggested_mtu, Some(1198));
    assert_eq!(node("au-01").tightest_peer.as_deref(), Some("hk-01"));
    assert_eq!(
        node("sg-01").suggested_mtu,
        Some(1440),
        "sg-01 只在一条宽路径上，不该被 au-01 那条拖下去"
    );
    assert_eq!(node("nat-01").suggested_mtu, None, "它那条没探出结果");
    assert_eq!(node("nat-01").inconclusive, 1);

    // Reporting the same pair again overwrites rather than accumulates.
    db.store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: now + 1,
                links: vec![LinkProbe {
                    peer_node_id: "au-01".to_owned(),
                    endpoint_host: "au1.example.net".to_owned(),
                    status: LinkProbeStatus::Ok,
                    path_mtu: Some(1492),
                    suggested_wg_mtu: Some(1432),
                }],
            },
        )
        .await
        .unwrap();
    let view = db.store.link_mtu_view(&system_admin()).await.unwrap();
    assert_eq!(view.links.len(), 3, "每对只留最新一条");
    let hk = view.nodes.iter().find(|n| n.node_id == "hk-01").unwrap();
    assert_eq!(
        hk.suggested_mtu,
        Some(1432),
        "au-01 那条变宽了，建议值跟着走"
    );

    // A delayed older request is accepted but cannot roll the latest path back.
    db.store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: now,
                links: vec![LinkProbe {
                    peer_node_id: "au-01".to_owned(),
                    endpoint_host: "au1.example.net".to_owned(),
                    status: LinkProbeStatus::Ok,
                    path_mtu: Some(1100),
                    suggested_wg_mtu: Some(1040),
                }],
            },
        )
        .await
        .unwrap();
    let view = db.store.link_mtu_view(&system_admin()).await.unwrap();
    let hk = view.nodes.iter().find(|n| n.node_id == "hk-01").unwrap();
    assert_eq!(hk.suggested_mtu, Some(1432), "旧包覆盖了新的 MTU 结果");
}

/// A node's own MTU overrides the global default, and the global default governs only those
/// that set none.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_mtu_overrides_the_global_default() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    for id in ["wide-01", "narrow-01"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }

    db.store
        .update_node(
            &system_admin(),
            "narrow-01",
            UpdateNodeRequest {
                mtu: Some(1198),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let mtu = |id: &str| snapshot.nodes.iter().find(|n| n.id == id).unwrap().mtu;
    assert_eq!(mtu("narrow-01"), Some(1198));
    assert_eq!(mtu("wide-01"), None, "没设过就是空，编译时才落到全局默认");

    // In the artifacts it lands in [Interface]: one interface, one MTU, and [Peer] has no such
    // key.
    let compiled = brocade_core::compile::compile(&snapshot);
    let plan = compiled.project_node("narrow-01").unwrap();
    assert_eq!(plan.wireguard.unwrap().mtu, Some(1198));
    let plan = compiled.project_node("wide-01").unwrap();
    assert_eq!(plan.wireguard.unwrap().mtu, Some(1420), "用全局默认");

    // Passing 0 clears it, returning to the default.
    db.store
        .update_node(
            &system_admin(),
            "narrow-01",
            UpdateNodeRequest {
                mtu: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == "narrow-01")
            .unwrap()
            .mtu,
        None
    );
}

/// A machine's connection policy overrides the global default one field at a time, and
/// clearing a field puts it back.
///
/// The per-field part is the point. Overriding the buffer must not drag the idle timeout along
/// with it — a merge that copied the whole block whenever any of it was set would look correct
/// on a machine that overrode everything and be silently wrong everywhere else.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_connection_policy_overrides_the_global_default_field_by_field() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    // An app has to exist for any machine to get an xray plan at all — `xray_plan` looks a node
    // up through the application layer, and with no apps there is nothing to look it up in.
    insert_minimal_fixture(db.pool()).await;
    for id in ["tuned-01", "default-01"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }

    // Only the buffer. The other three stay empty and must keep coming from the settings.
    db.store
        .update_node(
            &system_admin(),
            "tuned-01",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    buffer_size_kb: Some(64),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let conn = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap()
            .connection
    };
    assert_eq!(conn("tuned-01").buffer_size_kb, Some(64));
    assert_eq!(
        conn("tuned-01").conn_idle_secs,
        None,
        "没提的那几项不该被顺手写上"
    );
    assert_eq!(conn("default-01"), NodeConnection::default());

    // What each machine ends up with after the merge.
    let compiled = brocade_core::compile::compile(&snapshot);
    let policy = |id: &str| compiled.project_node(id).unwrap().xray.unwrap().connection;
    assert_eq!(policy("tuned-01").buffer_size_kb, Some(64));
    assert_eq!(policy("tuned-01").conn_idle_secs, 300, "回落到全局默认");
    assert_eq!(policy("default-01").buffer_size_kb, None);
    assert_eq!(policy("default-01").conn_idle_secs, 300);

    // Absent at both levels means the artifact names no buffer at all — xray then sizes it by
    // CPU architecture, and a number here would flatten arm64's 4 KB onto x86_64's 512 KB.
    let rendered = brocade_core::format::json::xray(&brocade_core::artifacts::xray::build(
        &compiled.project_node("default-01").unwrap(),
    ));
    assert!(
        !rendered.contains("bufferSize"),
        "两级都没设时不该写这个键：{rendered}"
    );

    // Clearing it: the whole block submitted with every field empty.
    db.store
        .update_node(
            &system_admin(),
            "tuned-01",
            UpdateNodeRequest {
                connection: Some(NodeConnection::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == "tuned-01")
            .unwrap()
            .connection,
        NodeConnection::default()
    );

    // A request that says nothing about the policy leaves it alone, rather than clearing it —
    // the distinction the `Option` around the block exists to carry.
    db.store
        .update_node(
            &system_admin(),
            "tuned-01",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    conn_idle_secs: Some(900),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store
        .update_node(
            &system_admin(),
            "tuned-01",
            UpdateNodeRequest {
                name: Some("renamed".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == "tuned-01")
            .unwrap()
            .connection
            .conn_idle_secs,
        Some(900),
        "这次请求没提连接策略，不该被当成清空"
    );
}

/// The bounds hold on both pages. One enforced on the global default but not on a machine's
/// override is a bound an operator walks around by typing the number on the other screen.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn connection_policy_bounds_hold_on_both_the_node_and_the_settings() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .provision_node(&system_admin(), provision_node_request("n1"))
        .await
        .unwrap();

    // 0 reaps a connection the moment it falls quiet, so it is not a timeout — refused.
    let error = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    conn_idle_secs: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::InvalidData(_)), "{error:?}");

    // 0 on a half-close is a real choice — close as soon as the other direction has — and passes.
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    uplink_only_secs: Some(0),
                    buffer_size_kb: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let settings = db.store.settings().await.unwrap();
    let error = db
        .store
        .update_settings(
            &system_admin(),
            ModelSettings {
                connection: ConnectionSettings {
                    conn_idle_secs: 0,
                    ..settings.connection
                },
                ..settings.clone()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::InvalidData(_)), "{error:?}");

    // Handshake has no per-node column, and its own bound is enforced here.
    let error = db
        .store
        .update_settings(
            &system_admin(),
            ModelSettings {
                connection: ConnectionSettings {
                    handshake_secs: 0,
                    ..settings.connection
                },
                ..settings
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::InvalidData(_)), "{error:?}");
}

/// Probe targets come from the control plane, because only it knows a peer's real endpoint and
/// encapsulation.
///
/// An agent deriving them from wireguard.conf is wrong: for a peer behind phantun that file's
/// `Endpoint` is by design `127.0.0.1:<local port>`, and probing it measures our own
/// loopback.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn probe_targets_use_public_ipv4_and_the_peers_transport() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    for id in ["direct-01", "phantun-01", "nat-01"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }
    // phantun-01's entrance is fake TCP; nat-01 is behind NAT and can still record a public
    // IP.
    db.store
        .update_node(
            &system_admin(),
            "phantun-01",
            UpdateNodeRequest {
                wg_transport: Some(WgTransport::FakeTcp { port: 39743 }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store
        .update_node(
            &system_admin(),
            "nat-01",
            UpdateNodeRequest {
                public_ipv4_nat: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let list = db.store.probe_targets("direct-01").await.unwrap();

    let ids = list
        .targets
        .iter()
        .map(|t| t.peer_node_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["phantun-01"],
        "nat-01 标记为 NAT，从这一侧探不到，该由它自己去探；自己也不探自己"
    );
    let target = &list.targets[0];
    assert_eq!(
        target.host, "phantun-01.example.net",
        "给的是真实落点，不是 wg conf 里那个回环"
    );
    assert_eq!(
        target.transport,
        ProbeTransport::FakeTcp,
        "假 TCP 的开销比直连多 12 字节，减错了建议值就偏大"
    );

    // Conversely: the NAT'd machine can probe every reachable peer, and its coverage does not
    // shrink because it is itself behind NAT
    let from_nat = db.store.probe_targets("nat-01").await.unwrap();
    let mut ids = from_nat
        .targets
        .iter()
        .map(|t| t.peer_node_id.as_str())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["direct-01", "phantun-01"]);
}

async fn stored_hop_security(pool: &PgPool) -> serde_json::Value {
    sqlx::query("SELECT hop_in_wire FROM steps WHERE chain_id = 'c-main' AND node_id = 'n1'")
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get("hop_in_wire")
        .unwrap()
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn provision_node_creates_revision_node_overlay_key_and_enrollment() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let result = db
        .store
        .provision_node(&system_admin(), provision_node_request("n-provision"))
        .await
        .unwrap();

    assert_eq!(result.revision_id, 2);
    assert_eq!(result.node.id, "n-provision");
    assert_eq!(result.node.overlay_addr.to_string(), "10.66.0.1");
    assert_eq!(result.node.wg_public_key.len(), 44);
    assert!(result.enrollment.token.starts_with(ENROLLMENT_TOKEN_PREFIX));
    assert_eq!(result.enrollment.token_prefix.len(), 28);

    let row = sqlx::query(
        "SELECT current_revision,
                wg_private_key,
                wg_public_key,
                created_revision
         FROM control_state
         CROSS JOIN nodes
         WHERE nodes.id = 'n-provision'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.try_get::<i64, _>("current_revision").unwrap(), 2);
    assert_eq!(row.try_get::<i64, _>("created_revision").unwrap(), 2);
    assert_eq!(
        row.try_get::<String, _>("wg_private_key").unwrap().len(),
        44
    );
    assert_eq!(
        row.try_get::<String, _>("wg_public_key").unwrap(),
        result.node.wg_public_key
    );

    let enrollment = sqlx::query(
        "SELECT token_hash, token_prefix, expires_at IS NULL AS never_expires, used_at::text AS used_at
         FROM node_enrollments
         WHERE node_id = 'n-provision'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        enrollment.try_get::<String, _>("token_prefix").unwrap(),
        result.enrollment.token_prefix
    );
    assert_ne!(
        enrollment.try_get::<String, _>("token_hash").unwrap(),
        result.enrollment.token
    );
    // The default without a TTL: never expires
    assert!(result.enrollment.expires_at.is_none());
    assert!(enrollment.try_get::<bool, _>("never_expires").unwrap());
    assert!(enrollment
        .try_get::<Option<String>, _>("used_at")
        .unwrap()
        .is_none());

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].id, "n-provision");
}

/// The strategy has to survive the whole loop: provision, update, materialize. Each leg is a
/// separate piece of SQL with its own parameter list, and getting one of them wrong shows up only
/// at runtime — the column would keep its default while the console reported the change as saved.
///
/// That it then reaches xray.json is checked in brocade-core's `xray` test, where a node with an
/// actual chain on it is cheap to build; a freshly provisioned machine carries none, so its xray
/// artifact is Disabled and has no outbound to look at.
///
/// The default leg matters as much as the changed one. `UseIP` reproduces what the artifact layer
/// used to hard-code, which is what lets an existing machine keep its behaviour without being
/// touched — and what keeps the golden artifacts byte-identical.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn domain_strategy_round_trips_from_provision_through_update_to_materialize() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let mut request = provision_node_request("n-strategy");
    request.domain_strategy = DomainStrategy::UseIpv4v6;
    let result = db
        .store
        .provision_node(&system_admin(), request)
        .await
        .unwrap();
    assert_eq!(result.node.domain_strategy, DomainStrategy::UseIpv4v6);

    let strategy_of = |snapshot: &brocade_core::model::ModelSnapshot| {
        snapshot
            .nodes
            .iter()
            .find(|node| node.id == "n-strategy")
            .unwrap()
            .domain_strategy
    };

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(strategy_of(&snapshot), DomainStrategy::UseIpv4v6);

    // The detail view's edit. A no-op save must not burn a revision, which is what the
    // ROW(...) IS DISTINCT FROM guard in update_node is for — the new column has to appear on
    // both sides of it or a real change would be judged as no change.
    let revision = || async {
        sqlx::query("SELECT current_revision FROM control_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get::<i64, _>("current_revision")
            .unwrap()
    };
    let before = revision().await;
    db.store
        .update_node(
            &system_admin(),
            "n-strategy",
            UpdateNodeRequest {
                domain_strategy: Some(DomainStrategy::UseIpv4v6),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        revision().await,
        before,
        "saving the value it already holds must not stamp a revision"
    );

    db.store
        .update_node(
            &system_admin(),
            "n-strategy",
            UpdateNodeRequest {
                domain_strategy: Some(DomainStrategy::AsIs),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(strategy_of(&snapshot), DomainStrategy::AsIs);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn provision_node_requires_system_admin() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let result = db
        .store
        .provision_node(
            &publisher("platform.acme"),
            provision_node_request("n-denied"),
        )
        .await;
    assert!(matches!(result, Err(StoreError::Forbidden(_))));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn provision_node_allocates_next_available_overlay_address() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    insert_basic_node(db.pool(), "n-existing", "platform.acme", "10.66.0.1", 51821).await;

    let result = db
        .store
        .provision_node(&system_admin(), provision_node_request("n-next"))
        .await
        .unwrap();

    assert_eq!(result.node.overlay_addr.to_string(), "10.66.0.2");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn redeem_node_enrollment_issues_node_token_and_rejects_reuse() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    let result = db
        .store
        .provision_node(&system_admin(), provision_node_request("n-enroll"))
        .await
        .unwrap();

    let issued = db
        .store
        .redeem_node_enrollment(&result.enrollment.token)
        .await
        .unwrap();
    assert_eq!(issued.node_id, "n-enroll");
    assert!(issued.token.starts_with(NODE_TOKEN_PREFIX));
    assert!(db
        .store
        .authenticate_node_token(&issued.token)
        .await
        .unwrap()
        .is_some());

    let reused = db
        .store
        .redeem_node_enrollment(&result.enrollment.token)
        .await;
    assert!(matches!(reused, Err(StoreError::Unauthorized(_))));

    let used: bool = sqlx::query(
        "SELECT used_at IS NOT NULL AS used
         FROM node_enrollments
         WHERE node_id = 'n-enroll'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("used")
    .unwrap();
    assert!(used);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn redeem_node_enrollment_rejects_expired_tokens() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();
    let result = db
        .store
        .provision_node(&system_admin(), provision_node_request("n-expired"))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_enrollments
         SET created_at = now() - interval '2 hours',
             expires_at = now() - interval '1 hour'
         WHERE node_id = 'n-expired'",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let expired = db
        .store
        .redeem_node_enrollment(&result.enrollment.token)
        .await;
    assert!(matches!(expired, Err(StoreError::Unauthorized(_))));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn admin_operator_token_issue_authenticate_and_revoke() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let invalid = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "bad-system".to_owned(),
                display_name: "Bad System".to_owned(),
                role: AdminRole::SystemAdmin,
                tenant_scope: Some("platform.acme".to_owned()),
                password: None,
            },
        )
        .await;
    assert!(invalid.is_err(), "system-admin must not carry tenant_scope");

    let operator = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "publisher-1".to_owned(),
                display_name: "Publisher One".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.acme".to_owned()),
                password: Some("publisher-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(operator.role, AdminRole::Publisher);
    assert_eq!(operator.tenant_scope.as_deref(), Some("platform.acme"));

    let issued = db
        .store
        .issue_admin_token(&system_admin(), "publisher-1")
        .await
        .unwrap();
    assert!(issued.token.starts_with("broc_admin_"));
    assert!(db
        .store
        .authenticate_node_token(&issued.token)
        .await
        .unwrap()
        .is_none());

    let authenticated = db
        .store
        .authenticate_admin_token(&issued.token)
        .await
        .unwrap()
        .expect("issued admin token should authenticate");
    assert_eq!(authenticated.operator_id, "publisher-1");
    assert_eq!(authenticated.role, AdminRole::Publisher);
    assert_eq!(authenticated.tenant_scope.as_deref(), Some("platform.acme"));

    let first_used: Option<String> = sqlx::query_scalar(
        "SELECT token_last_used_at::text FROM admin_operators WHERE id = 'publisher-1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.store
        .authenticate_admin_token(&issued.token)
        .await
        .unwrap()
        .unwrap();
    let second_used: Option<String> = sqlx::query_scalar(
        "SELECT token_last_used_at::text FROM admin_operators WHERE id = 'publisher-1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(first_used, second_used, "last_used 写入应按分钟限频");

    let used: bool = sqlx::query(
        "SELECT token_last_used_at IS NOT NULL AS used
         FROM admin_operators
         WHERE id = 'publisher-1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("used")
    .unwrap();
    assert!(used);

    assert!(db
        .store
        .revoke_admin_token(&system_admin(), "publisher-1")
        .await
        .unwrap());
    assert!(db
        .store
        .authenticate_admin_token(&issued.token)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn admin_operator_requires_existing_tenant_scope() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let missing = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "missing-tenant-publisher".to_owned(),
                display_name: "Missing Tenant Publisher".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.missing".to_owned()),
                password: None,
            },
        )
        .await;
    assert!(
        matches!(missing, Err(StoreError::NotFound(message)) if message == "tenant platform.missing")
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_admin_manages_only_operator_subtree() {
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

    let acme_admin = tenant_admin("platform.acme");
    let operator = db
        .store
        .create_admin_operator(
            &acme_admin,
            CreateAdminOperatorRequest {
                id: "acme-publisher".to_owned(),
                display_name: "Acme Publisher".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.acme.child".to_owned()),
                password: Some("publisher-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        operator.tenant_scope.as_deref(),
        Some("platform.acme.child")
    );

    let issued = db
        .store
        .issue_admin_token(&acme_admin, "acme-publisher")
        .await
        .unwrap();
    assert_eq!(issued.operator_id, "acme-publisher");
    assert!(db
        .store
        .revoke_admin_token(&acme_admin, "acme-publisher")
        .await
        .unwrap());

    let outside = db
        .store
        .create_admin_operator(
            &acme_admin,
            CreateAdminOperatorRequest {
                id: "other-publisher".to_owned(),
                display_name: "Other Publisher".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.other".to_owned()),
                password: Some("publisher-secret".to_owned()),
            },
        )
        .await;
    assert!(matches!(outside, Err(StoreError::Forbidden(_))));

    let system = db
        .store
        .create_admin_operator(
            &acme_admin,
            CreateAdminOperatorRequest {
                id: "system-2".to_owned(),
                display_name: "System Two".to_owned(),
                role: AdminRole::SystemAdmin,
                tenant_scope: None,
                password: None,
            },
        )
        .await;
    assert!(matches!(system, Err(StoreError::Forbidden(_))));

    let public = db
        .store
        .create_admin_operator(
            &acme_admin,
            CreateAdminOperatorRequest {
                id: "public".to_owned(),
                display_name: "Public".to_owned(),
                role: AdminRole::Readonly,
                tenant_scope: Some("platform.acme".to_owned()),
                password: None,
            },
        )
        .await;
    assert!(matches!(public, Err(StoreError::Forbidden(_))));

    db.store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "other-publisher".to_owned(),
                display_name: "Other Publisher".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.other".to_owned()),
                password: Some("publisher-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    let revoke_outside = db
        .store
        .revoke_admin_token(&acme_admin, "other-publisher")
        .await;
    assert!(matches!(revoke_outside, Err(StoreError::Forbidden(_))));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn admin_operator_password_set_reset_and_change() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    // A privileged account may never be made passwordless.
    let rejected = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "editor-1".to_owned(),
                display_name: "Editor One".to_owned(),
                role: AdminRole::Editor,
                tenant_scope: Some("platform.acme".to_owned()),
                password: None,
            },
        )
        .await;
    assert!(matches!(rejected, Err(StoreError::InvalidData(_))));

    let editor = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "editor-1".to_owned(),
                display_name: "Editor One".to_owned(),
                role: AdminRole::Editor,
                tenant_scope: Some("platform.acme".to_owned()),
                password: Some("editor-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(!editor.passwordless);
    let denied = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "editor-1".to_owned(),
            password: String::new(),
        })
        .await;
    assert!(matches!(denied, Err(StoreError::Unauthorized(_))));

    // A password given at creation means signing in immediately
    let with_password = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "publisher-1".to_owned(),
                display_name: "Publisher One".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.acme".to_owned()),
                password: Some("initial-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(!with_password.passwordless);
    let session = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "publisher-1".to_owned(),
            password: "initial-secret".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(session.admin.role, AdminRole::Publisher);
    let publisher_token = db
        .store
        .issue_admin_token(&system_admin(), "publisher-1")
        .await
        .unwrap();

    // Changing a display name must not erase the password
    db.store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "publisher-1".to_owned(),
                display_name: "Publisher Renamed".to_owned(),
                role: AdminRole::Publisher,
                tenant_scope: Some("platform.acme".to_owned()),
                password: None,
            },
        )
        .await
        .unwrap();
    db.store
        .login_admin(AdminLoginRequest {
            operator_id: "publisher-1".to_owned(),
            password: "initial-secret".to_owned(),
        })
        .await
        .unwrap();

    // Setting a password on someone's behalf: the old password is void and the old sessions
    // with it
    let reset = db
        .store
        .reset_admin_password(&system_admin(), "editor-1")
        .await
        .unwrap();
    assert_eq!(reset.operator_id, "editor-1");
    let reset_publisher = db
        .store
        .reset_admin_password(&system_admin(), "publisher-1")
        .await
        .unwrap();
    assert!(reset_publisher.sessions_revoked >= 1);
    assert!(db
        .store
        .authenticate_admin_session(&session.session.token)
        .await
        .unwrap()
        .is_none());
    assert!(db
        .store
        .authenticate_admin_token(&publisher_token.token)
        .await
        .unwrap()
        .is_none());
    let stale = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "publisher-1".to_owned(),
            password: "initial-secret".to_owned(),
        })
        .await;
    assert!(matches!(stale, Err(StoreError::Unauthorized(_))));

    // A self-service change: a wrong old password does not change anything
    let wrong = db
        .store
        .change_admin_password(
            "editor-1",
            None,
            ChangeAdminPasswordRequest {
                current_password: "not-the-password".to_owned(),
                new_password: "chosen-by-me".to_owned(),
            },
        )
        .await;
    assert!(matches!(wrong, Err(StoreError::Unauthorized(_))));

    // The session that made the change survives; the others are dropped
    let live = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "editor-1".to_owned(),
            password: reset.password.clone(),
        })
        .await
        .unwrap();
    let elsewhere = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "editor-1".to_owned(),
            password: reset.password.clone(),
        })
        .await
        .unwrap();
    let revoked = db
        .store
        .change_admin_password(
            "editor-1",
            Some(&live.session.token),
            ChangeAdminPasswordRequest {
                current_password: reset.password.clone(),
                new_password: "chosen-by-me".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(revoked, 1);
    assert!(db
        .store
        .authenticate_admin_session(&live.session.token)
        .await
        .unwrap()
        .is_some());
    assert!(db
        .store
        .authenticate_admin_session(&elsewhere.session.token)
        .await
        .unwrap()
        .is_none());
    db.store
        .login_admin(AdminLoginRequest {
            operator_id: "editor-1".to_owned(),
            password: "chosen-by-me".to_owned(),
        })
        .await
        .unwrap();

    // Too short a password is blocked on both paths
    let short = db
        .store
        .change_admin_password(
            "editor-1",
            None,
            ChangeAdminPasswordRequest {
                current_password: "chosen-by-me".to_owned(),
                new_password: "short".to_owned(),
            },
        )
        .await;
    assert!(matches!(short, Err(StoreError::InvalidData(_))));

    // A tenant-admin setting a password outside their scope must be blocked
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.other', 'Platform Other')")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "other-editor".to_owned(),
                display_name: "Other Editor".to_owned(),
                role: AdminRole::Editor,
                tenant_scope: Some("platform.other".to_owned()),
                password: Some("editor-secret".to_owned()),
            },
        )
        .await
        .unwrap();
    let acme_admin = tenant_admin("platform.acme");
    assert!(db
        .store
        .reset_admin_password(&acme_admin, "editor-1")
        .await
        .is_ok());
    let outside = db
        .store
        .reset_admin_password(&acme_admin, "other-editor")
        .await;
    assert!(matches!(outside, Err(StoreError::Forbidden(_))));
}

/// Passwordless login is a named public surface, never a property that can be attached to an
/// arbitrary operator or privileged role.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn only_the_public_readonly_operator_can_log_in_without_a_password() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Platform Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let created = db
        .store
        .create_admin_operator(
            &system_admin(),
            CreateAdminOperatorRequest {
                id: "public".to_owned(),
                display_name: "Public".to_owned(),
                role: AdminRole::Readonly,
                tenant_scope: Some("platform.acme".to_owned()),
                password: None,
            },
        )
        .await
        .unwrap();
    assert!(created.passwordless);

    // A legacy unsafe row is denied at authentication time even before it is rewritten.
    sqlx::query(
        "INSERT INTO admin_operators (id, display_name, role, tenant_scope, password_hash)
         VALUES ('legacy-editor', 'Legacy Editor', 'editor', 'platform.acme', NULL)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let legacy = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "legacy-editor".to_owned(),
            password: String::new(),
        })
        .await;
    assert!(matches!(legacy, Err(StoreError::Unauthorized(_))));

    let session = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "public".to_owned(),
            password: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(session.admin.role, AdminRole::Readonly);

    // Passwordless does not mean accepting anything: a password typed at random still does not
    // get in, or the login form is decorative
    let wrong = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "public".to_owned(),
            password: "whatever".to_owned(),
        })
        .await;
    assert!(matches!(wrong, Err(StoreError::Unauthorized(_))));

    // A passwordless account setting its own password leaves the current one blank — requiring
    // it to supply a current password first would leave such an account unable to ever set
    // one.
    db.store
        .change_admin_password(
            "public",
            None,
            ChangeAdminPasswordRequest {
                current_password: String::new(),
                new_password: "set-by-myself".to_owned(),
            },
        )
        .await
        .unwrap();

    // Once set it is no longer passwordless: an empty password no longer gets in, and the
    // passwordless warning badge in the list should come down
    let listed = db
        .store
        .list_admin_operators(&system_admin())
        .await
        .unwrap();
    let public = listed.iter().find(|o| o.id == "public").unwrap();
    assert!(!public.passwordless);
    let empty_denied = db
        .store
        .login_admin(AdminLoginRequest {
            operator_id: "public".to_owned(),
            password: String::new(),
        })
        .await;
    assert!(matches!(empty_denied, Err(StoreError::Unauthorized(_))));
    db.store
        .login_admin(AdminLoginRequest {
            operator_id: "public".to_owned(),
            password: "set-by-myself".to_owned(),
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_path_constraint_rejects_invalid_paths() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.ac_me', 'Acme Underscore')")
        .execute(db.pool())
        .await
        .unwrap();

    let invalid = sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.%bad', 'Bad')")
        .execute(db.pool())
        .await;
    assert!(
        invalid.is_err(),
        "tenant ids must reject SQL wildcard characters"
    );

    let empty_segment =
        sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform..bad', 'Bad')")
            .execute(db.pool())
            .await;
    assert!(
        empty_segment.is_err(),
        "tenant ids must reject empty path segments"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_scope_sql_escape_does_not_treat_underscore_as_wildcard() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query(
        "INSERT INTO tenants (id, name)
         VALUES
            ('platform.ac_me', 'Acme Underscore'),
            ('platform.ac_me.child', 'Acme Underscore Child'),
            ('platform.acxme', 'Acme X')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    insert_basic_node(db.pool(), "n-scope", "platform.ac_me", "10.66.0.11", 51831).await;
    insert_basic_node(
        db.pool(),
        "n-child",
        "platform.ac_me.child",
        "10.66.0.12",
        51832,
    )
    .await;
    insert_basic_node(db.pool(), "n-wild", "platform.acxme", "10.66.0.13", 51833).await;

    let plan = db
        .store
        .plan_deployment(&publisher("platform.ac_me"), 1)
        .await
        .unwrap();
    let nodes = plan
        .targets
        .iter()
        .map(|target| target.node_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(nodes, vec!["n-child", "n-scope"]);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn materialize_minimal_fixture_and_compile() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.users.len(), 1);
    assert_eq!(snapshot.apps.len(), 1);

    let node = &snapshot.nodes[0];
    assert_eq!(node.id, "n1");
    assert_eq!(node.tenant, "platform.acme");
    assert_eq!(node.dns, Dns::Servers(vec!["1.1.1.1".to_owned()]));

    let user = &snapshot.users[0];
    assert_eq!(user.id, "alice");
    assert_eq!(user.uuid, "2d2304da-f114-4574-8d44-625afdb1db5c");

    let app = &snapshot.apps[0];
    assert_eq!(app.chains[0].id, "c-main");
    assert_eq!(app.chains[0].name, "Main Chain");
    assert_eq!(app.grants.len(), 1);
    assert_eq!(app.grants[0].ingress, "i-main");

    let ingress = &app.ingresses[0];
    assert_eq!(ingress.id, "i-main");
    assert_eq!(ingress.port, 443);
    ingress.wires.reality().expect("REALITY 形状");
    assert_eq!(ingress.identity.short_ids, vec!["8337a0bf".to_owned()]);

    let step = &app.steps[0];
    assert_eq!(step.rules.len(), 1);
    assert_eq!(step.rules[0].dest_match, DestMatch::Any);
    assert_eq!(step.rules[0].action, Action::Egress { send_through: None });

    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);
    let node_plan = output.project_node("n1").unwrap();
    assert_eq!(node_plan.grant_sync.updates.len(), 1);
    assert_eq!(
        node_plan.grant_sync.updates[0].inbound_tag,
        "in:app-main/i-main"
    );
    // Two clients: one real user and one derived credential for end-to-end probing. The latter
    // ships down the same channel as a real user — the only way it can reach the ingress (argued
    // in `physical/node.rs`).
    let clients = &node_plan.grant_sync.updates[0].clients;
    assert_eq!(clients.len(), 2, "{clients:#?}");
    assert_eq!(
        clients
            .iter()
            .filter(|client| brocade_core::model::is_probe_label(&client.label))
            .count(),
        1,
        "探测凭据不多不少一份：{clients:#?}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn egress_dns_is_stored_once_per_machine_without_changing_chain_rules() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-shared-dns', 'app-main', 'platform.acme', 'Shared DNS Chain', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let selector = DestMatch::DomainSuffix(vec!["media.example".to_owned()]);
    let stored_rule = Rule {
        dest_match: selector.clone(),
        action: Action::Egress { send_through: None },
    };
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules)
         VALUES ('c-shared-dns', 'n1', $1)",
    )
    .bind(serde_json::to_value(vec![stored_rule.clone()]).unwrap())
    .execute(db.pool())
    .await
    .unwrap();

    let resolution = EgressDnsResolution {
        address: "192.0.2.53".to_owned(),
        port: 53,
        transport: EgressDnsTransport::Tcp,
        address_strategy: EgressDnsAddressStrategy::UseIpv4v6,
        fallback: EgressDnsFallback::Stop,
    };
    put_step_draft(
        &db,
        "app-main",
        "c-main",
        "n1",
        PutStepRequest {
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: selector.clone(),
                action: Action::Egress { send_through: None },
            }],
            note: None,
        },
    )
    .await;

    let policy_count_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_egress_dns WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        policy_count_before, 0,
        "put_step cannot create or own a machine DNS policy"
    );
    let policy_revision = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::SetNodeEgressDns {
                node_id: "n1".to_owned(),
                selector: selector.clone(),
                resolution: Some(resolution.clone()),
            }],
            None,
        )
        .await
        .unwrap();

    let raw_rules: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT rules FROM steps WHERE node_id = 'n1' ORDER BY chain_id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(
        raw_rules
            .iter()
            .all(|rules| !rules.to_string().contains("resolution")),
        "steps 不应复制机器级 DNS 配置"
    );
    let policy_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_egress_dns WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(policy_count, 1);

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let matching_routes = snapshot.apps[0]
        .steps
        .iter()
        .filter(|step| step.node == "n1")
        .filter(|step| {
            step.rules[0].dest_match == selector
                && matches!(step.rules[0].action, Action::Egress { .. })
        })
        .collect::<Vec<_>>();
    assert_eq!(matching_routes.len(), 2);
    assert!(!serde_json::to_string(&snapshot.apps)
        .unwrap()
        .contains("resolution"));

    let console = db
        .store
        .redacted_snapshot(&system_admin(), None)
        .await
        .unwrap();
    assert_eq!(console.node_egress_dns.len(), 1);
    assert_eq!(console.node_egress_dns[0].node, "n1");
    assert_eq!(console.node_egress_dns[0].selector, selector);
    assert_eq!(console.node_egress_dns[0].resolution, resolution);

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteChain {
                app_id: "app-main".to_owned(),
                chain_id: "c-shared-dns".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    let after_chain_delete = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(after_chain_delete.node_egress_dns.len(), 1);
    assert_eq!(after_chain_delete.node_egress_dns[0].selector, selector);

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::SetNodeEgressDns {
                node_id: "n1".to_owned(),
                selector,
                resolution: None,
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_egress_dns WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    let historical = db
        .store
        .materialize_snapshot(Some(policy_revision.revision_id))
        .await
        .unwrap();
    assert_eq!(historical.node_egress_dns.len(), 1);
    assert_eq!(historical.node_egress_dns[0].resolution, resolution);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn egress_dns_priority_is_persisted_and_reordered_as_one_machine_list() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let first = DestMatch::DomainSuffix(vec!["first.example".to_owned()]);
    let second = DestMatch::Geosite(vec!["second".to_owned()]);
    let resolution = EgressDnsResolution {
        address: "192.0.2.53".to_owned(),
        port: 53,
        transport: EgressDnsTransport::Tcp,
        address_strategy: EgressDnsAddressStrategy::UseIp,
        fallback: EgressDnsFallback::Stop,
    };
    db.store
        .apply_draft(
            &system_admin(),
            vec![
                ModelOp::SetNodeEgressDns {
                    node_id: "n1".to_owned(),
                    selector: first.clone(),
                    resolution: Some(resolution.clone()),
                },
                ModelOp::SetNodeEgressDns {
                    node_id: "n1".to_owned(),
                    selector: second.clone(),
                    resolution: Some(resolution),
                },
            ],
            None,
        )
        .await
        .unwrap();

    let appended = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(appended.node_egress_dns[0].selector, first);
    assert_eq!(appended.node_egress_dns[0].position, 0);
    assert_eq!(appended.node_egress_dns[1].selector, second);
    assert_eq!(appended.node_egress_dns[1].position, 1);

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::ReorderNodeEgressDns {
                node_id: "n1".to_owned(),
                selectors: vec![second.clone(), first.clone()],
            }],
            None,
        )
        .await
        .unwrap();
    let reordered = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(reordered.node_egress_dns[0].selector, second);
    assert_eq!(reordered.node_egress_dns[0].position, 0);
    assert_eq!(reordered.node_egress_dns[1].selector, first);
    assert_eq!(reordered.node_egress_dns[1].position, 1);

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::SetNodeEgressDns {
                node_id: "n1".to_owned(),
                selector: second,
                resolution: None,
            }],
            None,
        )
        .await
        .unwrap();
    let compacted = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(compacted.node_egress_dns.len(), 1);
    assert_eq!(compacted.node_egress_dns[0].selector, first);
    assert_eq!(compacted.node_egress_dns[0].position, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_writes_structure_blobs_and_a_separate_frozen_grants_snapshot() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let result = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-create-1"),
        )
        .await
        .unwrap();

    assert_eq!(result.status, "planned");
    assert!(!result.reused);
    assert_eq!(result.plan.summary.total_targets, 1);
    assert_eq!(result.plan.summary.changed_targets, 1);

    let deployment = sqlx::query(
        "SELECT status, active, actor, warnings
         FROM deployments
         WHERE id = $1",
    )
    .bind(result.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        deployment.try_get::<String, _>("status").unwrap(),
        "planned"
    );
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert_eq!(
        deployment.try_get::<Option<String>, _>("actor").unwrap(),
        Some("tester".to_owned())
    );
    assert_eq!(
        deployment
            .try_get::<serde_json::Value, _>("warnings")
            .unwrap(),
        json!([])
    );

    let target = sqlx::query(
        "SELECT status
         FROM deployment_targets
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(result.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(target.try_get::<String, _>("status").unwrap(), "pending");

    let state = sqlx::query(
        "SELECT wave, disruptive, desired_structure, desired_grants, dispatched_grants,
                usage_generation_id
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(result.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state.try_get::<i32, _>("wave").unwrap(), 1);
    assert!(state.try_get::<bool, _>("disruptive").unwrap());
    let desired = state
        .try_get::<serde_json::Value, _>("desired_structure")
        .unwrap();
    let desired_grants = state
        .try_get::<serde_json::Value, _>("desired_grants")
        .unwrap();
    assert_eq!(
        state
            .try_get::<Option<serde_json::Value>, _>("dispatched_grants")
            .unwrap(),
        None,
        "the in-flight comparison copy exists only after dispatch"
    );
    assert!(
        desired_grants
            .to_string()
            .contains("2d2304da-f114-4574-8d44-625afdb1db5c"),
        "the work order must freeze its creation-time permissions"
    );
    assert_eq!(desired["wireguard"]["state"], "present");
    assert_eq!(desired["xray"]["state"], "present");
    let usage_generation_id: i64 = state.try_get("usage_generation_id").unwrap();
    let bindings: serde_json::Value = sqlx::query_scalar(
        "SELECT bindings FROM usage_generations WHERE id = $1 AND deployment_id = $2",
    )
    .bind(usage_generation_id)
    .bind(result.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(bindings["alice@platform.acme#i-main"]["kind"], "user");
    assert!(desired.get("grants").is_none());
    assert!(desired["actions"]
        .as_array()
        .unwrap()
        .contains(&json!("apply-wire-guard")));
    assert!(desired["actions"]
        .as_array()
        .unwrap()
        .contains(&json!("apply-xray")));
    assert!(desired["actions"]
        .as_array()
        .unwrap()
        .contains(&json!("sync-grants")));
    assert!(
        !desired
            .to_string()
            .contains("2d2304da-f114-4574-8d44-625afdb1db5c"),
        "desired_structure must not persist grant UUIDs"
    );

    let blob_count: i64 = sqlx::query("SELECT count(*) AS n FROM artifact_blobs")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(
        blob_count, 2,
        "only xray and wireguard structure blobs are stored"
    );

    let blob_text: Option<String> = sqlx::query(
        "SELECT string_agg(content, E'\n') AS content
         FROM artifact_blobs",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("content")
    .unwrap();
    assert!(
        !blob_text
            .unwrap_or_default()
            .contains("2d2304da-f114-4574-8d44-625afdb1db5c"),
        "artifact_blobs must not persist grant UUIDs"
    );

    let snapshot_count: i64 = sqlx::query("SELECT count(*) AS n FROM artifact_snapshots")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(
        snapshot_count, 0,
        "the old artifact snapshot table is still not grant history"
    );
}

// Reproducing the remote machine's model: one tenant plus one overlay node, an empty application
// layer, never converged. The node page says there are changes to push while the release plan
// preview is empty, and the two must give one answer.
// An id becomes a slug in the artifacts verbatim, so characters like uppercase must be blocked at
// the write path: reporting label.charset at compile time comes after the machine has entered the
// model, where it cannot be deleted (there is no DELETE /nodes).
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn write_entrypoints_reject_ids_outside_the_slug_charset() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.acme', 'Acme')")
        .execute(db.pool())
        .await
        .unwrap();

    let error = db
        .store
        .provision_node(&system_admin(), provision_node_request("HK-01"))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StoreError::InvalidData(m) if m.contains("HK-01")),
        "大写 id 应该在纳管时就被拒: {error:?}"
    );

    // The same name in lowercase can be created, proving what is blocked is the character set
    // itself
    db.store
        .provision_node(&system_admin(), provision_node_request("hk-01"))
        .await
        .unwrap();

    let error = db
        .store
        .create_user(
            &system_admin(),
            CreateUserRequest {
                tenant_id: "platform.acme".to_owned(),
                id: "Alice".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StoreError::InvalidData(m) if m.contains("Alice")),
        "大写用户 id 也该被拒: {error:?}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn plan_and_verify_agree_for_a_lone_backbone_node_without_app_layer() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform', 'Platform')")
        .execute(db.pool())
        .await
        .unwrap();
    insert_basic_node(db.pool(), "HK-01", "platform", "10.66.0.1", 51820).await;

    let plan = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    let verify = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: None,
                node_id: Some("HK-01".to_owned()),
            },
        )
        .await;

    eprintln!(
        "PLAN  total={} changed={} skipped={} targets={:?}",
        plan.summary.total_targets,
        plan.summary.changed_targets,
        plan.summary.skipped_targets,
        plan.targets
            .iter()
            .map(|t| (t.node_id.as_str(), t.status, t.actions.clone()))
            .collect::<Vec<_>>()
    );
    match &verify {
        Ok(v) => eprintln!(
            "VERIFY converged={} rev={} total={} changed={}",
            v.converged, v.revision_id, v.summary.total_targets, v.summary.changed_targets
        ),
        Err(error) => eprintln!("VERIFY error={error:?}"),
    }

    // Both come from one source and must conclude alike: a machine that is a change target in the
    // plan must be reported as unaligned by verify, and the converse.
    let in_plan = plan
        .targets
        .iter()
        .any(|t| t.node_id == "HK-01" && t.status != PlannedTargetStatus::Skipped);
    match verify {
        Ok(v) => assert_eq!(in_plan, !v.converged, "计划预览与节点页验证结论不一致"),
        Err(_) => assert!(!in_plan, "计划里有这台变更目标，verify 却报错"),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn verify_deployment_summary_is_scoped_to_the_requested_node() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    // Neither has converged, so globally there are two machines to push.
    let global = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: None,
                node_id: None,
            },
        )
        .await
        .unwrap();
    assert!(!global.converged);
    assert_eq!(global.summary.total_targets, 2);
    assert_eq!(global.summary.changed_targets, 2);

    // With a node_id the summary is recomputed for that machine: total and changed both cap at 1,
    // and it describes that machine rather than the whole fleet. A UI presenting it as a
    // fleet-wide count would mislead.
    let single = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: None,
                node_id: Some("n1".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(single.targets.len(), 1);
    assert_eq!(single.targets[0].node_id, "n1");
    assert_eq!(single.summary.total_targets, 1);
    assert_eq!(single.summary.changed_targets, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_scoped_deployment_filters_targets_and_visibility() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    let acme = publisher("platform.acme");
    let other = publisher("platform.other");
    let system_plan = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    assert_eq!(system_plan.summary.total_targets, 2);

    let acme_plan = db.store.plan_deployment(&acme, 1).await.unwrap();
    assert_eq!(acme_plan.summary.total_targets, 1);
    assert_eq!(acme_plan.targets[0].node_id, "n1");

    let other_plan = db.store.plan_deployment(&other, 1).await.unwrap();
    assert_eq!(other_plan.summary.total_targets, 1);
    assert_eq!(other_plan.targets[0].node_id, "n-other");

    // An out-of-scope node being isolated does not turn a tenant-scoped order into a global
    // serving proof. It has no target or obligation tying it to this revision.
    sqlx::query(
        "INSERT INTO node_operational_isolations (node_id, actor, reason)
         VALUES ('n-other', 'test', 'outside the scoped deployment')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let created = db
        .store
        .create_deployment(&acme, create_deployment_request(1, "deploy-acme-scoped"))
        .await
        .unwrap();
    assert_eq!(created.plan.summary.total_targets, 1);
    assert_eq!(created.plan.targets[0].node_id, "n1");

    let target_nodes: Vec<String> = sqlx::query(
        "SELECT node_id
         FROM deployment_targets
         WHERE deployment_id = $1
         ORDER BY node_id",
    )
    .bind(created.deployment_id)
    .fetch_all(db.pool())
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get("node_id").unwrap())
    .collect();
    assert_eq!(target_nodes, vec!["n1".to_owned()]);

    let acme_list = db.store.list_deployments(&acme, 10, None).await.unwrap();
    assert_eq!(acme_list.deployments.len(), 1);
    assert_eq!(acme_list.deployments[0].total_targets, 1);
    assert_eq!(acme_list.deployments[0].changed_targets, 1);

    let other_list = db.store.list_deployments(&other, 10, None).await.unwrap();
    assert!(other_list.deployments.is_empty());

    let acme_detail = db
        .store
        .deployment_detail(&acme, created.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(acme_detail.targets.len(), 1);
    assert_eq!(acme_detail.targets[0].node_id, "n1");

    let other_detail = db
        .store
        .deployment_detail(&other, created.deployment_id, false)
        .await;
    assert!(matches!(other_detail, Err(StoreError::NotFound(_))));

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let result = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    assert_eq!(result.deployment_status, "succeeded");
    let serving_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM subscription_serving_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        serving_rows, 0,
        "a successful tenant-scoped release must not pretend the whole fleet converged"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_scoped_plan_filters_outside_warnings() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    sqlx::query("UPDATE steps SET rules = $1")
        .bind(json!([
            {
                "match": { "t": "any" },
                "action": { "t": "block" }
            }
        ]))
        .execute(db.pool())
        .await
        .unwrap();

    let system_plan = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    assert!(system_plan
        .warnings
        .iter()
        .any(|warning| warning.location == "n1"));
    assert!(system_plan
        .warnings
        .iter()
        .any(|warning| warning.location == "n-other"));

    let acme_plan = db
        .store
        .plan_deployment(&publisher("platform.acme"), 1)
        .await
        .unwrap();
    assert!(acme_plan
        .warnings
        .iter()
        .any(|warning| warning.location == "n1"));
    assert!(!acme_plan
        .warnings
        .iter()
        .any(|warning| warning.location == "n-other"));
}

/// Retirement is a model transition and an operational handoff in one transaction. Historical
/// work remains readable, but its target is canceled and fenced by epoch; only the replacement
/// all-disabled target can be claimed. The token stays alive exactly long enough to report that
/// teardown and is revoked in the same transaction which marks the lifecycle retired.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retirement_fences_old_target_and_converges_through_teardown_deployment() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let existing = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "retirement-overlap-existing"),
        )
        .await
        .unwrap();
    assert!(existing
        .plan
        .targets
        .iter()
        .any(|target| target.node_id == "n1"));
    let token = db.store.issue_node_token("n1").await.unwrap();
    let stale_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("the pre-retirement target is dispatched to characterize a late report");
    assert_eq!(stale_claim.deployment_id, existing.deployment_id);

    let retired = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
                note: Some("characterize retirement overlap".to_owned()),
            },
        )
        .await
        .unwrap();

    let frozen = db
        .store
        .deployment_detail(&system_admin(), existing.deployment_id, false)
        .await
        .unwrap();
    assert!(
        frozen.targets.iter().any(|target| target.node_id == "n1"),
        "immutable history must retain the original target"
    );
    assert_eq!(frozen.status, "canceled");
    assert!(retired
        .canceled_deployment_ids
        .contains(&existing.deployment_id));
    let stale_report = db
        .store
        .report_target_result(applied_report(&stale_claim))
        .await;
    assert!(
        matches!(stale_report, Err(StoreError::Unsupported(_))),
        "a report from the prior lifecycle must be fenced after retirement"
    );
    let teardown_id = retired
        .deployment_id
        .expect("retirement creates a replacement teardown deployment");

    let current = db
        .store
        .plan_deployment(&system_admin(), retired.revision_id)
        .await
        .unwrap();
    let teardown_wave = current
        .targets
        .iter()
        .find(|target| target.node_id == "n1")
        .map(|target| target.wave)
        .expect("retirement target has a wave");
    assert!(
        current.targets.iter().any(|target| {
            target.node_id == "n1"
                && target.status == PlannedTargetStatus::Pending
                && target.actions.contains(&PlannedAction::DisableXray)
                && target.actions.contains(&PlannedAction::DisableWireGuard)
        }),
        "the current plan must carry the retired node until teardown is confirmed"
    );

    let fleet_verify = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: Some(retired.revision_id),
                node_id: None,
            },
        )
        .await
        .unwrap();
    assert!(
        !fleet_verify.converged && fleet_verify.summary.changed_targets > 0,
        "retirement debt must keep fleet verification unconverged"
    );
    let node_verify = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: Some(retired.revision_id),
                node_id: Some("n1".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(!node_verify.converged);
    assert_eq!(
        node_verify.node_lifecycle.unwrap().phase,
        brocade_store::NodeLifecyclePhase::Retiring
    );

    let artifacts = db
        .store
        .artifact_index(&system_admin(), Some(retired.revision_id))
        .await
        .unwrap();
    assert!(
        artifacts
            .artifacts
            .iter()
            .any(|artifact| artifact.target_id == "n1" && artifact.state == "disabled"),
        "artifact preview must retain the pure disabled contract"
    );

    let replacement_token = db.store.issue_node_token("n1").await;
    assert!(matches!(replacement_token, Err(StoreError::Forbidden(_))));
    assert!(matches!(
        db.store.revoke_node_token("n1").await,
        Err(StoreError::Unsupported(_))
    ));
    let authenticated = db
        .store
        .authenticate_node_token(&token.token)
        .await
        .unwrap()
        .expect("the existing token remains valid while teardown is owed");
    assert_eq!(
        authenticated.lifecycle_phase,
        brocade_store::NodeLifecyclePhase::Retiring
    );

    db.store
        .confirm_deployment_wave(&system_admin(), teardown_id, teardown_wave, None)
        .await
        .unwrap();

    let claimed = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("the replacement teardown target must be claimable");
    assert_eq!(claimed.deployment_id, teardown_id);
    assert!(matches!(
        claimed.desired.phantun,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        claimed.desired.hy2_port_hop,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        claimed.desired.wireguard,
        DesiredArtifact::Disabled { .. }
    ));
    assert!(matches!(
        claimed.desired.xray,
        DesiredArtifact::Disabled { .. }
    ));

    db.store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();

    let lifecycle = db.store.node_lifecycle("n1").await.unwrap();
    assert_eq!(lifecycle.phase, brocade_store::NodeLifecyclePhase::Retired);
    assert_eq!(lifecycle.deployment_id, Some(teardown_id));
    assert!(db
        .store
        .authenticate_node_token(&token.token)
        .await
        .unwrap()
        .is_none());

    let converged = db
        .store
        .plan_deployment(&system_admin(), retired.revision_id)
        .await
        .unwrap();
    assert!(converged
        .targets
        .iter()
        .all(|target| target.node_id != "n1"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forced_retirement_is_a_fenced_auditable_terminal_state_and_restore_uses_a_new_epoch() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let old_token = db.store.issue_node_token("n1").await.unwrap();

    let retiring = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
                note: Some("force retirement setup".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(retiring.lifecycle.lifecycle_epoch, 1);
    assert_eq!(
        retiring.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Retiring
    );
    let first_teardown = retiring.deployment_id.expect("retirement has a teardown");
    db.store
        .cancel_deployment(&system_admin(), first_teardown)
        .await
        .unwrap();
    let repaired = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
                note: Some("repair canceled teardown".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(repaired.revision_id, retiring.revision_id);
    assert_eq!(repaired.lifecycle.lifecycle_epoch, 1);
    assert_ne!(repaired.deployment_id, Some(first_teardown));

    let abandoned = db
        .store
        .abandon_node(
            &system_admin(),
            "n1",
            AbandonNodeRequest {
                reason: "machine is permanently unreachable".to_owned(),
                unregister_warp: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(abandoned.lifecycle.lifecycle_epoch, 2);
    assert_eq!(
        abandoned.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Abandoned
    );
    assert!(abandoned.lifecycle.completed_at.is_some());
    assert!(db
        .store
        .authenticate_node_token(&old_token.token)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        db.store.issue_node_token("n1").await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    let terminal_plan = db
        .store
        .plan_deployment(&system_admin(), abandoned.revision_id)
        .await
        .unwrap();
    assert!(
        terminal_plan
            .targets
            .iter()
            .all(|target| target.node_id != "n1"),
        "a force-retired machine must not poison every future deployment"
    );
    let terminal_verify = db
        .store
        .verify_deployment(
            &system_admin(),
            VerifyDeploymentRequest {
                revision_id: Some(abandoned.revision_id),
                node_id: Some("n1".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(!terminal_verify.converged);
    assert_eq!(terminal_verify.summary.total_targets, 1);
    assert_eq!(terminal_verify.summary.changed_targets, 1);

    let events = sqlx::query_as::<_, (i64, String)>(
        "SELECT lifecycle_epoch, event
           FROM node_lifecycle_events
          WHERE node_id = 'n1'
          ORDER BY lifecycle_epoch, id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        events,
        vec![
            (1, "retirement-requested".to_owned()),
            (2, "retirement-abandoned".to_owned()),
        ]
    );

    let restored = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "active".to_owned(),
                note: Some("machine recovered".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(restored.lifecycle.lifecycle_epoch, 3);
    assert_eq!(
        restored.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Active
    );
    assert!(db
        .store
        .authenticate_node_token(&old_token.token)
        .await
        .unwrap()
        .is_none());
    let new_token = db.store.issue_node_token("n1").await.unwrap();
    assert!(db
        .store
        .authenticate_node_token(&new_token.token)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_node_retirement_tears_down_and_reactivation_creates_new_epoch_debt() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let initial = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "isolated-lifecycle-initial"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            initial.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "test lifecycle while isolated".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();

    let retiring = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".to_owned(),
                note: Some("retire isolated node".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), initial.deployment_id, "n1").await,
        "canceled",
        "the old epoch debt must close visibly"
    );
    let teardown_id = retiring
        .deployment_id
        .expect("retirement still creates a teardown while isolated");
    let teardown = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("retiring target bypasses active-node isolation debt delivery");
    assert_eq!(teardown.deployment_id, teardown_id);
    db.store
        .report_target_result(applied_report(&teardown))
        .await
        .unwrap();
    assert_eq!(
        db.store.node_lifecycle("n1").await.unwrap().phase,
        brocade_store::NodeLifecyclePhase::Retired
    );

    let reactivated = db
        .store
        .update_node_status(
            &system_admin(),
            "n1",
            brocade_store::UpdateNodeStatusRequest {
                status: "active".to_owned(),
                note: Some("reactivate but keep operational isolation".to_owned()),
            },
        )
        .await
        .unwrap();
    let reactivation_id = reactivated
        .deployment_id
        .expect("reactivation creates a convergence deployment");
    let detail = db
        .store
        .deployment_detail(&system_admin(), reactivation_id, false)
        .await
        .unwrap();
    assert_eq!(detail.status, "succeeded");
    assert_eq!(detail.activation_status, "activated");
    assert_eq!(detail.settlement_status, "debt");
    assert_eq!(detail.active, None);
    let reactivation = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("reactivated isolated node receives the new lifecycle epoch debt");
    assert_eq!(reactivation.deployment_id, reactivation_id);
    assert!(reactivation.claim_generation > 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retirement_and_an_inflight_agent_report_have_one_serializable_winner_without_deadlock() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let deployment = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "retirement-report-race"),
        )
        .await
        .unwrap();
    let claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("the active target is in flight before retirement races it");
    assert_eq!(claim.deployment_id, deployment.deployment_id);

    let admin = system_admin();
    let (report, retirement) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            db.store.report_target_result(applied_report(&claim)),
            db.store.update_node_status(
                &admin,
                "n1",
                brocade_store::UpdateNodeStatusRequest {
                    status: "retired".to_owned(),
                    note: Some("race an in-flight report".to_owned()),
                },
            )
        )
    })
    .await
    .expect("retirement/report race must not wait on an inverted database lock order");
    let retirement = retirement.expect("retirement is the durable winner of the lifecycle change");
    assert_eq!(
        retirement.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Retiring
    );
    if let Err(error) = report {
        assert!(
            matches!(error, StoreError::Unsupported(_) | StoreError::Conflict(_)),
            "the losing report must be rejected as stale, not fail internally: {error}"
        );
    }
    let lifecycle = db.store.node_lifecycle("n1").await.unwrap();
    assert_eq!(lifecycle.lifecycle_epoch, 1);
    assert_eq!(lifecycle.phase, brocade_store::NodeLifecyclePhase::Retiring);
    assert!(lifecycle.deployment_id.is_some());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_is_idempotent_and_rejects_second_active_publish() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-idempotent"),
        )
        .await
        .unwrap();
    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-idempotent"),
        )
        .await
        .unwrap();

    assert_eq!(second.deployment_id, first.deployment_id);
    assert!(second.reused);
    assert_eq!(deployment_count(db.pool()).await, 1);

    let blocked = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-second-active"),
        )
        .await;
    assert!(
        blocked.is_err(),
        "a second non-idempotent active deployment must be rejected"
    );
    assert_eq!(deployment_count(db.pool()).await, 1);
}

/// A deployment that moves no machine may not be created: with the machines present and not one
/// byte of the artifacts changed, this used to pass. The UI disables the plan preview and the
/// create button by the same rule, but the test lives here.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_rejects_a_plan_where_every_node_is_already_converged() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let first = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "converge-1"))
        .await
        .unwrap();
    // Actually converge the machine to this version: node_applied_state is the plan's baseline,
    // and without advancing it a second plan computes the same pile of targets. This fixture has
    // only n1.
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(desired.deployment_id, first.deployment_id);
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    let again = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "converge-2"))
        .await;
    let message = again.unwrap_err().to_string();
    assert!(
        message.contains("already converged"),
        "一台都不用动的单必须被拒，实际错误：{message}"
    );
    assert_eq!(deployment_count(db.pool()).await, 1);
}

/// The baseline for artifact diffs. It pins three things: a first release has no baseline; a
/// second deployment takes the last successful version; and the two kinds are computed
/// separately — were a grants deployment to displace the configuration line, a configuration
/// deployment's diff would take a version that never landed on disk as its predecessor.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn deployment_base_revision_is_the_last_succeeded_of_the_same_kind() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let pool = db.pool();

    let first = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "base-first"))
        .await
        .unwrap();
    assert_eq!(
        base_of(pool, first.deployment_id).await,
        None,
        "第一单没有可比的上一版"
    );
    mark_succeeded(pool, first.deployment_id).await;

    // A deployment succeeded on the grants line, pushing a newer revision at that. It moves only
    // the runtime list and changes no artifact on disk, so the configuration line's baseline must
    // not follow it.
    //
    // Inserted through SQL directly: really shipping a grants deployment requires the machine to
    // have converged once, and that whole apparatus is unrelated to the semantics pinned here.
    let newer_revision: i64 = sqlx::query(
        "INSERT INTO revisions (author, note) VALUES ('tester', 'grants') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("id")
    .unwrap();
    sqlx::query(
        "INSERT INTO deployments (revision_id, status, kind, idempotency_key)
         VALUES ($1, 'succeeded', 'grants', 'base-grants')",
    )
    .bind(newer_revision)
    .execute(pool)
    .await
    .unwrap();

    let second = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "base-second"))
        .await
        .unwrap();
    assert_eq!(
        base_of(pool, second.deployment_id).await,
        Some(1),
        "基线取同类上一次成功的 revision"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_writes_compile_warnings() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query("UPDATE steps SET rules = $1 WHERE chain_id = 'c-main' AND node_id = 'n1'")
        .bind(json!([
            {
                "match": { "t": "any" },
                "action": { "t": "block" }
            }
        ]))
        .execute(db.pool())
        .await
        .unwrap();

    let result = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-warning"),
        )
        .await
        .unwrap();

    let warnings: serde_json::Value = sqlx::query("SELECT warnings FROM deployments WHERE id = $1")
        .bind(result.deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("warnings")
        .unwrap();
    assert!(warnings
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning["code"] == "node.dns-unused"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_blocks_compile_errors_and_empty_target_sets() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let empty = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-empty"),
        )
        .await;
    assert!(
        empty.is_err(),
        "empty target sets must not create deployments"
    );
    assert_eq!(deployment_count(db.pool()).await, 0);

    insert_minimal_fixture(db.pool()).await;
    // Manufacture a compilation error: point the ingress at a chain that does not exist
    // (ingress.no-chain). The chain_id foreign key blocks writing a reference to a nonexistent
    // chain — what is tested here is the compiler rather than database integrity, so the
    // constraint is dropped first; this test owns its container and affects no other case.
    sqlx::query("ALTER TABLE ingresses DROP CONSTRAINT ingresses_chain_id_fkey")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE ingresses SET chain_id = 'c-gone' WHERE id = 'i-main'")
        .execute(db.pool())
        .await
        .unwrap();

    let blocked = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deploy-compile-error"),
        )
        .await;
    assert!(
        blocked.is_err(),
        "compiler errors must block deployment creation"
    );
    assert_eq!(deployment_count(db.pool()).await, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn create_deployment_requires_the_current_explicit_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let result = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(2, "deploy-wrong-revision"),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(deployment_count(db.pool()).await, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn load_desired_for_node_keeps_the_grants_snapshot_from_deployment_creation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "desired-current"),
        )
        .await
        .unwrap();

    insert_extra_user_grant(db.pool()).await;

    let desired = db
        .store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .expect("n1 should have current wave desired state");

    assert_eq!(desired.deployment_id, created.deployment_id);
    assert_eq!(desired.node_id, "n1");
    assert_eq!(desired.wave, 1);
    assert!(desired.actions.contains(&PlannedAction::ApplyXray));

    let DesiredArtifact::Present {
        content: wg_content,
        sha256: wg_sha,
    } = &desired.desired.wireguard
    else {
        panic!("wireguard should be present");
    };
    assert_eq!(wg_sha.len(), 64);
    assert!(wg_content.contains("[Interface]"));

    let DesiredArtifact::Present {
        content: xray_content,
        sha256: xray_sha,
    } = &desired.desired.xray
    else {
        panic!("xray should be present");
    };
    assert_eq!(xray_sha.len(), 64);
    assert!(xray_content.contains("\"inbounds\""));

    let DesiredGrants::Present { inbounds } = &desired.desired.grants else {
        panic!("grants should be present");
    };
    let clients = inbounds
        .iter()
        .flat_map(|inbound| inbound.clients.iter())
        .map(|client| (client.email.as_str(), client.uuid.as_str()))
        .collect::<Vec<_>>();
    assert!(clients.contains(&(
        "alice@platform.acme#i-main",
        "2d2304da-f114-4574-8d44-625afdb1db5c",
    )));
    assert!(
        !clients
            .iter()
            .any(|(email, _)| *email == "bob@platform.acme#i-main"),
        "a permission added after work-order creation must not be folded into that order"
    );
}

// Permission edits are operational: one click commits the revision and a durable outbox row in
// the same transaction.  The worker creates a grants-only deployment; no second human publish is
// involved.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn permission_change_is_automatically_released_from_the_durable_queue() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let base_deployment = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "grants-auto-base"),
        )
        .await
        .unwrap();
    for wave in 1..=base_deployment.plan.summary.max_wave {
        if wave > 1 {
            db.store
                .confirm_deployment_wave(
                    &system_admin(),
                    base_deployment.deployment_id,
                    wave,
                    Some("tester".to_owned()),
                )
                .await
                .unwrap();
        }
        let nodes = base_deployment
            .plan
            .targets
            .iter()
            .filter(|target| target.wave == wave)
            .map(|target| target.node_id.clone())
            .collect::<Vec<_>>();
        for node_id in nodes {
            let desired = db
                .store
                .claim_desired_for_node(&node_id)
                .await
                .unwrap()
                .expect("the current base wave should be claimable");
            db.store
                .report_target_result(applied_report(&desired))
                .await
                .unwrap();
        }
    }

    let changed = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: Some("撤掉 Alice".to_owned()),
            },
        )
        .await
        .unwrap();

    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
         WHERE kind = 'grants-deployment' AND status = 'queued'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        queued, 1,
        "permission revision and outbox row must commit together"
    );
    let manual_plan = db
        .store
        .plan_deployment(&system_admin(), changed.revision_id)
        .await
        .unwrap();
    assert_eq!(
        manual_plan.summary.changed_targets, 0,
        "permission-only work must not light up the console's manual config release"
    );

    let outcome = db.store.process_grant_automation().await.unwrap();
    assert_eq!(outcome.revision_id, Some(changed.revision_id));
    assert_eq!(outcome.merged_jobs, 1);
    let deployment_id = outcome
        .deployment_id
        .expect("worker should create a grants order");

    let targets: Vec<String> = sqlx::query_scalar(
        "SELECT node_id FROM deployment_targets WHERE deployment_id = $1 ORDER BY node_id",
    )
    .bind(deployment_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        targets,
        vec!["n1"],
        "the automatic order should contain only the machine whose grants changed"
    );

    let row = sqlx::query("SELECT kind, actor, revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.try_get::<String, _>("kind").unwrap(), "grants");
    assert_eq!(row.try_get::<String, _>("actor").unwrap(), "system:grants");
    assert_eq!(
        row.try_get::<i64, _>("revision_id").unwrap() as u64,
        changed.revision_id
    );

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("automatic grants order should be claimable");
    assert_eq!(desired.deployment_id, deployment_id);
    let DesiredGrants::Present { ref inbounds } = desired.desired.grants else {
        panic!("grants should be present");
    };
    assert!(
        inbounds
            .iter()
            .flat_map(|inbound| &inbound.clients)
            .all(|client| client.email != "alice@platform.acme#i-main"),
        "the automatic order must contain the revoked state (probe identities may remain)"
    );
    assert!(
        db.store
            .claim_desired_for_node("n-other")
            .await
            .unwrap()
            .is_none(),
        "an already-matching machine must not receive a synthetic grants target"
    );
    let result = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    assert_eq!(result.deployment_status, "succeeded");

    let serving_permissions: (i64, Option<i64>) = sqlx::query_as(
        "SELECT permissions_revision_id, permissions_deployment_id
           FROM subscription_serving_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        serving_permissions,
        (
            i64::try_from(changed.revision_id).unwrap(),
            Some(deployment_id),
        ),
        "a settled automatic grants order advances globally even though unchanged machines have no target rows"
    );
    db.store
        .clash_subscription_by_uuid("f98b74ba-58f1-41d0-aaad-8fa5724c6d2d")
        .await
        .expect("the stable subscription must reopen after the changed target converges");
}

// A grants deployment is projected onto the exact configuration a machine is running. That may
// be an old stored snapshot, not the current tables. Development snapshots written while XHTTP
// called its client dialer field `mux` used to fail before a deployment row could be created, so
// the durable job retried invisibly forever. This reproduces that production path rather than
// merely deserializing an isolated JSON fixture.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn automatic_grants_fold_legacy_xhttp_mux_in_the_running_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "UPDATE ingresses
         SET transport_kind = 'vless-reality-xhttp',
             reality_flow = '',
             xhttp_path = '/legacy',
             xhttp_mode = NULL
         WHERE id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ingress_client_settings
            SET xhttp_host = NULL, xhttp_xmux = NULL
          WHERE ingress_id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "legacy-xhttp-running-base"),
        )
        .await
        .unwrap();
    let base = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base))
        .await
        .unwrap();

    // Mutate only the historical serialized shape. The live table remains a valid current
    // revision, exactly as in an upgraded control plane holding older model_snapshots rows.
    let mut legacy: serde_json::Value =
        sqlx::query_scalar("SELECT snapshot FROM model_snapshots WHERE revision_id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let legacy_xhttp = legacy["apps"][0]["ingresses"][0]["wires"]["vless"]["xhttp"]
        .as_object_mut()
        .expect("fixture has one XHTTP wire");
    legacy_xhttp.insert("mux".to_owned(), serde_json::Value::Null);
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(1_i64)
        .bind(legacy)
        .execute(db.pool())
        .await
        .unwrap();

    let changed = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: Some("legacy XHTTP snapshot regression".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(changed.revision_id > 1);

    // Once revision 1 is historical, reading it must remove the old key while preserving the
    // XHTTP hierarchy. This is the same read performed by create_automatic_grants_deployment.
    let running = db.store.materialize_snapshot(Some(1)).await.unwrap();
    let xhttp = running.apps[0].ingresses[0]
        .wires
        .xhttp()
        .expect("the legacy transport remains XHTTP");
    assert_eq!(xhttp.path, "/legacy");
    assert_eq!(xhttp.xmux, None);

    // Model a worker that has already failed a few rounds. This checks the status endpoint's data
    // source as well as recovery: there is still no deployment row at this point.
    sqlx::query(
        "UPDATE jobs
         SET attempts = 3,
             last_error = 'legacy snapshot could not be decoded',
             updated_at = now()
         WHERE kind = 'grants-deployment' AND status = 'queued'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let waiting = db.store.grant_automation_status().await.unwrap();
    assert_eq!(waiting.pending_jobs, 1);
    assert_eq!(waiting.retrying_jobs, 1);
    assert_eq!(waiting.max_attempts, 3);
    assert_eq!(waiting.latest_revision_id, Some(changed.revision_id));
    assert!(waiting.oldest_pending_at.is_some());
    assert!(waiting.last_attempt_at.is_some());
    assert_eq!(
        waiting.last_error.as_deref(),
        Some("legacy snapshot could not be decoded")
    );

    let outcome = db.store.process_grant_automation().await.unwrap();
    assert_eq!(outcome.revision_id, Some(changed.revision_id));
    assert!(outcome.waiting.is_none(), "{outcome:?}");
    assert!(
        outcome.deployment_id.is_some(),
        "the old running topology must no longer block the permission order"
    );

    let recovered = db.store.grant_automation_status().await.unwrap();
    assert_eq!(recovered.pending_jobs, 0);
    assert_eq!(recovered.retrying_jobs, 0);
    assert_eq!(recovered.last_error, None);
}

// Flow is represented on grant accounts but owned by topology. A global Flow edit may still wake
// the generic grants outbox, but it must produce no permission-only deployment; the configuration
// plan is the only path allowed to apply it and must restart Xray before restoring grants.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_indirect_flow_change_requires_a_disruptive_config_deployment() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "UPDATE ingresses
         SET reality_dest = NULL,
             reality_server_names = '[]'::jsonb,
             reality_flow = NULL,
             reality_fallback_mode = 'global-site'
         WHERE id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ingress_client_settings
            SET reality_fingerprint = NULL
          WHERE ingress_id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE control_state
            SET reality_dest = 'www.example.com:443',
                reality_server_names = '[\"www.example.com\"]'::jsonb,
                reality_fingerprint = 'chrome',
                reality_flow = 'xtls-rprx-vision'
          WHERE id = TRUE",
    )
    .execute(db.pool())
    .await
    .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "indirect-grants-base"),
        )
        .await
        .unwrap();
    let base = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base))
        .await
        .unwrap();

    let mut settings = db.store.settings().await.unwrap();
    settings.reality_site.flow = None;
    let changed = db
        .store
        .update_settings(&system_admin(), settings)
        .await
        .unwrap();

    let queued_revision: i64 = sqlx::query_scalar(
        "SELECT (payload->>'revision_id')::bigint
         FROM jobs
         WHERE kind = 'grants-deployment' AND status = 'queued'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(queued_revision as u64, changed.revision_id);

    let config_plan = db
        .store
        .plan_deployment(&system_admin(), changed.revision_id)
        .await
        .unwrap();
    let target = config_plan
        .targets
        .iter()
        .find(|target| target.node_id == "n1")
        .expect("the Flow change must target its Xray node");
    assert_eq!(
        target.actions,
        vec![PlannedAction::ApplyXray, PlannedAction::SyncGrants]
    );
    assert!(target.disruptive);

    let outcome = db.store.process_grant_automation().await.unwrap();
    assert_eq!(outcome.revision_id, Some(changed.revision_id));
    assert!(outcome.waiting.is_none());
    assert_eq!(
        outcome.deployment_id, None,
        "Flow must never be hot-synchronized through a grants deployment"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn queued_permission_edits_coalesce_before_but_not_after_work_order_creation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "grants-merge-base"),
        )
        .await
        .unwrap();
    let base = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base))
        .await
        .unwrap();

    db.store
        .rotate_user_uuid(&system_admin(), "platform.acme", "alice")
        .await
        .unwrap();
    let latest = db
        .store
        .update_user_status(
            &system_admin(),
            "platform.acme",
            "alice",
            UpdateUserStatusRequest {
                status: "disabled".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();

    let outcome = db.store.process_grant_automation().await.unwrap();
    assert_eq!(outcome.merged_jobs, 2);
    assert_eq!(outcome.revision_id, Some(latest.revision_id));
    let deployment_id = outcome
        .deployment_id
        .expect("one merged order should be created");
    let job_rows = sqlx::query(
        "SELECT status, payload->>'deployment_id' AS deployment_id
         FROM jobs WHERE kind = 'grants-deployment' ORDER BY id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(job_rows.len(), 2);
    for row in job_rows {
        assert_eq!(row.try_get::<String, _>("status").unwrap(), "succeeded");
        assert_eq!(
            row.try_get::<String, _>("deployment_id")
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            deployment_id
        );
    }

    // A later edit cannot change the deployment row or its stored grants snapshot; it creates the
    // next queued unit of work instead.
    let frozen_before: serde_json::Value = sqlx::query_scalar(
        "SELECT desired_grants FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.store
        .update_user_status(
            &system_admin(),
            "platform.acme",
            "alice",
            UpdateUserStatusRequest {
                status: "active".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let frozen_after: serde_json::Value = sqlx::query_scalar(
        "SELECT desired_grants FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen_after, frozen_before);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
         WHERE kind = 'grants-deployment' AND status = 'queued'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(queued, 1, "post-creation edits belong to a new job");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn discarding_an_unreleased_permission_revision_cancels_its_automatic_job() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "discard-grants-base"),
        )
        .await
        .unwrap();
    let base = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base))
        .await
        .unwrap();

    let revoked = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(live_grants(db.pool()).await, 0);

    db.store
        .discard_pending_changes(&system_admin(), revoked.revision_id)
        .await
        .unwrap();
    assert_eq!(live_grants(db.pool()).await, 1);
    let status: String = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE kind = 'grants-deployment' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "canceled");
    let worker = db.store.process_grant_automation().await.unwrap();
    assert_eq!(worker.merged_jobs, 0);
    assert!(worker.deployment_id.is_none());
}

// With no running xray there is nowhere to hot-sync. The permission still must not wait for a
// second release: it is folded into the unclaimed first configuration target, which installs the
// inbound and the newest list together.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn automatic_grants_rebase_an_unclaimed_first_config_order() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "config-before-permission"),
        )
        .await
        .unwrap();
    let changed = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();

    let folded = db.store.process_grant_automation().await.unwrap();
    assert_eq!(folded.revision_id, Some(changed.revision_id));
    assert!(folded.deferred.is_empty());
    assert!(folded.waiting.is_none());
    assert!(
        folded.deployment_id.is_none(),
        "xray 还不存在，权限应折进首个配置单而不是造一张无法执行的热更新单"
    );

    let config_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("首个配置单应该直接可取");
    assert_eq!(config_desired.deployment_id, config.deployment_id);
    let DesiredGrants::Present { inbounds } = &config_desired.desired.grants else {
        panic!("配置单应该带上折入的最新权限");
    };
    assert!(
        inbounds
            .iter()
            .flat_map(|inbound| &inbound.clients)
            .all(|client| client.email != "alice@platform.acme#i-main"),
        "尚未下发的配置单仍带着撤销前的 Alice"
    );
}

// Even a queued configuration that replaces the inbound protocol is not a reason to delay a hot
// permission update: it has not reached the agent and can be ordered safely. The current VLESS
// listener receives a grants order first; the queued Hysteria configuration's separate grants
// snapshot is rebased so its later restart cannot restore the old list.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn pending_config_gets_latest_grants_while_permission_is_released_immediately() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let base = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "grants-pending-config-base"),
        )
        .await
        .unwrap();
    let base_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(base_desired.deployment_id, base.deployment_id);
    db.store
        .report_target_result(applied_report(&base_desired))
        .await
        .unwrap();

    // Hysteria terminates TLS on the node itself. Give the fixture a ready certificate so the
    // protocol replacement below is a publishable future topology.
    sqlx::query(
        "INSERT INTO cert_domains (id, domain, acme_directory)
         VALUES ('pending-config-domain', 'pending.example.net', 'https://acme.test/directory')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    // A group with one serving certificate, and n1 drawing from it. Written directly because this
    // test is about deployment, not about how a certificate gets issued.
    sqlx::query(
        "INSERT INTO cert_labels (id, domain_id, label, name)
         VALUES ('pending-config-group', 'pending-config-domain', 'a1b2c3d4', '默认')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO certificates (id, label_id, cert_pem, key_pem_sealed, issued_at, expires_at, status)
         VALUES ('pending-config-cert', 'pending-config-group', 'cert', 'sealed',
                 now(), now() + interval '30 days', 'serving')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_cert_label (node_id, label_id)
         VALUES ('n1', 'pending-config-group')
         ON CONFLICT (node_id) DO UPDATE SET label_id = EXCLUDED.label_id",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let config_revision = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            CreateIngressRequest {
                id: "i-main".to_owned(),
                chain_id: "c-main".to_owned(),
                node_id: "n1".to_owned(),
                bind: "0.0.0.0".parse().unwrap(),
                port: 443,
                front_id: None,
                guard: brocade_core::model::IngressGuard::OPEN,
                reality: CreateRealityIngressRequest {
                    fallback_mode: Some(RealityFallbackMode::CustomSite),
                    fallback_limits: Some(RealityFallbackLimits::Balanced),
                    fallback_guard: Some(true),
                    dest: Some("www.example.com:443".to_owned()),
                    server_names: vec!["www.example.com".to_owned()],
                    fingerprint: Some("chrome".to_owned()),
                    flow: Some("xtls-rprx-vision".to_owned()),
                },
                wires: WiresRequest {
                    vless: None,
                    anytls: None,
                    hysteria2: Some(Hysteria2 {
                        port: 50000,
                        ..Default::default()
                    }),
                },
                projection: Projection::default(),
                note: Some("replace VLESS with Hysteria before permission".to_owned()),
            },
        )
        .await
        .unwrap()
        .revision_id;
    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(config_revision, "config-before-hot-grants"),
        )
        .await
        .unwrap();

    let revoked = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();

    let released = db.store.process_grant_automation().await.unwrap();
    assert!(released.waiting.is_none());
    assert!(released.deferred.is_empty());
    let grants_id = released
        .deployment_id
        .expect("pending config must not block immediate permissions");

    let rebased = sqlx::query(
        "SELECT desired_grants,
                desired_structure->>'grants_revision' AS grants_revision,
                usage_generation_id
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(config.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let rebased_grants: DesiredGrants =
        serde_json::from_value(rebased.try_get("desired_grants").unwrap()).unwrap();
    assert_no_alice(&rebased_grants);
    assert_eq!(
        rebased.try_get::<String, _>("grants_revision").unwrap(),
        revoked.revision_id.to_string(),
        "未来配置单没有记住权限快照来自哪一版"
    );
    let usage_bindings: serde_json::Value =
        sqlx::query_scalar("SELECT bindings FROM usage_generations WHERE id = $1")
            .bind(rebased.try_get::<i64, _>("usage_generation_id").unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        usage_bindings.get("alice@platform.acme#i-main").is_none(),
        "未领取配置单重基权限时也必须重基尚未激活的计费 generation"
    );

    let immediate = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("权限单应该抢在尚未下发的配置单前面");
    assert_eq!(immediate.deployment_id, grants_id);
    assert_no_alice(&immediate.desired.grants);
    let DesiredGrants::Present { inbounds } = &immediate.desired.grants else {
        unreachable!();
    };
    assert_eq!(
        inbounds
            .iter()
            .map(|inbound| inbound.tag.as_str())
            .collect::<Vec<_>>(),
        vec!["in:app-main/i-main"],
        "立即权限单必须针对机器当前还在跑的 VLESS 入站"
    );
    db.store
        .report_target_result(applied_report(&immediate))
        .await
        .unwrap();

    let future = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("权限完成后配置单继续收敛");
    assert_eq!(future.deployment_id, config.deployment_id);
    assert_no_alice(&future.desired.grants);
    let DesiredGrants::Present { inbounds } = &future.desired.grants else {
        unreachable!();
    };
    assert_eq!(
        inbounds
            .iter()
            .map(|inbound| inbound.tag.as_str())
            .collect::<Vec<_>>(),
        vec!["in:app-main/i-main:hy2"],
        "未来配置单必须带它自己 Hysteria 入站对应的最新权限"
    );
}

// Once xray-changing work has been handed to the agent, rewriting its frozen grants would make
// the report judge a state different from what was dispatched. That is a real conflict: wait for
// the report, then immediately compensate with the newest permissions.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn dispatched_xray_config_is_a_permission_conflict_until_it_reports() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "grants-conflict-base"),
        )
        .await
        .unwrap();
    let base = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base))
        .await
        .unwrap();

    let config_revision = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    conn_idle_secs: Some(902),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .revision_id;
    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(config_revision, "dispatched-config-before-grants"),
        )
        .await
        .unwrap();
    let in_flight = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("配置先进入执行态");
    assert_eq!(in_flight.deployment_id, config.deployment_id);

    let revoked = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();
    let blocked = db.store.process_grant_automation().await.unwrap();
    assert_eq!(blocked.deferred, vec!["n1"]);
    assert!(blocked.waiting.as_deref().unwrap().contains("冲突"));
    assert!(blocked.deployment_id.is_none());

    db.store
        .report_target_result(applied_report(&in_flight))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE jobs SET run_after = now()
         WHERE kind = 'grants-deployment' AND status = 'queued'",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let compensated = db.store.process_grant_automation().await.unwrap();
    assert_eq!(compensated.revision_id, Some(revoked.revision_id));
    assert!(compensated.waiting.is_none());
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("配置落地后立刻补最新权限");
    assert_eq!(desired.deployment_id, compensated.deployment_id.unwrap());
    assert_no_alice(&desired.desired.grants);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn load_desired_for_node_gates_later_waves_until_current_wave_finishes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "desired-waves"),
        )
        .await
        .unwrap();

    let n1 = db.store.load_desired_for_node("n1").await.unwrap();
    let n2 = db.store.load_desired_for_node("n2").await.unwrap();
    assert!(n1.is_some(), "wave 1 target should be visible");
    assert!(n2.is_none(), "wave 2 target must not be visible yet");

    sqlx::query(
        "UPDATE deployment_targets
         SET status = 'succeeded'
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .execute(db.pool())
    .await
    .unwrap();

    let n2 = db.store.load_desired_for_node("n2").await.unwrap();
    assert!(
        n2.is_none(),
        "destructive wave 2 must wait for manual confirmation"
    );

    let confirmed = db
        .store
        .confirm_deployment_wave(
            &system_admin(),
            created.deployment_id,
            2,
            Some("tester".to_owned()),
        )
        .await
        .unwrap();
    assert!(confirmed.confirmed);
    assert!(!confirmed.reused);

    let n2 = db
        .store
        .load_desired_for_node("n2")
        .await
        .unwrap()
        .expect("wave 2 target should become visible after confirmation");
    assert_eq!(n2.wave, 2);
}

/// A release records a digest per artifact and the machine fetches the content by that digest.
/// Every digest recorded must therefore have content stored beside it.
///
/// The one that got away was port hopping: its digest went into `desired_structure` while its
/// content went nowhere, so `/agent/v1/desired` errored on every poll. Nothing reported it — the
/// target simply stayed `pending` while the machine asked every fifteen seconds and the console
/// showed a confirmed wave doing nothing. Asserting over the artifacts the release itself names,
/// rather than over a list written here, is the point: a fifth artifact is covered on arrival.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn every_artifact_a_release_names_has_its_content_stored() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    enable_hy2_port_hopping(db.pool()).await;

    let created = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "blobs"))
        .await
        .unwrap();

    let rows = sqlx::query(
        "SELECT node_id, desired_structure
         FROM deployment_target_state
         WHERE deployment_id = $1",
    )
    .bind(created.deployment_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert!(!rows.is_empty(), "这一单一台机器都没有，测不到东西");

    let mut checked = 0;
    for row in &rows {
        let node_id: String = row.try_get("node_id").unwrap();
        let structure: serde_json::Value = row.try_get("desired_structure").unwrap();
        let object = structure.as_object().expect("desired_structure 是对象");
        for (field, metadata) in object {
            if field == "actions" || metadata["state"] != "present" {
                continue;
            }
            let sha256 = metadata["sha256"]
                .as_str()
                .expect("present 的产物有 sha256");
            let stored: i64 =
                sqlx::query_scalar("SELECT count(*) FROM artifact_blobs WHERE sha256 = $1")
                    .bind(sha256)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(
                stored, 1,
                "{node_id} 的 {field} 记了 sha {sha256}，但 artifact_blobs 里没有这份内容"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "至少该查到 wireguard、xray、端口跳转三份，实际只有 {checked} 份"
    );

    let hop_state: Option<String> = sqlx::query_scalar(
        "SELECT desired_structure->'hy2_port_hop'->>'state'
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(hop_state.as_deref(), Some("present"));
}

/// Gives `n1` a certificate and its ingress a Hysteria 2 half hopping over a range, which is what
/// makes the machine's port-hop artifact `Present`.
async fn enable_hy2_port_hopping(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO cert_domains (id, domain, acme_directory)
         VALUES ('d1', 'example.net', 'https://acme.test/directory')",
    )
    .execute(pool)
    .await
    .unwrap();
    // A group with one serving certificate, and n1 drawing from it. Written directly because this
    // test is about which artifacts a release names, not about how a certificate gets issued.
    sqlx::query(
        "INSERT INTO cert_labels (id, domain_id, label, name)
         VALUES ('g1', 'd1', 'a1b2c3d4', '默认')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO certificates (id, label_id, cert_pem, key_pem_sealed, issued_at, expires_at, status)
         VALUES ('cert1', 'g1', 'cert', 'sealed', now(), now() + interval '30 days', 'serving')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_cert_label (node_id, label_id) VALUES ('n1', 'g1')
         ON CONFLICT (node_id) DO UPDATE SET label_id = EXCLUDED.label_id",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ingresses
            SET hy2_enabled = TRUE, hy2_port = 50001, hy2_hop_start = 50001, hy2_hop_end = 50010
          WHERE id = 'i-main'",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn load_desired_for_node_does_not_return_skipped_targets() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let preview = db.store.plan_deployment(&system_admin(), 1).await.unwrap();
    insert_matching_applied_state(db.pool(), "n1", find_target(&preview, "n1")).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "desired-skipped"),
        )
        .await
        .unwrap();
    let n1_status: String = sqlx::query(
        "SELECT status
         FROM deployment_targets
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("status")
    .unwrap();
    assert_eq!(n1_status, "skipped");

    let n1 = db.store.load_desired_for_node("n1").await.unwrap();
    assert!(
        n1.is_none(),
        "skipped target must not receive desired state"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn claim_desired_marks_target_dispatched_and_deployment_running() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "claim-desired"),
        )
        .await
        .unwrap();
    db.store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .expect("pure load should return desired");
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n1").await,
        "pending"
    );

    let claimed = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("claim should return desired");
    assert_eq!(claimed.deployment_id, created.deployment_id);

    let target = sqlx::query(
        "SELECT dt.status,
                dts.dispatched_at IS NOT NULL AS dispatched,
                dts.dispatched_grants
         FROM deployment_targets dt
         JOIN deployment_target_state dts
           ON dts.deployment_id = dt.deployment_id
          AND dts.node_id = dt.node_id
         WHERE dt.deployment_id = $1 AND dt.node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(target.try_get::<String, _>("status").unwrap(), "dispatched");
    assert!(target.try_get::<bool, _>("dispatched").unwrap());
    let dispatched_grants: serde_json::Value = target.try_get("dispatched_grants").unwrap();
    assert!(dispatched_grants
        .to_string()
        .contains("2d2304da-f114-4574-8d44-625afdb1db5c"));

    let deployment = sqlx::query(
        "SELECT status, active, started_at IS NOT NULL AS started
         FROM deployments
         WHERE id = $1",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        deployment.try_get::<String, _>("status").unwrap(),
        "running"
    );
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert!(deployment.try_get::<bool, _>("started").unwrap());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn claim_desired_finishes_with_a_single_connection_pool() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "single-connection-claim"),
        )
        .await
        .unwrap();

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(1))
        .connect(&db.url)
        .await
        .unwrap();
    let single_connection_store = PgStore::from_pool(pool);
    let claimed = tokio::time::timeout(
        Duration::from_secs(2),
        single_connection_store.claim_desired_for_node("n1"),
    )
    .await
    .expect("claim tried to acquire a second pool connection")
    .unwrap()
    .expect("the pending target should be claimable");

    assert_eq!(claimed.node_id, "n1");
}

// The agent's retry spool routes on 4xx versus 5xx: a 4xx means the control plane says outright
// this is useless, so it is discarded and the queue advances; a 5xx is kept for retry. So "the
// deployment or target is gone" must be a 4xx — filed as 5xx, an observation that can never land
// sits at the head of the queue and blocks everything behind it.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_result_says_not_found_for_a_deployment_that_is_gone() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "gone"))
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();

    sqlx::query("DELETE FROM deployments WHERE id = $1")
        .bind(created.deployment_id)
        .execute(db.pool())
        .await
        .unwrap();

    let error = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .expect_err("发布单没了，这条观测送不进去");
    assert!(
        matches!(error, StoreError::NotFound(_)),
        "该是 NotFound（映射成 404），拿到的是 {error:?}"
    );

    // A target row gone on its own is the same — the deployment survives while this machine is no
    // longer on its list.
    let created = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "gone-target"))
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("DELETE FROM deployment_targets WHERE deployment_id = $1 AND node_id = 'n1'")
        .bind(created.deployment_id)
        .execute(db.pool())
        .await
        .unwrap();

    let error = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .expect_err("目标行没了，这条观测同样送不进去");
    assert!(
        matches!(error, StoreError::NotFound(_)),
        "该是 NotFound（映射成 404），拿到的是 {error:?}"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn claim_desired_uses_dispatch_lease_before_reclaiming_stale_target() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "claim-lease"))
        .await
        .unwrap();

    let first = db.store.claim_desired_for_node("n1").await.unwrap();
    assert!(first.is_some(), "first claim should dispatch the target");
    let duplicate = db.store.claim_desired_for_node("n1").await.unwrap();
    assert!(
        duplicate.is_none(),
        "an unexpired dispatch lease must not be handed out twice"
    );

    sqlx::query(
        "UPDATE deployment_target_state
         SET dispatched_at = now() - interval '16 minutes'
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .execute(db.pool())
    .await
    .unwrap();

    let reclaimed = db.store.claim_desired_for_node("n1").await.unwrap();
    assert!(
        reclaimed.is_some(),
        "an expired dispatch lease should be claimable again"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_success_updates_applied_state_and_completes_deployment() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "report-success"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let usage_generation_id = desired
        .usage_generation_id
        .expect("xray-changing target carries a frozen usage generation");

    let result = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    assert_eq!(result.target_status, "succeeded");
    assert_eq!(result.deployment_status, "succeeded");
    let activated: (Option<i64>, i64) = sqlx::query_as(
        "SELECT s.usage_generation_id,
                (SELECT count(*) FROM usage_generation_activations
                 WHERE node_id = 'n1' AND generation_id = $1)
         FROM node_agent_state s WHERE s.node_id = 'n1'",
    )
    .bind(usage_generation_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(activated, (Some(usage_generation_id), 1));

    let deployment = sqlx::query(
        "SELECT status, active, finished_at IS NOT NULL AS finished
         FROM deployments
         WHERE id = $1",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        deployment.try_get::<String, _>("status").unwrap(),
        "succeeded"
    );
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        None
    );
    assert!(deployment.try_get::<bool, _>("finished").unwrap());

    let serving: (i64, i64, i64, i64, Option<i64>, Option<i64>, i64) = sqlx::query_as(
        "SELECT topology_revision_id, permissions_revision_id,
                client_snapshot_id,
                (SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE),
                topology_deployment_id, permissions_deployment_id, generation
           FROM subscription_serving_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        serving.2 > 0,
        "the first serving row must have a client snapshot"
    );
    assert_eq!(
        serving,
        (
            1,
            1,
            serving.2,
            serving.2,
            Some(created.deployment_id),
            Some(created.deployment_id),
            1,
        ),
        "the final successful report must atomically establish the first serving checkpoint"
    );
    let subscription = db
        .store
        .clash_subscription_by_uuid("2d2304da-f114-4574-8d44-625afdb1db5c")
        .await
        .unwrap();
    assert!(subscription.content.contains("name: \"Main Chain\""));

    let cleared_grants: Option<serde_json::Value> = sqlx::query(
        "SELECT dispatched_grants
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("dispatched_grants")
    .unwrap();
    assert_eq!(
        cleared_grants, None,
        "in-flight grants must be cleared after target report"
    );

    let applied = sqlx::query(
        "SELECT wireguard_state, wireguard_sha256, xray_state, xray_sha256,
                grants_state, source_deployment_id
         FROM node_applied_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        applied.try_get::<String, _>("wireguard_state").unwrap(),
        "present"
    );
    assert_eq!(
        applied.try_get::<String, _>("wireguard_sha256").unwrap(),
        artifact_sha(&desired.desired.wireguard)
    );
    assert_eq!(
        applied.try_get::<String, _>("xray_state").unwrap(),
        "present"
    );
    assert_eq!(
        applied.try_get::<String, _>("xray_sha256").unwrap(),
        artifact_sha(&desired.desired.xray)
    );
    assert_eq!(
        applied.try_get::<String, _>("grants_state").unwrap(),
        "present"
    );
    assert_eq!(
        applied
            .try_get::<Option<i64>, _>("source_deployment_id")
            .unwrap(),
        Some(created.deployment_id)
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn deployment_detail_returns_raw_observation_and_optional_known_content() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deployment-detail"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    let detail = db
        .store
        .deployment_detail(&system_admin(), created.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(detail.id, created.deployment_id);
    assert_eq!(detail.status, "succeeded");
    assert_eq!(detail.targets.len(), 1);
    let target = &detail.targets[0];
    assert_eq!(target.node_id, "n1");
    assert_eq!(target.status, "succeeded");
    assert_eq!(
        target.verdict.as_ref().unwrap()["desired_matched"],
        json!(true)
    );
    assert_eq!(
        target.observed_before.as_ref().unwrap()["xray"]["state"],
        json!("unknown")
    );
    assert_eq!(
        target.observed_after.as_ref().unwrap()["xray"]["sha256"],
        json!(artifact_sha(&desired.desired.xray))
    );
    assert!(
        target.desired_structure["xray"].get("content").is_none(),
        "default detail must not include secret artifact content"
    );
    assert!(
        target.observed_after.as_ref().unwrap()["xray"]
            .get("content")
            .is_none(),
        "default observed state must not include secret artifact content"
    );

    let detail = db
        .store
        .deployment_detail(&system_admin(), created.deployment_id, true)
        .await
        .unwrap();
    let target = &detail.targets[0];
    assert_eq!(
        target.desired_structure["xray"]["content"],
        json!(artifact_content(&desired.desired.xray))
    );
    assert_eq!(
        target.desired_structure["wireguard"]["content"],
        json!(artifact_content(&desired.desired.wireguard))
    );
    assert_eq!(
        target.observed_after.as_ref().unwrap()["xray"]["content"],
        json!(artifact_content(&desired.desired.xray))
    );
    assert!(
        target.observed_before.as_ref().unwrap()["xray"]
            .get("content")
            .is_none(),
        "unknown before state has no control-plane artifact content to attach"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn list_deployments_returns_recent_status_and_target_counts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "deployment-list"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    let list = db
        .store
        .list_deployments(&system_admin(), 10, None)
        .await
        .unwrap();
    let item = list
        .deployments
        .iter()
        .find(|item| item.id == created.deployment_id)
        .expect("created deployment should be listed");
    assert_eq!(item.status, "succeeded");
    assert_eq!(item.total_targets, 1);
    assert_eq!(item.changed_targets, 1);
    assert_eq!(item.skipped_targets, 0);
    assert_eq!(item.failed_targets, 0);
    assert_eq!(item.disruptive_targets, 1);
    assert_eq!(item.max_wave, 1);
}

// Keep ordinary rounds close to now. The protocol deliberately accepts arbitrarily old spool
// replay; only a clock more than ten minutes in the future is retryably refused.
fn usage_report_base_unix() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64;
    now - 120
}

fn protocol_v3_usage(
    sequence: u64,
    read_at: i64,
    epoch: &str,
    uplink_bytes: u64,
    downlink_bytes: u64,
) -> UsageReportRequest {
    UsageReportRequest {
        agent_instance_id: Some("0123456789abcdef0123456789abcdef".to_owned()),
        sequence: Some(sequence),
        usage_generation_id: None,
        read_at_unix_secs: read_at,
        xray_started_at_unix_secs: read_at - 60,
        xray_epoch: Some(epoch.to_owned()),
        route: None,
        counters: vec![UsageCounter {
            label: "alice@platform.acme#i-main".to_owned(),
            uplink_bytes,
            downlink_bytes,
        }],
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unknown_counter_growth_is_ephemeral_and_not_persisted() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();

    let mut first = protocol_v3_usage(1, base, "boot-a:100", 10, 20);
    first.counters[0].label = "removed-user".to_owned();
    first.counters.push(UsageCounter {
        label: "probe#i-main".to_owned(),
        uplink_bytes: 100,
        downlink_bytes: 200,
    });
    assert_eq!(
        db.store
            .record_usage_report("n1", first)
            .await
            .unwrap()
            .skipped_counters,
        1
    );
    let first_list = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    let first_result = first_list
        .nodes
        .iter()
        .find(|node| node.node_id == "n1")
        .unwrap()
        .usage_last_result
        .as_ref()
        .unwrap();
    assert_eq!(first_result["growing_unknown_counters"], 0);

    let mut second = protocol_v3_usage(2, base + 30, "boot-a:100", 11, 20);
    second.counters[0].label = "removed-user".to_owned();
    second.counters.push(UsageCounter {
        label: "probe#i-main".to_owned(),
        uplink_bytes: 150,
        downlink_bytes: 280,
    });
    db.store.record_usage_report("n1", second).await.unwrap();
    let second_list = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    let second_result = second_list
        .nodes
        .iter()
        .find(|node| node.node_id == "n1")
        .unwrap()
        .usage_last_result
        .as_ref()
        .unwrap();
    assert_eq!(second_result["growing_unknown_counters"], 1);

    // Probe traffic uses a real Xray account but is neither billable nor an attribution anomaly.
    // It keeps moving while the genuine unknown stays flat; the latest growth finding must clear.
    let mut probe_only_growth = protocol_v3_usage(3, base + 60, "boot-a:100", 11, 20);
    probe_only_growth.counters[0].label = "removed-user".to_owned();
    probe_only_growth.counters.push(UsageCounter {
        label: "probe#i-main".to_owned(),
        uplink_bytes: 200,
        downlink_bytes: 360,
    });
    let probe_only_result = db
        .store
        .record_usage_report("n1", probe_only_growth)
        .await
        .unwrap();
    assert_eq!(probe_only_result.skipped_counters, 1);
    let latest_list = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    let latest_result = latest_list
        .nodes
        .iter()
        .find(|node| node.node_id == "n1")
        .unwrap()
        .usage_last_result
        .as_ref()
        .unwrap();
    assert_eq!(latest_result["growing_unknown_counters"], 0);

    let persisted: serde_json::Value =
        sqlx::query_scalar("SELECT usage_last_result FROM node_agent_state WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(persisted.get("growing_unknown_counters").is_none());

    // A fresh store represents a restarted control plane and intentionally has no baseline.
    let restarted = PgStore::from_pool(db.pool().clone());
    let restarted_list = restarted
        .list_node_agent_states(&system_admin())
        .await
        .unwrap();
    let restarted_result = restarted_list
        .nodes
        .iter()
        .find(|node| node.node_id == "n1")
        .unwrap()
        .usage_last_result
        .as_ref()
        .unwrap();
    assert!(restarted_result.get("growing_unknown_counters").is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_receipt_is_exactly_once_under_retry_and_concurrency() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();

    let first = protocol_v3_usage(1, base, "boot-a:100", 100, 200);
    let (a, b) = tokio::join!(
        db.store.record_usage_report("n1", first.clone()),
        db.store.record_usage_report("n1", first.clone())
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(
        a.duplicate, b.duplicate,
        "one commit and one durable replay"
    );
    assert_eq!(a.usage_generation_id, b.usage_generation_id);

    let mut reused = first;
    reused.counters[0].uplink_bytes = 101;
    assert!(matches!(
        db.store.record_usage_report("n1", reused).await,
        Err(StoreError::InvalidData(message)) if message.contains("reused")
    ));

    let second = protocol_v3_usage(2, base + 30, "boot-a:100", 150, 280);
    let landed = db
        .store
        .record_usage_report("n1", second.clone())
        .await
        .unwrap();
    assert_eq!(landed.inserted_samples, 1);
    assert!(
        db.store
            .record_usage_report("n1", second)
            .await
            .unwrap()
            .duplicate
    );
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM usage_report_receipts),
                (SELECT count(*) FROM usage_readings),
                (SELECT count(*) FROM usage_samples)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(counts, (2, 2, 1));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn exact_xray_epoch_rejects_counter_regression_and_new_epoch_marks_a_gap() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();
    db.store
        .record_usage_report("n1", protocol_v3_usage(1, base, "boot-a:100", 100, 200))
        .await
        .unwrap();

    let regressed = db
        .store
        .record_usage_report("n1", protocol_v3_usage(2, base + 20, "boot-a:100", 50, 100))
        .await
        .unwrap();
    assert_eq!(regressed.rejected_counters, 1);
    assert_eq!(regressed.inserted_samples, 0);

    let recovered = db
        .store
        .record_usage_report(
            "n1",
            protocol_v3_usage(3, base + 40, "boot-a:100", 150, 260),
        )
        .await
        .unwrap();
    assert_eq!(recovered.inserted_samples, 1);
    let recovered_bytes: (i64, i64) = sqlx::query_as(
        "SELECT uplink_bytes, downlink_bytes FROM usage_samples ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        recovered_bytes,
        (50, 60),
        "bad reading did not replace the head"
    );

    let restarted = db
        .store
        .record_usage_report("n1", protocol_v3_usage(4, base + 60, "boot-a:900", 20, 30))
        .await
        .unwrap();
    assert_eq!(restarted.inserted_samples, 1);
    assert_eq!(restarted.gap_samples, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn legacy_report_after_an_exact_epoch_does_not_rebill_climbing_counters() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let base = usage_report_base_unix();
    db.store
        .record_usage_report("n1", protocol_v3_usage(1, base, "boot-a:100", 100, 200))
        .await
        .unwrap();

    // During a rolling downgrade (or while the final old-Agent spool entry drains), the report
    // has no exact process epoch. Its counters are still authoritative: if they climbed, this is
    // a delta rather than a restart whose entire absolute value should be billed again.
    let legacy = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base + 30,
                xray_started_at_unix_secs: base + 20,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 140,
                    downlink_bytes: 260,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(legacy.inserted_samples, 1);
    assert_eq!(legacy.gap_samples, 0);
    let bytes: (i64, i64) = sqlx::query_as(
        "SELECT uplink_bytes, downlink_bytes FROM usage_samples ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(bytes, (40, 60));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn frozen_usage_generation_survives_model_removal_and_raw_retention() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let now = usage_report_base_unix();
    let old = now - 3 * 86_400;
    let first = db
        .store
        .record_usage_report("n1", protocol_v3_usage(1, old, "boot-a:100", 100, 200))
        .await
        .unwrap();
    let generation = first.usage_generation_id.unwrap();

    // The model no longer owns this label, but a delayed report from the already-running
    // generation still has an immutable, auditable owner.
    sqlx::query("DELETE FROM grants WHERE user_id = 'alice' AND ingress_id = 'i-main'")
        .execute(db.pool())
        .await
        .unwrap();
    let mut delayed = protocol_v3_usage(2, old + 30, "boot-a:100", 160, 290);
    delayed.usage_generation_id = Some(generation);
    let delayed = db.store.record_usage_report("n1", delayed).await.unwrap();
    assert_eq!(delayed.inserted_samples, 1);
    assert_eq!(delayed.skipped_counters, 0);

    let deleted = db.store.prune_usage_readings(1).await.unwrap();
    assert_eq!(deleted, 2);
    let heads: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_counter_heads")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        heads, 1,
        "retention must never delete the accounting baseline"
    );

    let mut current = protocol_v3_usage(3, now, "boot-a:100", 200, 350);
    current.usage_generation_id = Some(generation);
    let current = db.store.record_usage_report("n1", current).await.unwrap();
    assert_eq!(
        current.inserted_samples, 1,
        "head survives raw history pruning"
    );
    let bytes: (i64, i64) = sqlx::query_as(
        "SELECT uplink_bytes, downlink_bytes FROM usage_samples ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(bytes, (40, 60));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn first_report_after_new_authorization_counts_the_complete_counter() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    // Establish a known, already-running counter namespace. Without this witness a rolling
    // upgrade must keep treating every first reading as an unknown historical baseline.
    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "usage-new-user-base"),
        )
        .await
        .unwrap();
    let base_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("base deployment should be claimable");
    db.store
        .report_target_result(applied_report(&base_desired))
        .await
        .unwrap();

    db.store
        .create_user(
            &system_admin(),
            CreateUserRequest {
                tenant_id: "platform.acme".to_owned(),
                id: "bob".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    db.store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "bob".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: true,
                note: None,
            },
        )
        .await
        .unwrap();
    let release = db.store.process_grant_automation().await.unwrap();
    assert_eq!(release.merged_jobs, 1);
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("new authorization should create a grants deployment");
    let generation = desired
        .usage_generation_id
        .expect("grants deployment should freeze a usage generation");
    let bindings: serde_json::Value =
        sqlx::query_scalar("SELECT bindings FROM usage_generations WHERE id = $1")
            .bind(generation)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        bindings["bob@platform.acme#i-main"]["first_reading"],
        "count-from-zero"
    );

    let activated_at = usage_report_base_unix();
    let mut report = applied_report(&desired);
    report.usage_activated_at_unix_secs = Some(activated_at);
    db.store.report_target_result(report).await.unwrap();

    let first = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: Some("0123456789abcdef0123456789abcdef".to_owned()),
                sequence: Some(1),
                usage_generation_id: Some(generation),
                xray_epoch: Some("boot-a:100".to_owned()),
                read_at_unix_secs: activated_at + 30,
                xray_started_at_unix_secs: activated_at - 300,
                route: None,
                counters: vec![UsageCounter {
                    label: "bob@platform.acme#i-main".to_owned(),
                    uplink_bytes: 100,
                    downlink_bytes: 200,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(first.accepted_readings, 1);
    assert_eq!(first.inserted_samples, 1);
    assert_eq!(first.gap_samples, 0);

    let sample: (i64, i64, i64, bool) = sqlx::query_as(
        "SELECT uplink_bytes, downlink_bytes,
                extract(epoch FROM window_start)::bigint, has_gap
         FROM usage_samples
         WHERE user_id = 'bob' AND node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(sample, (100, 200, activated_at, false));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_records_delta_samples_and_restart_gaps() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let base = usage_report_base_unix();
    let first = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base,
                xray_started_at_unix_secs: base - 1000,
                route: None,
                counters: vec![
                    UsageCounter {
                        label: "alice@platform.acme#i-main".to_owned(),
                        uplink_bytes: 100,
                        downlink_bytes: 200,
                    },
                    UsageCounter {
                        label: "unknown@platform.acme#i-main".to_owned(),
                        uplink_bytes: 1,
                        downlink_bytes: 1,
                    },
                ],
            },
        )
        .await
        .unwrap();
    assert_eq!(first.accepted_readings, 1);
    assert_eq!(first.inserted_samples, 0);
    assert_eq!(first.skipped_counters, 1);

    let second = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base + 60,
                xray_started_at_unix_secs: base - 1000,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 150,
                    downlink_bytes: 280,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(second.accepted_readings, 1);
    assert_eq!(second.inserted_samples, 1);
    assert_eq!(second.gap_samples, 0);

    let restart = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base + 120,
                xray_started_at_unix_secs: base + 90,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 20,
                    downlink_bytes: 30,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(restart.inserted_samples, 1);
    assert_eq!(restart.gap_samples, 1);

    let samples = db
        .store
        .list_usage_samples(
            &system_admin(),
            10,
            Some("platform.acme"),
            Some("alice"),
            Some("n1"),
        )
        .await
        .unwrap();
    assert_eq!(samples.samples.len(), 2);
    assert!(samples.samples[0].has_gap);
    assert_eq!(samples.samples[0].uplink_bytes, 20);
    assert_eq!(samples.samples[0].downlink_bytes, 30);
    assert!(!samples.samples[1].has_gap);
    assert_eq!(samples.samples[1].uplink_bytes, 50);
    assert_eq!(samples.samples[1].downlink_bytes, 80);

    let row = sqlx::query(
        "SELECT last_usage_report_at IS NOT NULL AS reported,
                xray_started_at IS NOT NULL AS has_started
         FROM node_agent_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(row.try_get::<bool, _>("reported").unwrap());
    assert!(row.try_get::<bool, _>("has_started").unwrap());
}

// A start time that jumps while the counters keep climbing is not a restart, and must not be read
// as one. On the node the instant comes from whichever process is named `xray`, and the e2e prober
// spawns one short-lived `xray` per chain every few minutes; when the agent picked one of those,
// an ingress serving continuously reported itself as freshly started. Taking that at face value
// booked the entire cumulative counter as this window's traffic, over and over — 682 GiB in three
// days on one machine, all of it invented. The counters are the witness: they never went backwards.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_takes_a_delta_when_the_start_time_jumps_but_counters_climb() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let base = usage_report_base_unix();
    db.store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base,
                xray_started_at_unix_secs: base - 1000,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 100,
                    downlink_bytes: 200,
                }],
            },
        )
        .await
        .unwrap();

    let flapped = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base + 60,
                // Two seconds old: a probe child's start time, not the server's.
                xray_started_at_unix_secs: base + 58,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 150,
                    downlink_bytes: 280,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(flapped.inserted_samples, 1);
    assert_eq!(
        flapped.gap_samples, 0,
        "climbing counters are not a restart"
    );

    let samples = db
        .store
        .list_usage_samples(
            &system_admin(),
            10,
            Some("platform.acme"),
            Some("alice"),
            Some("n1"),
        )
        .await
        .unwrap();
    // The first round has nothing to difference against, so the second is the only sample.
    assert_eq!(samples.samples.len(), 1);
    assert!(!samples.samples[0].has_gap);
    // The difference, not the 150/280 the counter stands at.
    assert_eq!(samples.samples[0].uplink_bytes, 50);
    assert_eq!(samples.samples[0].downlink_bytes, 80);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_rejects_labels_for_another_nodes_ingress() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // n2 exists, but i-main sits on n1 and it receives no client entry for alice.
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            'n2', 'platform.acme', 'Node 2', 'n2.example.net', '10.66.0.2',
            'wg-private-2', 'wg-public-2', 51820,
            10085, TRUE, TRUE,
            'servers', '[\"1.1.1.1\"]'::jsonb
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let base = usage_report_base_unix();
    // Two incrementing reports: accepted, the second would land a usage_sample.
    for (read_at, uplink, downlink) in [(base, 10_u64, 20_u64), (base + 60, 999_999, 999_999)] {
        let result = db
            .store
            .record_usage_report(
                "n2",
                UsageReportRequest {
                    agent_instance_id: None,
                    sequence: None,
                    usage_generation_id: None,
                    xray_epoch: None,
                    read_at_unix_secs: read_at,
                    xray_started_at_unix_secs: base - 1000,
                    route: None,
                    counters: vec![UsageCounter {
                        label: "alice@platform.acme#i-main".to_owned(),
                        uplink_bytes: uplink,
                        downlink_bytes: downlink,
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(result.rejected_counters, 1);
        assert_eq!(result.accepted_readings, 0);
        assert_eq!(result.inserted_samples, 0);
        assert_eq!(result.skipped_counters, 0);
    }

    let forged: i64 = sqlx::query("SELECT count(*) AS n FROM usage_readings WHERE node_id = 'n2'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(forged, 0);

    let samples: i64 = sqlx::query("SELECT count(*) AS n FROM usage_samples")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(samples, 0);

    // The same label reported by n1, which really carries i-main, still books normally.
    let owned = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base,
                xray_started_at_unix_secs: base - 1000,
                route: None,
                counters: vec![UsageCounter {
                    label: "alice@platform.acme#i-main".to_owned(),
                    uplink_bytes: 10,
                    downlink_bytes: 20,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(owned.accepted_readings, 1);
    assert_eq!(owned.rejected_counters, 0);
}

// A reverse-access hop's bytes are read by the portal machine while the label names the bridge —
// because the traffic is dialed up by the downstream and travels back along that connection,
// entering the portal's relay port and carrying the downstream's credential. The downstream itself
// has no counter at all (traffic injected by the bridge carries no user identity).
//
// So this family of labels cannot be handled by "refuse a label that is not yours": refused,
// neither end books the hop and the relay traffic is carried for nothing. Nor can it be admitted
// wholesale — only the case where this machine really is its reverse-access upstream on this
// chain.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn usage_report_accepts_reverse_hop_counters_reported_by_the_portal() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            'n2', 'platform.acme', 'Node 2', 'n2.example.net', '10.66.0.2',
            'wg-private-2', 'wg-public-2', 51820,
            10085, TRUE, TRUE,
            'servers', '[\"1.1.1.1\"]'::jsonb
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // n2 is the next hop on the chain, and the credential is in its own name.
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules, accept_uuid, accept_label, hop_in_port, hop_in_wire)
         VALUES ('c-main', 'n2', '[]'::jsonb,
                 '3f7b1f5e-9a2c-4c1d-8b3a-6d5e4f2c1b0a', 'c-main@n2',
                 20001, '{\"t\": \"none\"}'::jsonb)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // n1's step forwards to n2 over reverse access: n2 dials n1, not the other way.
    let reverse_rules = json!([
        {
            "m": { "t": "any" },
            "a": { "t": "forward", "to": "n2", "dial": { "t": "reverse", "v": "v4" } }
        }
    ]);
    sqlx::query("UPDATE steps SET rules = $1 WHERE chain_id = 'c-main' AND node_id = 'n1'")
        .bind(&reverse_rules)
        .execute(db.pool())
        .await
        .unwrap();

    let base = usage_report_base_unix();
    for (read_at, uplink, downlink) in [(base, 10_u64, 20_u64), (base + 60, 110, 220)] {
        let result = db
            .store
            .record_usage_report(
                "n1",
                UsageReportRequest {
                    agent_instance_id: None,
                    sequence: None,
                    usage_generation_id: None,
                    xray_epoch: None,
                    read_at_unix_secs: read_at,
                    xray_started_at_unix_secs: base - 1000,
                    route: None,
                    counters: vec![UsageCounter {
                        label: "c-main@n2".to_owned(),
                        uplink_bytes: uplink,
                        downlink_bytes: downlink,
                    }],
                },
            )
            .await
            .unwrap();
        assert_eq!(result.rejected_counters, 0, "portal 替 bridge 报量不是异常");
        assert_eq!(result.accepted_readings, 1);
    }

    // The accounting goes to the machine that forwards, not the one that read the counter.
    let row = sqlx::query(
        "SELECT node_id, uplink_bytes, downlink_bytes
         FROM usage_chain_samples
         WHERE hop_label = 'c-main@n2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.try_get::<String, _>("node_id").unwrap(), "n2");
    assert_eq!(row.try_get::<i64, _>("uplink_bytes").unwrap(), 100);
    assert_eq!(row.try_get::<i64, _>("downlink_bytes").unwrap(), 200);

    let frozen_binding: serde_json::Value = sqlx::query_scalar(
        "SELECT g.bindings -> 'c-main@n2'
           FROM node_agent_state s
           JOIN usage_generations g ON g.id = s.usage_generation_id
          WHERE s.node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen_binding["kind"], "chain-hop");
    assert_eq!(frozen_binding["node_id"], "n2");

    // The raw reading is still recorded against whoever read it — differences must be taken
    // against one source.
    let readings: i64 = sqlx::query(
        "SELECT count(*) AS n FROM usage_readings WHERE node_id = 'n1' AND label = 'c-main@n2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(readings, 2);

    // Editing the live model alone must not reinterpret a report from the generation already
    // running on the machine. Until a deployment converges and activates a new frozen map, n1 is
    // still the legitimate reporter for this label.
    let overlay_rules = json!([
        {
            "m": { "t": "any" },
            "a": { "t": "forward", "to": "n2", "dial": { "t": "overlay" } }
        }
    ]);
    sqlx::query("UPDATE steps SET rules = $1 WHERE chain_id = 'c-main' AND node_id = 'n1'")
        .bind(&overlay_rules)
        .execute(db.pool())
        .await
        .unwrap();

    let still_owned_by_frozen_generation = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
                agent_instance_id: None,
                sequence: None,
                usage_generation_id: None,
                xray_epoch: None,
                read_at_unix_secs: base + 120,
                xray_started_at_unix_secs: base - 1000,
                route: None,
                counters: vec![UsageCounter {
                    label: "c-main@n2".to_owned(),
                    uplink_bytes: 999,
                    downlink_bytes: 999,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(still_owned_by_frozen_generation.rejected_counters, 0);
    assert_eq!(still_owned_by_frozen_generation.accepted_readings, 1);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tenant_scoped_usage_samples_do_not_cross_subtrees() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            grant_label, uplink_bytes, downlink_bytes
         )
         VALUES
            (
                to_timestamp(1800000000), to_timestamp(1800000060),
                'n1', 'platform.acme', 'alice', 'i-main',
                'alice@platform.acme#i-main', 10, 20
            ),
            (
                to_timestamp(1800000000), to_timestamp(1800000060),
                'n-other', 'platform.other', 'charlie', 'i-other',
                'charlie@platform.other#i-other', 30, 40
            )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let acme = db
        .store
        .list_usage_samples(&publisher("platform.acme"), 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(acme.samples.len(), 1);
    assert_eq!(acme.samples[0].tenant_id, "platform.acme");

    let acme_asks_other = db
        .store
        .list_usage_samples(
            &publisher("platform.acme"),
            10,
            Some("platform.other"),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(acme_asks_other.samples.is_empty());

    let system = db
        .store
        .list_usage_samples(&system_admin(), 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(system.samples.len(), 2);
}

// ── Automatic quota enforcement ─────────────────────────────────────────
// These cases all use the fixture's n1, which never converged, so a release is necessarily stopped
// by the gate (the plan is nothing but ApplyXray / ApplyWireGuard). That is exactly half of what
// is asserted: the model changed and not one byte was pushed to a machine.

/// Insert one already-booked usage sample into this month (app_id hardcoded, as it is frozen at
/// insert). `minute` is the window's offset within the month — (node, label, window) is the unique
/// key, so a second insert in one test needs a different window or it collides with the constraint
/// rather than the assertion.
async fn insert_month_sample(
    pool: &PgPool,
    ingress: &str,
    app: &str,
    minute: i32,
    up: i64,
    down: i64,
) {
    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            app_id, grant_label, uplink_bytes, downlink_bytes, has_gap
         )
         VALUES (
            (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                + make_interval(mins => $3)) AT TIME ZONE 'Asia/Hong_Kong',
            (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                + make_interval(mins => $3 + 1)) AT TIME ZONE 'Asia/Hong_Kong',
            'n1', 'platform.acme', 'alice', $1, $2,
            'alice@platform.acme#' || $1, $4, $5, FALSE
         )",
    )
    .bind(ingress)
    .bind(app)
    .bind(minute)
    .bind(up)
    .bind(down)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_quota(db: &TestPg, limit: Option<u64>) {
    db.store
        .set_user_app_quota(
            &system_admin(),
            SetUserAppQuotaRequest {
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                app_id: "app-main".to_owned(),
                limit_bytes: limit,
            },
        )
        .await
        .unwrap();
}

async fn live_grants(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM grants WHERE user_id = 'alice'")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn suspensions(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM quota_suspensions WHERE user_id = 'alice'")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn revision_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A second ingress, used to verify that a round stamps one revision — one view with two
/// ingresses is two grants.
async fn insert_second_ingress(pool: &PgPool) {
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
            'i-alt', 'app-main', 'c-main', 'n1', '0.0.0.0', 8443, NULL, 'vless-reality',
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
         VALUES ('i-alt', 'chrome')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-main', 'platform.acme', 'alice', 'i-alt')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_suspends_every_ingress_in_one_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_ingress(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;
    assert_eq!(live_grants(db.pool()).await, 2);

    let before = revision_count(db.pool()).await;
    let outcome = db.store.enforce_quotas().await.unwrap();

    // Two grants under one view are revoked together into one revision — one apiece would flood
    // the history
    assert_eq!(outcome.suspended, 2);
    assert_eq!(outcome.restored, 0);
    assert_eq!(revision_count(db.pool()).await, before + 1);
    assert_eq!(live_grants(db.pool()).await, 0);
    assert_eq!(suspensions(db.pool()).await, 2);

    let author: String =
        sqlx::query_scalar("SELECT author FROM revisions ORDER BY id DESC LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(author, "system:quota");

    // This machine never converged and also owes configuration changes, so it cannot enter a
    // grants deployment — the list waits for a configuration deployment to build the inbound
    // first. So the model changes and not one byte is pushed to a machine.
    assert_eq!(outcome.deferred, vec!["n1".to_owned()]);
    assert!(outcome.deployment_id.is_none());
    let deployments: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(deployments, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_is_idempotent_across_rounds() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;

    db.store.enforce_quotas().await.unwrap();
    let after_first = revision_count(db.pool()).await;

    // Second round: already fully revoked, so it should touch neither the model nor stamp
    // another revision
    let second = db.store.enforce_quotas().await.unwrap();
    assert_eq!(second.suspended, 0);
    assert_eq!(second.restored, 0);
    assert_eq!(revision_count(db.pool()).await, after_first);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_restores_when_the_limit_is_raised_or_dropped() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;
    db.store.enforce_quotas().await.unwrap();
    assert_eq!(live_grants(db.pool()).await, 0);

    // A raised quota means the next round restores them
    set_quota(&db, Some(1_000_000)).await;
    let raised = db.store.enforce_quotas().await.unwrap();
    assert_eq!(raised.restored, 1);
    assert_eq!(live_grants(db.pool()).await, 1);
    assert_eq!(suspensions(db.pool()).await, 0);

    // Revoke again, then delete the quota entirely — unlimited must restore them too
    set_quota(&db, Some(1000)).await;
    db.store.enforce_quotas().await.unwrap();
    assert_eq!(live_grants(db.pool()).await, 0);

    set_quota(&db, None).await;
    let dropped = db.store.enforce_quotas().await.unwrap();
    assert_eq!(dropped.restored, 1);
    assert_eq!(live_grants(db.pool()).await, 1);
    assert_eq!(suspensions(db.pool()).await, 0);
}

// A grant a person revoked by hand is not the system's to add back at the start of a month. This
// is exactly why the quota_suspensions table exists — without it, who revoked what cannot be told
// apart.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_never_restores_a_manually_revoked_grant() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;
    db.store.enforce_quotas().await.unwrap();
    assert_eq!(live_grants(db.pool()).await, 0);
    assert_eq!(suspensions(db.pool()).await, 1);

    // The grant is already absent, but pressing revoke is still meaningful: ownership of that
    // disabled state moves from the quota worker to the operator, so a later reset must not add it
    // back. This was the dangerous state the old test did not cover.
    db.store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: Some("keep manually disabled".to_owned()),
            },
        )
        .await
        .unwrap();
    assert_eq!(suspensions(db.pool()).await, 0);

    set_quota(&db, Some(1_000_000)).await;
    let outcome = db.store.enforce_quotas().await.unwrap();
    assert_eq!(outcome.restored, 0);
    assert_eq!(
        live_grants(db.pool()).await,
        0,
        "人撤掉的授权被系统加回来了"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_rejects_operator_reenable_while_still_over_limit() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;
    db.store.enforce_quotas().await.unwrap();

    let error = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: true,
                note: None,
            },
        )
        .await
        .expect_err("a normal grant write must not bypass a hard quota");
    assert!(matches!(error, StoreError::Unsupported(_)));
    assert_eq!(live_grants(db.pool()).await, 0);
    assert_eq!(suspensions(db.pool()).await, 1);
}

// Usage below the quota calls for no action — only used >= limit counts as spent.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_leaves_users_under_the_limit_alone() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 400, 500).await; // 900 < 1000

    let outcome = db.store.enforce_quotas().await.unwrap();
    assert_eq!(outcome.suspended, 0);
    assert_eq!(live_grants(db.pool()).await, 1);

    // One more pushes it over the line
    insert_month_sample(db.pool(), "i-main", "app-main", 3, 100, 0).await; // 合计 1000 = 额度
    let outcome = db.store.enforce_quotas().await.unwrap();
    assert_eq!(outcome.suspended, 1);
    assert_eq!(live_grants(db.pool()).await, 0);
}

// When a grants deployment lands on a machine, the desired state the agent receives must carry no
// configuration at all. That is the whole point of splitting the kinds: the agent converges on
// desired without looking at actions, and an xray that is Present in desired gets an
// unconditional `pkill` and restart (apply_xray in main.rs). So "changing a grant drops nobody"
// rests not on the control plane's restraint but on there being no xray in that deployment to
// push.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn a_grants_deployment_carries_no_config_for_the_agent_to_apply() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // Converge n1 first, then change only grants
    db.store
        .create_deployment(&system_admin(), create_deployment_request(1, "base"))
        .await
        .unwrap();
    let first = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&first))
        .await
        .unwrap();

    // Revoke alice's grant on an existing ingress: only the list moves and not one byte of
    // xray.json changes. Opening a new ingress would not do — that adds an inbound to xray, which
    // is a configuration change.
    let revoked = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();
    let grants = db
        .store
        .create_deployment(
            &system_admin(),
            create_grants_deployment_request(revoked.revision_id, "grants-only"),
        )
        .await
        .unwrap();

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("权限单该派得下来");
    assert_eq!(desired.deployment_id, grants.deployment_id);

    // All three artifacts must be Unmanaged — the agent's converge_linux_* returns on seeing it
    // and never reaches the pkill. Any one of them Present means this deployment restarts a
    // process or reconnects a tunnel.
    assert!(
        matches!(desired.desired.xray, DesiredArtifact::Unmanaged { .. }),
        "权限单带了 xray，agent 会 pkill 重启：{:?}",
        desired.desired.xray
    );
    assert!(matches!(
        desired.desired.wireguard,
        DesiredArtifact::Unmanaged { .. }
    ));
    assert!(matches!(
        desired.desired.phantun,
        DesiredArtifact::Unmanaged { .. }
    ));
    // The list itself is still supplied, or the deployment accomplishes nothing
    assert!(matches!(
        desired.desired.grants,
        DesiredGrants::Present { .. }
    ));

    // Those three come back as Unmanaged in the report and must not overwrite the applied
    // state — the xray on the machine is plainly still running.
    let xray_before: Option<String> =
        sqlx::query_scalar("SELECT xray_sha256 FROM node_applied_state WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(xray_before.is_some());

    // Serving accepts a sparse grants target set only with the durable worker's proof that
    // whole-fleet planning found no deferred machine. Production writes this before the target
    // converges; this test creates the grants order directly, so reproduce that proof explicitly.
    sqlx::query(
        "INSERT INTO jobs (kind, status, payload)
         VALUES (
             'grants-deployment',
             'succeeded',
             jsonb_build_object('revision_id', $1::bigint, 'deployment_id', $2::bigint)
         )",
    )
    .bind(i64::try_from(revoked.revision_id).unwrap())
    .bind(grants.deployment_id)
    .execute(db.pool())
    .await
    .unwrap();

    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    let serving_permissions: (i64, Option<i64>) = sqlx::query_as(
        "SELECT permissions_revision_id, permissions_deployment_id
           FROM subscription_serving_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        serving_permissions,
        (
            i64::try_from(revoked.revision_id).unwrap(),
            Some(grants.deployment_id),
        ),
        "a successful grants release advances only the serving permission line"
    );

    let after = sqlx::query(
        "SELECT xray_state, xray_sha256, grants_state
         FROM node_applied_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        after.try_get::<Option<String>, _>("xray_sha256").unwrap(),
        xray_before,
        "权限单把 xray 的观察状态覆盖掉了"
    );
    assert_eq!(after.try_get::<String, _>("xray_state").unwrap(), "present");
}

// A machine owing both an MTU change and a grant must not have the grant wait with it — the list
// goes into an xray inbound and has nothing to do with wg's MTU. This is precisely what was most
// painful while the deployment kinds were undivided. (An owed xray change genuinely does wait: the
// inbound may not be built yet, which the cases above cover.)
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn grants_do_not_wait_on_an_unrelated_wireguard_change() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    db.store
        .create_deployment(&system_admin(), create_deployment_request(1, "base"))
        .await
        .unwrap();
    let first = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&first))
        .await
        .unwrap();

    // Change the MTU (moving only wg) plus revoke one grant (moving only the list), neither
    // released yet
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                mtu: Some(1280),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let revoked = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();

    // The grants deployment ships regardless: an owed wg change does not affect the list
    let grants = db
        .store
        .create_deployment(
            &system_admin(),
            create_grants_deployment_request(revoked.revision_id, "grants-with-pending-wg"),
        )
        .await
        .expect("wg 欠着账不该挡住名单");
    assert_eq!(grants.plan.summary.changed_targets, 1);

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(desired.deployment_id, grants.deployment_id);
    // The crux: wg owes a change and this deployment still does not push it — pushing would
    // reconnect the tunnel, while this deployment should move only the list.
    assert!(matches!(
        desired.desired.wireguard,
        DesiredArtifact::Unmanaged { .. }
    ));
    assert!(matches!(
        desired.desired.xray,
        DesiredArtifact::Unmanaged { .. }
    ));
}

// The two lines are single-flight independently: while somebody pushes xray in waves, grant
// changes should not queue behind it.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn config_and_grants_deployments_do_not_block_each_other() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // A configuration deployment is in flight (not yet converged)
    db.store
        .create_deployment(&system_admin(), create_deployment_request(1, "cfg"))
        .await
        .unwrap();

    // A second of the same kind must be blocked
    let same_kind = db
        .store
        .create_deployment(&system_admin(), create_deployment_request(1, "cfg-2"))
        .await;
    assert!(same_kind.is_err(), "同类工单必须单飞");

    // A grants deployment is unaffected. This machine never converged and narrowing leaves no
    // target, so what is asserted here is rejection for an empty target rather than for the
    // single-flight lock — both are Err and only the error text tells them apart.
    let other_kind = db
        .store
        .create_deployment(
            &system_admin(),
            create_grants_deployment_request(1, "grants-1"),
        )
        .await;
    let message = other_kind.unwrap_err().to_string();
    assert!(
        message.contains("empty target set"),
        "权限单不该被配置单的单飞锁挡住，实际错误：{message}"
    );
}

// Artifacts a configuration deployment did not touch are marked Unmanaged too: a machine that only
// changed wg should not get an xray restart thrown in. This is the other half of one narrowing
// mechanism.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn a_config_deployment_only_carries_the_artifacts_it_changes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    db.store
        .create_deployment(&system_admin(), create_deployment_request(1, "base"))
        .await
        .unwrap();
    let first = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&first))
        .await
        .unwrap();

    // Changing only the MTU: wg is re-emitted and not one byte of xray changes
    db.store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                mtu: Some(1280),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let rev = db.store.list_revisions(&system_admin(), 1).await.unwrap();
    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(rev.current_revision, "mtu"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();

    assert!(
        matches!(desired.desired.wireguard, DesiredArtifact::Present { .. }),
        "改了 MTU，wg 必须推"
    );
    assert!(
        matches!(desired.desired.xray, DesiredArtifact::Unmanaged { .. }),
        "xray 没变却带上了，agent 会白重启一次：{:?}",
        desired.desired.xray
    );
    assert!(matches!(
        desired.desired.grants,
        DesiredGrants::Unmanaged { .. }
    ));

    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    let grants_state: String =
        sqlx::query_scalar("SELECT grants_state FROM node_applied_state WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        grants_state, "present",
        "an Unmanaged grants observation must preserve the last known runtime list"
    );
}

// Where quota enforcement did nothing, it must not take a single step further.
// `create_deployment` ships the whole current revision, and going on would push out changes others
// left unreleased — even where every one is non-destructive, that is pressing the release button
// on somebody's behalf. In a database with no quotas at all, not one deployment should
// appear.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_never_publishes_someone_elses_pending_work() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // Not one quota, yet the model has work waiting to ship (the machine never converged and the
    // plan is non-empty)
    let outcome = db.store.enforce_quotas().await.unwrap();
    assert_eq!(outcome.suspended, 0);
    assert_eq!(outcome.restored, 0);
    assert!(outcome.deployment_id.is_none());
    assert!(
        outcome.deferred.is_empty(),
        "配额没动过模型，不该有等着推的机器"
    );

    let deployments: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(deployments, 0, "配额没做任何事，却发布了一次");

    // Setting a generous quota must likewise trigger no release — nothing is over, so there is
    // nothing to push
    set_quota(&db, Some(1_000_000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 10, 10).await;
    db.store.enforce_quotas().await.unwrap();
    let deployments: i64 = sqlx::query_scalar("SELECT count(*) FROM deployments")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(deployments, 0);
}

// The side where the gate lets through: the machine has converged, revoking a grant produces only
// SyncGrants, and quota enforcement releases it itself. Together with the cases above this is the
// complete guarantee — pushing grants is allowed, pushing configuration is not.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn quota_enforcement_publishes_when_only_grants_change() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // Converge n1 first, after which only grants change in the model
    db.store
        .create_deployment(&system_admin(), create_deployment_request(1, "quota-base"))
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    set_quota(&db, Some(1000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 600, 600).await;

    let outcome = db.store.enforce_quotas().await.unwrap();
    assert_eq!(outcome.suspended, 1);
    assert!(
        outcome.deferred.is_empty(),
        "配置已经对齐，名单不该被推迟：{:?}",
        outcome.deferred
    );
    let deployment_id = outcome.deployment_id.expect("应该自己发了一次");

    let row = sqlx::query("SELECT actor, revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.try_get::<String, _>("actor").unwrap(), "system:quota");
    assert_eq!(
        row.try_get::<i64, _>("revision_id").unwrap() as u64,
        outcome.revision_id
    );

    // This release does one thing: synchronize grants. Any other action slipping in means the
    // gate leaked.
    let actions: serde_json::Value = sqlx::query_scalar(
        "SELECT desired_structure->'actions'
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(actions, json!(["sync-grants"]));

    // The frozen list that ships with the release, and the reason this test watches the column
    // rather than only the action: enforcement plans against the *unsplit* plan and narrows to
    // Grants itself. Handed the console-facing plan instead — already narrowed to Config — every
    // machine not restarting xray comes back with grants Unmanaged and no actions, the round finds
    // nothing to ship, and nobody is ever cut off. That is a silent failure: no error, no release,
    // a quota that simply stops enforcing. `state` must therefore be the list itself.
    let desired_grants: serde_json::Value = sqlx::query_scalar(
        "SELECT desired_grants
         FROM deployment_target_state
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        desired_grants["state"],
        json!("present"),
        "权限单必须带上名单本身，不能是被配置单收窄掉的 unmanaged：{desired_grants}"
    );
    // And the list is the post-suspension one: the user whose quota is spent is gone from it.
    let emails = desired_grants["inbounds"]
        .as_array()
        .expect("inbounds 应该是数组")
        .iter()
        .flat_map(|inbound| inbound["clients"].as_array().cloned().unwrap_or_default())
        .map(|client| client["email"].as_str().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !emails.iter().any(|email| email.contains("alice")),
        "配额用尽的用户不该还留在下发的名单里：{emails:?}"
    );

    // And it needs no confirmation — grant-only is wave 0 and non-destructive, so the agent claims
    // it on its next round
    let claimable = db.store.claim_desired_for_node("n1").await.unwrap();
    assert!(claimable.is_some(), "grant-only 发布不该卡在波次确认上");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_app_quotas_round_trip_and_clear() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let set = |limit: Option<u64>| SetUserAppQuotaRequest {
        tenant_id: "platform.acme".to_owned(),
        user_id: "alice".to_owned(),
        app_id: "app-main".to_owned(),
        limit_bytes: limit,
    };

    let created = db
        .store
        .set_user_app_quota(&system_admin(), set(Some(50 * 1024 * 1024 * 1024)))
        .await
        .unwrap();
    assert_eq!(created.quota.as_ref().unwrap().limit_bytes, 53_687_091_200);

    // Setting the same key again overwrites rather than inserting a second row
    db.store
        .set_user_app_quota(&system_admin(), set(Some(1024)))
        .await
        .unwrap();
    let listed = db
        .store
        .list_user_app_quotas(&system_admin(), None)
        .await
        .unwrap();
    assert_eq!(listed.quotas.len(), 1);
    assert_eq!(listed.quotas[0].limit_bytes, 1024);
    assert_eq!(listed.quotas[0].app_id, "app-main");

    // No limit_bytes cancels the quota
    let cleared = db
        .store
        .set_user_app_quota(&system_admin(), set(None))
        .await
        .unwrap();
    assert!(cleared.quota.is_none());
    assert!(db
        .store
        .list_user_app_quotas(&system_admin(), None)
        .await
        .unwrap()
        .quotas
        .is_empty());

    // 0 is not unlimited — those two must stay apart
    assert!(db
        .store
        .set_user_app_quota(&system_admin(), set(Some(0)))
        .await
        .is_err());

    // Hanging one on a nonexistent view or user is refused on the spot rather than leaving a quota
    // row pointing at nothing
    assert!(db
        .store
        .set_user_app_quota(
            &system_admin(),
            SetUserAppQuotaRequest {
                tenant_id: "platform.acme".to_owned(),
                user_id: "alice".to_owned(),
                app_id: "app-nope".to_owned(),
                limit_bytes: Some(1024),
            },
        )
        .await
        .is_err());
    assert!(db
        .store
        .set_user_app_quota(
            &system_admin(),
            SetUserAppQuotaRequest {
                tenant_id: "platform.acme".to_owned(),
                user_id: "nobody".to_owned(),
                app_id: "app-main".to_owned(),
                limit_bytes: Some(1024),
            },
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_app_quotas_are_scoped_to_the_actors_tenant_subtree() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    for (tenant, user, app) in [
        ("platform.acme", "alice", "app-main"),
        ("platform.other", "charlie", "app-other"),
    ] {
        db.store
            .set_user_app_quota(
                &system_admin(),
                SetUserAppQuotaRequest {
                    tenant_id: tenant.to_owned(),
                    user_id: user.to_owned(),
                    app_id: app.to_owned(),
                    limit_bytes: Some(4096),
                },
            )
            .await
            .unwrap();
    }

    let all = db
        .store
        .list_user_app_quotas(&system_admin(), None)
        .await
        .unwrap();
    assert_eq!(all.quotas.len(), 2);

    let acme = db
        .store
        .list_user_app_quotas(&publisher("platform.acme"), None)
        .await
        .unwrap();
    assert_eq!(acme.quotas.len(), 1);
    assert_eq!(acme.quotas[0].tenant_id, "platform.acme");

    // A write outside one's scope must be blocked rather than silently succeed
    assert!(db
        .store
        .set_user_app_quota(
            &publisher("platform.acme"),
            SetUserAppQuotaRequest {
                tenant_id: "platform.other".to_owned(),
                user_id: "charlie".to_owned(),
                app_id: "app-other".to_owned(),
                limit_bytes: Some(1),
            },
        )
        .await
        .is_err());
}

// View attribution is frozen at insert: an ingress later moved to another view must not take this
// month's consumption with it — were it to jump, a quota's numerator would change the instant the
// model did.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn monthly_usage_keeps_the_app_frozen_on_the_sample() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;

    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            app_id, grant_label, uplink_bytes, downlink_bytes, has_gap
         )
         VALUES (
            (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                + INTERVAL '1 minute') AT TIME ZONE 'Asia/Hong_Kong',
            (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                + INTERVAL '2 minutes') AT TIME ZONE 'Asia/Hong_Kong',
            'n1', 'platform.acme', 'alice', 'i-main',
            'app-main', 'alice@platform.acme#i-main', 10, 20, FALSE
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    // The ingress is moved to another view
    sqlx::query("UPDATE ingresses SET app_id = 'app-other' WHERE id = 'i-main'")
        .execute(db.pool())
        .await
        .unwrap();

    let summary = db
        .store
        .list_monthly_usage_summary(&system_admin())
        .await
        .unwrap();
    let alice = summary.views.iter().find(|r| r.user_id == "alice").unwrap();
    assert_eq!(alice.app_id, "app-main", "已经落账的样本不该跟着模型改归属");

    // The ingress is deleted entirely. Samples have no foreign key to ingresses —
    // usage_samples carries none deliberately, on the principle that historical rows must not
    // require the current model object to remain present. So orphan samples genuinely occur, and
    // an INNER JOIN discards them as a batch, presenting as consumption inexplicably losing a
    // chunk. The frozen column plus a LEFT JOIN is what carries that principle to the query
    // side.
    sqlx::query("DELETE FROM grants WHERE ingress_id = 'i-main'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM ingresses WHERE id = 'i-main'")
        .execute(db.pool())
        .await
        .unwrap();

    let after = db
        .store
        .list_monthly_usage_summary(&system_admin())
        .await
        .unwrap();
    let alice = after.views.iter().find(|r| r.user_id == "alice").unwrap();
    assert_eq!(alice.app_id, "app-main");
    assert_eq!(alice.uplink_bytes, 10);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn monthly_usage_summary_aggregates_current_month_per_user() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    // usage_samples.node_id has a foreign key, so the nodes are created first (n1 and n-other are
    // both in the fixture)
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    // The fixture has only alice; bob is the second person this test needs
    sqlx::query("INSERT INTO users (tenant_id, id, uuid) VALUES ('platform.acme', 'bob', 'b8dfa1b1-2f29-47f5-8a4e-5b27b3b8d55c')")
        .execute(db.pool())
        .await
        .unwrap();

    // Samples are written to the database directly (month boundaries use the server's +08
    // truncation expression, so the test agrees whichever day it runs).
    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            grant_label, uplink_bytes, downlink_bytes, has_gap
         )
         VALUES
            -- alice：两行，一行有缺口 → 40/60，has_gap
            (
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '1 minute') AT TIME ZONE 'Asia/Hong_Kong',
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '2 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                'n1', 'platform.acme', 'alice', 'i-main',
                'alice@platform.acme#i-main', 10, 20, FALSE
            ),
            (
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '3 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '4 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                'n1', 'platform.acme', 'alice', 'i-main',
                'alice@platform.acme#i-main', 30, 40, TRUE
            ),
            -- bob：一行，无缺口
            (
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '5 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '6 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                'n1', 'platform.acme', 'bob', 'i-main',
                'bob@platform.acme#i-main', 5, 7, FALSE
            ),
            -- charlie 在别的租户：子树裁剪后看不见
            (
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '7 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    + INTERVAL '8 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                'n-other', 'platform.other', 'charlie', 'i-other',
                'charlie@platform.other#i-other', 3, 4, FALSE
            ),
            -- 上个月的 alice：不该算进自然月
            (
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    - INTERVAL '1 month' + INTERVAL '1 minute') AT TIME ZONE 'Asia/Hong_Kong',
                (date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                    - INTERVAL '1 month' + INTERVAL '2 minutes') AT TIME ZONE 'Asia/Hong_Kong',
                'n1', 'platform.acme', 'alice', 'i-main',
                'alice@platform.acme#i-main', 999, 999, FALSE
            )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let system = db
        .store
        .list_monthly_usage_summary(&system_admin())
        .await
        .unwrap();
    // Month boundaries must equal the server's +08 truncation: the start is midnight on the 1st
    // and the end midnight on the 1st of the next month, as wall-clock strings (which do not
    // drift with the database session's timezone — under a UTC session the timestamptz text
    // renders as 16:00 on the 31st of the previous month, and the UI slices the wrong
    // month).
    let boundaries_ok: bool = sqlx::query_scalar(
        "SELECT $1 = to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong'),
                             'YYYY-MM-DD HH24:MI:SS')
            AND $2 = to_char(date_trunc('month', now() AT TIME ZONE 'Asia/Hong_Kong')
                             + INTERVAL '1 month', 'YYYY-MM-DD HH24:MI:SS')",
    )
    .bind(&system.month_start)
    .bind(&system.month_end)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        boundaries_ok,
        "month_start={} month_end={}",
        system.month_start, system.month_end
    );
    // The header string starts with the current HKT calendar month — this is the regression
    // assertion for the 2026-08 bug
    let month_ok: bool = sqlx::query_scalar(
        "SELECT $1 LIKE to_char(now() AT TIME ZONE 'Asia/Hong_Kong', 'YYYY-MM') || '%'",
    )
    .bind(&system.month_start)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(month_ok, "month_start 的月份不对: {}", system.month_start);
    assert_eq!(system.views.len(), 3);

    // alice has two sample rows on this month's view (app-main) → 40/60, has_gap
    let alice = system.views.iter().find(|r| r.user_id == "alice").unwrap();
    assert_eq!(alice.tenant_id, "platform.acme");
    assert_eq!(alice.app_id, "app-main");
    assert_eq!(alice.uplink_bytes, 40);
    assert_eq!(alice.downlink_bytes, 60);
    assert!(alice.has_gap);

    let bob = system.views.iter().find(|r| r.user_id == "bob").unwrap();
    assert_eq!(bob.app_id, "app-main");
    assert_eq!(bob.uplink_bytes, 5);
    assert_eq!(bob.downlink_bytes, 7);
    assert!(!bob.has_gap);

    // charlie is in a different tenant, on the app-other view
    let charlie = system
        .views
        .iter()
        .find(|r| r.user_id == "charlie")
        .unwrap();
    assert_eq!(charlie.app_id, "app-other");
    assert_eq!(charlie.uplink_bytes, 3);

    // Subtree scoping: platform.acme's publisher sees only their own tenant
    let acme = db
        .store
        .list_monthly_usage_summary(&publisher("platform.acme"))
        .await
        .unwrap();
    assert_eq!(acme.views.len(), 2);
    assert!(acme.views.iter().all(|r| r.tenant_id == "platform.acme"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_result_uses_claimed_grants_not_the_current_model() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "report-claimed-grants"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();

    insert_extra_user_grant(db.pool()).await;

    let result = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    assert_eq!(result.target_status, "succeeded");
    assert_eq!(result.deployment_status, "succeeded");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_success_advances_waves_and_final_success_releases_active() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "report-waves"),
        )
        .await
        .unwrap();
    let n1 = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let first = db
        .store
        .report_target_result(applied_report(&n1))
        .await
        .unwrap();
    assert_eq!(first.target_status, "succeeded");
    assert_eq!(first.deployment_status, "running");

    assert!(db
        .store
        .claim_desired_for_node("n2")
        .await
        .unwrap()
        .is_none());
    db.store
        .confirm_deployment_wave(
            &system_admin(),
            created.deployment_id,
            2,
            Some("tester".to_owned()),
        )
        .await
        .unwrap();

    let n2 = db
        .store
        .claim_desired_for_node("n2")
        .await
        .unwrap()
        .expect("second wave should be claimable after first wave succeeds");
    assert_eq!(n2.wave, 2);

    let second = db
        .store
        .report_target_result(applied_report(&n2))
        .await
        .unwrap();
    assert_eq!(second.target_status, "succeeded");
    assert_eq!(second.deployment_status, "succeeded");

    let deployment = sqlx::query("SELECT status, active FROM deployments WHERE id = $1")
        .bind(created.deployment_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        deployment.try_get::<String, _>("status").unwrap(),
        "succeeded"
    );
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_failure_halts_deployment_and_hides_later_waves() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "report-failure"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();

    let result = db
        .store
        .report_target_result(failed_recovered_report(&desired))
        .await
        .unwrap();
    assert_eq!(result.target_status, "failed-recovered");
    assert_eq!(result.deployment_status, "halted");

    let deployment = sqlx::query(
        "SELECT status, active, halted_at IS NOT NULL AS halted
         FROM deployments
         WHERE id = $1",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(deployment.try_get::<String, _>("status").unwrap(), "halted");
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert!(deployment.try_get::<bool, _>("halted").unwrap());

    let n2 = db.store.load_desired_for_node("n2").await.unwrap();
    assert!(
        n2.is_none(),
        "halted deployments should not expose later waves"
    );

    let blocked = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "report-failure-blocks"),
        )
        .await;
    assert!(
        blocked.is_err(),
        "halted active deployment should still block ordinary publish"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn report_target_applied_with_mismatched_observation_becomes_failed_dirty() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let desired = {
        db.store
            .create_deployment(
                &system_admin(),
                create_deployment_request(1, "report-dirty"),
            )
            .await
            .unwrap();
        db.store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .unwrap()
    };
    let mut report = applied_report(&desired);
    report.observed_after.xray = AppliedArtifactState::Dirty {
        reason: "xray config hash did not match after apply".to_owned(),
    };

    let result = db.store.report_target_result(report).await.unwrap();

    assert_eq!(result.target_status, "failed-dirty");
    assert_eq!(result.deployment_status, "halted");

    let applied = sqlx::query(
        "SELECT xray_state, xray_sha256
         FROM node_applied_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(applied.try_get::<String, _>("xray_state").unwrap(), "dirty");
    assert_eq!(
        applied.try_get::<Option<String>, _>("xray_sha256").unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retry_target_reopens_failed_target_and_makes_desired_visible_again() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "retry-target"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(failed_recovered_report(&desired))
        .await
        .unwrap();
    assert!(db
        .store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());

    let retry = db
        .store
        .retry_target(&system_admin(), created.deployment_id, "n1")
        .await
        .unwrap();
    assert_eq!(retry.target_status, "pending");
    assert_eq!(retry.deployment_status, "running");

    let desired = db.store.load_desired_for_node("n1").await.unwrap();
    assert!(
        desired.is_some(),
        "retried target should receive desired again"
    );

    let deployment = sqlx::query(
        "SELECT status, active, halted_at IS NULL AS halted_cleared
         FROM deployments
         WHERE id = $1",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        deployment.try_get::<String, _>("status").unwrap(),
        "running"
    );
    assert_eq!(
        deployment.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert!(deployment.try_get::<bool, _>("halted_cleared").unwrap());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cancel_deployment_cancels_open_targets_and_releases_active() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "cancel-deployment"),
        )
        .await
        .unwrap();

    let canceled = db
        .store
        .cancel_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();
    assert_eq!(canceled.status, "canceled");
    assert_eq!(canceled.active, None);
    assert_eq!(canceled.sync_deployment_id, None);
    assert_eq!(canceled.rollback_deployment_id, None);
    assert!(db
        .store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());
    assert!(db
        .store
        .load_desired_for_node("n2")
        .await
        .unwrap()
        .is_none());

    let open_targets: i64 = sqlx::query(
        "SELECT count(*) AS n
         FROM deployment_targets
         WHERE deployment_id = $1
           AND status IN ('pending', 'dispatched', 'converging')",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(open_targets, 0);
    assert_eq!(
        deployment_count(db.pool()).await,
        1,
        "pure pending cancel should not create a state-sync work order"
    );

    let next = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "cancel-allows-next"),
        )
        .await;
    assert!(
        next.is_ok(),
        "cancel should release active so ordinary publish can be created"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cancel_deployment_stops_dispatched_target_without_state_sync_work_order() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "cancel-dispatched-sync"),
        )
        .await
        .unwrap();
    let claimed = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("n1 should be dispatched before cancel");
    assert_eq!(claimed.deployment_id, created.deployment_id);

    let canceled = db
        .store
        .cancel_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();

    assert_eq!(canceled.status, "canceled");
    assert_eq!(canceled.active, None);
    assert_eq!(canceled.sync_deployment_id, None);
    assert_eq!(canceled.rollback_deployment_id, None);
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n1").await,
        "canceled"
    );
    assert_eq!(
        deployment_count(db.pool()).await,
        1,
        "cancel should stop the current deployment without creating a follow-up work order"
    );

    let dirty = sqlx::query(
        "SELECT phantun_state, wireguard_state, xray_state, grants_state, source_deployment_id
         FROM node_applied_state
         WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        dirty.try_get::<String, _>("phantun_state").unwrap(),
        "dirty"
    );
    assert_eq!(
        dirty.try_get::<String, _>("wireguard_state").unwrap(),
        "dirty"
    );
    assert_eq!(dirty.try_get::<String, _>("xray_state").unwrap(), "dirty");
    assert_eq!(dirty.try_get::<String, _>("grants_state").unwrap(), "dirty");
    assert_eq!(
        dirty
            .try_get::<Option<i64>, _>("source_deployment_id")
            .unwrap(),
        Some(created.deployment_id)
    );

    let desired = db.store.load_desired_for_node("n1").await.unwrap();
    assert!(
        desired.is_none(),
        "plain cancel should not leave a follow-up desired state for the agent"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cancel_deployment_after_partial_success_does_not_create_state_sync_work_order() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_second_node(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "cancel-partial-success-sync"),
        )
        .await
        .unwrap();
    let n1 = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let report = db
        .store
        .report_target_result(applied_report(&n1))
        .await
        .unwrap();
    assert_eq!(report.target_status, "succeeded");

    let canceled = db
        .store
        .cancel_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();

    assert_eq!(canceled.status, "canceled");
    assert_eq!(canceled.sync_deployment_id, None);
    assert_eq!(canceled.rollback_deployment_id, None);
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n1").await,
        "succeeded"
    );
    assert_eq!(
        target_status(db.pool(), created.deployment_id, "n2").await,
        "canceled"
    );
    assert_eq!(
        deployment_count(db.pool()).await,
        1,
        "plain cancel should not create a state-sync work order after partial success"
    );
    assert!(db
        .store
        .load_desired_for_node("n2")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cancel_deployment_and_rollback_restores_latest_succeeded_snapshot_and_force_syncs() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let first_identity = db.store.materialize_snapshot(None).await.unwrap().apps[0].ingresses[0]
        .identity
        .clone();

    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "cancel-rollback-first"),
        )
        .await
        .unwrap();
    let first_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let first_xray_sha = artifact_sha(&first_desired.desired.xray).to_owned();
    db.store
        .report_target_result(applied_report(&first_desired))
        .await
        .unwrap();
    assert_eq!(
        deployment_status(db.pool(), first.deployment_id).await,
        "succeeded"
    );

    // Certificate state changes outside revisions. Issue one only after the rollback target was
    // recorded: the TLS revision needs it, and rolling back must not erase it.
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let domain = db
        .store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "rollback.example.test".to_owned(),
                dns_credential: Some("token".to_owned()),
                acme_directory: Some(brocade_store::ACME_LETSENCRYPT.to_owned()),
                acme_contact: Some("ops@example.test".to_owned()),
                renew_before_days: Some(30),
            },
        )
        .await
        .unwrap();
    let _ = &domain;
    issue_certificate_for(&db, "n1", "Test CA").await;

    let second_revision = insert_revision(db.pool(), "cancel rollback test edit").await;
    sqlx::query(
        "UPDATE ingresses
            SET port = 8443, transport_kind = 'vless-tls',
                reality_private_key = 'changed-private',
                reality_public_key = 'changed-public',
                reality_short_ids = '[\"deadbeef\"]'::jsonb,
                created_revision = $1
          WHERE id = 'i-main'",
    )
    .bind(second_revision)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE control_state SET current_revision = $1 WHERE id = TRUE")
        .bind(second_revision)
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;

    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(second_revision as u64, "cancel-rollback-second"),
        )
        .await
        .unwrap();
    let second_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("second deployment should be dispatched before cancel-and-rollback");
    assert_ne!(
        artifact_sha(&second_desired.desired.xray),
        first_xray_sha,
        "fixture update should create a different xray structure"
    );

    let command = db
        .store
        .cancel_deployment_and_rollback(&system_admin(), second.deployment_id)
        .await
        .unwrap();
    let rollback_id = command
        .rollback_deployment_id
        .expect("cancel-and-rollback should create a rollback work order");

    assert_eq!(command.status, "canceled");
    assert_eq!(command.active, None);
    assert_eq!(command.sync_deployment_id, None);
    assert_eq!(
        deployment_count(db.pool()).await,
        3,
        "cancel-and-rollback should create exactly one follow-up rollback work order"
    );
    assert_eq!(
        target_status(db.pool(), second.deployment_id, "n1").await,
        "canceled"
    );
    assert_eq!(target_status(db.pool(), rollback_id, "n1").await, "pending");

    let rollback_row = sqlx::query(
        "SELECT active, rollback_of_deployment_id
         FROM deployments
         WHERE id = $1",
    )
    .bind(rollback_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        rollback_row.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert_eq!(
        rollback_row
            .try_get::<Option<i64>, _>("rollback_of_deployment_id")
            .unwrap(),
        Some(first.deployment_id)
    );

    let restored = db.store.materialize_snapshot(None).await.unwrap();
    assert!(restored.revision > second_revision as u64);
    assert_eq!(restored.apps[0].ingresses[0].port, 443);
    assert_eq!(restored.apps[0].ingresses[0].identity, first_identity);
    assert_eq!(
        restored.apps[0].ingresses[0].wires.vless_kind(),
        Some("vless-reality")
    );
    assert!(restored.nodes[0].certificate_name.is_some());

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("rollback deployment should be visible to the agent");
    assert_eq!(desired.deployment_id, rollback_id);
    assert_eq!(artifact_sha(&desired.desired.xray), first_xray_sha);
    assert!(
        desired.actions.contains(&PlannedAction::ApplyXray),
        "cancel-and-rollback sync is forced even when remembered state may already match"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn rollback_deployment_restores_target_snapshot_then_force_syncs_when_active_target_is_in_flight(
) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let target_snapshot = db.store.materialize_snapshot(None).await.unwrap();

    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "rollback-first"),
        )
        .await
        .unwrap();
    let first_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    let first_xray_sha = artifact_sha(&first_desired.desired.xray).to_owned();
    db.store
        .report_target_result(applied_report(&first_desired))
        .await
        .unwrap();

    let second_revision = insert_revision(db.pool(), "rollback test edit").await;
    sqlx::query("UPDATE ingresses SET port = 8443, created_revision = $1 WHERE id = 'i-main'")
        .bind(second_revision)
        .execute(db.pool())
        .await
        .unwrap();
    // Exercise every family the old rollback writer omitted. Connection and geodata affect
    // artifacts; ports and probe settings affect future allocation/agent work and still belong to
    // the revisioned model. Node connection overrides were omitted by the second restore writer.
    sqlx::query(
        "UPDATE control_state SET
            current_revision = $1,
            geodata_cron = 'CRON_TZ=UTC 1 2 3 4 5',
            geodata_geoip_url = 'https://changed.example/geoip.dat',
            geodata_geosite_url = 'https://changed.example/geosite.dat',
            conn_idle_secs = 901,
            conn_uplink_only_secs = 12,
            conn_downlink_only_secs = 13,
            conn_buffer_size_kb = 64,
            conn_handshake_secs = 61,
            stats_user_online = TRUE,
            port_ingress_base = 9443,
            port_anytls_base = 16123,
            port_hop_base = 31000,
            port_hy2_base = 41000,
            probe_endpoint_url = 'http://changed.example/trace',
            probe_timeout_secs = 19,
            probe_interval_secs = 91
         WHERE id = TRUE",
    )
    .bind(second_revision)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE nodes SET
            conn_idle_secs = 777,
            conn_uplink_only_secs = 21,
            conn_downlink_only_secs = 22,
            conn_buffer_size_kb = 32
         WHERE id = 'n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;

    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(second_revision as u64, "rollback-second"),
        )
        .await
        .unwrap();
    let second_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("second deployment should be dispatched before rollback");
    assert_ne!(
        artifact_sha(&second_desired.desired.xray),
        first_xray_sha,
        "fixture update should create a different xray structure"
    );

    // Rollback itself also owns a transaction while loading historical snapshots and applied
    // state. One connection proves none of those reads silently goes back to the pool.
    let rollback_pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(1))
        .connect(&db.url)
        .await
        .unwrap();
    let rollback_store = PgStore::from_pool(rollback_pool);
    let rollback = rollback_store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(first.deployment_id, "rollback-to-first"),
        )
        .await
        .unwrap();

    assert_eq!(
        deployment_count(db.pool()).await,
        3,
        "rollback should cancel the active deployment directly, not create an extra sync work order"
    );
    assert_eq!(
        target_status(db.pool(), second.deployment_id, "n1").await,
        "canceled"
    );
    assert_eq!(rollback.status, "planned");
    assert_eq!(rollback.plan.summary.changed_targets, 1);
    assert_eq!(rollback.plan.summary.skipped_targets, 0);
    assert!(
        rollback.plan.targets[0]
            .actions
            .contains(&PlannedAction::ApplyXray),
        "rollback sync is forced; xray should be applied even when the remembered state may already match"
    );
    assert_eq!(
        target_status(db.pool(), rollback.deployment_id, "n1").await,
        "pending"
    );

    let rollback_row = sqlx::query(
        "SELECT active, rollback_of_deployment_id
         FROM deployments
         WHERE id = $1",
    )
    .bind(rollback.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        rollback_row.try_get::<Option<bool>, _>("active").unwrap(),
        Some(true)
    );
    assert_eq!(
        rollback_row
            .try_get::<Option<i64>, _>("rollback_of_deployment_id")
            .unwrap(),
        Some(first.deployment_id)
    );
    assert!(
        rollback.plan.revision > second_revision as u64,
        "rollback should restore the target snapshot as a new model revision"
    );
    let restored = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(restored.revision, rollback.plan.revision);
    let mut expected = target_snapshot;
    expected.revision = restored.revision;
    assert_eq!(
        restored, expected,
        "rollback must restore the complete model"
    );

    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("rollback deployment should be visible to the agent");
    assert_eq!(desired.deployment_id, rollback.deployment_id);
    assert_eq!(artifact_sha(&desired.desired.xray), first_xray_sha);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn discard_pending_changes_restores_settings_and_node_connection_policy() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO apps (id, label, position)
         VALUES ('app-secondary', 'Secondary App', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-secondary', 'app-main', 'platform.acme', 'Secondary Chain', 1)",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode
         )
         SELECT 'i-secondary', app_id, 'c-secondary', node_id, bind, 444, front_id,
                transport_kind, reality_private_key || '-secondary',
                reality_public_key || '-secondary', reality_short_ids,
                reality_dest, reality_server_names, reality_flow,
                reality_fallback_mode
         FROM ingresses
         WHERE id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules)
         SELECT 'c-secondary', node_id, rules FROM steps WHERE chain_id = 'c-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-main', 'platform.acme', 'alice', 'i-secondary')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;
    let target_snapshot = db.store.materialize_snapshot(None).await.unwrap();

    let published = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "discard-complete-restore-base"),
        )
        .await
        .unwrap();
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();
    assert_eq!(
        deployment_status(db.pool(), published.deployment_id).await,
        "succeeded"
    );

    let reordered = db
        .store
        .apply_draft(
            &system_admin(),
            vec![
                ModelOp::ReorderApps {
                    ids: vec!["app-secondary".to_owned(), "app-main".to_owned()],
                },
                ModelOp::ReorderChains {
                    app_id: "app-main".to_owned(),
                    ids: vec!["c-secondary".to_owned(), "c-main".to_owned()],
                },
            ],
            None,
        )
        .await
        .unwrap();
    assert!(reordered.revision_id > 1);
    let pending_order: Vec<String> = sqlx::query_scalar("SELECT id FROM apps ORDER BY position")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(pending_order, ["app-secondary", "app-main"]);
    let pending_chain_order: Vec<String> =
        sqlx::query_scalar("SELECT id FROM chains WHERE app_id = 'app-main' ORDER BY position, id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(pending_chain_order, ["c-secondary", "c-main"]);

    let mut changed_settings = target_snapshot.settings.clone();
    changed_settings.geodata.cron = "CRON_TZ=UTC 15 3 * * *".to_owned();
    changed_settings.geodata.geoip_url = "https://changed.example/geoip.dat".to_owned();
    changed_settings.connection.conn_idle_secs = 901;
    changed_settings.connection.buffer_size_kb = Some(64);
    changed_settings.stats_user_online = !changed_settings.stats_user_online;
    db.store
        .update_settings(&system_admin(), changed_settings)
        .await
        .unwrap();
    let changed_node = db
        .store
        .update_node(
            &system_admin(),
            "n1",
            UpdateNodeRequest {
                connection: Some(NodeConnection {
                    conn_idle_secs: Some(777),
                    uplink_only_secs: Some(7),
                    downlink_only_secs: Some(8),
                    buffer_size_kb: Some(32),
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let discarded = db
        .store
        .discard_pending_changes(&system_admin(), changed_node.revision_id)
        .await
        .unwrap();
    let restored = db.store.materialize_snapshot(None).await.unwrap();
    let mut expected = target_snapshot;
    expected.revision = discarded.current_revision;
    assert_eq!(
        restored, expected,
        "discard must restore the complete model"
    );
    let restored_order: Vec<&str> = restored.apps.iter().map(|app| app.id.as_str()).collect();
    assert_eq!(restored_order, ["app-main", "app-secondary"]);
    let restored_chain_order: Vec<&str> = restored.apps[0]
        .chains
        .iter()
        .map(|chain| chain.id.as_str())
        .collect();
    assert_eq!(restored_chain_order, ["c-main", "c-secondary"]);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn rollback_deployment_preserves_usage_history_referencing_restored_model_objects() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "rollback-usage-first"),
        )
        .await
        .unwrap();
    let first_desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&first_desired))
        .await
        .unwrap();
    insert_usage_history_for_main_fixture(db.pool()).await;

    let second_revision = insert_revision(db.pool(), "rollback usage edit").await;
    sqlx::query("UPDATE ingresses SET port = 8443, created_revision = $1 WHERE id = 'i-main'")
        .bind(second_revision)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE control_state SET current_revision = $1 WHERE id = TRUE")
        .bind(second_revision)
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;

    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(second_revision as u64, "rollback-usage-second"),
        )
        .await
        .unwrap();
    db.store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("second deployment should be active before rollback");

    let rollback = db
        .store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(first.deployment_id, "rollback-usage-to-first"),
        )
        .await
        .unwrap();

    assert_eq!(
        target_status(db.pool(), second.deployment_id, "n1").await,
        "canceled"
    );
    assert_eq!(
        target_status(db.pool(), rollback.deployment_id, "n1").await,
        "pending"
    );
    assert_eq!(usage_sample_count(db.pool()).await, 1);
    assert_eq!(usage_chain_sample_count(db.pool()).await, 1);
    let restored = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(restored.apps[0].ingresses[0].id, "i-main");
    assert_eq!(restored.apps[0].ingresses[0].port, 443);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn halt_deployment_hides_desired_without_canceling_targets() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "halt-deployment"),
        )
        .await
        .unwrap();
    let halted = db
        .store
        .halt_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();
    assert_eq!(halted.status, "halted");
    assert_eq!(halted.active, Some(true));
    assert!(db
        .store
        .load_desired_for_node("n1")
        .await
        .unwrap()
        .is_none());

    let target_status: String = sqlx::query(
        "SELECT status
         FROM deployment_targets
         WHERE deployment_id = $1 AND node_id = 'n1'",
    )
    .bind(created.deployment_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("status")
    .unwrap();
    assert_eq!(target_status, "pending");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn migration_enforces_reality_short_id_shape() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    assert_insert_ingress_short_ids_fails(db.pool(), "i-empty", 444, json!([])).await;
    assert_insert_ingress_short_ids_fails(db.pool(), "i-non-string", 445, json!([123])).await;
    assert_insert_ingress_short_ids_fails(db.pool(), "i-non-hex", 446, json!(["not-hex"])).await;
    assert_insert_ingress_short_ids_fails(db.pool(), "i-odd-length", 449, json!(["abc"])).await;
    assert_insert_ingress_short_ids_fails(
        db.pool(),
        "i-too-long",
        447,
        json!(["0123456789abcdef00"]),
    )
    .await;

    let generated = generate_reality_short_id().unwrap();
    assert!(is_reality_short_id(&generated));
    insert_ingress_short_ids(db.pool(), "i-generated", 448, json!([generated]))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn migration_enforces_reality_client_policy_shape() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let bad_min = sqlx::query(
        "UPDATE control_state
         SET reality_min_client_ver = '1.x.0'
         WHERE id = TRUE",
    )
    .execute(db.pool())
    .await;
    assert!(bad_min.is_err(), "invalid min client version should fail");

    let bad_range = sqlx::query(
        "UPDATE control_state
         SET reality_min_client_ver = '1.10.0',
             reality_max_client_ver = '1.9.9'
         WHERE id = TRUE",
    )
    .execute(db.pool())
    .await;
    assert!(bad_range.is_err(), "min > max should fail");

    let bad_time = sqlx::query(
        "UPDATE control_state
         SET reality_max_time_diff_ms = 86400001
         WHERE id = TRUE",
    )
    .execute(db.pool())
    .await;
    assert!(bad_time.is_err(), "excessive max time diff should fail");
}

async fn base_of(pool: &PgPool, deployment_id: i64) -> Option<i64> {
    sqlx::query("SELECT base_revision_id FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get::<Option<i64>, _>("base_revision_id")
        .unwrap()
}

/// Where only the fact that a deployment became the serving baseline is needed, the state is
/// written directly. Execution success alone is deliberately insufficient after activation and
/// settlement became separate dimensions.
async fn mark_succeeded(pool: &PgPool, deployment_id: i64) {
    sqlx::query(
        "UPDATE deployments
            SET status = 'succeeded',
                active = NULL,
                activation_status = 'activated',
                activated_at = now()
          WHERE id = $1",
    )
    .bind(deployment_id)
    .execute(pool)
    .await
    .unwrap();
}

fn create_deployment_request(revision_id: u64, idempotency_key: &str) -> CreateDeploymentRequest {
    CreateDeploymentRequest {
        revision_id,
        idempotency_key: idempotency_key.to_owned(),
        actor: Some("tester".to_owned()),
        note: None,
        // A configuration deployment: the button somebody presses on the release page. Grants
        // deployments are constructed separately.
        kind: DeploymentKind::Config,
    }
}

fn create_grants_deployment_request(
    revision_id: u64,
    idempotency_key: &str,
) -> CreateDeploymentRequest {
    CreateDeploymentRequest {
        kind: DeploymentKind::Grants,
        ..create_deployment_request(revision_id, idempotency_key)
    }
}

fn assert_no_alice(grants: &DesiredGrants) {
    let DesiredGrants::Present { inbounds } = grants else {
        panic!("权限目标应该是已启用的入站客户端列表");
    };
    assert!(
        inbounds
            .iter()
            .flat_map(|inbound| &inbound.clients)
            .all(|client| client.email != "alice@platform.acme#i-main"),
        "权限目标仍带着已撤销的 Alice"
    );
}

fn create_rollback_request(
    target_deployment_id: i64,
    idempotency_key: &str,
) -> CreateRollbackRequest {
    CreateRollbackRequest {
        target_deployment_id,
        idempotency_key: idempotency_key.to_owned(),
        actor: Some("tester".to_owned()),
        note: None,
    }
}

/// A release with no note gets a sentence from the server about the deployment itself.
///
/// The console used to fill this in as `console · revision 71` — repeating a number already
/// displayed to its right, so that every row of the release list read identically while the list
/// is precisely what distinguishes one deployment from another by its note.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn deployment_note_says_what_changed_and_how_many_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    async fn note_of(db: &TestPg, deployment_id: i64) -> String {
        sqlx::query("SELECT note FROM deployments WHERE id = $1")
            .bind(deployment_id)
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get::<Option<String>, _>("note")
            .unwrap()
            .unwrap_or_default()
    }

    // Stamp a revision whose note is the draft layer's assembled `提交：机器 n1`.
    let applied = db
        .store
        .apply_draft(
            &system_admin(),
            vec![brocade_store::ModelOp::UpdateNode {
                node_id: "n1".to_owned(),
                node: UpdateNodeRequest {
                    name: Some("香港入口".to_owned()),
                    ..Default::default()
                },
            }],
            None,
        )
        .await
        .unwrap();

    let created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(applied.revision_id, "deploy-note-auto"),
        )
        .await
        .unwrap();
    assert_eq!(
        note_of(&db, created.deployment_id).await,
        "机器 n1 · 1 台",
        "改了什么（去掉草稿层的「提交：」）· 有产物变更的机器数"
    );

    // Notes people wrote are kept verbatim — rewriting somebody's note is worse than writing
    // none.
    let mut request = create_deployment_request(applied.revision_id, "deploy-note-written");
    request.note = Some("  港新线路换落地  ".to_owned());
    // The previous deployment on this revision still holds the single-flight lock; clear it
    // first.
    db.store
        .cancel_deployment(&system_admin(), created.deployment_id)
        .await
        .unwrap();
    let written = db
        .store
        .create_deployment(&system_admin(), request)
        .await
        .unwrap();
    assert_eq!(
        note_of(&db, written.deployment_id).await,
        "港新线路换落地",
        "人写的话只收两头空白，不动内容"
    );
}

fn find_target<'a>(plan: &'a DeploymentPlan, node_id: &str) -> &'a PlannedTarget {
    plan.targets
        .iter()
        .find(|target| target.node_id == node_id)
        .unwrap()
}

fn artifact_sha(artifact: &DesiredArtifact) -> &str {
    let DesiredArtifact::Present { sha256, .. } = artifact else {
        panic!("expected present artifact");
    };
    sha256
}

fn artifact_content(artifact: &DesiredArtifact) -> &str {
    let DesiredArtifact::Present { content, .. } = artifact else {
        panic!("expected present artifact");
    };
    content
}

/// The shape that lands in `node_applied_state.grants_observed`: it carries `(email, uuid, flow)`
/// and no level.
fn grants_observed(grants: &DesiredGrants) -> serde_json::Value {
    match grants {
        DesiredGrants::Present { .. } => {
            let AppliedGrantsState::Present { inbounds } = reported_grants_from_desired(grants)
            else {
                unreachable!("Present 映射过去还是 Present")
            };
            json!({ "inbounds": inbounds })
        }
        DesiredGrants::Disabled { .. } | DesiredGrants::Unmanaged { .. } => {
            json!({ "inbounds": [] })
        }
    }
}

fn applied_report(desired: &NodeDesiredDeployment) -> TargetConvergenceReport {
    TargetConvergenceReport {
        claim_generation: desired.claim_generation,
        deployment_id: desired.deployment_id,
        node_id: desired.node_id.clone(),
        result: TargetApplyResult::Applied,
        observed_before: unknown_reported_state(),
        observed_after: reported_state_from_desired(desired),
        error: None,
        usage_activated_at_unix_secs: None,
    }
}

fn failed_recovered_report(desired: &NodeDesiredDeployment) -> TargetConvergenceReport {
    let recovered = unknown_reported_state();
    TargetConvergenceReport {
        claim_generation: desired.claim_generation,
        deployment_id: desired.deployment_id,
        node_id: desired.node_id.clone(),
        result: TargetApplyResult::FailedRecovered,
        observed_before: recovered.clone(),
        observed_after: recovered,
        error: Some("apply failed and local rollback restored previous state".to_owned()),
        usage_activated_at_unix_secs: None,
    }
}

fn reported_state_from_desired(desired: &NodeDesiredDeployment) -> ReportedNodeState {
    ReportedNodeState {
        phantun: reported_artifact_from_desired(&desired.desired.phantun),
        hy2_port_hop: reported_artifact_from_desired(&desired.desired.hy2_port_hop),
        wireguard: reported_artifact_from_desired(&desired.desired.wireguard),
        xray: reported_artifact_from_desired(&desired.desired.xray),
        grants: reported_grants_from_desired(&desired.desired.grants),
    }
}

fn reported_artifact_from_desired(artifact: &DesiredArtifact) -> AppliedArtifactState {
    match artifact {
        DesiredArtifact::Present { sha256, .. } => AppliedArtifactState::Present {
            sha256: sha256.clone(),
        },
        DesiredArtifact::Disabled { .. } => AppliedArtifactState::Disabled,
        DesiredArtifact::Unmanaged { .. } => AppliedArtifactState::Unmanaged,
    }
}

/// The observation an agent reports after converging. It must be lossy — `inbounduser` supplies
/// `(email, uuid, flow)` and no level. The fixture's ingress carries `xtls-rprx-vision`, so
/// copying the expectation wholesale here would leave a mistaken comparison on the judging side
/// untested.
fn reported_grants_from_desired(grants: &DesiredGrants) -> AppliedGrantsState {
    match grants {
        DesiredGrants::Present { inbounds } => AppliedGrantsState::Present {
            inbounds: inbounds
                .iter()
                .map(|inbound| ObservedInbound {
                    tag: inbound.tag.clone(),
                    clients: inbound
                        .clients
                        .iter()
                        .map(|client| ObservedClient {
                            email: client.email.clone(),
                            uuid: client.uuid.clone(),
                            flow: client.flow.clone(),
                        })
                        .collect(),
                })
                .collect(),
        },
        DesiredGrants::Disabled { .. } => AppliedGrantsState::Disabled,
        DesiredGrants::Unmanaged { .. } => AppliedGrantsState::Unmanaged,
    }
}

fn unknown_reported_state() -> ReportedNodeState {
    ReportedNodeState {
        hy2_port_hop: AppliedArtifactState::Unmanaged,
        phantun: AppliedArtifactState::Unmanaged,
        wireguard: AppliedArtifactState::Unknown,
        xray: AppliedArtifactState::Unknown,
        grants: AppliedGrantsState::Unknown,
    }
}

async fn deployment_count(pool: &PgPool) -> i64 {
    sqlx::query("SELECT count(*) AS n FROM deployments")
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
}

async fn usage_sample_count(pool: &PgPool) -> i64 {
    sqlx::query("SELECT count(*) AS n FROM usage_samples")
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
}

async fn usage_chain_sample_count(pool: &PgPool) -> i64 {
    sqlx::query("SELECT count(*) AS n FROM usage_chain_samples")
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get("n")
        .unwrap()
}

async fn deployment_status(pool: &PgPool, deployment_id: i64) -> String {
    sqlx::query("SELECT status FROM deployments WHERE id = $1")
        .bind(deployment_id)
        .fetch_one(pool)
        .await
        .unwrap()
        .try_get("status")
        .unwrap()
}

async fn insert_revision(pool: &PgPool, note: &str) -> i64 {
    sqlx::query(
        "INSERT INTO revisions (author, note)
         VALUES ('test-system', $1)
         RETURNING id",
    )
    .bind(note)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("id")
    .unwrap()
}

async fn store_current_model_snapshot(pool: &PgPool, store: &PgStore) {
    let snapshot = store.materialize_snapshot(None).await.unwrap();
    let revision_id = i64::try_from(snapshot.revision).unwrap();
    let snapshot = serde_json::to_value(snapshot).unwrap();
    sqlx::query(
        "INSERT INTO model_snapshots (revision_id, snapshot)
         VALUES ($1, $2)
         ON CONFLICT (revision_id) DO UPDATE SET
            snapshot = EXCLUDED.snapshot",
    )
    .bind(revision_id)
    .bind(snapshot)
    .execute(pool)
    .await
    .unwrap();
}

async fn target_status(pool: &PgPool, deployment_id: i64, node_id: &str) -> String {
    sqlx::query(
        "SELECT status
         FROM deployment_targets
         WHERE deployment_id = $1 AND node_id = $2",
    )
    .bind(deployment_id)
    .bind(node_id)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("status")
    .unwrap()
}

async fn insert_matching_applied_state(pool: &PgPool, node_id: &str, target: &PlannedTarget) {
    let (phantun_state, phantun_sha256, phantun_observed) =
        matching_artifact_state(&target.desired.phantun);
    let (wireguard_state, wireguard_sha256, wireguard_observed) =
        matching_artifact_state(&target.desired.wireguard);
    let (xray_state, xray_sha256, xray_observed) = matching_artifact_state(&target.desired.xray);
    let (hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed) =
        matching_artifact_state(&target.desired.hy2_port_hop);
    let (grants_state, grants_observed) = matching_grants_state(&target.desired.grants);

    sqlx::query(
        "INSERT INTO node_applied_state (
            node_id,
            phantun_state, phantun_sha256, phantun_observed,
            wireguard_state, wireguard_sha256, wireguard_observed,
            xray_state, xray_sha256, xray_observed,
            hy2_port_hop_state, hy2_port_hop_sha256, hy2_port_hop_observed,
            grants_state, grants_observed,
            observed_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, now())",
    )
    .bind(node_id)
    .bind(phantun_state)
    .bind(phantun_sha256)
    .bind(phantun_observed)
    .bind(wireguard_state)
    .bind(wireguard_sha256)
    .bind(wireguard_observed)
    .bind(xray_state)
    .bind(xray_sha256)
    .bind(xray_observed)
    .bind(hy2_port_hop_state)
    .bind(hy2_port_hop_sha256)
    .bind(hy2_port_hop_observed)
    .bind(grants_state)
    .bind(grants_observed)
    .execute(pool)
    .await
    .unwrap();
}

fn matching_artifact_state(
    artifact: &DesiredArtifact,
) -> (&'static str, Option<String>, serde_json::Value) {
    match artifact {
        DesiredArtifact::Present { sha256, .. } => (
            "present",
            Some(sha256.clone()),
            serde_json::to_value(AppliedArtifactState::Present {
                sha256: sha256.clone(),
            })
            .unwrap(),
        ),
        DesiredArtifact::Disabled { .. } => (
            "disabled",
            None,
            serde_json::to_value(AppliedArtifactState::Disabled).unwrap(),
        ),
        DesiredArtifact::Unmanaged { .. } => (
            "unmanaged",
            None,
            serde_json::to_value(AppliedArtifactState::Unmanaged).unwrap(),
        ),
    }
}

fn matching_grants_state(grants: &DesiredGrants) -> (&'static str, serde_json::Value) {
    match grants {
        DesiredGrants::Present { .. } => ("present", grants_observed(grants)),
        DesiredGrants::Disabled { .. } => (
            "disabled",
            serde_json::to_value(AppliedGrantsState::Disabled).unwrap(),
        ),
        DesiredGrants::Unmanaged { .. } => (
            "unmanaged",
            serde_json::to_value(AppliedGrantsState::Unmanaged).unwrap(),
        ),
    }
}

async fn assert_table_exists(pool: &PgPool, table: &str) {
    let count: i64 = sqlx::query(
        "SELECT count(*) AS n
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = $1",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(count, 1, "missing table {table}");
}

async fn assert_table_missing(pool: &PgPool, table: &str) {
    let count: i64 = sqlx::query(
        "SELECT count(*) AS n
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = $1",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(count, 0, "unexpected table {table}");
}

async fn assert_column_exists(pool: &PgPool, table: &str, column: &str) {
    assert_column_count(pool, table, column, 1).await;
}

async fn assert_column_missing(pool: &PgPool, table: &str, column: &str) {
    assert_column_count(pool, table, column, 0).await;
}

async fn assert_column_count(pool: &PgPool, table: &str, column: &str, expected: i64) {
    let count: i64 = sqlx::query(
        "SELECT count(*) AS n
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(
        count, expected,
        "unexpected column count for {table}.{column}"
    );
}

async fn assert_constraint_missing(pool: &PgPool, constraint: &str) {
    let count: i64 = sqlx::query(
        "SELECT count(*) AS n
         FROM pg_constraint
         WHERE conname = $1",
    )
    .bind(constraint)
    .fetch_one(pool)
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(count, 0, "unexpected constraint {constraint}");
}

async fn insert_second_node(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            'n2', 'platform.acme', 'Node 2', 'n2.example.net', '10.66.0.2',
            'wg-private-n2', 'wg-public-n2', 51821,
            10086, TRUE, TRUE,
            'system', '[]'::jsonb
         )",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_basic_node(
    pool: &PgPool,
    id: &str,
    tenant_id: &str,
    overlay_addr: &str,
    port: i32,
) {
    sqlx::query(
        "INSERT INTO nodes (
            id, tenant_id, name, public_ipv4, overlay_addr,
            wg_private_key, wg_public_key, wg_listen_port,
            api_port, overlay, egress_allowed,
            dns_kind, dns_servers
         ) VALUES (
            $1, $2, $1, $3, $4::inet,
            $5, $6, $7,
            10085, TRUE, TRUE,
            'system', '[]'::jsonb
         )",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(format!("{id}.example.net"))
    .bind(overlay_addr)
    .bind(format!("wg-private-{id}"))
    .bind(format!("wg-public-{id}"))
    .bind(port)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn deep_network_observation_round_trips_as_one_optional_window_detail() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform', 'Platform')")
        .execute(db.pool())
        .await
        .unwrap();
    insert_basic_node(db.pool(), "net-observe", "platform", "10.66.0.90", 51990).await;
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let network = NetworkDetailSample {
        tcp_curr_estab: Some(82),
        tcp_inuse: Some(95),
        tcp_time_wait: Some(31),
        tcp_orphan: Some(0),
        tcp_alloc: Some(142),
        tcp_mem_bytes: Some(512 * 1024),
        udp_inuse: Some(18),
        udp_mem_bytes: Some(64 * 1024),
        tcp_active_opens: Some(41),
        tcp_passive_opens: Some(36),
        tcp_attempt_fails: Some(2),
        tcp_estab_resets: Some(1),
        tcp_retrans_segs: Some(3),
        tcp_syn_retrans: Some(1),
        tcp_in_errors: Some(0),
        tcp_out_resets: Some(2),
        tcp_timeouts: Some(0),
        tcp_listen_overflows: Some(0),
        tcp_listen_drops: Some(0),
        udp_in_errors: Some(0),
        udp_no_ports: Some(1),
        udp_rcvbuf_errors: Some(0),
        udp_sndbuf_errors: Some(0),
        ..Default::default()
    };
    let disk = DiskDetailSample {
        total_bytes: Some(8 * 1024 * 1024),
        inode_total: Some(100_000),
        inode_free: Some(90_000),
        read_bps: Some(2048),
        write_bps: Some(4096),
        read_iops: Some(1.5),
        write_iops: Some(2.5),
        read_await_ms: Some(0.8),
        write_await_ms: Some(1.2),
        busy_pct: Some(12.0),
        queue_depth: Some(0.4),
        in_flight: Some(1),
        pressure_some_pct: Some(0.2),
        pressure_full_pct: Some(0.0),
    };
    let sample = LoadSample {
        window_start_unix_secs: now - 30,
        window_end_unix_secs: now,
        has_gap: false,
        cpu_user_pct: 1.0,
        cpu_sys_pct: 1.0,
        cpu_softirq_pct: 1.0,
        cpu_peak_pct: 4.0,
        cpu_steal_pct: 0.0,
        load1: 0.1,
        cpu_detail: None,
        mem_available_bytes: 1024,
        swap_used_bytes: 0,
        memory_detail: None,
        oom_kills: 0,
        disk_free_bytes: 2048,
        disk_inode_free_pct: 99.0,
        disk_detail: Some(disk.clone()),
        nic_rx_bps: 100,
        nic_tx_bps: 200,
        nic_rx_drop: 0,
        nic_tx_drop: 0,
        nic_err: 0,
        conntrack_count: Some(1200),
        network_detail: Some(network.clone()),
        uptime_secs: 3600,
    };
    let report = LoadReportRequest {
        read_at_unix_secs: now,
        btime_unix_secs: now - 3600,
        host: HostFacts {
            kernel: "6.8.0".to_owned(),
            cpu_model: "test".to_owned(),
            cores: 2,
            cpu_freq_max_mhz: None,
            cpu_governor: None,
            cc_algo: "bbr".to_owned(),
            available_cc: vec!["bbr".to_owned()],
            default_qdisc: "fq".to_owned(),
            nic_qdisc: "fq".to_owned(),
            nic: "eth0".to_owned(),
            nic_mtu: Some(1500),
            mem_total_bytes: 4096,
            disk_total_bytes: 8192,
            disk_mount: None,
            disk_filesystem: None,
            disk_device: None,
            disk_read_only: None,
            conntrack_max: Some(262_144),
            ephemeral_port_low: None,
            ephemeral_port_high: None,
            ephemeral_port_capacity: None,
            sysctl_managed: true,
            arch: "aarch64".to_owned(),
            os_pretty: "Linux".to_owned(),
            virt: "KVM".to_owned(),
            rmem_max: 4096,
            wmem_max: 4096,
            somaxconn: 4096,
        },
        samples: vec![sample],
        processes: vec![],
        hops: vec![],
    };

    let accepted = db
        .store
        .record_load_report("net-observe", report)
        .await
        .unwrap();
    assert_eq!(accepted.accepted_samples, 1);
    let view = db
        .store
        .node_load_view(&system_admin(), "net-observe", now - 60, now + 1, 64)
        .await
        .unwrap();
    assert_eq!(view.range_start_unix_secs, now - 60);
    assert_eq!(view.range_end_unix_secs, now + 1);
    assert_eq!(view.series.len(), 1);
    assert_eq!(view.series[0].network_detail.as_ref(), Some(&network));
    assert_eq!(view.series[0].disk_detail.as_ref(), Some(&disk));
    assert_eq!(view.series[0].conntrack_count, Some(1200));

    let stale = db
        .store
        .node_load_view(&system_admin(), "net-observe", now + 60, now + 120, 64)
        .await
        .unwrap();
    assert!(stale.series.is_empty());
}

async fn insert_usage_history_for_main_fixture(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO usage_samples (
            window_start, window_end, node_id, tenant_id, user_id, ingress_id,
            grant_label, uplink_bytes, downlink_bytes
         )
         VALUES (
            now() - interval '60 seconds', now() - interval '30 seconds',
            'n1', 'platform.acme', 'alice', 'i-main',
            'alice@platform.acme#i-main', 10, 20
         )",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO usage_chain_samples (
            window_start, window_end, node_id, tenant_id, app_id, chain_id,
            hop_label, uplink_bytes, downlink_bytes
         )
         VALUES (
            now() - interval '60 seconds', now() - interval '30 seconds',
            'n1', 'platform.acme', 'app-main', 'c-main',
            'c-main@n1', 30, 40
         )",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_extra_user_grant(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'bob', '0d7f5b76-4185-4f33-8a24-e8cb8ec65c33')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-main', 'platform.acme', 'bob', 'i-main')",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Moving a domain to a different CA makes every certificate under it due again.
///
/// The operator's complaint this exists for: the directory was switched from staging to
/// production, the console showed production, and the fleet went on serving staging certificates
/// that no client trusts — because the only thing that made a certificate due was its expiry, and
/// these had months left. The account was correctly thrown away on the switch; the certificates
/// were not, and they are the part that is actually wrong.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn changing_the_ca_makes_the_certificates_it_issued_due_again() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    // Sealing refuses to store anything without a key, and the DNS credential has to be stored
    // for a domain to be issuable at all. Set here rather than expected from the environment: a
    // test that quietly skips when a variable is missing is a test that passes everywhere and
    // checks nothing.
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );

    let domain = |directory: &str| CertDomainInput {
        domain: "example.test".to_owned(),
        dns_credential: Some("token".to_owned()),
        acme_directory: Some(directory.to_owned()),
        acme_contact: Some("ops@example.test".to_owned()),
        renew_before_days: Some(30),
    };
    let staging = "https://acme-staging-v02.api.letsencrypt.org/directory";
    let production = "https://acme-v02.api.letsencrypt.org/directory";

    let created = db
        .store
        .upsert_cert_domain(&system_admin(), domain(staging))
        .await
        .unwrap();
    let _ = &created;

    // Issued against staging, and nowhere near expiry.
    issue_certificate_for(&db, "n1", "(STAGING) Pretend Pear").await;
    assert!(
        db.store.certificates_due(0).await.unwrap().is_empty(),
        "刚签好、离过期还早，不该再排队"
    );

    db.store
        .upsert_cert_domain(&system_admin(), domain(production))
        .await
        .unwrap();

    let due = db.store.certificates_due(0).await.unwrap();
    assert_eq!(due.len(), 1, "换了 CA 之后应该重新排队：{due:#?}");
    assert_eq!(due[0].acme_directory, production);
}

/// A new entrance arrives guarded, and changing what it refuses is a model change.
///
/// Two properties, and both matter for different reasons. The default: a request that says nothing
/// about the guard has to produce the protected shape, because the request that says nothing is the
/// one somebody wrote before this feature existed — and the unsafe direction cannot be the one
/// silence lands on. And the revision: refusing traffic is what this machine does with its
/// artifacts, so an edit has to be publishable work rather than a setting that quietly takes effect
/// somewhere between two releases.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_entrance_is_guarded_by_default_and_changing_it_stamps_a_revision() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    // Written the way a caller that predates the field writes it: the JSON simply has no `guard`.
    let request: CreateIngressRequest = serde_json::from_value(serde_json::json!({
        "id": "i-garude",
        "chain_id": "c-main",
        "node_id": "n1",
        "bind": "0.0.0.0",
        "port": 8443,
        "reality": {
            "dest": "www.example.com:443",
            "server_names": ["www.example.com"],
            "fingerprint": "chrome"
        }
    }))
    .unwrap();
    assert!(
        request.guard.no_private,
        "没提到 guard 时必须落在受保护的那一边"
    );
    assert!(request.guard.no_bittorrent);
    assert!(request.guard.no_mail);
    assert!(request.guard.no_udp_amplification);
    assert!(
        !request.guard.tcp_and_quic_only,
        "最狠的那一条会断游戏和语音，不能默认开"
    );

    let created = db
        .store
        .upsert_ingress(&system_admin(), "app-main", request.clone())
        .await
        .unwrap();

    let stored: (bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT guard_no_private, guard_no_bittorrent, guard_no_mail,
                guard_no_udp_amplification, guard_tcp_and_quic_only
           FROM ingresses WHERE id = 'i-garude'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stored, (true, true, true, true, false));

    // Re-sending the identical request burns no revision — the guard columns join the row
    // comparison rather than sitting outside it, which would have made every save look like a
    // change.
    let unchanged = db
        .store
        .upsert_ingress(&system_admin(), "app-main", request.clone())
        .await
        .unwrap();
    assert_eq!(
        unchanged.revision_id, created.revision_id,
        "一模一样地再存一次不该盖修订"
    );

    // Turning one off is a real change, and the model reads back what was asked for.
    let opened = CreateIngressRequest {
        guard: brocade_core::model::IngressGuard {
            no_bittorrent: false,
            ..Default::default()
        },
        ..request
    };
    let after = db
        .store
        .upsert_ingress(&system_admin(), "app-main", opened)
        .await
        .unwrap();
    assert!(
        after.revision_id > created.revision_id,
        "改了拦什么就是改了模型，必须盖修订"
    );

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let ingress = snapshot
        .apps
        .iter()
        .flat_map(|app| app.ingresses.iter())
        .find(|ingress| ingress.id == "i-garude")
        .expect("i-garude 在快照里");
    assert!(!ingress.guard.no_bittorrent);
    assert!(ingress.guard.no_private, "没动的那几条要原样留着");
}

/// The column takes an https URL and nothing else.
///
/// Guarded in the store rather than left to the CHECK constraint, because the message matters: a
/// typo here points the fleet at a CA that does not exist, and the failure surfaces one node at a
/// time, half an hour apart. There is no sentinel any more — self-signing was removed, so an
/// empty directory is a refusal rather than a fallback.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn the_directory_takes_an_https_url_and_nothing_else() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );

    let with_directory = |directory: &str| CertDomainInput {
        domain: "lab.test".to_owned(),
        dns_credential: None,
        acme_directory: Some(directory.to_owned()),
        acme_contact: None,
        renew_before_days: Some(30),
    };

    assert!(db
        .store
        .upsert_cert_domain(&system_admin(), with_directory("self-signed"))
        .await
        .is_err());
    assert!(db
        .store
        .upsert_cert_domain(&system_admin(), with_directory("http://insecure.test/dir"))
        .await
        .is_err());
    // An absent directory is refused rather than defaulted: a fleet that never chose a CA must
    // not be pointed at one, and there is no longer a setting that works with nothing supplied.
    assert!(db
        .store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "lab.test".to_owned(),
                dns_credential: None,
                acme_directory: None,
                acme_contact: None,
                renew_before_days: Some(30),
            }
        )
        .await
        .is_err());
    assert!(db
        .store
        .upsert_cert_domain(
            &system_admin(),
            with_directory(brocade_store::ACME_LETSENCRYPT)
        )
        .await
        .is_ok());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn renewal_failure_does_not_withdraw_an_unexpired_certificate() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let domain = db
        .store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "example.test".to_owned(),
                dns_credential: Some("token".to_owned()),
                acme_directory: Some(brocade_store::ACME_LETSENCRYPT.to_owned()),
                acme_contact: Some("ops@example.test".to_owned()),
                renew_before_days: Some(30),
            },
        )
        .await
        .unwrap();
    let _ = &domain;
    issue_certificate_for(&db, "n1", "Test CA").await;

    // The renewal that fails is a *different* row: the serving one is never overwritten, which is
    // what keeps an unexpired certificate in place while renewal keeps failing.
    let renewal = db
        .store
        .request_spare_certificate(&system_admin(), &label_of_node(&db, "n1").await)
        .await
        .unwrap();
    db.store
        .record_certificate_failure(&renewal, "renewal temporarily failed")
        .await
        .unwrap();

    assert!(db.store.cert_delta_for_node("n1").await.unwrap().is_some());
    let dns_targets = db.store.certificate_dns_targets().await.unwrap();
    assert_eq!(dns_targets.len(), 1);
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot.nodes[0].certificate_name.as_deref(),
        Some(dns_targets[0].name.as_str())
    );
}

/// The desired response owes the certificate unless the node reports holding the serving one.
///
/// All four states, on one node: no serving certificate, never reported, reported as absent or
/// with the wrong sha, and reported with the right sha. Only the last is owed nothing.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cert_delta_is_some_until_the_node_reports_the_serving_certificate() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );

    // No group bound, nothing issued: nothing is expected of this dimension.
    assert!(db.store.cert_delta_for_node("n1").await.unwrap().is_none());

    db.store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "example.test".to_owned(),
                dns_credential: Some("token".to_owned()),
                acme_directory: Some(brocade_store::ACME_LETSENCRYPT.to_owned()),
                acme_contact: Some("ops@example.test".to_owned()),
                renew_before_days: Some(30),
            },
        )
        .await
        .unwrap();
    issue_certificate_for(&db, "n1", "Test CA").await;

    // Serving now exists and the node has never reported: owed.
    let material = db.store.cert_delta_for_node("n1").await.unwrap().unwrap();

    // Reported absent: still owed.
    db.store
        .record_certificate_observation("n1", "absent", None)
        .await
        .unwrap();
    assert!(db.store.cert_delta_for_node("n1").await.unwrap().is_some());

    // Reported with a wrong sha: still owed.
    db.store
        .record_certificate_observation(
            "n1",
            "present",
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
        )
        .await
        .unwrap();
    assert!(db.store.cert_delta_for_node("n1").await.unwrap().is_some());

    // Reported with the serving sha: converged on this dimension.
    let serving_sha = brocade_core::hash::sha256_hex(material.cert_pem.as_bytes());
    db.store
        .record_certificate_observation("n1", "present", Some(&serving_sha))
        .await
        .unwrap();
    assert!(db.store.cert_delta_for_node("n1").await.unwrap().is_none());
}

/// A renewal takes over on arrival; a spare waits to be chosen.
///
/// This is the difference the `origin` column exists to make, and getting it backwards is how a
/// certificate expires with its replacement sitting in the database: the group keeps serving the
/// old one, the renewal sits in `ready`, and every later scan sees something waiting and skips.
/// Both halves are asserted here because either one alone would pass with the wrong rule.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn a_renewal_takes_over_while_a_spare_waits() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    db.store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "example.test".to_owned(),
                dns_credential: Some("token".to_owned()),
                acme_directory: Some(brocade_store::ACME_LETSENCRYPT.to_owned()),
                acme_contact: None,
                renew_before_days: Some(30),
            },
        )
        .await
        .unwrap();

    // First certificate: the group serves nothing, so it takes over whatever its origin.
    let first = issue_certificate_for(&db, "n1", "Test CA").await;
    let label_id = label_of_node(&db, "n1").await;
    assert_eq!(status_of(&db, &first).await, "serving");

    // A spare an operator asked for: issued, and deliberately not in charge.
    let spare = db
        .store
        .request_spare_certificate(&system_admin(), &label_id)
        .await
        .unwrap();
    record_test_certificate(&db, &spare).await;
    assert_eq!(status_of(&db, &spare).await, "ready", "备用不该自己接管");
    assert_eq!(status_of(&db, &first).await, "serving");

    // A renewal, which is what the scan creates. It takes over on arrival.
    let renewal: String = {
        let id = format!("{first}-renewal");
        sqlx::query("INSERT INTO certificates (id, label_id, origin) VALUES ($1, $2, 'renewal')")
            .bind(&id)
            .bind(&label_id)
            .execute(db.pool())
            .await
            .unwrap();
        id
    };
    record_test_certificate(&db, &renewal).await;
    assert_eq!(
        status_of(&db, &renewal).await,
        "serving",
        "续期必须自己接管"
    );
    assert_eq!(status_of(&db, &first).await, "superseded");
    // Untouched: it was never the one being replaced.
    assert_eq!(status_of(&db, &spare).await, "ready");
}

async fn status_of(db: &TestPg, certificate_id: &str) -> String {
    sqlx::query("SELECT status FROM certificates WHERE id = $1")
        .bind(certificate_id)
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("status")
        .unwrap()
}

async fn record_test_certificate(db: &TestPg, certificate_id: &str) {
    db.store
        .record_certificate(
            certificate_id,
            "-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\ny\n-----END PRIVATE KEY-----\n",
            "2099-01-01T00:00:00Z",
            "Test CA",
        )
        .await
        .unwrap();
}

/// An ingress can have flow off on its own, without the fleet losing it.
///
/// NULL and the empty string used to be read alike, which made this state unreachable: the only
/// way to turn Vision off for one ingress was to turn it off globally. That matters now, because
/// XHTTP cannot run with Vision — so without this, "one ingress on XHTTP" and "every client's flow
/// changed" were the same act.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_ingress_can_turn_flow_off_without_the_fleet_losing_it() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let face = |id: &str, flow: Option<String>| CreateIngressRequest {
        wires: Default::default(),
        id: id.to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port: if id == "i-main" { 443 } else { 8443 },
        front_id: None,
        guard: brocade_core::model::IngressGuard::OPEN,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: None,
            dest: Some("www.example.com:443".to_owned()),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: Some("chrome".to_owned()),
            flow,
        },
        projection: Projection::default(),
        note: None,
    };

    // One left alone, one turned off explicitly.
    db.store
        .upsert_ingress(&system_admin(), "app-main", face("i-main", None))
        .await
        .unwrap();
    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face("i-bacemu", Some(String::new())),
        )
        .await
        .unwrap();

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let flow_of = |id: &str| {
        snapshot
            .apps
            .iter()
            .flat_map(|app| &app.ingresses)
            .find(|ingress| ingress.id == id)
            .map(|ingress| {
                let reality = ingress.wires.reality().expect("REALITY 形状");
                reality.flow.clone()
            })
            .expect("接入面还在")
    };

    // The untouched one still follows the global default, which a fresh database sets to Vision.
    assert_eq!(flow_of("i-main").as_deref(), Some("xtls-rprx-vision"));
    // The other one is off, and being off is `None` at the model layer — that is what "ship no
    // flow" is, and it is what lets an XHTTP ingress compile.
    assert_eq!(flow_of("i-bacemu"), None);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_ingress_equal_to_the_global_reality_site_keeps_following_it() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let settings = db.store.settings().await.unwrap();
    let global = settings.reality_site.clone();
    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            CreateIngressRequest {
                wires: Default::default(),
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
                    fallback_guard: None,
                    dest: global.dest.clone(),
                    server_names: global.server_names.clone(),
                    fingerprint: global.fingerprint.clone(),
                    flow: None,
                },
                projection: Projection::default(),
                note: None,
            },
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT ingress.reality_dest,
                ingress.reality_server_names,
                client.reality_fingerprint
           FROM ingresses AS ingress
           LEFT JOIN ingress_client_settings AS client
             ON client.ingress_id = ingress.id
          WHERE ingress.app_id = 'app-main' AND ingress.id = 'i-main'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        row.try_get::<Option<String>, _>("reality_dest").unwrap(),
        None
    );
    assert_eq!(
        row.try_get::<serde_json::Value, _>("reality_server_names")
            .unwrap(),
        serde_json::json!([])
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("reality_fingerprint")
            .unwrap(),
        None
    );

    let mut changed = settings;
    changed.reality_site = RealitySite {
        dest: Some("updates.example.net:443".to_owned()),
        server_names: vec!["updates.example.net".to_owned()],
        fingerprint: Some("firefox".to_owned()),
        flow: global.flow,
    };
    db.store
        .update_settings(&system_admin(), changed)
        .await
        .unwrap();

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let reality = snapshot.apps[0].ingresses[0]
        .wires
        .reality()
        .expect("REALITY ingress");
    assert_eq!(reality.dest, "updates.example.net:443");
    assert_eq!(reality.server_names, vec!["updates.example.net"]);
    assert_eq!(reality.fingerprint, "firefox");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn reality_fallback_mode_and_limits_round_trip_without_an_external_target() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    let upload = RealityFallbackRateLimit {
        after_bytes: 1024,
        bytes_per_sec: 2048,
        burst_bytes_per_sec: 4096,
    };
    let download = RealityFallbackRateLimit {
        after_bytes: 8192,
        bytes_per_sec: 16384,
        burst_bytes_per_sec: 32768,
    };

    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            CreateIngressRequest {
                wires: Default::default(),
                id: "i-main".to_owned(),
                chain_id: "c-main".to_owned(),
                node_id: "n1".to_owned(),
                bind: "0.0.0.0".parse().unwrap(),
                port: 443,
                front_id: None,
                guard: brocade_core::model::IngressGuard::OPEN,
                reality: CreateRealityIngressRequest {
                    fallback_mode: Some(RealityFallbackMode::NodeCertificate),
                    fallback_limits: Some(RealityFallbackLimits::Custom { upload, download }),
                    fallback_guard: None,
                    dest: Some("must-not-be-stored.example:443".to_owned()),
                    server_names: vec!["must-not-be-stored.example".to_owned()],
                    fingerprint: Some("chrome".to_owned()),
                    flow: Some(String::new()),
                },
                projection: Projection::default(),
                note: None,
            },
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT reality_dest, reality_server_names, reality_fallback_mode, reality_fallback_limits
         FROM ingresses WHERE id = 'i-main'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        row.try_get::<Option<String>, _>("reality_dest").unwrap(),
        None
    );
    assert_eq!(
        row.try_get::<serde_json::Value, _>("reality_server_names")
            .unwrap(),
        json!([])
    );
    assert_eq!(
        row.try_get::<String, _>("reality_fallback_mode").unwrap(),
        "node-certificate"
    );

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let reality = snapshot.apps[0].ingresses[0]
        .wires
        .reality()
        .expect("REALITY ingress");
    assert_eq!(reality.fallback_mode, RealityFallbackMode::NodeCertificate);
    assert_eq!(
        reality.fallback_limits,
        RealityFallbackLimits::Custom { upload, download }
    );
}

/// The stream survives the round trip, and — more to the point — an unrelated edit does not quietly
/// reset it.
///
/// That second half is the failure this guards. The upsert overwrites wholesale, so a caller that
/// changes a port and forgets to carry the stream turns an XHTTP ingress back into a TCP one. The
/// artifacts would then be regenerated, the machine restarted, and every client configuration
/// already handed out would stop working — with nothing in the console suggesting why.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_ingress_keeps_its_stream_across_writes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let face = |wires: WiresRequest, port: u16| CreateIngressRequest {
        wires,
        id: "i-main".to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port,
        front_id: None,
        guard: brocade_core::model::IngressGuard::OPEN,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: None,
            dest: Some("www.example.com:443".to_owned()),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: Some("chrome".to_owned()),
            // Empty rather than Vision: the two cannot be combined, and the compiler refuses the
            // pair, so a stored XHTTP ingress always has flow off.
            flow: Some(String::new()),
        },
        projection: Projection::default(),
        note: None,
    };

    let xhttp = Xhttp {
        path: "/a1b2c3d4".to_owned(),
        host: Some("upload.route.example".to_owned()),
        xmux: Some(XhttpXmux {
            max_concurrency: None,
            max_connections: Some(4),
            h_max_request_times: XhttpXmuxRange::new(700, 800),
            h_max_reusable_secs: XhttpXmuxRange::new(1200, 1800),
            h_keep_alive_period_secs: Some(15),
        }),
        tuning: Some(XhttpTuning {
            x_padding_bytes: Some(XhttpXmuxRange::new(200, 600)),
        }),
        mode: XhttpMode::PacketUp,
        download: None,
    };
    let shape = WiresRequest {
        vless: Some(TransportRequest::VlessRealityXhttp {
            xhttp: xhttp.clone(),
        }),
        anytls: None,
        hysteria2: None,
    };
    let written = db
        .store
        .upsert_ingress(&system_admin(), "app-main", face(shape.clone(), 443))
        .await
        .unwrap();
    // The echo is JSON, so the assertion goes through the same serialization a caller sees.
    assert_eq!(
        written.ingress["wires"]["vless"]["kind"],
        "vless-reality-xhttp"
    );
    assert_eq!(
        written.ingress["wires"]["vless"]["xhttp"]["path"],
        "/a1b2c3d4"
    );
    assert_eq!(
        written.ingress["wires"]["vless"]["xhttp"]["host"],
        "upload.route.example"
    );
    assert_eq!(
        written.ingress["wires"]["vless"]["xhttp"]["xmux"],
        json!({
            "max_connections": 4,
            "h_max_request_times": { "from": 700, "to": 800 },
            "h_max_reusable_secs": { "from": 1200, "to": 1800 },
            "h_keep_alive_period_secs": 15
        })
    );
    assert_eq!(
        written.ingress["wires"]["vless"]["xhttp"]["tuning"],
        json!({
            "x_padding_bytes": { "from": 200, "to": 600 }
        })
    );
    assert_eq!(
        written.ingress["wires"]["vless"]["xhttp"]["mode"],
        "packet-up"
    );

    // Read back through the same path the compiler uses, not the write's echo: the echo could be
    // right while what landed in the database is not.
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot
        .apps
        .iter()
        .flat_map(|app| &app.ingresses)
        .find(|ingress| ingress.id == "i-main")
        .expect("接入面还在");
    assert_eq!(stored.wires.xhttp(), Some(&xhttp));

    let incomplete_xmux = sqlx::query(
        "UPDATE ingress_client_settings
         SET xhttp_xmux = '{\"max_concurrency\": 16}'::jsonb
         WHERE ingress_id = 'i-main'",
    )
    .execute(db.pool())
    .await;
    assert!(
        incomplete_xmux.is_err(),
        "a present XMUX object must include both lifecycle ranges"
    );

    // A second write carrying the stream along changes only the port.
    let moved = db
        .store
        .upsert_ingress(&system_admin(), "app-main", face(shape.clone(), 8443))
        .await
        .unwrap();
    assert_eq!(moved.ingress["port"], 8443);
    assert_eq!(
        moved.ingress["wires"]["vless"]["xhttp"]["path"],
        "/a1b2c3d4"
    );

    let tls_xhttp = WiresRequest {
        vless: Some(TransportRequest::VlessTlsXhttp {
            xhttp: xhttp.clone(),
        }),
        anytls: None,
        hysteria2: None,
    };
    let written = db
        .store
        .upsert_ingress(&system_admin(), "app-main", face(tls_xhttp, 8443))
        .await
        .unwrap();
    assert_eq!(written.ingress["wires"]["vless"]["kind"], "vless-tls-xhttp");
    assert!(written.ingress["wires"]["vless"].get("alpn").is_none());
    assert!(written.ingress["wires"]["vless"]
        .get("fingerprint")
        .is_none());
    assert_column_missing(db.pool(), "ingresses", "xhttp_alpn").await;
    assert_column_missing(db.pool(), "ingresses", "tls_fingerprint").await;
    let stored_tuning: serde_json::Value =
        sqlx::query_scalar("SELECT xhttp_tuning FROM ingresses WHERE id = 'i-main'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        stored_tuning,
        json!({ "x_padding_bytes": { "from": 200, "to": 600 } })
    );
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot
        .apps
        .iter()
        .flat_map(|app| &app.ingresses)
        .find(|ingress| ingress.id == "i-main")
        .unwrap();
    let Some(Transport::VlessTlsXhttp(_)) = stored.wires.vless() else {
        panic!("expected TLS + XHTTP");
    };
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn anytls_session_settings_use_client_storage_and_advance_its_checkpoint() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    db.store
        .upsert_cert_domain(
            &system_admin(),
            CertDomainInput {
                domain: "example.test".to_owned(),
                dns_credential: Some("token".to_owned()),
                acme_directory: Some(brocade_store::ACME_LETSENCRYPT.to_owned()),
                acme_contact: Some("ops@example.test".to_owned()),
                renew_before_days: Some(30),
            },
        )
        .await
        .unwrap();
    issue_certificate_for(&db, "n1", "Test CA").await;

    let face = |check, timeout, minimum| CreateIngressRequest {
        id: "i-main".to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port: 443,
        front_id: None,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: None,
            dest: Some("www.example.com:443".to_owned()),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: Some("chrome".to_owned()),
            flow: None,
        },
        wires: WiresRequest {
            vless: Some(TransportRequest::VlessReality),
            anytls: Some(AnyTls {
                port: 19443,
                idle_session_check_interval_secs: check,
                idle_session_timeout_secs: timeout,
                min_idle_session: minimum,
                ..AnyTls::default()
            }),
            hysteria2: None,
        },
        projection: Projection::default(),
        guard: brocade_core::model::IngressGuard::OPEN,
        note: None,
    };

    let first = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(Some(11), Some(22), Some(3)),
        )
        .await
        .unwrap();
    let first_client: i64 = sqlx::query_scalar(
        "SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO subscription_serving_state (
             id, topology_revision_id, permissions_revision_id, client_snapshot_id, generation
         ) VALUES (TRUE, $1, $1, $2, 1)",
    )
    .bind(i64::try_from(first.revision_id).unwrap())
    .bind(first_client)
    .execute(db.pool())
    .await
    .unwrap();
    let stored: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT anytls_idle_session_check_interval, anytls_idle_session_timeout,
                anytls_min_idle_session
           FROM ingress_client_settings
          WHERE ingress_id = 'i-main'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stored, (Some(11), Some(22), Some(3)));
    assert_column_missing(db.pool(), "ingresses", "anytls_idle_session_check_interval").await;

    let second = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertIngress {
                app_id: "app-main".to_owned(),
                ingress: face(Some(12), Some(24), None),
            }],
            Some("adjust AnyTLS client sessions".to_owned()),
        )
        .await
        .unwrap();
    assert!(second.revision_id > first.revision_id);
    assert!(matches!(
        second.client_config.status,
        brocade_store::ClientConfigCommitStatus::Activated
    ));
    assert!(second.client_config.pending_topology.is_empty());
    let second_client: i64 = sqlx::query_scalar(
        "SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(second_client, first_client);

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let anytls = snapshot.apps[0].ingresses[0].wires.anytls().unwrap();
    assert_eq!(anytls.idle_session_check_interval_secs, Some(12));
    assert_eq!(anytls.idle_session_timeout_secs, Some(24));
    assert_eq!(anytls.min_idle_session, None);
    let deployments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM deployments WHERE kind = 'config'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        deployments, 0,
        "client-only tuning must not create a deployment"
    );

    let oversized = sqlx::query(
        "UPDATE ingress_client_settings
            SET anytls_idle_session_timeout = 4294967296
          WHERE ingress_id = 'i-main'",
    )
    .execute(db.pool())
    .await;
    assert!(
        oversized.is_err(),
        "database must enforce the model's u32 upper bound"
    );

    sqlx::query(
        "ALTER TABLE ingress_client_settings
         DROP CONSTRAINT ingress_client_settings_anytls_idle_session_timeout_check",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE ingress_client_settings
            SET anytls_idle_session_timeout = 4294967296
          WHERE ingress_id = 'i-main'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let corrupt = db.store.materialize_snapshot(None).await;
    assert!(
        matches!(corrupt, Err(StoreError::InvalidData(message)) if message.contains("anytls_idle_session_timeout out of range")),
        "an oversized stored value must not silently become the client default"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn hysteria2_ingress_round_trips_preserves_redacted_obfs_and_uses_udp_port_namespace() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let hysteria = |password: &str| Hysteria2 {
        bbr_profile: brocade_core::model::HysteriaBbrProfile::default(),
        quic: brocade_core::model::HysteriaQuic::default(),
        port: 50000,
        hop: None,
        bandwidth: HysteriaBandwidth {
            up: Some("20 mbps".to_owned()),
            down: Some("100 mbps".to_owned()),
        },
        congestion: HysteriaCongestion::ForceBrutal,
        obfs: Some(HysteriaObfs::Salamander {
            password: password.to_owned(),
        }),
        masquerade: HysteriaMasquerade::Proxy {
            url: "https://cover.example.net/".to_owned(),
        },
    };
    let face = |id: &str, port: u16, wires: WiresRequest| CreateIngressRequest {
        wires,
        id: id.to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port,
        front_id: None,
        guard: brocade_core::model::IngressGuard::OPEN,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: None,
            dest: Some("www.example.com:443".to_owned()),
            server_names: vec!["www.example.com".to_owned()],
            fingerprint: Some("chrome".to_owned()),
            flow: Some(String::new()),
        },
        projection: Projection::default(),
        note: None,
    };

    let written = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(
                "i-main",
                443,
                WiresRequest {
                    vless: None,
                    anytls: None,
                    hysteria2: Some(hysteria("salamander-secret")),
                },
            ),
        )
        .await
        .unwrap();
    assert!(written.ingress["wires"]["vless"].is_null());
    assert!(written.ingress["wires"]["hysteria2"].is_object());
    assert_eq!(
        written.ingress["wires"]["hysteria2"]["obfs"]["password"],
        "<redacted>"
    );

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot.apps[0]
        .ingresses
        .iter()
        .find(|ingress| ingress.id == "i-main")
        .expect("Hysteria 2 ingress");
    assert_eq!(
        stored.wires,
        IngressWires::Hysteria2(hysteria("salamander-secret"))
    );

    // The console sends the masked value back on ordinary edits. It means “keep the existing
    // secret”, not “replace it with the literal marker”.
    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(
                "i-main",
                8443,
                WiresRequest {
                    vless: None,
                    anytls: None,
                    hysteria2: Some(hysteria("<redacted>")),
                },
            ),
        )
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let stored = snapshot.apps[0]
        .ingresses
        .iter()
        .find(|ingress| ingress.id == "i-main")
        .unwrap();
    assert_eq!(
        stored.wires,
        IngressWires::Hysteria2(hysteria("salamander-secret"))
    );

    // UDP Hysteria and TCP VLESS may share a numeric port. The generated protocol column and
    // unique constraint must distinguish them, just like the runtime socket namespace does.
    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face("i-velesu", 8443, WiresRequest::default()),
        )
        .await
        .expect("TCP and UDP should be allowed to share one numeric port");
}
/// A projection's storage round trip: written, read back, turned off, and an empty host turned
/// away at the door.
///
/// "No projection" is both columns NULL rather than an empty string — a rule with one guard each
/// in the model, the database constraints, and the write path, all three walked together here.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ingress_projection_round_trips_and_refuses_a_blank_host() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let face = |projection: Projection| {
        let download = XhttpDownload {
            v4: projection
                .v4
                .as_ref()
                .and_then(|endpoint| endpoint.download.clone()),
            v6: projection
                .v6
                .as_ref()
                .and_then(|endpoint| endpoint.download.clone()),
        };
        let projection = Projection {
            v4: projection.v4.map(|endpoint| ProjectionEndpoint {
                host: endpoint.host,
                port: endpoint.port,
                download: None,
            }),
            v6: projection.v6.map(|endpoint| ProjectionEndpoint {
                host: endpoint.host,
                port: endpoint.port,
                download: None,
            }),
        };
        CreateIngressRequest {
            wires: WiresRequest {
                hysteria2: None,
                anytls: None,
                vless: Some(TransportRequest::VlessRealityXhttp {
                    xhttp: Xhttp {
                        path: "/projection-test".to_owned(),
                        host: Some("upload.route.example".to_owned()),
                        xmux: None,
                        tuning: None,
                        mode: XhttpMode::Auto,
                        download: (!download.is_empty()).then_some(download),
                    },
                }),
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
                fallback_guard: None,
                dest: Some("www.example.com:443".to_owned()),
                server_names: vec!["www.example.com".to_owned()],
                fingerprint: Some("chrome".to_owned()),
                flow: Some("xtls-rprx-vision".to_owned()),
            },
            projection,
            note: None,
        }
    };

    // v4 only.
    let written = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(Projection {
                v4: Some(ProjectionEndpoint {
                    host: "cu.acc.example.net".to_owned(),
                    port: 20443,
                    download: Some(ProjectionDownloadEndpoint {
                        host: "down.acc.example.net".to_owned(),
                        port: 30443,
                        origin_port: Some(40443),
                        http_host: Some("download.route.example".to_owned()),
                        mux: Some(24),
                    }),
                }),
                v6: None,
            }),
        )
        .await
        .unwrap();
    let after_write = written.revision_id;

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let stored = &snapshot.apps[0].ingresses[0];
    assert_eq!(
        stored.projection.v4,
        Some(ProjectionEndpoint {
            host: "cu.acc.example.net".to_owned(),
            port: 20443,
            download: None,
        })
    );
    assert_eq!(stored.projection.v6, None);
    assert_eq!(
        stored.wires.xhttp().unwrap().host.as_deref(),
        Some("upload.route.example")
    );
    assert_eq!(
        stored.wires.xhttp().unwrap().download,
        Some(XhttpDownload {
            v4: Some(ProjectionDownloadEndpoint {
                host: "down.acc.example.net".to_owned(),
                port: 30443,
                origin_port: Some(40443),
                http_host: Some("download.route.example".to_owned()),
                mux: Some(24),
            }),
            v6: None,
        })
    );

    // Writing the same contents again must not advance the revision number: the change-detection
    // ROW comparison includes the projection columns, and omitting them would judge a changed
    // projection as unchanged and leave the artifacts uncomputed.
    let unchanged = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(Projection {
                v4: Some(ProjectionEndpoint {
                    host: "cu.acc.example.net".to_owned(),
                    port: 20443,
                    download: Some(ProjectionDownloadEndpoint {
                        host: "down.acc.example.net".to_owned(),
                        port: 30443,
                        origin_port: Some(40443),
                        http_host: Some("download.route.example".to_owned()),
                        mux: Some(24),
                    }),
                }),
                v6: None,
            }),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.revision_id, after_write, "内容没变不该推修订");

    // Changing only the node-side origin port counts as a change. Missing it from the ROW
    // comparison would silently retain an old listener while claiming the write succeeded.
    let repointed = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(Projection {
                v4: Some(ProjectionEndpoint {
                    host: "cu.acc.example.net".to_owned(),
                    port: 20443,
                    download: Some(ProjectionDownloadEndpoint {
                        host: "down.acc.example.net".to_owned(),
                        port: 30443,
                        origin_port: Some(40444),
                        http_host: Some("download.route.example".to_owned()),
                        mux: Some(24),
                    }),
                }),
                v6: None,
            }),
        )
        .await
        .unwrap();
    assert!(repointed.revision_id > after_write, "改了投影就该推修订");

    // Turned off: both columns return to NULL.
    db.store
        .upsert_ingress(&system_admin(), "app-main", face(Projection::default()))
        .await
        .unwrap();
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(
        snapshot.apps[0].ingresses[0].projection,
        Projection::default()
    );

    // An empty host is not "no projection" but an error — turned back by the write path rather
    // than left to the database's CHECK.
    let blank = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(Projection {
                v4: Some(ProjectionEndpoint {
                    host: "   ".to_owned(),
                    port: 20443,
                    download: None,
                }),
                v6: None,
            }),
        )
        .await;
    assert!(
        matches!(blank, Err(StoreError::InvalidData(_))),
        "空 host 该被拒：{blank:?}"
    );

    // That CHECK must still really be on the database — it guards against whoever bypasses the
    // write path.
    let raw = sqlx::query(
        "UPDATE ingresses SET projection_v4_host = '', projection_v4_port = 20443 \
         WHERE id = 'i-main'",
    )
    .execute(db.pool())
    .await;
    assert!(raw.is_err(), "库约束该拦下空 host");

    let half_download = sqlx::query(
        "UPDATE ingresses SET \
            projection_v4_host = 'cu.acc.example.net', projection_v4_port = 20443, \
            projection_v4_download_host = 'down.acc.example.net', \
            projection_v4_download_port = NULL \
         WHERE id = 'i-main'",
    )
    .execute(db.pool())
    .await;
    assert!(half_download.is_err(), "下载地址和端口必须一起填写");
}

/// Operational isolation lets the serving revision move forward without pretending the missing
/// machine converged. A newer release replaces the old debt, and an old claim cannot settle it.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_target_becomes_supersedable_debt_and_requires_explicit_reentry() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let revision = db.store.materialize_snapshot(None).await.unwrap().revision;

    let first = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "isolation-first"),
        )
        .await
        .unwrap();
    assert_eq!(
        target_status(db.pool(), first.deployment_id, "n1").await,
        "pending"
    );

    let isolated = db
        .store
        .isolate_deployment_target(
            &system_admin(),
            first.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "test node is unreachable".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();
    assert!(isolated.isolated);
    assert_eq!(isolated.debt_count, 1);
    let serving_generation: i64 =
        sqlx::query_scalar("SELECT generation FROM subscription_serving_state WHERE id = TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        isolated.serving_generation,
        Some(u64::try_from(serving_generation).unwrap()),
        "isolation result must return the transaction's final serving generation"
    );

    let first_detail = db
        .store
        .deployment_detail(&system_admin(), first.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(first_detail.status, "succeeded");
    assert_eq!(first_detail.activation_status, "activated");
    assert_eq!(first_detail.settlement_status, "debt");
    assert_eq!(first_detail.active, None, "隔离后必须释放同类发布锁");
    assert_eq!(first_detail.debt_targets, 1);

    let old_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("isolated node still receives convergence debt");
    assert!(old_claim.claim_generation > 0);

    let changed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpdateNode {
                node_id: "n1".to_owned(),
                node: UpdateNodeRequest {
                    name: Some("Node 1 newer".to_owned()),
                    ..Default::default()
                },
            }],
            None,
        )
        .await
        .unwrap();
    let second = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(changed.revision_id, "isolation-second"),
        )
        .await
        .unwrap();
    let second_detail = db
        .store
        .deployment_detail(&system_admin(), second.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(second_detail.status, "succeeded");
    assert_eq!(second_detail.activation_status, "activated");
    assert_eq!(second_detail.settlement_status, "debt");
    assert_eq!(
        target_status(db.pool(), first.deployment_id, "n1").await,
        "superseded"
    );

    let stale = db
        .store
        .report_target_result(applied_report(&old_claim))
        .await;
    assert!(
        matches!(
            stale,
            Err(StoreError::Conflict(_)) | Err(StoreError::Unsupported(_))
        ),
        "旧债务 claim 不得覆盖新期望：{stale:?}"
    );

    let newest = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("newest obligation is claimable");
    assert_eq!(newest.deployment_id, second.deployment_id);
    db.store
        .report_target_result(applied_report(&newest))
        .await
        .unwrap();
    let settled = db
        .store
        .deployment_detail(&system_admin(), second.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(settled.settlement_status, "converged");

    db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll("n1", Some("test-agent"))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_agent_state
            SET runtime_reported_at = now(),
                wireguard_health = '{\"enabled\":true,\"peers\":[]}'::jsonb
          WHERE node_id = 'n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let state = db
        .store
        .list_node_agent_states(&system_admin())
        .await
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.node_id == "n1")
        .unwrap();
    assert!(state.operationally_isolated);
    assert_eq!(state.convergence_debt_count, 0);
    assert!(
        state.service_reentry_ready,
        "re-entry blockers: {:?}",
        state.service_reentry_blockers
    );

    let restored = db
        .store
        .restore_node_service(
            &system_admin(),
            "n1",
            RestoreNodeServiceRequest {
                reason: "test convergence and runtime checks passed".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(!restored.isolated);
    let isolation_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM node_operational_isolations WHERE node_id = 'n1')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!isolation_exists);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolating_a_dispatched_target_requires_ack_and_fences_its_report() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let deployment = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "isolation-dispatched"),
        )
        .await
        .unwrap();
    let stale_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("target should be dispatched before isolation");
    let unacknowledged = db
        .store
        .isolate_deployment_target(
            &system_admin(),
            deployment.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "dispatched".to_owned(),
                reason: "agent disappeared after claim".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await;
    assert!(matches!(unacknowledged, Err(StoreError::Conflict(_))));

    let isolated = db
        .store
        .isolate_deployment_target(
            &system_admin(),
            deployment.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "dispatched".to_owned(),
                reason: "agent disappeared after claim".to_owned(),
                acknowledge_uncertain: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(isolated.debt_count, 1);
    let applied = sqlx::query(
        "SELECT wireguard_state, xray_state, grants_state
           FROM node_applied_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        applied.try_get::<String, _>("wireguard_state").unwrap(),
        "dirty"
    );
    assert_eq!(applied.try_get::<String, _>("xray_state").unwrap(), "dirty");
    assert_eq!(
        applied.try_get::<String, _>("grants_state").unwrap(),
        "dirty"
    );
    assert!(matches!(
        db.store
            .report_target_result(applied_report(&stale_claim))
            .await,
        Err(StoreError::Conflict(_)) | Err(StoreError::Unsupported(_))
    ));
    let replacement = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("isolation creates a fresh full-state claim");
    assert!(replacement.claim_generation > 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn rollback_over_an_isolated_node_activates_with_replacement_debt() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let base = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "isolated-rollback-base"),
        )
        .await
        .unwrap();
    let base_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&base_claim))
        .await
        .unwrap();

    let changed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpdateNode {
                node_id: "n1".to_owned(),
                node: UpdateNodeRequest {
                    mtu: Some(1300),
                    ..Default::default()
                },
            }],
            None,
        )
        .await
        .unwrap();
    let forward = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(changed.revision_id, "isolated-rollback-forward"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            forward.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "isolate before rollback".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();

    let rollback = db
        .store
        .create_rollback_deployment(
            &system_admin(),
            create_rollback_request(base.deployment_id, "rollback while isolated"),
        )
        .await
        .unwrap();
    assert_eq!(rollback.status, "succeeded");
    let detail = db
        .store
        .deployment_detail(&system_admin(), rollback.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(detail.activation_status, "activated");
    assert_eq!(detail.settlement_status, "debt");
    assert_eq!(detail.debt_targets, 1);
    assert_eq!(detail.active, None);
    assert_eq!(
        target_status(db.pool(), forward.deployment_id, "n1").await,
        "superseded"
    );
    assert_eq!(
        target_status(db.pool(), rollback.deployment_id, "n1").await,
        "deferred"
    );
    let rollback_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("rollback replaces the isolated node's latest desired state");
    assert_eq!(rollback_claim.deployment_id, rollback.deployment_id);
}

/// A debt on one isolated node is local to that node. Healthy nodes must still receive both the
/// rest of the configuration release and later grant-only releases.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_config_debt_does_not_block_grants_on_healthy_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    insert_other_tenant_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let revision = db.store.materialize_snapshot(None).await.unwrap().revision;

    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "isolation-config-two-nodes"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            config.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "n1 unavailable during release".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();

    let healthy_config = db
        .store
        .claim_desired_for_node("n-other")
        .await
        .unwrap()
        .expect("healthy node receives configuration");
    db.store
        .report_target_result(applied_report(&healthy_config))
        .await
        .unwrap();
    let config_detail = db
        .store
        .deployment_detail(&system_admin(), config.deployment_id, false)
        .await
        .unwrap();
    assert_eq!(config_detail.status, "succeeded");
    assert_eq!(config_detail.settlement_status, "debt");
    assert!(matches!(
        db.store
            .clash_subscription_by_uuid("2d2304da-f114-4574-8d44-625afdb1db5c")
            .await,
        Err(StoreError::Unavailable(_))
    ));
    db.store
        .clash_subscription_by_uuid("f98b74ba-58f1-41d0-aaad-8fa5724c6d2d")
        .await
        .expect("另一个健康入口的用户不受隔离影响");

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.other', 'dave', 'd26e89a9-8eb1-4cc2-adb9-8f24cd2f9a77')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let grants_revision = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-other".to_owned(),
                tenant_id: "platform.other".to_owned(),
                user_id: "dave".to_owned(),
                ingress_id: "i-other".to_owned(),
                enabled: true,
                note: Some("grant another user on the healthy node".to_owned()),
            },
        )
        .await
        .unwrap();
    let automated = db.store.process_grant_automation().await.unwrap();
    assert_eq!(automated.revision_id, Some(grants_revision.revision_id));
    assert!(automated.waiting.is_none(), "{:?}", automated.waiting);
    let grants_id = automated
        .deployment_id
        .expect("automatic grants deployment");
    let healthy_grants = db
        .store
        .claim_desired_for_node("n-other")
        .await
        .unwrap()
        .expect("healthy node receives grants despite n1 debt");
    db.store
        .report_target_result(applied_report(&healthy_grants))
        .await
        .unwrap();
    let grants_detail = db
        .store
        .deployment_detail(&system_admin(), grants_id, false)
        .await
        .unwrap();
    assert_eq!(grants_detail.status, "succeeded");
    assert_eq!(grants_detail.activation_status, "activated");
    assert_eq!(grants_detail.settlement_status, "converged");
    assert_eq!(grants_detail.active, None);
    assert_eq!(
        target_status(db.pool(), grants_id, "n-other").await,
        "succeeded"
    );

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'erin', '4ed04a92-b0c0-42fa-b751-f1da11c9885b')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let isolated_grant_revision = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "erin".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: true,
                note: Some("grant a user on the isolated node".to_owned()),
            },
        )
        .await
        .unwrap();
    let isolated_automation = db.store.process_grant_automation().await.unwrap();
    assert_eq!(
        isolated_automation.revision_id,
        Some(isolated_grant_revision.revision_id)
    );
    assert!(
        isolated_automation.waiting.is_none(),
        "隔离节点的权限债务不得阻塞自动任务：{:?}",
        isolated_automation.waiting
    );

    let rebased = sqlx::query(
        "SELECT desired_grants,
                desired_structure->>'grants_revision' AS grants_revision,
                usage_generation_id
           FROM node_convergence_obligations
          WHERE node_id = 'n1'
            AND kind = 'config'
            AND status IN ('pending', 'dispatched', 'converging')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let rebased_grants: DesiredGrants =
        serde_json::from_value(rebased.try_get("desired_grants").unwrap()).unwrap();
    let DesiredGrants::Present { inbounds } = &rebased_grants else {
        panic!("isolated config debt should retain a complete grant snapshot");
    };
    assert!(inbounds
        .iter()
        .flat_map(|inbound| &inbound.clients)
        .any(|client| {
            client.email == "erin@platform.acme#i-main"
                && client.uuid == "4ed04a92-b0c0-42fa-b751-f1da11c9885b"
        }));
    assert_eq!(
        rebased.try_get::<String, _>("grants_revision").unwrap(),
        isolated_grant_revision.revision_id.to_string()
    );
    let usage_bindings: serde_json::Value =
        sqlx::query_scalar("SELECT bindings FROM usage_generations WHERE id = $1")
            .bind(rebased.try_get::<i64, _>("usage_generation_id").unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(usage_bindings.get("erin@platform.acme#i-main").is_some());

    let recovered_config = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("isolated node receives its rebased config debt first");
    assert_eq!(recovered_config.deployment_id, config.deployment_id);
    assert_eq!(recovered_config.desired.grants, rebased_grants);
}

/// A permission generation created after config was claimed must wait behind that immutable
/// config claim. The agent may never receive both writers concurrently for one isolated node.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn isolated_grants_wait_for_an_in_flight_config_obligation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;
    let revision = db.store.materialize_snapshot(None).await.unwrap().revision;

    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "isolation-config-in-flight"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            config.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "test config claim serialization".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();
    let config_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("config obligation should be claimable");

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'frank', 'c57227de-3c39-4767-ab5d-7d4622adc0cb')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let grant_revision = db
        .store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "frank".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: true,
                note: Some("grant while isolated config is in flight".to_owned()),
            },
        )
        .await
        .unwrap();
    let automated = db.store.process_grant_automation().await.unwrap();
    assert_eq!(automated.revision_id, Some(grant_revision.revision_id));
    assert!(automated.waiting.is_none(), "{:?}", automated.waiting);
    let grants_id = automated
        .deployment_id
        .expect("the newer grants generation must be durable");

    assert!(
        db.store
            .claim_desired_for_node("n1")
            .await
            .unwrap()
            .is_none(),
        "grants must not be handed out while the config claim is in flight"
    );
    db.store
        .report_target_result(applied_report(&config_claim))
        .await
        .unwrap();

    let grants_claim = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("grants become claimable after config settles");
    assert_eq!(grants_claim.deployment_id, grants_id);
    let DesiredGrants::Present { inbounds } = &grants_claim.desired.grants else {
        panic!("isolated grants debt should be a complete runtime client list");
    };
    assert!(inbounds
        .iter()
        .flat_map(|inbound| &inbound.clients)
        .any(|client| client.email == "frank@platform.acme#i-main"));
    db.store
        .report_target_result(applied_report(&grants_claim))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn service_restore_and_new_grant_leave_no_orphaned_isolation_debt() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    store_current_model_snapshot(db.pool(), &db.store).await;

    let config = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "restore-grant-race-config"),
        )
        .await
        .unwrap();
    db.store
        .isolate_deployment_target(
            &system_admin(),
            config.deployment_id,
            "n1",
            IsolateDeploymentTargetRequest {
                expected_target_status: "pending".to_owned(),
                reason: "prepare restore and grant race".to_owned(),
                acknowledge_uncertain: false,
            },
        )
        .await
        .unwrap();
    let debt = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_target_result(applied_report(&debt))
        .await
        .unwrap();
    db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll("n1", Some("test-agent"))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_agent_state
            SET runtime_reported_at = now(),
                wireguard_health = '{\"enabled\":true,\"peers\":[]}'::jsonb
          WHERE node_id = 'n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'grace', 'd21cd35b-51ab-4e25-82b0-a84a8eab9d7c')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                user_id: "grace".to_owned(),
                ingress_id: "i-main".to_owned(),
                enabled: true,
                note: Some("race grant with service restore".to_owned()),
            },
        )
        .await
        .unwrap();

    let admin = system_admin();
    let (restore, automation) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            db.store.restore_node_service(
                &admin,
                "n1",
                RestoreNodeServiceRequest {
                    reason: "race-safe restore".to_owned(),
                },
            ),
            db.store.process_grant_automation(),
        )
    })
    .await
    .expect("restore/grant race must not deadlock");
    assert!(restore.is_ok() || automation.is_ok());

    let isolated: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM node_operational_isolations WHERE node_id = 'n1')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let active_debt: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node_convergence_obligations
          WHERE node_id = 'n1'
            AND status IN (
                'pending', 'dispatched', 'converging', 'failed-recovered', 'failed-dirty'
            )",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        isolated || active_debt == 0,
        "a restored node must never be left with debt only the isolation path can deliver"
    );
}

async fn insert_minimal_fixture(pool: &PgPool) {
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

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.acme', 'alice', '2d2304da-f114-4574-8d44-625afdb1db5c')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO apps (id, label, position) VALUES ('app-main', 'Main App', 0)")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-main', 'app-main', 'platform.acme', 'Main Chain', 0)",
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
            'i-main', 'app-main', 'c-main', 'n1', '0.0.0.0', 443, NULL, 'vless-reality',
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
         VALUES ('i-main', 'chrome')",
    )
    .execute(pool)
    .await
    .unwrap();

    let rules = json!([
        {
            "match": { "t": "any" },
            "action": { "t": "egress", "send_through": null }
        }
    ]);
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules)
         VALUES ('c-main', 'n1', $1)",
    )
    .bind(rules)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-main', 'platform.acme', 'alice', 'i-main')",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_other_tenant_fixture(pool: &PgPool) {
    sqlx::query("INSERT INTO tenants (id, name) VALUES ('platform.other', 'Platform Other')")
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
            'n-other', 'platform.other', 'Other Node', 'other.example.net', '10.66.0.9',
            'wg-private-other', 'wg-public-other', 51829,
            10095, TRUE, TRUE,
            'servers', '[\"8.8.8.8\"]'::jsonb
         )",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO users (tenant_id, id, uuid)
         VALUES ('platform.other', 'charlie', 'f98b74ba-58f1-41d0-aaad-8fa5724c6d2d')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO apps (id, label, position) VALUES ('app-other', 'Other App', 1)")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name, position)
         VALUES ('c-other', 'app-other', 'platform.other', 'Other Chain', 0)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-other', 'app-other', 'c-other', 'n-other', '0.0.0.0', 443, NULL, 'vless-reality',
            'reality-private-other', 'reality-public-other', '[\"9337a0bf\"]'::jsonb,
            'www.other.example.com:443', '[\"www.other.example.com\"]'::jsonb, 'xtls-rprx-vision',
            'custom-site'
         )",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ingress_client_settings (ingress_id, reality_fingerprint)
         VALUES ('i-other', 'chrome')",
    )
    .execute(pool)
    .await
    .unwrap();

    let rules = json!([
        {
            "match": { "t": "any" },
            "action": { "t": "egress", "send_through": null }
        }
    ]);
    sqlx::query(
        "INSERT INTO steps (chain_id, node_id, rules)
         VALUES ('c-other', 'n-other', $1)",
    )
    .bind(rules)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-other', 'platform.other', 'charlie', 'i-other')",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn assert_insert_ingress_short_ids_fails(
    pool: &PgPool,
    id: &str,
    port: i32,
    short_ids: serde_json::Value,
) {
    let result = insert_ingress_short_ids(pool, id, port, short_ids).await;
    assert!(
        result.is_err(),
        "expected invalid reality_short_ids to fail"
    );
}

async fn insert_ingress_short_ids(
    pool: &PgPool,
    id: &str,
    port: i32,
    short_ids: serde_json::Value,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode
         ) VALUES (
            $1, 'app-main', 'c-main', 'n1', '0.0.0.0', $2, NULL, 'vless-reality',
            'reality-private', 'reality-public', $3,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'xtls-rprx-vision',
            'custom-site'
         )",
    )
    .bind(id)
    .bind(port)
    .bind(short_ids)
    .execute(pool)
    .await
}

/// Cascading deletes: deleting a middle hop removes the whole subtree reachable beneath it and
/// clears the rules referencing it; deleting the head deletes the whole chain (ingress, grants,
/// chain declaration).
///
/// Both paths are verified: the direct delete (`delete_step`, behind HTTP DELETE) and the draft
/// commit (`apply_draft` with `DeleteStep`, which is what the UI's delete takes).
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn delete_step_cascades_subtree_and_whole_chain() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    for (id, ip, wg, port) in [
        ("n2", "10.66.0.2", "wg-private-2", 51822),
        ("n3", "10.66.0.3", "wg-private-3", 51823),
        ("n4", "10.66.0.4", "wg-private-4", 51824),
    ] {
        sqlx::query(
            "INSERT INTO nodes (
                id, tenant_id, name, public_ipv4, overlay_addr,
                wg_private_key, wg_public_key, wg_listen_port,
                api_port, overlay, egress_allowed, dns_kind, dns_servers
             ) VALUES ($1, 'platform.acme', $1, $1 || '.example.net', $2::inet,
                       $3, $3 || '-pub', $4,
                       10100, TRUE, TRUE, 'servers', '[\"1.1.1.1\"]'::jsonb)",
        )
        .bind(id)
        .bind(ip)
        .bind(wg)
        .bind(port)
        .execute(db.pool())
        .await
        .unwrap();
    }
    db.store
        .upsert_chain(
            &system_admin(),
            "app-main",
            CreateChainRequest {
                id: "c-sobacu".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Sub".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();

    // A chain must carry an ingress (chain.no-ingress is a compilation error); it sits on n1,
    // which makes n1 the head.
    sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-toremi', 'app-main', 'c-sobacu', 'n1', '0.0.0.0', 8444, NULL, 'vless-reality',
            'reality-private', 'reality-public', '[\"8337a0bf\"]'::jsonb,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'xtls-rprx-vision',
            'custom-site'
         )",
    )
    .execute(db.pool())
    .await
    .unwrap();

    async fn put(db: &TestPg, chain: &str, node: &str, to: &[&str]) {
        let mut rules = to
            .iter()
            .map(|to| Rule {
                dest_match: DestMatch::Any,
                action: Action::Forward {
                    to: to.to_string(),
                    dial: HopDial::Overlay,
                    pool: HopPool::None,
                },
            })
            .collect::<Vec<_>>();
        if to.is_empty() {
            rules.push(Rule {
                dest_match: DestMatch::Any,
                action: Action::Egress { send_through: None },
            });
        }
        put_step_draft(
            db,
            "app-main",
            chain,
            node,
            PutStepRequest {
                accept: Some(StepAcceptRequest {
                    uuid: None,
                    label: None,
                }),
                hop_in: Some(HopInRequest {
                    port: 20000,
                    security: Some(HopWireRequest::Encryption),
                }),
                rules,
                note: None,
            },
        )
        .await;
    }
    async fn steps_of(db: &TestPg, chain: &str) -> Vec<String> {
        sqlx::query("SELECT node_id FROM steps WHERE chain_id = $1 ORDER BY node_id")
            .bind(chain)
            .fetch_all(db.pool())
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get::<String, _>("node_id").unwrap())
            .collect()
    }
    async fn forward_targets(db: &TestPg, chain: &str, node: &str) -> Vec<String> {
        let rules: serde_json::Value =
            sqlx::query("SELECT rules FROM steps WHERE chain_id = $1 AND node_id = $2")
                .bind(chain)
                .bind(node)
                .fetch_one(db.pool())
                .await
                .unwrap()
                .try_get("rules")
                .unwrap();
        rules
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|rule| {
                rule.get("a")?.get("t")?.as_str().and_then(|t| {
                    (t == "forward")
                        .then(|| rule["a"]["to"].as_str().unwrap_or_default().to_owned())
                })
            })
            .collect()
    }

    // ── (1) Deleting a middle hop: the whole subtree is removed and references cleared ──
    // n1 → n2 → n3, with n2 also forking to n4. Deleting n2 removes the n2/n3/n4 rows and clears
    // the Forward → n2 entry from n1's rule table.
    put(&db, "c-sobacu", "n1", &["n2"]).await;
    put(&db, "c-sobacu", "n2", &["n3", "n4"]).await;
    put(&db, "c-sobacu", "n3", &[]).await;
    put(&db, "c-sobacu", "n4", &[]).await;

    let removed = db
        .store
        .delete_step(&system_admin(), "app-main", "c-sobacu", "n2")
        .await
        .unwrap();
    assert!(removed.deleted, "删中间跳必须真删掉东西");
    assert!(!removed.chain_removed);
    assert_eq!(removed.removed_steps, vec!["n2", "n3", "n4"]);
    assert_eq!(
        steps_of(&db, "c-sobacu").await,
        vec!["n1"],
        "子树之外只剩 n1"
    );
    assert!(
        forward_targets(&db, "c-sobacu", "n1").await.is_empty(),
        "n1 规则表里指向 n2 的 Forward 必须被清掉"
    );

    // After the delete the compile must be green: no dangling references and no unreachable
    // members.
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    // Idempotent: deleting again is a no-op and the revision number falls back.
    let again = db
        .store
        .delete_step(&system_admin(), "app-main", "c-sobacu", "n2")
        .await
        .unwrap();
    assert!(!again.deleted);

    // ── (2) Deleting the head: the whole chain goes (declaration, ingress, grants) ──
    // c-main's ingress i-main sits on n1, which makes n1 the head.
    // A historical usage row referencing i-main is inserted first: since 0001's section 0026,
    // usage_samples has no foreign key to ingresses, and a record of historical fact does not
    // block deleting the model.
    sqlx::query(
        "INSERT INTO usage_samples (
            sampled_at, window_start, window_end, node_id, tenant_id, user_id,
            ingress_id, grant_label, uplink_bytes, downlink_bytes
         ) VALUES (now(), now() - interval '1 hour', now(), 'n1',
                   'platform.acme', 'alice', 'i-main', 'alice@i-main', 100, 200)",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let head = db
        .store
        .delete_step(&system_admin(), "app-main", "c-main", "n1")
        .await
        .unwrap();
    assert!(head.deleted);
    assert!(head.chain_removed, "删链头 = 整条链删除");
    assert_eq!(head.removed_steps, vec!["n1"], "c-main 链的成员清单");

    let chains_left: i64 = sqlx::query("SELECT count(*) FROM chains WHERE id = 'c-main'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(chains_left, 0, "链声明随链头删除一起消失");
    let ingresses_left: i64 = sqlx::query("SELECT count(*) FROM ingresses WHERE id = 'i-main'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(ingresses_left, 0, "接入面随链删除");
    let grants_left: i64 = sqlx::query("SELECT count(*) FROM grants WHERE ingress_id = 'i-main'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(grants_left, 0, "授权关系随接入面删除");
    let usage: Option<String> =
        sqlx::query("SELECT ingress_id::text FROM usage_samples WHERE node_id = 'n1'")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get(0)
            .unwrap();
    assert_eq!(
        usage.as_deref(),
        Some("i-main"),
        "历史用量样本保留，ingress_id 原值不动（0026 段起就无外键，是事实记录）"
    );

    // ── (3) The draft-commit path: the same DeleteStep op, the same cascade ──
    put(&db, "c-sobacu", "n1", &["n2"]).await;
    put(&db, "c-sobacu", "n2", &[]).await;
    let applied = db
        .store
        .apply_draft(
            &system_admin(),
            vec![brocade_store::ModelOp::DeleteStep {
                app_id: "app-main".to_owned(),
                chain_id: "c-sobacu".to_owned(),
                node_id: "n2".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(applied.changed, 1);
    assert_eq!(steps_of(&db, "c-sobacu").await, vec!["n1"]);
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    // ── (3.5) A dead chain blocks the release: the head may not exit and the subtree was removed
    // (the hole in the rule table got a Block fallback) ──
    // This is exactly the "cascade killed the chain" scenario. Compilation reports
    // chain.no-egress-path and create_deployment is blocked — the user must give the head a new
    // exit or delete the whole chain.
    sqlx::query("UPDATE nodes SET egress_allowed = FALSE WHERE id = 'n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let blocked = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(applied.revision_id, "deploy-dead"),
        )
        .await;
    assert!(
        matches!(&blocked, Err(StoreError::InvalidData(m)) if m.contains("deployment blocked by compiler: 1 error(s)")),
        "死链必须挡发布：{blocked:?}",
    );
    // No release happened, and the deployments table holds no row for it.
    let deployments: i64 =
        sqlx::query("SELECT count(*) FROM deployments WHERE idempotency_key = 'deploy-dead'")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get(0)
            .unwrap();
    assert_eq!(deployments, 0);

    // ── (4) The explicit DeleteChain draft operation: the whole chain goes without touching the
    // head, grants cleared along with it ──
    // The list page's delete button takes this path; the head detection in delete_step is merely
    // its convenience entrance.
    sqlx::query(
        "INSERT INTO grants (app_id, tenant_id, user_id, ingress_id)
         VALUES ('app-main', 'platform.acme', 'alice', 'i-toremi')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let applied = db
        .store
        .apply_draft(
            &system_admin(),
            vec![brocade_store::ModelOp::DeleteChain {
                app_id: "app-main".to_owned(),
                chain_id: "c-sobacu".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(applied.changed, 1);
    let c: i64 = sqlx::query("SELECT count(*) FROM chains WHERE id = 'c-sobacu'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let i: i64 = sqlx::query("SELECT count(*) FROM ingresses WHERE id = 'i-toremi'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let g: i64 = sqlx::query("SELECT count(*) FROM grants WHERE ingress_id = 'i-toremi'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let s: i64 = sqlx::query("SELECT count(*) FROM steps WHERE chain_id = 'c-sobacu'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!((c, i, g, s), (0, 0, 0, 0), "链、接入面、授权、成员行全清");
}

/// Stranded steps are removed by `PruneChain` once the whole rule tree has landed.
///
/// What this test watches is the order. The console saving a rule tree is a run of `PutStep`s
/// replayed one at a time by the draft, and every intermediate state is incomplete. The browser
/// used to compute who was stranded itself and wedge a `DeleteStep` into the middle of that run,
/// so that:
///
/// - it saw only the current table's draft while the others held the database's old rules;
/// - `DeleteStep` also clears whatever is unreachable from the head, and at that instant the new
///   target's `PutStep` had landed while the rule pointing at it had not — so the new target was
///   deleted on the spot as a dangling row.
///
/// The symptom is compilation reporting `relay.no-accept` after committing: a rule points at that
/// machine and its step is gone.
///
/// The shape now is "every `PutStep` lands, then one `PruneChain`", with the test on the server,
/// which sees the whole chain. Both stages are verified.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn prune_chain_drops_stranded_steps_after_whole_tree_lands() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    for (id, ip, wg, port) in [
        ("n2", "10.66.0.2", "wg-private-2", 51822),
        ("n3", "10.66.0.3", "wg-private-3", 51823),
        ("n4", "10.66.0.4", "wg-private-4", 51824),
    ] {
        sqlx::query(
            "INSERT INTO nodes (
                id, tenant_id, name, public_ipv4, overlay_addr,
                wg_private_key, wg_public_key, wg_listen_port,
                api_port, overlay, egress_allowed, dns_kind, dns_servers
             ) VALUES ($1, 'platform.acme', $1, $1 || '.example.net', $2::inet,
                       $3, $3 || '-pub', $4,
                       10100, TRUE, TRUE, 'servers', '[\"1.1.1.1\"]'::jsonb)",
        )
        .bind(id)
        .bind(ip)
        .bind(wg)
        .bind(port)
        .execute(db.pool())
        .await
        .unwrap();
    }

    // One PutStep draft operation. An empty `to` means exiting from this machine.
    fn put(node: &str, to: &[&str]) -> brocade_store::ModelOp {
        let mut rules = to
            .iter()
            .map(|to| Rule {
                dest_match: DestMatch::Any,
                action: Action::Forward {
                    to: to.to_string(),
                    dial: HopDial::Overlay,
                    pool: HopPool::None,
                },
            })
            .collect::<Vec<_>>();
        if to.is_empty() {
            rules.push(Rule {
                dest_match: DestMatch::Any,
                action: Action::Egress { send_through: None },
            });
        }
        brocade_store::ModelOp::PutStep {
            app_id: "app-main".to_owned(),
            chain_id: "c-main".to_owned(),
            node_id: node.to_owned(),
            step: PutStepRequest {
                accept: Some(StepAcceptRequest {
                    uuid: None,
                    label: None,
                }),
                hop_in: Some(HopInRequest {
                    port: 20000,
                    security: Some(HopWireRequest::Encryption),
                }),
                rules,
                note: None,
            },
        }
    }
    let prune = || brocade_store::ModelOp::PruneChain {
        app_id: "app-main".to_owned(),
        chain_id: "c-main".to_owned(),
    };
    async fn steps_of(db: &TestPg) -> Vec<String> {
        sqlx::query("SELECT node_id FROM steps WHERE chain_id = 'c-main' ORDER BY node_id")
            .fetch_all(db.pool())
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get::<String, _>("node_id").unwrap())
            .collect()
    }

    // Starting point: n1 (the head) → n2 → n3, with n2 also forking to n4.
    db.store
        .apply_draft(
            &system_admin(),
            vec![
                put("n1", &["n2"]),
                put("n2", &["n3", "n4"]),
                put("n3", &[]),
                put("n4", &[]),
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(steps_of(&db).await, ["n1", "n2", "n3", "n4"]);

    // ── (1) Rerouting to a machine not yet on the chain ──
    // n1 moves from pointing at n2 to pointing at n4 (which is still n2's downstream at that
    // moment), stranding n2 and n3. Pruning after the whole run lands: n2 and n3 are removed, n4
    // stays, and the compile is green.
    db.store
        .apply_draft(&system_admin(), vec![put("n1", &["n4"]), prune()], None)
        .await
        .unwrap();
    assert_eq!(
        steps_of(&db).await,
        ["n1", "n4"],
        "n2/n3 落单摘掉；n4 是新的下游，必须留着"
    );
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    // ── (2) Two tables changed together: one detaches and the other attaches ──
    // This is the relay.no-accept scenario verbatim. n1 → n2 → n3 with n2 also pointing at n4,
    // changed so that n1 attaches n4 directly and n2 keeps only n3. Not one machine should be
    // lost — n4 merely changed upstreams.
    db.store
        .apply_draft(
            &system_admin(),
            vec![
                put("n1", &["n2"]),
                put("n2", &["n3", "n4"]),
                put("n3", &[]),
                prune(),
            ],
            None,
        )
        .await
        .unwrap();
    assert_eq!(steps_of(&db).await, ["n1", "n2", "n3", "n4"]);

    db.store
        .apply_draft(
            &system_admin(),
            vec![put("n1", &["n2", "n4"]), put("n2", &["n3"]), prune()],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        steps_of(&db).await,
        ["n1", "n2", "n3", "n4"],
        "n4 换了上游，不是落单——从前这里会被误摘，然后编译报 relay.no-accept"
    );
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    let output = compile(&snapshot);
    assert_eq!(output.summary.errors, 0, "{:#?}", output.diagnostics);

    // ── (3) On an already clean chain it is a no-op and the revision number falls back ──
    let before: i64 = sqlx::query("SELECT max(id) FROM revisions")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let applied = db
        .store
        .apply_draft(&system_admin(), vec![prune()], None)
        .await
        .unwrap();
    assert_eq!(applied.changed, 0, "没有落单的就什么都不该删");
    let after: i64 = sqlx::query("SELECT max(id) FROM revisions")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(before, after, "空操作不留修订");

    // ── (4) Calling the HTTP path (prune_chain) directly behaves the same ──
    // Pressing "clean up stranded steps" takes this route: no draft involved, computed from the
    // database as it stands.
    db.store
        .apply_draft(&system_admin(), vec![put("n1", &["n2"])], None)
        .await
        .unwrap();
    let pruned = db
        .store
        .prune_chain(&system_admin(), "app-main", "c-main")
        .await
        .unwrap();
    assert_eq!(pruned.removed_steps, ["n4"], "n1 不指 n4 了，它就落单了");
    assert_eq!(steps_of(&db).await, ["n1", "n2", "n3"]);
}

/// Link views are scoped by visibility rather than gated by rank.
///
/// These two used to be system-admin only, on the grounds that the backbone is one global network
/// and cannot be scoped. It can: nodes have owners, and probe rows filter by node ownership. And
/// gating them costs a global read-only role — the very role that inspects with them — every last
/// number.
///
/// The two are scoped differently, and that difference is what this test watches:
/// - the node table is scoped strictly (it answers whose MTU should change, and the place to
///   change it is in view anyway);
/// - the link table keeps rows where at least one end is in view (a path's suggestion is decided
///   by both ends together, and filtering out rows whose peer is out of view raises the remaining
///   minimum — too large means large packets silently dropped).
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn link_mtu_view_is_scoped_by_tenant_not_gated_by_role() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    for id in ["platform.acme", "platform.other"] {
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $1)")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    for id in ["hk-01", "sg-01", "out-01"] {
        db.store
            .provision_node(&system_admin(), provision_node_request(id))
            .await
            .unwrap();
    }
    // out-01 moves to another branch. The tenant at provision time is fixed, and only ownership
    // moves here.
    sqlx::query("UPDATE nodes SET tenant_id = 'platform.other' WHERE id = 'out-01'")
        .execute(db.pool())
        .await
        .unwrap();
    let now: i64 = sqlx::query_scalar("SELECT extract(epoch FROM now())::bigint")
        .fetch_one(db.pool())
        .await
        .unwrap();

    db.store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: now,
                links: vec![
                    LinkProbe {
                        peer_node_id: "sg-01".to_owned(),
                        endpoint_host: "sg1.example.net".to_owned(),
                        status: LinkProbeStatus::Ok,
                        path_mtu: Some(1500),
                        suggested_wg_mtu: Some(1440),
                    },
                    // The cross-branch one: its peer is out of view, and it is the path holding
                    // hk-01 down
                    LinkProbe {
                        peer_node_id: "out-01".to_owned(),
                        endpoint_host: "out1.example.net".to_owned(),
                        status: LinkProbeStatus::Ok,
                        path_mtu: Some(1258),
                        suggested_wg_mtu: Some(1198),
                    },
                ],
            },
        )
        .await
        .unwrap();

    // readonly can read it too — this used to be Forbidden.
    let readonly = AdminContext::new("vis", AdminRole::Readonly, Some("platform.acme".to_owned()));
    let view = db.store.link_mtu_view(&readonly).await.unwrap();

    let ids: Vec<&str> = view.nodes.iter().map(|n| n.node_id.as_str()).collect();
    assert_eq!(ids, vec!["hk-01", "sg-01"], "节点表裁死在自己这一支");

    let hk = view.nodes.iter().find(|n| n.node_id == "hk-01").unwrap();
    assert_eq!(
        hk.suggested_mtu,
        Some(1198),
        "跨支那条路照样把建议值压下来——滤掉它的话这里会是 1440，偏大 242"
    );
    assert_eq!(hk.tightest_peer.as_deref(), Some("out-01"));
    assert_eq!(view.links.len(), 2, "两条都留着：各有一端在视野里");

    // A global operator sees everything.
    let all = db.store.link_mtu_view(&system_admin()).await.unwrap();
    assert_eq!(all.nodes.len(), 3);
}

/// A first start against a server that has no such database creates it, and a second start finds
/// it already there.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn a_missing_database_is_created_and_then_left_alone() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let url = db.url_for("brocade_fresh_install");

    let created = PgStore::create_database_if_absent(&url)
        .await
        .expect("建库不该失败");
    assert_eq!(
        created.as_deref(),
        Some("brocade_fresh_install"),
        "第一次要报出建了哪个库——main.rs 靠这个名字把打错的 DATABASE_URL 喊出来"
    );

    assert_eq!(
        PgStore::create_database_if_absent(&url).await.unwrap(),
        None,
        "第二次不该报建过"
    );

    let store = PgStore::connect(&url).await.expect("连得上新建的库");
    store.migrate().await.expect("迁得动");
    let revisions = store
        .list_revisions(&system_admin(), 10)
        .await
        .expect("读得了");
    let baseline = revisions.revisions.first().expect("新库该有基线修订");
    assert_eq!(baseline.note.as_deref(), Some("initial schema revision"));
    assert!(baseline.current, "基线就是当前修订");
}

/// The dangerous half. "Cannot connect" covers a wrong password just as much as a missing database,
/// and mistaking one for the other would quietly stand up a second, empty database next to the real
/// one while the operator stares at a console that has lost everything.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn a_wrong_password_is_not_mistaken_for_a_missing_database() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let wrong = db.url_for("brocade_never_created").replace(
        "postgres://postgres:postgres@",
        "postgres://postgres:not-the-password@",
    );

    let error = PgStore::create_database_if_absent(&wrong)
        .await
        .expect_err("密码错了就该报错");
    let text = error.to_string();
    assert!(
        text.contains("password") || text.contains("authenticat"),
        "报出来的该是认证失败，实际是：{text}"
    );

    let exists: bool = sqlx::query_scalar(
        "select exists (select 1 from pg_database where datname = 'brocade_never_created')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!exists, "密码错了却把库建出来了");
}

/// The nine QUIC columns, from the console request down to what the compiler reads back.
///
/// Worth a database rather than a unit test for the reason `pg_reality_guard` states: this INSERT
/// takes positional parameters, and these nine were appended after `created_revision` precisely so
/// that nothing before them shifts. A column inserted in the wrong place produces no compile error
/// and no failure anywhere else — the ingress simply comes back with its neighbour's value. Every
/// field here carries a distinct number so a swap cannot pass.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn hysteria2_quic_tuning_round_trips_field_by_field() {
    use brocade_core::model::{HysteriaBbrProfile, HysteriaQuic};

    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let tuned = HysteriaQuic {
        init_stream_receive_window: Some(131_072),
        max_stream_receive_window: Some(262_144),
        init_connection_receive_window: Some(327_680),
        max_connection_receive_window: Some(655_360),
        max_idle_timeout_secs: Some(45),
        keep_alive_period_secs: Some(15),
        max_incoming_streams: Some(64),
        disable_path_mtu_discovery: true,
    };
    let face = |quic: HysteriaQuic, profile: HysteriaBbrProfile| CreateIngressRequest {
        wires: WiresRequest {
            vless: None,
            anytls: None,
            hysteria2: Some(Hysteria2 {
                port: 50000,
                hop: None,
                bandwidth: HysteriaBandwidth::default(),
                congestion: HysteriaCongestion::Reno,
                bbr_profile: profile,
                quic,
                obfs: None,
                masquerade: HysteriaMasquerade::NotFound,
            }),
        },
        id: "i-pucivo".to_owned(),
        chain_id: "c-main".to_owned(),
        node_id: "n1".to_owned(),
        bind: "0.0.0.0".parse().unwrap(),
        port: 8443,
        front_id: None,
        guard: brocade_core::model::IngressGuard::OPEN,
        reality: CreateRealityIngressRequest {
            fallback_mode: None,
            fallback_limits: None,
            fallback_guard: None,
            dest: None,
            server_names: Vec::new(),
            fingerprint: None,
            flow: None,
        },
        projection: Projection::default(),
        note: None,
    };

    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(tuned, HysteriaBbrProfile::Aggressive),
        )
        .await
        .unwrap();

    // Read through the compiler's path, not the write's echo — the echo can be right while the
    // row is not.
    let read_back = || async {
        let snapshot = db.store.materialize_snapshot(None).await.unwrap();
        snapshot
            .apps
            .iter()
            .flat_map(|app| &app.ingresses)
            .find(|ingress| ingress.id == "i-pucivo")
            .and_then(|ingress| ingress.wires.hysteria2().cloned())
            .expect("接入面还在")
    };
    let stored = read_back().await;
    assert_eq!(stored.quic, tuned, "九个字段有一个对不上就是绑定错位了");
    assert_eq!(stored.bbr_profile, HysteriaBbrProfile::Aggressive);
    assert_eq!(
        stored.congestion,
        HysteriaCongestion::Reno,
        "reno 是本次新加的第四档"
    );

    // Clearing goes back to all-absent rather than sticking at whatever landed first.
    db.store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(HysteriaQuic::default(), HysteriaBbrProfile::Standard),
        )
        .await
        .unwrap();
    let cleared = read_back().await;
    assert_eq!(cleared.quic, HysteriaQuic::default());
    assert_eq!(cleared.bbr_profile, HysteriaBbrProfile::Standard);

    // The CHECKs are the durable half of the range rules; the compiler's diagnostic is the other.
    let below_floor = HysteriaQuic {
        init_stream_receive_window: Some(1024),
        ..HysteriaQuic::default()
    };
    let rejected = db
        .store
        .upsert_ingress(
            &system_admin(),
            "app-main",
            face(below_floor, HysteriaBbrProfile::Standard),
        )
        .await;
    assert!(rejected.is_err(), "小于 16384 的窗口应当被列约束挡下");
}
