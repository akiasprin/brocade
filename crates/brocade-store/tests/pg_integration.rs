use brocade_core::{
    compile::compile,
    model::{
        Action, ConnectionSettings, DestMatch, Dns, DomainStrategy, ExternalOutboundProtocol,
        ExternalOutboundSecurity, ExternalVlessTransport, ExternalVlessXhttp,
        ExternalVlessXhttpDownload, HopDial, HopPool, Hysteria2, HysteriaBandwidth,
        HysteriaCongestion, HysteriaMasquerade, HysteriaObfs, IngressWires, ModelSettings,
        NodeConnection, OverlaySettings, Projection, ProjectionDownloadEndpoint,
        ProjectionEndpoint, RealityClientPolicy, RealityFallbackLimits, RealityFallbackMode,
        RealityFallbackRateLimit, RealitySite, Rule, Xhttp, XhttpMode,
    },
};
use brocade_deployment::plan::{
    AppliedArtifactState, AppliedGrantsState, DeploymentKind, DeploymentPlan, DesiredArtifact,
    DesiredGrants, ObservedClient, ObservedInbound, PlannedAction, PlannedTarget,
    PlannedTargetStatus,
};
use brocade_deployment::protocol::{
    GeodataFileState, GeodataObservation, LocalReconcileReport, NodeRuntimeReport, NodeVersions,
    SpoolBacklog,
};
use brocade_store::{
    generate_reality_short_id, is_reality_short_id, node_token_display_prefix, node_token_hash,
    AdminContext, AdminLoginRequest, AdminRole, CertDomainInput, ChangeAdminPasswordRequest,
    CreateAdminOperatorRequest, CreateChainRequest, CreateDeploymentRequest, CreateGrantRequest,
    CreateIngressRequest, CreateRealityIngressRequest, CreateRollbackRequest, CreateUserRequest,
    HopInRequest, HopWireRequest, LinkProbe, LinkProbeRequest, LinkProbeStatus, ModelOp,
    NodeDesiredDeployment, PgStore, ProbeTransport, ProvisionNodeRequest, PutStepRequest,
    ReportedNodeState, SetUserAppQuotaRequest, StepAcceptRequest, StoreError, TargetApplyResult,
    TargetConvergenceReport, TransportRequest, UpdateNodeRequest, UpdateUserStatusRequest,
    UpsertExternalOutboundRequest, UsageCounter, UsageReportRequest, VerifyDeploymentRequest,
    WiresRequest, ENROLLMENT_TOKEN_PREFIX, NODE_TOKEN_PREFIX,
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
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn external_outbound_round_trips_sealed_and_redacted() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let request = UpsertExternalOutboundRequest {
        app_id: "app-main".to_owned(),
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

    let sealed: String = sqlx::query(
        "SELECT credential_sealed FROM external_outbounds WHERE app_id = 'app-main' AND id = 'vendor-edge'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("credential_sealed")
    .unwrap();
    assert!(sealed.starts_with("v1."));
    assert!(!sealed.contains(request.protocol.credential()));

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
async fn external_vless_xhttp_round_trips_and_legacy_rows_default_to_raw() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

    let request = UpsertExternalOutboundRequest {
        app_id: "app-main".to_owned(),
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

    let options: serde_json::Value = sqlx::query(
        "SELECT protocol_options FROM external_outbounds WHERE app_id = 'app-main' AND id = 'xhttp-edge'",
    )
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
        "UPDATE external_outbounds SET protocol_options = protocol_options - 'transport' WHERE app_id = 'app-main' AND id = 'xhttp-edge'",
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
                ports: Default::default(),
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

    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert_eq!(snapshot.revision, result.revision_id);
    assert_eq!(
        snapshot.settings.reality_client.max_client_ver.as_deref(),
        Some("1.9.9")
    );
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
async fn deployment_schema_matches_convergence_design() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;

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
    assert_column_exists(db.pool(), "deployment_target_state", "desired_grants").await;
    assert_column_exists(db.pool(), "deployment_target_state", "dispatched_grants").await;
    assert_table_exists(db.pool(), "deployment_wave_confirmations").await;
    assert_column_exists(db.pool(), "deployments", "sync_of_deployment_id").await;
    assert_column_exists(db.pool(), "usage_samples", "has_gap").await;
    assert_constraint_missing(db.pool(), "usage_samples_ingress_id_fkey").await;
    assert_constraint_missing(db.pool(), "usage_chain_samples_app_id_fkey").await;
    assert_constraint_missing(db.pool(), "usage_chain_samples_chain_id_fkey").await;
    assert_column_exists(db.pool(), "control_state", "reality_min_client_ver").await;
    assert_column_exists(db.pool(), "control_state", "reality_max_client_ver").await;
    assert_column_exists(db.pool(), "control_state", "reality_max_time_diff_ms").await;

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
async fn clash_subscription_is_fresh_by_uuid_and_reports_combined_quota() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    set_quota(&db, Some(10_000)).await;
    insert_month_sample(db.pool(), "i-main", "app-main", 1, 1_200, 800).await;

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

    // No artifact cache: changing the live model is visible on the next call to the same URL.
    sqlx::query("UPDATE chains SET name = 'Renamed Chain' WHERE id = 'c-main'")
        .execute(db.pool())
        .await
        .unwrap();
    let renamed = db.store.clash_subscription_by_uuid(uuid).await.unwrap();
    assert!(renamed.content.contains("name: \"Renamed Chain\""));
    assert!(!renamed.content.contains("name: \"Main Chain\""));

    // Rotating the UUID invalidates the old bearer immediately and creates no compatibility
    // window. The new UUID resolves the same current user.
    let next_uuid = "f98b74ba-58f1-41d0-aaad-8fa5724c6d2d";
    sqlx::query(
        "UPDATE users SET uuid = $1::uuid WHERE tenant_id = 'platform.acme' AND id = 'alice'",
    )
    .bind(next_uuid)
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
    assert!(matches!(
        db.store.clash_subscription_by_uuid(next_uuid).await,
        Err(StoreError::NotFound(_))
    ));
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

    db.store
        .record_node_runtime(
            "n1",
            &NodeRuntimeReport {
                certificate: Default::default(),
                versions: versions.clone(),
                geodata: Some(geodata.clone()),
                local_reconcile: Some(reconcile.clone()),
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
                geodata_observed,
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
        "SELECT runtime_versions, spool_backlog, last_local_reconcile, geodata_observed
         FROM node_agent_state WHERE node_id = 'n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    let kept: LocalReconcileReport =
        serde_json::from_value(row.try_get("last_local_reconcile").unwrap()).unwrap();
    assert_eq!(kept, reconcile, "空报告把上一次的本地对账抹掉了");
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
                note: None,
            },
        )
        .await
        .unwrap();

    async fn set(db: &TestPg, security: HopWireRequest) {
        db.store
            .put_step(
                &system_admin(),
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
            .await
            .unwrap();
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

    let result = db
        .store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: 1_785_000_000,
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
                probed_at_unix_secs: 1_785_003_600,
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
    sqlx::query(
        "UPDATE nodes SET wg_transport = '{\"t\":\"fake_tcp\",\"v\":{\"port\":39743}}'::jsonb
         WHERE id = 'phantun-01'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET public_ipv4_nat = TRUE WHERE id = 'nat-01'")
        .execute(db.pool())
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
        "SELECT wave, disruptive, desired_structure, desired_grants, dispatched_grants
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
    store_current_model_snapshot(db.pool(), &db.store).await;

    db.store
        .create_deployment(
            &system_admin(),
            create_deployment_request(1, "grants-auto-base"),
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
    let DesiredGrants::Present { inbounds } = desired.desired.grants else {
        panic!("grants should be present");
    };
    assert!(
        inbounds
            .iter()
            .flat_map(|inbound| &inbound.clients)
            .all(|client| client.email != "alice@platform.acme#i-main"),
        "the automatic order must contain the revoked state (probe identities may remain)"
    );
}

// Grants are a compiled artifact, not just rows in the grants table.  A global flow change alters
// every client account on an ingress that follows the global REALITY site, so it must enter the
// same durable automation path even though no grant row was directly edited.
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn an_indirect_grants_artifact_change_is_automatically_queued() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "UPDATE ingresses
         SET reality_dest = NULL,
             reality_server_names = '[]'::jsonb,
             reality_fingerprint = NULL,
             reality_flow = NULL,
             reality_fallback_mode = 'global-site'
         WHERE id = 'i-main'",
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

    let outcome = db.store.process_grant_automation().await.unwrap();
    assert_eq!(outcome.revision_id, Some(changed.revision_id));
    assert!(outcome.waiting.is_none());
    let deployment_id = outcome
        .deployment_id
        .expect("the changed client flow should create a grants order");
    let desired = db
        .store
        .claim_desired_for_node("n1")
        .await
        .unwrap()
        .expect("the indirect permission order should be claimable");
    assert_eq!(desired.deployment_id, deployment_id);
    let DesiredGrants::Present { inbounds } = desired.desired.grants else {
        panic!("grants should be present");
    };
    let alice = inbounds
        .iter()
        .flat_map(|inbound| &inbound.clients)
        .find(|client| client.email == "alice@platform.acme#i-main")
        .expect("Alice should remain granted");
    assert_eq!(
        alice.flow, None,
        "the automatic order must carry the indirectly changed client flow"
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
        "SELECT desired_grants, desired_structure->>'grants_revision' AS grants_revision
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

    let result = db
        .store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

    assert_eq!(result.target_status, "succeeded");
    assert_eq!(result.deployment_status, "succeeded");

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

// A reported reading's instant has to sit close to the control plane's present:
// record_usage_report checks it against now() and refuses the whole round beyond 10 minutes
// (MAX_CLOCK_SKEW_SECS in usage.rs, "better to take nothing than to take it into the wrong
// month"). Hardcoding an absolute second leaves these cases green only within ten minutes of that
// instant — 1_800_000_000 is 2027-01-15, and until then they were permanently red.
// The base is two minutes ago, so that the most-offset round (+120) lands exactly on now.
fn usage_report_base_unix() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64;
    now - 120
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

    // The converse: the same label and the same machine with only the dial method changed to
    // overlay. There n1 dials n2's relay port, the bytes should be read by n2 itself, and n1
    // reporting them is an anomaly, refused as usual.
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

    let rejected = db
        .store
        .record_usage_report(
            "n1",
            UsageReportRequest {
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
    assert_eq!(rejected.rejected_counters, 1);
    assert_eq!(rejected.accepted_readings, 0);
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
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-alt', 'app-main', 'c-main', 'n1', '0.0.0.0', 8443, NULL, 'vless-reality',
            'reality-private', 'reality-public', '[\"8337a0bf\"]'::jsonb,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'chrome', 'xtls-rprx-vision',
            'custom-site'
         )",
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

    db.store
        .report_target_result(applied_report(&desired))
        .await
        .unwrap();

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

/// Where only the fact that a deployment succeeded is needed, the state is written directly — the
/// convergence flow has its own tests, and walking the whole flow only buries the semantics being
/// pinned under a long string of agent reports.
async fn mark_succeeded(pool: &PgPool, deployment_id: i64) {
    sqlx::query("UPDATE deployments SET status = 'succeeded', active = NULL WHERE id = $1")
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
        deployment_id: desired.deployment_id,
        node_id: desired.node_id.clone(),
        result: TargetApplyResult::Applied,
        observed_before: unknown_reported_state(),
        observed_after: reported_state_from_desired(desired),
        error: None,
    }
}

fn failed_recovered_report(desired: &NodeDesiredDeployment) -> TargetConvergenceReport {
    let recovered = unknown_reported_state();
    TargetConvergenceReport {
        deployment_id: desired.deployment_id,
        node_id: desired.node_id.clone(),
        result: TargetApplyResult::FailedRecovered,
        observed_before: recovered.clone(),
        observed_after: recovered,
        error: Some("apply failed and local rollback restored previous state".to_owned()),
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
        "id": "i-guarded",
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
           FROM ingresses WHERE id = 'i-guarded'",
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
        .find(|ingress| ingress.id == "i-guarded")
        .expect("i-guarded 在快照里");
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
        .record_certificate_observation("n1", "present", Some("deadbeef"))
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
            face("i-off", Some(String::new())),
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
    assert_eq!(flow_of("i-off"), None);
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
        "SELECT reality_dest, reality_server_names, reality_fingerprint
         FROM ingresses WHERE app_id = 'app-main' AND id = 'i-main'",
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
        mux: Some(16),
        mode: XhttpMode::PacketUp,
    };
    let shape = WiresRequest {
        vless: Some(TransportRequest::VlessRealityXhttp {
            xhttp: xhttp.clone(),
        }),
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
    assert_eq!(written.ingress["wires"]["vless"]["xhttp"]["mux"], 16);
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
            face("i-vless", 8443, WiresRequest::default()),
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

    let face = |projection: Projection| CreateIngressRequest {
        wires: WiresRequest {
            hysteria2: None,
            vless: Some(TransportRequest::VlessRealityXhttp {
                xhttp: Xhttp {
                    path: "/projection-test".to_owned(),
                    host: Some("upload.route.example".to_owned()),
                    mux: None,
                    mode: XhttpMode::Auto,
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
    let stored = &snapshot.apps[0].ingresses[0].projection;
    assert_eq!(
        stored.v4,
        Some(ProjectionEndpoint {
            host: "cu.acc.example.net".to_owned(),
            port: 20443,
            download: Some(ProjectionDownloadEndpoint {
                host: "down.acc.example.net".to_owned(),
                port: 30443,
                origin_port: Some(40443),
                http_host: Some("download.route.example".to_owned()),
                mux: Some(24),
            }),
        })
    );
    assert_eq!(stored.v6, None);
    assert_eq!(
        snapshot.apps[0].ingresses[0]
            .wires
            .xhttp()
            .unwrap()
            .host
            .as_deref(),
        Some("upload.route.example")
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

    sqlx::query("INSERT INTO apps (id, label) VALUES ('app-main', 'Main App')")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name)
         VALUES ('c-main', 'app-main', 'platform.acme', 'Main Chain')",
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
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-main', 'app-main', 'c-main', 'n1', '0.0.0.0', 443, NULL, 'vless-reality',
            'reality-private', 'reality-public', '[\"8337a0bf\"]'::jsonb,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'chrome', 'xtls-rprx-vision',
            'custom-site'
         )",
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

    sqlx::query("INSERT INTO apps (id, label) VALUES ('app-other', 'Other App')")
        .execute(pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO chains (id, app_id, tenant_id, name)
         VALUES ('c-other', 'app-other', 'platform.other', 'Other Chain')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ingresses (
            id, app_id, chain_id, node_id, bind, port, front_id, transport_kind,
            reality_private_key, reality_public_key, reality_short_ids,
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-other', 'app-other', 'c-other', 'n-other', '0.0.0.0', 443, NULL, 'vless-reality',
            'reality-private-other', 'reality-public-other', '[\"9337a0bf\"]'::jsonb,
            'www.other.example.com:443', '[\"www.other.example.com\"]'::jsonb, 'chrome', 'xtls-rprx-vision',
            'custom-site'
         )",
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
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode
         ) VALUES (
            $1, 'app-main', 'c-main', 'n1', '0.0.0.0', $2, NULL, 'vless-reality',
            'reality-private', 'reality-public', $3,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'chrome', 'xtls-rprx-vision',
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
                id: "c-sub".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Sub".to_owned(),
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
            reality_dest, reality_server_names, reality_fingerprint, reality_flow,
            reality_fallback_mode
         ) VALUES (
            'i-sub', 'app-main', 'c-sub', 'n1', '0.0.0.0', 8444, NULL, 'vless-reality',
            'reality-private', 'reality-public', '[\"8337a0bf\"]'::jsonb,
            'www.example.com:443', '[\"www.example.com\"]'::jsonb, 'chrome', 'xtls-rprx-vision',
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
        db.store
            .put_step(
                &system_admin(),
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
            .await
            .unwrap();
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
    put(&db, "c-sub", "n1", &["n2"]).await;
    put(&db, "c-sub", "n2", &["n3", "n4"]).await;
    put(&db, "c-sub", "n3", &[]).await;
    put(&db, "c-sub", "n4", &[]).await;

    let removed = db
        .store
        .delete_step(&system_admin(), "app-main", "c-sub", "n2")
        .await
        .unwrap();
    assert!(removed.deleted, "删中间跳必须真删掉东西");
    assert!(!removed.chain_removed);
    assert_eq!(removed.removed_steps, vec!["n2", "n3", "n4"]);
    assert_eq!(steps_of(&db, "c-sub").await, vec!["n1"], "子树之外只剩 n1");
    assert!(
        forward_targets(&db, "c-sub", "n1").await.is_empty(),
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
        .delete_step(&system_admin(), "app-main", "c-sub", "n2")
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
    put(&db, "c-sub", "n1", &["n2"]).await;
    put(&db, "c-sub", "n2", &[]).await;
    let applied = db
        .store
        .apply_draft(
            &system_admin(),
            vec![brocade_store::ModelOp::DeleteStep {
                app_id: "app-main".to_owned(),
                chain_id: "c-sub".to_owned(),
                node_id: "n2".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(applied.changed, 1);
    assert_eq!(steps_of(&db, "c-sub").await, vec!["n1"]);
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
         VALUES ('app-main', 'platform.acme', 'alice', 'i-sub')",
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
                chain_id: "c-sub".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(applied.changed, 1);
    let c: i64 = sqlx::query("SELECT count(*) FROM chains WHERE id = 'c-sub'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let i: i64 = sqlx::query("SELECT count(*) FROM ingresses WHERE id = 'i-sub'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let g: i64 = sqlx::query("SELECT count(*) FROM grants WHERE ingress_id = 'i-sub'")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    let s: i64 = sqlx::query("SELECT count(*) FROM steps WHERE chain_id = 'c-sub'")
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

    db.store
        .record_link_probe(
            "hk-01",
            LinkProbeRequest {
                probed_at_unix_secs: 1_785_000_000,
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
        id: "i-quic".to_owned(),
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
            .find(|ingress| ingress.id == "i-quic")
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
