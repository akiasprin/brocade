use brocade_core::model::{ExternalOutboundProtocol, ExternalOutboundSecurity};
use brocade_store::{
    AdminContext, AdminRole, CreateTenantRequest, ModelOp, PgStore, StoreError,
    TunnelProbeCompletion, TunnelProbeJobStatus, TunnelProbePhase, TunnelProbeResultStatus,
    TunnelProbeSource, TunnelProbeTrigger, UpdateTunnelProbePolicy, UpsertExternalOutboundRequest,
};
use sqlx::{PgPool, Row};
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;

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
        let store = PgStore::connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
        ))
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

fn system_admin() -> AdminContext {
    AdminContext::system_admin("probe-test")
}

async fn seed_serving_state(db: &TestPg, revision: u64) {
    let head: Option<i64> = sqlx::query_scalar(
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
    .bind(i64::try_from(revision).unwrap())
    .bind(head.expect("a committed external outbound creates the client checkpoint"))
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn tunnel_probe_policy_queue_claim_cancel_and_history_are_durable() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    db.store
        .create_tenant(
            &system_admin(),
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let committed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-1111-1111".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Vendor edge".to_owned(),
                    address: "127.0.0.1".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    seed_serving_state(&db, committed.revision_id).await;

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-2222-2222".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Future edge".to_owned(),
                    address: "127.0.0.2".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    let future = db
        .store
        .tunnel_probe_view(&system_admin(), "platform.acme", "custom-2222-2222", 86_400)
        .await
        .unwrap();
    assert!(!future.item.supported);
    assert!(future
        .item
        .unsupported_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("尚未进入 Serving")));
    assert!(matches!(
        db.store
            .start_tunnel_probe(&system_admin(), "platform.acme", "custom-2222-2222")
            .await,
        Err(StoreError::NotFound(_))
    ));

    let (queued, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-1111-1111")
        .await
        .unwrap();
    assert!(!reused);
    assert_eq!(queued.status, TunnelProbeJobStatus::Queued);
    let (same, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-1111-1111")
        .await
        .unwrap();
    assert!(reused);
    assert_eq!(same.id, queued.id);
    let outsider = AdminContext::new(
        "other-editor",
        AdminRole::Editor,
        Some("other.example".to_owned()),
    );
    assert!(db
        .store
        .tunnel_probes(&outsider)
        .await
        .unwrap()
        .items
        .iter()
        .all(|item| item.outbound_id != "custom-1111-1111"));
    assert!(matches!(
        db.store.tunnel_probe_run(&outsider, queued.id).await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store
            .update_tunnel_probe_policy(
                &outsider,
                "platform.acme",
                "custom-1111-1111",
                UpdateTunnelProbePolicy {
                    enabled: true,
                    interval_secs: 60,
                    timeout_secs: 5,
                },
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));

    let claim = db
        .store
        .claim_next_tunnel_probe("console-a")
        .await
        .unwrap()
        .expect("queued run is claimable");
    assert_eq!(claim.run.phase, TunnelProbePhase::Preparing);
    assert!(db.store.renew_tunnel_probe_lease(&claim).await.unwrap());
    assert!(db
        .store
        .set_tunnel_probe_config_sha256(&claim, &"a".repeat(64))
        .await
        .unwrap());
    assert!(db
        .store
        .update_tunnel_probe_phase(&claim, TunnelProbePhase::Requesting)
        .await
        .unwrap());
    let frozen = db
        .store
        .claimed_tunnel_probe_outbound(&claim)
        .await
        .unwrap();
    assert_eq!(frozen.id, "custom-1111-1111");
    assert_eq!(frozen.address, "127.0.0.1");
    assert!(db
        .store
        .complete_tunnel_probe(
            &claim,
            TunnelProbeCompletion {
                status: TunnelProbeJobStatus::Succeeded,
                result: TunnelProbeResultStatus::Ok,
                ttfb_ms: Some(42),
                http_status: Some(204),
                exit_ip: Some("203.0.113.9".to_owned()),
                exit_loc: Some("TW".to_owned()),
                attempt_count: 1,
                error_code: None,
                error_detail: None,
            },
        )
        .await
        .unwrap());
    assert!(!db
        .store
        .complete_tunnel_probe(
            &claim,
            TunnelProbeCompletion {
                status: TunnelProbeJobStatus::Failed,
                result: TunnelProbeResultStatus::Interrupted,
                ttfb_ms: None,
                http_status: None,
                exit_ip: None,
                exit_loc: None,
                attempt_count: 1,
                error_code: Some("late-owner".to_owned()),
                error_detail: Some("must be fenced".to_owned()),
            },
        )
        .await
        .unwrap());

    let view = db
        .store
        .tunnel_probe_view(&system_admin(), "platform.acme", "custom-1111-1111", 86_400)
        .await
        .unwrap();
    assert_eq!(view.summary.total, 1);
    assert_eq!(view.summary.succeeded, 1);
    assert_eq!(view.summary.p50_ms, Some(42));
    assert_eq!(view.points.len(), 1);
    assert_eq!(view.recent_runs[0].http_status, Some(204));

    let policy = db
        .store
        .update_tunnel_probe_policy(
            &system_admin(),
            "platform.acme",
            "custom-1111-1111",
            UpdateTunnelProbePolicy {
                enabled: true,
                interval_secs: 60,
                timeout_secs: 5,
            },
        )
        .await
        .unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.interval_secs, 60);
    let list = db.store.tunnel_probes(&system_admin()).await.unwrap();
    assert_eq!(list.origin, "console");
    let listed = list
        .items
        .iter()
        .find(|item| item.outbound_id == "custom-1111-1111")
        .expect("new tunnel is listed");
    assert_eq!(listed.policy.as_ref().unwrap().timeout_secs, 5);

    let (cancel_me, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-1111-1111")
        .await
        .unwrap();
    assert!(!reused);
    let canceled = db
        .store
        .cancel_tunnel_probe_run(&system_admin(), cancel_me.id)
        .await
        .unwrap();
    assert_eq!(canceled.status, TunnelProbeJobStatus::Canceled);

    let (expire_me, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-1111-1111")
        .await
        .unwrap();
    assert!(!reused);
    let expired_claim = db
        .store
        .claim_next_tunnel_probe("console-expired")
        .await
        .unwrap()
        .expect("manual run is claimable");
    assert_eq!(expired_claim.run.id, expire_me.id);
    sqlx::query(
        "UPDATE external_outbound_probe_runs SET lease_until = now() - interval '1 second'
          WHERE id = $1",
    )
    .bind(expire_me.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.store.reap_expired_tunnel_probes().await.unwrap(), 1);
    let interrupted = db
        .store
        .tunnel_probe_run(&system_admin(), expire_me.id)
        .await
        .unwrap();
    assert_eq!(interrupted.status, TunnelProbeJobStatus::Failed);
    assert_eq!(
        interrupted.result,
        Some(TunnelProbeResultStatus::Interrupted)
    );

    // A mutable-head edit that has not been published must not leak into a
    // scheduled run whose topology revision says it measured Serving.
    sqlx::query(
        "UPDATE external_outbounds
            SET name = 'Unpublished rename', address = '192.0.2.99'
          WHERE id = 'custom-1111-1111'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let serving_item = db
        .store
        .tunnel_probes(&system_admin())
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.outbound_id == "custom-1111-1111")
        .unwrap();
    assert_eq!(serving_item.protocol, "socks5");
    assert!(serving_item.supported);
    db.store
        .update_tunnel_probe_policy(
            &system_admin(),
            "platform.acme",
            "custom-1111-1111",
            UpdateTunnelProbePolicy {
                enabled: true,
                interval_secs: 60,
                timeout_secs: 5,
            },
        )
        .await
        .expect("Serving SOCKS5 remains schedulable while mutable HEAD differs");
    sqlx::query(
        "UPDATE external_outbound_probe_policies SET next_run_at = now() - interval '1 second'
          WHERE outbound_id = 'custom-1111-1111'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.store.enqueue_due_tunnel_probes().await.unwrap(), 1);
    let scheduled = db
        .store
        .claim_next_tunnel_probe("console-b")
        .await
        .unwrap()
        .expect("scheduled run is claimable");
    assert_eq!(scheduled.run.trigger, TunnelProbeTrigger::Scheduled);
    assert_eq!(scheduled.run.outbound_name, "Vendor edge");
    assert_eq!(scheduled.run.protocol, "socks5");
    let frozen = db
        .store
        .claimed_tunnel_probe_outbound(&scheduled)
        .await
        .unwrap();
    assert_eq!(frozen.address, "127.0.0.1");
    let requested = db
        .store
        .cancel_tunnel_probe_run(&system_admin(), scheduled.run.id)
        .await
        .unwrap();
    assert!(requested.cancel_requested);
    assert!(db
        .store
        .tunnel_probe_cancel_requested(&scheduled)
        .await
        .unwrap());
    assert!(db
        .store
        .complete_tunnel_probe(
            &scheduled,
            TunnelProbeCompletion {
                status: TunnelProbeJobStatus::Canceled,
                result: TunnelProbeResultStatus::Canceled,
                ttfb_ms: None,
                http_status: None,
                exit_ip: None,
                exit_loc: None,
                attempt_count: 0,
                error_code: Some("canceled".to_owned()),
                error_detail: Some("操作者取消了拨测".to_owned()),
            },
        )
        .await
        .unwrap());

    let persisted: (String, Option<String>, i64) = sqlx::query(
        "SELECT status, config_sha256, serving_generation
           FROM external_outbound_probe_runs WHERE id = $1",
    )
    .bind(queued.id)
    .fetch_one(db.pool())
    .await
    .map(|row| {
        (
            row.get("status"),
            row.get("config_sha256"),
            row.get("serving_generation"),
        )
    })
    .unwrap();
    assert_eq!(persisted.0, "succeeded");
    assert_eq!(persisted.1, Some("a".repeat(64)));
    assert_eq!(
        persisted.2,
        i64::try_from(
            queued
                .serving_generation
                .expect("Serving run has a generation")
        )
        .unwrap()
    );
    sqlx::query(
        "UPDATE external_outbound_probe_runs SET finished_at = now() - interval '8 days'
          WHERE id = $1",
    )
    .bind(queued.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.store.prune_tunnel_probes(7).await.unwrap(), 1);
    assert!(matches!(
        db.store.tunnel_probe_run(&system_admin(), queued.id).await,
        Err(StoreError::NotFound(_))
    ));

    // Historical rows are intentionally retained when a tunnel is deleted. Reusing the globally
    // unique id under another tenant must neither reveal those rows nor let the new policy execute
    // the old tenant's still-Serving topology.
    sqlx::query("UPDATE external_outbounds SET protocol = 'socks5' WHERE id = 'custom-1111-1111'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteExternalOutbound {
                tenant_id: "platform.acme".to_owned(),
                id: "custom-1111-1111".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    db.store
        .create_tenant(
            &system_admin(),
            CreateTenantRequest {
                id: "other.example".to_owned(),
                name: "Other".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-1111-1111".to_owned(),
                    tenant_id: "other.example".to_owned(),
                    name: "Other vendor".to_owned(),
                    address: "127.0.0.3".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
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
        .update_tunnel_probe_policy(
            &system_admin(),
            "other.example",
            "custom-1111-1111",
            UpdateTunnelProbePolicy {
                enabled: true,
                interval_secs: 60,
                timeout_secs: 5,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE external_outbound_probe_policies SET next_run_at = now() - interval '1 second'
          WHERE outbound_id = 'custom-1111-1111'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.store.enqueue_due_tunnel_probes().await.unwrap(), 0);
    assert!(db
        .store
        .tunnel_probes(&system_admin())
        .await
        .unwrap()
        .items
        .iter()
        .all(|item| item.outbound_id != "custom-1111-1111"));
    let recreated = db
        .store
        .tunnel_probe_view(&system_admin(), "other.example", "custom-1111-1111", 86_400)
        .await
        .unwrap()
        .item;
    assert_eq!(recreated.tenant_id, "other.example");
    assert!(recreated.latest_run.is_none());
    assert_eq!(recreated.health, brocade_store::TunnelProbeHealth::Unknown);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn manual_draft_probe_freezes_an_encrypted_source_without_affecting_serving_health() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    db.store
        .create_tenant(
            &system_admin(),
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    let committed = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-1111-1111".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Serving edge".to_owned(),
                    address: "127.0.0.1".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: Some("serving-user".to_owned()),
                        credential: "serving-secret".to_owned(),
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    seed_serving_state(&db, committed.revision_id).await;

    let draft_ops = vec![ModelOp::UpsertExternalOutbound {
        outbound: UpsertExternalOutboundRequest {
            id: "custom-1111-1111".to_owned(),
            tenant_id: "platform.acme".to_owned(),
            name: "Draft edge".to_owned(),
            address: "127.0.0.9".to_owned(),
            port: 2080,
            protocol: ExternalOutboundProtocol::Socks5 {
                username: Some("draft-user".to_owned()),
                credential: "draft-secret".to_owned(),
            },
            security: ExternalOutboundSecurity::None,
            note: None,
        },
    }];
    let (draft_run, reused) = db
        .store
        .start_tunnel_probe_from(
            &system_admin(),
            "platform.acme",
            "custom-1111-1111",
            TunnelProbeSource::Draft,
            draft_ops.clone(),
        )
        .await
        .unwrap();
    assert!(!reused);
    assert_eq!(draft_run.source, TunnelProbeSource::Draft);
    assert_eq!(draft_run.serving_generation, None);
    assert_eq!(draft_run.draft_sha256.as_deref().map(str::len), Some(64));
    let mut changed_draft_ops = draft_ops.clone();
    if let ModelOp::UpsertExternalOutbound { outbound } = &mut changed_draft_ops[0] {
        outbound.address = "127.0.0.10".to_owned();
    }
    let (same_draft, reused) = db
        .store
        .start_tunnel_probe_from(
            &system_admin(),
            "platform.acme",
            "custom-1111-1111",
            TunnelProbeSource::Draft,
            draft_ops,
        )
        .await
        .unwrap();
    assert!(reused);
    assert_eq!(same_draft.id, draft_run.id);
    assert!(matches!(
        db.store
            .start_tunnel_probe_from(
                &system_admin(),
                "platform.acme",
                "custom-1111-1111",
                TunnelProbeSource::Draft,
                changed_draft_ops,
            )
            .await,
        Err(StoreError::Conflict(_))
    ));

    let sealed: String = sqlx::query_scalar(
        "SELECT outbound_sealed FROM external_outbound_probe_runs WHERE id = $1",
    )
    .bind(draft_run.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(sealed.starts_with("v1."));
    assert!(!sealed.contains("draft-secret"));
    assert!(!sealed.contains("127.0.0.9"));

    // Serving and draft are distinct active identities. One cannot accidentally reuse the other.
    let (serving_run, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-1111-1111")
        .await
        .unwrap();
    assert!(!reused);
    assert_ne!(serving_run.id, draft_run.id);
    assert_eq!(serving_run.source, TunnelProbeSource::Serving);
    assert_eq!(serving_run.serving_generation, Some(1));

    let draft_claim = db
        .store
        .claim_next_tunnel_probe("console-draft")
        .await
        .unwrap()
        .expect("draft run is claimable");
    assert_eq!(draft_claim.run.id, draft_run.id);
    let frozen_draft = db
        .store
        .claimed_tunnel_probe_outbound(&draft_claim)
        .await
        .unwrap();
    assert_eq!(frozen_draft.name, "Draft edge");
    assert_eq!(frozen_draft.address, "127.0.0.9");
    assert_eq!(frozen_draft.port, 2080);
    assert_eq!(frozen_draft.protocol.credential(), "draft-secret");
    assert!(db
        .store
        .complete_tunnel_probe(
            &draft_claim,
            TunnelProbeCompletion {
                status: TunnelProbeJobStatus::Failed,
                result: TunnelProbeResultStatus::ConnectFailed,
                ttfb_ms: None,
                http_status: None,
                exit_ip: None,
                exit_loc: None,
                attempt_count: 1,
                error_code: Some("draft-connect".to_owned()),
                error_detail: Some("draft failed".to_owned()),
            },
        )
        .await
        .unwrap());
    let cleared: Option<String> = sqlx::query_scalar(
        "SELECT outbound_sealed FROM external_outbound_probe_runs WHERE id = $1",
    )
    .bind(draft_run.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        cleared.is_none(),
        "terminal draft runs must erase their sealed input"
    );

    let serving_claim = db
        .store
        .claim_next_tunnel_probe("console-serving")
        .await
        .unwrap()
        .expect("Serving run is claimable");
    assert_eq!(serving_claim.run.id, serving_run.id);
    let frozen_serving = db
        .store
        .claimed_tunnel_probe_outbound(&serving_claim)
        .await
        .unwrap();
    assert_eq!(frozen_serving.name, "Serving edge");
    assert_eq!(frozen_serving.address, "127.0.0.1");
    assert_eq!(frozen_serving.protocol.credential(), "serving-secret");
    assert!(db
        .store
        .complete_tunnel_probe(
            &serving_claim,
            TunnelProbeCompletion {
                status: TunnelProbeJobStatus::Succeeded,
                result: TunnelProbeResultStatus::Ok,
                ttfb_ms: Some(31),
                http_status: Some(204),
                exit_ip: Some("203.0.113.10".to_owned()),
                exit_loc: Some("TW".to_owned()),
                attempt_count: 1,
                error_code: None,
                error_detail: None,
            },
        )
        .await
        .unwrap());

    let view = db
        .store
        .tunnel_probe_view(&system_admin(), "platform.acme", "custom-1111-1111", 86_400)
        .await
        .unwrap();
    assert_eq!(
        view.summary.total, 1,
        "draft failures do not enter Serving metrics"
    );
    assert_eq!(view.summary.succeeded, 1);
    assert_eq!(view.points.len(), 1);
    assert!(view
        .recent_runs
        .iter()
        .any(|run| run.source == TunnelProbeSource::Draft));
    assert!(view
        .recent_runs
        .iter()
        .any(|run| run.source == TunnelProbeSource::Serving));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn manual_probe_does_not_reuse_an_active_run_after_the_id_changes_tenant() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    for (id, name) in [("platform.acme", "Platform"), ("other.example", "Other")] {
        db.store
            .create_tenant(
                &system_admin(),
                CreateTenantRequest {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    note: None,
                },
            )
            .await
            .unwrap();
    }

    let original = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-3333-3333".to_owned(),
                    tenant_id: "platform.acme".to_owned(),
                    name: "Platform edge".to_owned(),
                    address: "127.0.0.1".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    seed_serving_state(&db, original.revision_id).await;
    let (old_run, reused) = db
        .store
        .start_tunnel_probe(&system_admin(), "platform.acme", "custom-3333-3333")
        .await
        .unwrap();
    assert!(!reused);

    db.store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::DeleteExternalOutbound {
                tenant_id: "platform.acme".to_owned(),
                id: "custom-3333-3333".to_owned(),
            }],
            None,
        )
        .await
        .unwrap();
    let recreated = db
        .store
        .apply_draft(
            &system_admin(),
            vec![ModelOp::UpsertExternalOutbound {
                outbound: UpsertExternalOutboundRequest {
                    id: "custom-3333-3333".to_owned(),
                    tenant_id: "other.example".to_owned(),
                    name: "Other edge".to_owned(),
                    address: "127.0.0.2".to_owned(),
                    port: 1080,
                    protocol: ExternalOutboundProtocol::Socks5 {
                        username: None,
                        credential: String::new(),
                    },
                    security: ExternalOutboundSecurity::None,
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE subscription_serving_state
            SET topology_revision_id = $1, generation = generation + 1
          WHERE id = TRUE",
    )
    .bind(i64::try_from(recreated.revision_id).unwrap())
    .execute(db.pool())
    .await
    .unwrap();

    let other_editor = AdminContext::new(
        "other-editor",
        AdminRole::Editor,
        Some("other.example".to_owned()),
    );
    let (new_run, reused) = db
        .store
        .start_tunnel_probe(&other_editor, "other.example", "custom-3333-3333")
        .await
        .unwrap();
    assert!(!reused, "another tenant's active run must not be reused");
    assert_ne!(new_run.id, old_run.id);
    assert_eq!(new_run.tenant_id, "other.example");
    assert_eq!(old_run.tenant_id, "platform.acme");
    let view = db
        .store
        .tunnel_probe_view(&other_editor, "other.example", "custom-3333-3333", 86_400)
        .await
        .unwrap();
    assert_eq!(
        view.item.latest_run.as_ref().map(|run| run.id),
        Some(new_run.id)
    );
    assert!(view
        .recent_runs
        .iter()
        .all(|run| run.tenant_id == "other.example"));
}
