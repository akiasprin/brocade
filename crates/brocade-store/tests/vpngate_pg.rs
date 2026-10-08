use brocade_core::model::{
    Action, AppView, DestMatch, ExternalOutboundProtocol, ExternalOutboundSecurity, Rule, Step,
};
use brocade_deployment::protocol::{
    VpngateAdmissionPolicy, VpngateCandidate, VpngateCountryPolicy, VpngateIpIntelligenceFailure,
    VpngateIpIntelligenceObservation, VpngateIpIntelligenceReport, VpngateIpNetwork,
    VpngateIpProvider, VpngateIpScore, VpngateNetworkType, VpngateProbeMode,
    VpngateRiskDecisionPolicy, VpngateTransport, AGENT_PROTOCOL_VERSION,
};
use brocade_store::{
    AdminContext, CreateAppRequest, CreateChainRequest, CreateTenantRequest, ModelOp, PgStore,
    PutStepRequest, RequestVpngatePoolSwitch, UpdateVpngateIntelligenceCredentials,
    UpdateVpngateIntelligenceNode, UpdateVpngateProbeNode, UpdateVpngateProbeSettings,
    UpsertExternalOutboundRequest, VpngateDirectoryFilter, VpngateIntelligenceCredentialUpdateMode,
    VpngateIntelligencePolicy, VpngateIntelligenceRefreshMode, VpngateManualSwitchResult,
    VpngateManualSwitchStatus, VpngatePoolReport, VpngateProbeNodeOrigin, VpngateProbeReport,
    VpngateProbeSample, VpngateProbeStatus, VpngateReconcileReport, VpngateRuntimeSelection,
    VpngateServerInput, VpngateServerPageRequest, VpngateSyncBatch,
};
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

    fn pool(&self) -> &sqlx::PgPool {
        self.store.pool()
    }
}

fn server(id: &str, country: &str, ip: &str, score: u64) -> VpngateServerInput {
    let profile = format!(
        "client\ndev tun\nproto udp\nremote {ip} 1194\nscript-security 1\n\
         <ca>\nCA\n</ca>\n<cert>\nCERT\n</cert>\n<key>\nKEY\n</key>\n"
    );
    VpngateServerInput {
        id: id.to_owned(),
        hostname: id.to_owned(),
        ip: ip.to_owned(),
        country_code: country.to_owned(),
        country_name: match country {
            "JP" => "Japan",
            "KR" => "Korea Republic of",
            _ => "Unknown",
        }
        .to_owned(),
        score,
        ping_ms: Some(25),
        speed_bps: 30_000_000,
        vpn_sessions: 2,
        uptime_millis: 600_000,
        total_users: 100,
        total_traffic_bytes: 1_000_000,
        log_type: "2weeks".to_owned(),
        operator_name: "volunteer".to_owned(),
        message: String::new(),
        profile_sha256: brocade_core::hash::sha256_hex(profile.as_bytes()),
        remote_address: ip.to_owned(),
        remote_port: 1194,
        transport: VpngateTransport::Udp,
        openvpn_config: profile,
    }
}

fn scores(risk: u8) -> Vec<VpngateIpScore> {
    [
        VpngateIpProvider::Proxycheck,
        VpngateIpProvider::Ffraud,
        VpngateIpProvider::Iplogs,
    ]
    .into_iter()
    .map(|provider| VpngateIpScore {
        provider,
        score: risk,
        country_code: "JP".to_owned(),
    })
    .collect()
}

fn networks() -> Vec<VpngateIpNetwork> {
    [
        VpngateIpProvider::Proxycheck,
        VpngateIpProvider::Ffraud,
        VpngateIpProvider::Iplogs,
    ]
    .into_iter()
    .map(|provider| VpngateIpNetwork {
        provider,
        isp: Some("Example ISP".to_owned()),
        network_type: VpngateNetworkType::Business,
    })
    .collect()
}

fn vpngate_outbound(id: &str, country_code: &str) -> UpsertExternalOutboundRequest {
    UpsertExternalOutboundRequest {
        id: id.to_owned(),
        tenant_id: "platform.acme".to_owned(),
        name: format!("VPN Gate {country_code}"),
        address: "managed.vpngate.invalid".to_owned(),
        port: 1,
        protocol: ExternalOutboundProtocol::Vpngate {
            country_code: country_code.to_owned(),
            server_id: None,
            server_ids: Vec::new(),
            max_connect_ms: 15_000,
            min_download_bps: 1_000_000,
            max_candidates: 10,
        },
        security: ExternalOutboundSecurity::None,
        note: None,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn intelligence_due_index_serves_new_and_expired_leases() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("intelligence-index-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.intelligence".to_owned(),
                name: "Intelligence".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
        (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
         wg_listen_port, egress_allowed, dns_kind)
        VALUES ('intel-index', 'platform.intelligence', 'Intel', '10.88.0.22',
                'private-index', 'public-index', 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state (node_id, agent_protocol_version, runtime_reported_at, runtime_versions)
                 VALUES ('intel-index', $1, now(), '{}'::jsonb)",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .update_vpngate_intelligence_node(
            &admin,
            "intel-index",
            UpdateVpngateIntelligenceNode { enabled: true },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO vpngate_exit_reputations (exit_ip, next_check_at)
        SELECT '198.18.0.0'::inet + n, now() + interval '1 day'
          FROM generate_series(1, 5000) n",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE vpngate_exit_reputations SET next_check_at=now()-interval '1 minute'
                 WHERE exit_ip='198.18.0.1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("ANALYZE vpngate_exit_reputations")
        .execute(db.pool())
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar(
        "EXPLAIN (FORMAT JSON)
        SELECT host(exit_ip), lease_generation FROM vpngate_exit_reputations
        WHERE next_check_at <= now() AND (lease_until IS NULL OR lease_until <= now())
          AND last_seen_at >= now()-interval '48 hours'
        ORDER BY next_check_at, exit_ip FOR UPDATE SKIP LOCKED LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        plan.to_string().contains("vpngate_exit_reputations_due"),
        "{plan}"
    );
    let first = db
        .store
        .claim_vpngate_exit_intelligence("intel-index")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.exit_ip, "198.18.0.1");
    assert!(db
        .store
        .claim_vpngate_exit_intelligence("intel-index")
        .await
        .unwrap()
        .is_none());
    sqlx::query(
        "UPDATE vpngate_exit_reputations SET lease_until=now()-interval '1 second'
                 WHERE exit_ip='198.18.0.1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let reclaimed = db
        .store
        .claim_vpngate_exit_intelligence("intel-index")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.exit_ip, first.exit_ip);
    assert!(reclaimed.lease_generation > first.lease_generation);
    assert!(db
        .store
        .record_vpngate_ip_intelligence_report(
            "intel-index",
            &intelligence_report(&first.exit_ip, first.lease_generation, 9)
        )
        .await
        .is_err());
    db.store
        .record_vpngate_ip_intelligence_report(
            "intel-index",
            &intelligence_report(&reclaimed.exit_ip, reclaimed.lease_generation, 9),
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn selected_probe_node_becomes_stale_only_after_the_startup_grace_period() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-probe-health-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.health".to_owned(),
                name: "Health".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('probe-health', 'platform.health', 'Probe Health', '10.88.0.60',
                 'private-health', 'public-health', 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES ('probe-health', $1, '{\"openvpn\":\"OpenVPN 2.6.12\"}'::jsonb, now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .update_vpngate_probe_node(
            &admin,
            "probe-health",
            UpdateVpngateProbeNode {
                enabled: true,
                workers: Some(16),
            },
        )
        .await
        .unwrap();

    let fresh = db
        .store
        .list_node_agent_states(&admin)
        .await
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.node_id == "probe-health")
        .unwrap();
    assert!(fresh.vpngate_probe_enabled);
    assert!(!fresh.vpngate_probe_data_stale);
    assert!(fresh.vpngate_probe_reported_at.is_none());

    sqlx::query(
        "UPDATE vpngate_probe_nodes
            SET selected_at = now() - interval '31 minutes'
          WHERE node_id = 'probe-health'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let stale = db
        .store
        .list_node_agent_states(&admin)
        .await
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.node_id == "probe-health")
        .unwrap();
    assert!(stale.vpngate_probe_data_stale);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn draft_prunes_vpngate_pools_after_their_last_reference_is_removed() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-prune-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('edge', 'platform.acme', 'Edge', '10.88.0.10', 'private', 'public',
                 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .upsert_app(
            &admin,
            CreateAppRequest {
                id: "app-1111".to_owned(),
                label: "VPN Gate".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    db.store
        .upsert_chain(
            &admin,
            "app-1111",
            CreateChainRequest {
                id: "chn-1111-1111".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "Provider".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();

    db.store
        .apply_draft(
            &admin,
            vec![
                ModelOp::UpsertExternalOutbound {
                    outbound: vpngate_outbound("vpngate-1111-1111", "JP"),
                },
                ModelOp::UpsertExternalOutbound {
                    outbound: vpngate_outbound("vpngate-2222-2222", "KR"),
                },
                ModelOp::UpsertExternalOutbound {
                    outbound: UpsertExternalOutboundRequest {
                        id: "custom-1111-1111".to_owned(),
                        tenant_id: "platform.acme".to_owned(),
                        name: "Reusable SOCKS".to_owned(),
                        address: "127.0.0.1".to_owned(),
                        port: 1080,
                        protocol: ExternalOutboundProtocol::Socks5 {
                            username: None,
                            credential: String::new(),
                        },
                        security: ExternalOutboundSecurity::None,
                        note: None,
                    },
                },
                ModelOp::PutStep {
                    app_id: "app-1111".to_owned(),
                    chain_id: "chn-1111-1111".to_owned(),
                    node_id: "edge".to_owned(),
                    step: PutStepRequest {
                        accept: None,
                        hop_in: None,
                        rules: vec![Rule {
                            dest_match: DestMatch::Any,
                            action: Action::Proxy {
                                outbound: "vpngate-1111-1111".to_owned(),
                            },
                        }],
                        note: None,
                    },
                },
            ],
            None,
        )
        .await
        .unwrap();
    let initial = db.store.materialize_snapshot(None).await.unwrap();
    let initial_ids = initial
        .external_outbounds
        .iter()
        .map(|outbound| outbound.id.as_str())
        .collect::<Vec<_>>();
    assert!(initial_ids.contains(&"vpngate-1111-1111"));
    assert!(!initial_ids.contains(&"vpngate-2222-2222"));
    assert!(initial_ids.contains(&"custom-1111-1111"));
    assert!(initial_ids.iter().any(|id| id.starts_with("warp-")));

    db.store
        .apply_draft(
            &admin,
            vec![ModelOp::PutStep {
                app_id: "app-1111".to_owned(),
                chain_id: "chn-1111-1111".to_owned(),
                node_id: "edge".to_owned(),
                step: PutStepRequest {
                    accept: None,
                    hop_in: None,
                    rules: vec![Rule {
                        dest_match: DestMatch::Any,
                        action: Action::Egress { send_through: None },
                    }],
                    note: None,
                },
            }],
            None,
        )
        .await
        .unwrap();
    let final_snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(final_snapshot
        .external_outbounds
        .iter()
        .all(|outbound| outbound.id != "vpngate-1111-1111"));
    assert!(final_snapshot
        .external_outbounds
        .iter()
        .any(|outbound| outbound.id == "custom-1111-1111"));
}

fn intelligence_report(
    exit_ip: &str,
    lease_generation: u64,
    risk: u8,
) -> VpngateIpIntelligenceReport {
    VpngateIpIntelligenceReport {
        exit_ip: exit_ip.to_owned(),
        lease_generation,
        observations: [
            VpngateIpProvider::Proxycheck,
            VpngateIpProvider::Ffraud,
            VpngateIpProvider::Iplogs,
        ]
        .into_iter()
        .map(|provider| VpngateIpIntelligenceObservation {
            provider,
            score: risk,
            country_code: "JP".to_owned(),
            isp: Some("Example ISP".to_owned()),
            network_type: VpngateNetworkType::Business,
        })
        .collect(),
        failures: Vec::new(),
    }
}

fn partial_intelligence_report(
    exit_ip: &str,
    lease_generation: u64,
) -> VpngateIpIntelligenceReport {
    VpngateIpIntelligenceReport {
        exit_ip: exit_ip.to_owned(),
        lease_generation,
        observations: vec![VpngateIpIntelligenceObservation {
            provider: VpngateIpProvider::Proxycheck,
            score: 8,
            country_code: "JP".to_owned(),
            isp: Some("Example ISP".to_owned()),
            network_type: VpngateNetworkType::Business,
        }],
        failures: [VpngateIpProvider::Ffraud, VpngateIpProvider::Iplogs]
            .into_iter()
            .map(|provider| VpngateIpIntelligenceFailure {
                provider,
                code: "provider-rejected".to_owned(),
            })
            .collect(),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn completed_catalogue_leases_reject_replayed_results_without_null_decode_errors() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();

    let global_claim = db
        .store
        .claim_vpngate_sync("legacy-replay-worker", "startup")
        .await
        .unwrap()
        .expect("the legacy collector receives the initial global lease");
    db.store
        .complete_vpngate_sync(
            "legacy-replay-worker",
            &global_claim,
            VpngateSyncBatch {
                content_sha256: "a".repeat(64),
                fetched_rows: 1,
                rejected_rows: 0,
                servers: vec![server("legacy-replay.example", "JP", "198.18.0.40", 10)],
            },
        )
        .await
        .unwrap();
    let replayed_global = db
        .store
        .fail_vpngate_sync(
            "legacy-replay-worker",
            &global_claim,
            "replayed-result",
            "the success response was not observed",
        )
        .await;
    assert!(matches!(
        replayed_global,
        Err(brocade_store::StoreError::Conflict(_))
    ));

    let admin = AdminContext::system_admin("vpngate-replayed-result-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.replay".to_owned(),
                name: "Replay".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('replay-collector', 'platform.replay', 'Replay Collector', '10.88.0.40',
                 'private-replay', 'public-replay', 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES ('replay-collector', $1, '{}'::jsonb, now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .update_vpngate_intelligence_node(
            &admin,
            "replay-collector",
            UpdateVpngateIntelligenceNode { enabled: true },
        )
        .await
        .unwrap();

    let collector_claim = db
        .store
        .claim_vpngate_catalog_sync("replay-collector")
        .await
        .unwrap()
        .expect("the selected Agent receives its independent origin lease");
    db.store
        .complete_vpngate_sync(
            "replay-collector",
            &collector_claim,
            VpngateSyncBatch {
                content_sha256: "b".repeat(64),
                fetched_rows: 1,
                rejected_rows: 0,
                servers: vec![server("collector-replay.example", "JP", "198.18.0.41", 11)],
            },
        )
        .await
        .unwrap();
    let replayed_collector = db
        .store
        .fail_vpngate_sync(
            "replay-collector",
            &collector_claim,
            "replayed-result",
            "the success response was not observed",
        )
        .await;
    assert!(matches!(
        replayed_collector,
        Err(brocade_store::StoreError::Conflict(_))
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn selected_agents_merge_origin_catalogues_but_compete_for_one_ip_intelligence_lease() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-intelligence-test");
    let mut policy = VpngateAdmissionPolicy {
        minimum_successful_sources: 2,
        country_policy: VpngateCountryPolicy::Ignore,
        risk_decision_policy: VpngateRiskDecisionPolicy::AnyAvailablePass,
        ..VpngateAdmissionPolicy::default()
    };
    policy.provider_rules[0].maximum_score = 31;
    assert_eq!(
        db.store
            .update_vpngate_admission_policy(&admin, policy.clone())
            .await
            .unwrap(),
        policy
    );
    assert_eq!(
        db.store.vpngate_overview().await.unwrap().admission_policy,
        policy
    );
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.intelligence".to_owned(),
                name: "Intelligence".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES
            ('intel-a', 'platform.intelligence', 'Intel A', '10.88.0.20', 'private-a', 'public-a',
             51820, TRUE, 'system'),
            ('intel-b', 'platform.intelligence', 'Intel B', '10.88.0.21', 'private-b', 'public-b',
             51821, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES
            ('intel-a', $1, '{}'::jsonb, now()),
            ('intel-b', $1, '{}'::jsonb, now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    for node_id in ["intel-a", "intel-b"] {
        db.store
            .update_vpngate_intelligence_node(
                &admin,
                node_id,
                UpdateVpngateIntelligenceNode { enabled: true },
            )
            .await
            .unwrap();
    }

    let catalogue_a = db
        .store
        .claim_vpngate_catalog_sync("intel-a")
        .await
        .unwrap()
        .expect("the first selected Agent receives its due catalogue collection");
    let catalogue_b = db
        .store
        .claim_vpngate_catalog_sync("intel-b")
        .await
        .unwrap()
        .expect("the second selected Agent keeps its independent origin lease");
    assert_ne!(catalogue_a.run_id, catalogue_b.run_id);
    db.store
        .complete_vpngate_sync(
            "intel-a",
            &catalogue_a,
            VpngateSyncBatch {
                content_sha256: "a".repeat(64),
                fetched_rows: 2,
                rejected_rows: 0,
                servers: vec![
                    server("only-a.example", "JP", "198.18.0.10", 10),
                    server("shared.example", "JP", "198.18.0.11", 11),
                ],
            },
        )
        .await
        .unwrap();
    db.store
        .complete_vpngate_sync(
            "intel-b",
            &catalogue_b,
            VpngateSyncBatch {
                content_sha256: "b".repeat(64),
                fetched_rows: 2,
                rejected_rows: 0,
                servers: vec![
                    server("only-b.example", "KR", "198.18.0.12", 12),
                    server("shared.example", "JP", "198.18.0.11", 13),
                ],
            },
        )
        .await
        .unwrap();
    let current_ids =
        sqlx::query_scalar::<_, String>("SELECT id FROM vpngate_servers WHERE current ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        current_ids,
        ["only-a.example", "only-b.example", "shared.example"]
    );
    let only_b_version_before = sqlx::query_scalar::<_, String>(
        "SELECT xmin::text FROM vpngate_servers WHERE id = 'only-b.example'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE vpngate_intelligence_nodes
            SET catalogue_next_sync_at = now()
          WHERE node_id = 'intel-a'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let catalogue_a_next = db
        .store
        .claim_vpngate_catalog_sync("intel-a")
        .await
        .unwrap()
        .expect("one collector can replace only its own previous origin snapshot");
    db.store
        .complete_vpngate_sync(
            "intel-a",
            &catalogue_a_next,
            VpngateSyncBatch {
                content_sha256: "c".repeat(64),
                fetched_rows: 2,
                rejected_rows: 0,
                servers: vec![
                    server("new-a.example", "JP", "198.18.0.13", 14),
                    server("shared.example", "JP", "198.18.0.11", 15),
                ],
            },
        )
        .await
        .unwrap();
    let current_ids =
        sqlx::query_scalar::<_, String>("SELECT id FROM vpngate_servers WHERE current ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        current_ids,
        ["new-a.example", "only-b.example", "shared.example"]
    );
    let only_b_version_after = sqlx::query_scalar::<_, String>(
        "SELECT xmin::text FROM vpngate_servers WHERE id = 'only-b.example'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(only_b_version_after, only_b_version_before);
    let overview = db.store.vpngate_overview().await.unwrap();
    assert_eq!(
        overview
            .sync_history
            .iter()
            .map(|point| point.current_servers)
            .collect::<Vec<_>>(),
        vec![2, 3, 3]
    );
    assert_eq!(
        overview
            .sync_history
            .iter()
            .map(|point| point.first_seen_servers)
            .collect::<Vec<_>>(),
        vec![2, 1, 1]
    );

    sqlx::query(
        "UPDATE vpngate_intelligence_nodes
            SET catalogue_next_sync_at = now()
          WHERE node_id = 'intel-a'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let failed_a = db
        .store
        .claim_vpngate_catalog_sync("intel-a")
        .await
        .unwrap()
        .expect("the collector receives its next independent lease");
    db.store
        .fail_vpngate_sync("intel-a", &failed_a, "test-failure", "upstream unavailable")
        .await
        .unwrap();
    let after_failure =
        sqlx::query_scalar::<_, String>("SELECT id FROM vpngate_servers WHERE current ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(after_failure, current_ids);
    let collector_error = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT catalogue_last_error_code, catalogue_last_error_detail
           FROM vpngate_intelligence_nodes
          WHERE node_id = 'intel-a'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(collector_error.0.as_deref(), Some("test-failure"));
    assert_eq!(collector_error.1.as_deref(), Some("upstream unavailable"));
    let global_error = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT last_error_code, last_error_detail
           FROM vpngate_catalog_state
          WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(global_error, (None, None));
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET last_error_code = 'test-failure', last_error_detail = 'upstream unavailable'
          WHERE id = TRUE",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let overview_after_failure = db.store.vpngate_overview().await.unwrap();
    assert_eq!(overview_after_failure.status.last_error_code, None);
    assert_eq!(overview_after_failure.status.last_error_detail, None);

    let recorded_workers = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT worker_id FROM vpngate_sync_runs ORDER BY worker_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(recorded_workers, ["intel-a", "intel-b"]);
    assert_eq!(
        db.store
            .vpngate_overview()
            .await
            .unwrap()
            .status
            .retained_observations,
        6
    );

    sqlx::query("INSERT INTO vpngate_exit_reputations (exit_ip) VALUES ('198.51.100.40'::inet)")
        .execute(db.pool())
        .await
        .unwrap();

    let claim = db
        .store
        .claim_vpngate_exit_intelligence("intel-a")
        .await
        .unwrap()
        .expect("the first selected Agent receives the due IP");
    assert_eq!(claim.exit_ip, "198.51.100.40");
    assert!(db
        .store
        .claim_vpngate_exit_intelligence("intel-b")
        .await
        .unwrap()
        .is_none());

    db.store
        .record_vpngate_ip_intelligence_report(
            "intel-a",
            &intelligence_report(&claim.exit_ip, claim.lease_generation, 9),
        )
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT country_code, ip_scores, ip_networks, verified_at IS NOT NULL AS verified
           FROM vpngate_exit_reputations
          WHERE exit_ip = '198.51.100.40'::inet",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    use sqlx::Row as _;
    assert_eq!(row.try_get::<String, _>("country_code").unwrap(), "JP");
    assert!(row.try_get::<bool, _>("verified").unwrap());
    assert_eq!(
        serde_json::from_value::<Vec<VpngateIpScore>>(row.try_get("ip_scores").unwrap()).unwrap(),
        scores(9)
    );
    assert_eq!(
        serde_json::from_value::<Vec<VpngateIpNetwork>>(row.try_get("ip_networks").unwrap())
            .unwrap(),
        networks()
    );
    let shared_profile = sqlx::query_scalar::<_, String>(
        "SELECT profile_sha256 FROM vpngate_servers WHERE id = 'shared.example'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let now = sqlx::query_scalar::<_, i64>("SELECT EXTRACT(EPOCH FROM now())::BIGINT")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .record_vpngate_probe_report(
            "intel-a",
            VpngateProbeReport {
                catalog_generation: 1,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: "shared.example".to_owned(),
                    profile_sha256: shared_profile.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.40".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(700),
                    download_bps: Some(20_000_000),
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: now,
                }],
            },
        )
        .await
        .unwrap();
    db.store
        .record_vpngate_probe_report(
            "intel-a",
            VpngateProbeReport {
                catalog_generation: 1,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: "shared.example".to_owned(),
                    profile_sha256: shared_profile,
                    status: VpngateProbeStatus::Failed,
                    exit_ip: None,
                    exit_country_code: None,
                    connect_ms: None,
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: Some("openvpn-connect".to_owned()),
                    error_detail: Some("connection timed out".to_owned()),
                    probed_at_unix_secs: now + 1,
                }],
            },
        )
        .await
        .unwrap();
    let shared_view = db
        .store
        .vpngate_country_servers("JP")
        .await
        .unwrap()
        .into_iter()
        .find(|server| server.id == "shared.example")
        .unwrap();
    assert_eq!(shared_view.latest_probe_status.as_deref(), Some("failed"));
    assert_eq!(shared_view.latest_exit_ip.as_deref(), Some("198.51.100.40"));
    assert_eq!(shared_view.latest_ip_scores, scores(9));
    assert_eq!(shared_view.latest_successful_probed_at_unix_secs, Some(now));
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT next_check_at = 'infinity'::timestamptz
           FROM vpngate_exit_reputations
          WHERE exit_ip = '198.51.100.40'::inet",
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    assert!(db
        .store
        .claim_vpngate_exit_intelligence("intel-b")
        .await
        .unwrap()
        .is_none());

    let periodic = VpngateIntelligencePolicy {
        refresh_mode: VpngateIntelligenceRefreshMode::Periodic,
        refresh_interval_hours: 24,
        active_window_hours: 72,
        ..VpngateIntelligencePolicy::default()
    };
    assert_eq!(
        db.store
            .update_vpngate_intelligence_policy(&admin, periodic.clone())
            .await
            .unwrap(),
        periodic
    );
    sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET next_check_at = now(), last_seen_at = now() - interval '73 hours'
          WHERE exit_ip = '198.51.100.40'::inet",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db
        .store
        .claim_vpngate_exit_intelligence("intel-a")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        db.store
            .request_vpngate_intelligence_refresh(&admin)
            .await
            .unwrap()
            .queued,
        0
    );

    sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET last_seen_at = now()
          WHERE exit_ip = '198.51.100.40'::inet",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        db.store
            .request_vpngate_intelligence_refresh(&admin)
            .await
            .unwrap()
            .queued,
        1
    );
    assert_eq!(
        db.store
            .claim_vpngate_exit_intelligence("intel-b")
            .await
            .unwrap()
            .expect("a recently dialled exit can be refreshed")
            .exit_ip,
        "198.51.100.40"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn proxycheck_keys_are_sealed_random_start_round_robined_and_legacy_values_remain_readable() {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-credentials-test");
    assert!(
        !db.store
            .vpngate_overview()
            .await
            .unwrap()
            .intelligence_credentials
            .proxycheck_api_key_configured
    );
    assert!(db
        .store
        .update_vpngate_intelligence_credentials(
            &admin,
            UpdateVpngateIntelligenceCredentials {
                proxycheck_api_keys: vec!["invalid".to_owned()],
                mode: VpngateIntelligenceCredentialUpdateMode::Replace,
            },
        )
        .await
        .is_err());

    let legacy_key = "000000-111111-222222-333333";
    let legacy_sealed =
        brocade_store::secrets::seal(brocade_store::secrets::CTX_PROXYCHECK_API_KEY, legacy_key)
            .unwrap();
    sqlx::query(
        "UPDATE vpngate_catalog_state
            SET proxycheck_api_key_sealed = $1
          WHERE id = TRUE",
    )
    .bind(&legacy_sealed)
    .execute(db.pool())
    .await
    .unwrap();

    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.credentials".to_owned(),
                name: "Credentials".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('intel-key', 'platform.credentials', 'Intel key', '10.88.0.30',
                 'private-key', 'public-key', 51830, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES ('intel-key', $1, '{}'::jsonb, now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .update_vpngate_intelligence_node(
            &admin,
            "intel-key",
            UpdateVpngateIntelligenceNode { enabled: true },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO vpngate_exit_reputations (exit_ip, next_check_at, last_seen_at)
         VALUES ('192.0.2.44', now(), now()),
                ('192.0.2.45', now(), now()),
                ('192.0.2.46', now(), now())",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let legacy_claim = db
        .store
        .claim_vpngate_exit_intelligence("intel-key")
        .await
        .unwrap()
        .expect("selected current Agent receives one due intelligence task");
    assert_eq!(legacy_claim.exit_ip, "192.0.2.44");
    assert_eq!(legacy_claim.proxycheck_api_key.as_deref(), Some(legacy_key));

    let replacements = ["111111-222222-333333-444444", "aaaaaa-bbbbbb-cccccc-dddddd"];
    let credentials = db
        .store
        .update_vpngate_intelligence_credentials(
            &admin,
            UpdateVpngateIntelligenceCredentials {
                proxycheck_api_keys: replacements.iter().map(ToString::to_string).collect(),
                mode: VpngateIntelligenceCredentialUpdateMode::Replace,
            },
        )
        .await
        .unwrap();
    assert!(credentials.proxycheck_api_key_configured);
    let sealed: String = sqlx::query_scalar(
        "SELECT proxycheck_api_key_sealed FROM vpngate_catalog_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(sealed.starts_with("v1."));
    for replacement in replacements {
        assert!(!sealed.contains(replacement));
    }

    let mut claimed_keys = Vec::new();
    for expected_ip in ["192.0.2.45", "192.0.2.46"] {
        let claim = db
            .store
            .claim_vpngate_exit_intelligence("intel-key")
            .await
            .unwrap()
            .expect("each due intelligence task receives one key from the pool");
        assert_eq!(claim.exit_ip, expected_ip);
        claimed_keys.push(claim.proxycheck_api_key.expect("the pool supplies one key"));
    }
    claimed_keys.sort();
    let mut expected_keys = replacements.map(ToString::to_string);
    expected_keys.sort();
    assert_eq!(claimed_keys, expected_keys);

    let appended_key = "zzzzzz-yyyyyy-xxxxxx-wwwwww";
    db.store
        .update_vpngate_intelligence_credentials(
            &admin,
            UpdateVpngateIntelligenceCredentials {
                proxycheck_api_keys: vec![appended_key.to_owned(), replacements[0].to_owned()],
                mode: VpngateIntelligenceCredentialUpdateMode::Append,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO vpngate_exit_reputations (exit_ip, next_check_at, last_seen_at)
         VALUES ('192.0.2.47', now(), now()),
                ('192.0.2.48', now(), now()),
                ('192.0.2.49', now(), now())",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let mut appended_claim_keys = Vec::new();
    for expected_ip in ["192.0.2.47", "192.0.2.48", "192.0.2.49"] {
        let claim = db
            .store
            .claim_vpngate_exit_intelligence("intel-key")
            .await
            .unwrap()
            .expect("an appended pool still supplies every retained key");
        assert_eq!(claim.exit_ip, expected_ip);
        appended_claim_keys.push(
            claim
                .proxycheck_api_key
                .expect("the appended pool supplies a key"),
        );
    }
    appended_claim_keys.sort();
    let mut expected_appended_keys = replacements
        .into_iter()
        .chain([appended_key])
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    expected_appended_keys.sort();
    assert_eq!(appended_claim_keys, expected_appended_keys);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn country_view_returns_the_complete_retained_directory() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let claim = db
        .store
        .claim_vpngate_sync("complete-country-view", "startup")
        .await
        .unwrap()
        .expect("a fresh catalogue is due");
    let servers = (0_u16..205)
        .map(|index| {
            let third = index / 250;
            let fourth = index % 250 + 1;
            server(
                &format!("vpn-{index}.opengw.net"),
                "JP",
                &format!("198.18.{third}.{fourth}"),
                u64::from(index),
            )
        })
        .collect::<Vec<_>>();
    db.store
        .complete_vpngate_sync(
            "complete-country-view",
            &claim,
            VpngateSyncBatch {
                content_sha256: "c".repeat(64),
                fetched_rows: u32::try_from(servers.len()).unwrap(),
                rejected_rows: 0,
                servers,
            },
        )
        .await
        .unwrap();

    let view = db.store.vpngate_country_servers("JP").await.unwrap();
    assert_eq!(view.len(), 205);

    let first_page = db
        .store
        .vpngate_country_server_page("JP", VpngateServerPageRequest::default())
        .await
        .unwrap();
    assert_eq!(first_page.items.len(), 100);
    assert_eq!(first_page.total, 205);
    assert_eq!(first_page.page, 1);

    let second_page = db
        .store
        .vpngate_country_server_page(
            "JP",
            VpngateServerPageRequest {
                page: 2,
                ..VpngateServerPageRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second_page.items.len(), 100);
    assert_eq!(second_page.total, 205);
    assert_eq!(second_page.page, 2);
    assert!(first_page
        .items
        .iter()
        .all(|item| !second_page.items.iter().any(|next| next.id == item.id)));

    let past_last_page = db
        .store
        .vpngate_country_server_page(
            "JP",
            VpngateServerPageRequest {
                page: 4,
                ..VpngateServerPageRequest::default()
            },
        )
        .await
        .unwrap();
    assert!(past_last_page.items.is_empty());
    assert_eq!(past_last_page.total, 205);

    let searched = db
        .store
        .vpngate_country_server_page(
            "JP",
            VpngateServerPageRequest {
                search: "vpn-204.opengw.net".to_owned(),
                ..VpngateServerPageRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.total, 1);
    assert_eq!(searched.items[0].hostname, "vpn-204.opengw.net");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn probe_report_survives_a_concurrent_catalogue_identity_change() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-probe-race-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.probe-race".to_owned(),
                name: "Probe race".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('probe-race', 'platform.probe-race', 'Probe race', '10.88.0.30',
                 'private', 'public', 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();

    let first = db
        .store
        .claim_vpngate_sync("probe-race", "startup")
        .await
        .unwrap()
        .expect("a fresh catalogue is due");
    let assigned = server("moving.example", "JP", "192.0.2.30", 100);
    db.store
        .complete_vpngate_sync(
            "probe-race",
            &first,
            VpngateSyncBatch {
                content_sha256: "d".repeat(64),
                fetched_rows: 1,
                rejected_rows: 0,
                servers: vec![assigned.clone()],
            },
        )
        .await
        .unwrap();

    // A catalogue probe can take longer than the independent collection loop. The provider may
    // move one server ID to another country and profile while the old assignment is in flight.
    db.store.request_vpngate_sync(&admin).await.unwrap();
    let second = db
        .store
        .claim_vpngate_sync("probe-race", "manual")
        .await
        .unwrap()
        .expect("the requested refresh is due");
    db.store
        .complete_vpngate_sync(
            "probe-race",
            &second,
            VpngateSyncBatch {
                content_sha256: "e".repeat(64),
                fetched_rows: 1,
                rejected_rows: 0,
                servers: vec![server("moving.example", "KR", "192.0.2.31", 110)],
            },
        )
        .await
        .unwrap();

    let receipt = db
        .store
        .record_vpngate_probe_report(
            "probe-race",
            VpngateProbeReport {
                catalog_generation: first.run_id,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: assigned.id.clone(),
                    profile_sha256: assigned.profile_sha256.clone(),
                    status: VpngateProbeStatus::Failed,
                    exit_ip: None,
                    exit_country_code: None,
                    connect_ms: None,
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: Some("catalogue-probe-failed".to_owned()),
                    error_detail: Some("test failure".to_owned()),
                    probed_at_unix_secs: 1_800_000_000,
                }],
            },
        )
        .await
        .expect("an in-flight assignment remains valid after the current projection changes");
    assert_eq!(receipt.accepted_samples, 1);

    let stored_country = sqlx::query_scalar::<_, String>(
        "SELECT country_code
           FROM vpngate_candidate_probe_samples
          WHERE node_id = 'probe-race' AND server_id = 'moving.example'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stored_country, "JP");

    let never_observed = db
        .store
        .record_vpngate_probe_report(
            "probe-race",
            VpngateProbeReport {
                catalog_generation: first.run_id,
                country_code: "US".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: assigned.id,
                    profile_sha256: assigned.profile_sha256,
                    status: VpngateProbeStatus::Failed,
                    exit_ip: None,
                    exit_country_code: None,
                    connect_ms: None,
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: Some("catalogue-probe-failed".to_owned()),
                    error_detail: Some("test failure".to_owned()),
                    probed_at_unix_secs: 1_800_000_001,
                }],
            },
        )
        .await;
    assert!(never_observed.is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn same_region_probe_nodes_partition_one_resilient_queue() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-shared-probe-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, public_ipv4, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES
            ('jp-a', 'platform.acme', 'Japan A', '192.0.2.101', '10.88.0.11',
             'private-a', 'public-a', 51820, TRUE, 'system'),
            ('jp-b', 'platform.acme', 'Japan B', '192.0.2.102', '10.88.0.12',
             'private-b', 'public-b', 51821, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES
            ('jp-a', $1,
             '{\"openvpn\":\"OpenVPN 2.6.12\",\"vpngate_catalog_probe_workers\":1}'::jsonb,
             now()),
            ('jp-b', $1,
             '{\"openvpn\":\"OpenVPN 2.6.12\",\"vpngate_catalog_probe_workers\":1}'::jsonb,
             now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    for node_id in ["jp-a", "jp-b"] {
        db.store
            .update_vpngate_probe_node(
                &admin,
                node_id,
                UpdateVpngateProbeNode {
                    enabled: true,
                    workers: Some(1),
                },
            )
            .await
            .unwrap();
    }

    let sync = db
        .store
        .claim_vpngate_sync("test-worker", "startup")
        .await
        .unwrap()
        .expect("fresh database is immediately due");
    db.store
        .complete_vpngate_sync(
            "test-worker",
            &sync,
            VpngateSyncBatch {
                content_sha256: "a".repeat(64),
                fetched_rows: 2,
                rejected_rows: 0,
                servers: vec![
                    server("vpn-jp-a", "JP", "192.0.2.10", 100),
                    server("vpn-jp-b", "JP", "192.0.2.11", 90),
                ],
            },
        )
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar(
        "SELECT revision_id FROM model_snapshots ORDER BY revision_id DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let client_snapshot_id: i64 = sqlx::query_scalar(
        "SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO subscription_serving_state
            (id, topology_revision_id, permissions_revision_id, client_snapshot_id, generation)
         VALUES (TRUE, $1, $1, $2, 1)",
    )
    .bind(revision)
    .bind(client_snapshot_id)
    .execute(db.pool())
    .await
    .unwrap();

    let origins = vec![
        VpngateProbeNodeOrigin {
            node_id: "jp-a".to_owned(),
            country_code: "JP".to_owned(),
        },
        VpngateProbeNodeOrigin {
            node_id: "jp-b".to_owned(),
            country_code: "JP".to_owned(),
        },
    ];
    let (desired_a, desired_b) = tokio::join!(
        db.store
            .vpngate_agent_desired_with_probe_origins("jp-a", &origins),
        db.store
            .vpngate_agent_desired_with_probe_origins("jp-b", &origins),
    );
    let desired_a = desired_a.unwrap().unwrap();
    let desired_b = desired_b.unwrap().unwrap();
    let candidate_a = desired_a.probe_assignments[0].candidates[0].clone();
    let candidate_b = desired_b.probe_assignments[0].candidates[0].clone();
    assert_eq!(candidate_a.probe_mode, VpngateProbeMode::Performance);
    assert_eq!(candidate_b.probe_mode, VpngateProbeMode::Performance);
    assert_ne!(
        candidate_a.server_id, candidate_b.server_id,
        "same-region nodes divide profiles instead of repeating the complete queue"
    );
    let repeated_a = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated_a.probe_assignments[0].candidates[0].server_id, candidate_a.server_id,
        "without a report, a machine's own oldest item remains stable"
    );

    let probed_at: i64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::BIGINT")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .record_vpngate_probe_report(
            "jp-a",
            VpngateProbeReport {
                catalog_generation: desired_a.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: candidate_a.server_id.clone(),
                    profile_sha256: candidate_a.profile_sha256.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.100".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(500),
                    download_bps: Some(20_000_000),
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: probed_at,
                }],
            },
        )
        .await
        .unwrap();
    let cooling = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    assert!(
        cooling
            .probe_assignments
            .iter()
            .flat_map(|assignment| &assignment.candidates)
            .all(|candidate| candidate.server_id != candidate_a.server_id),
        "a successful profile is not assigned again during its thirty-minute cooldown"
    );
    let updated_schedule = db
        .store
        .update_vpngate_probe_settings(
            &admin,
            UpdateVpngateProbeSettings {
                success_cooldown_secs: 60 * 60,
                performance_cooldown_secs: 6 * 60 * 60,
                shard_rotation_secs: 6 * 60 * 60,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated_schedule.probe_success_cooldown_secs, 60 * 60);
    assert_eq!(
        updated_schedule.probe_performance_cooldown_secs,
        6 * 60 * 60
    );
    assert_eq!(updated_schedule.probe_shard_rotation_secs, 6 * 60 * 60);
    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '31 minutes'
          WHERE server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&candidate_a.server_id)
    .bind(&candidate_a.profile_sha256)
    .execute(db.pool())
    .await
    .unwrap();
    let still_cooling = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    assert!(
        still_cooling
            .probe_assignments
            .iter()
            .flat_map(|assignment| &assignment.candidates)
            .all(|candidate| candidate.server_id != candidate_a.server_id),
        "the saved cooldown applies to the next desired batch"
    );
    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '61 minutes'
          WHERE server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&candidate_a.server_id)
    .bind(&candidate_a.profile_sha256)
    .execute(db.pool())
    .await
    .unwrap();
    let cooled = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cooled.probe_assignments[0].candidates[0].server_id, candidate_a.server_id,
        "the profile becomes due after its successful cooldown"
    );
    assert_eq!(
        cooled.probe_assignments[0].candidates[0].probe_mode,
        VpngateProbeMode::Connectivity,
        "a recent full measurement turns the next scheduled check into connectivity-only work"
    );
    db.store
        .record_vpngate_probe_report(
            "jp-a",
            VpngateProbeReport {
                catalog_generation: desired_a.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: candidate_a.server_id.clone(),
                    profile_sha256: candidate_a.profile_sha256.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.100".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(450),
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: probed_at + 1,
                }],
            },
        )
        .await
        .unwrap();
    let retained_performance: Option<i64> = sqlx::query_scalar(
        "SELECT last_success_download_bps
           FROM vpngate_candidate_probe_latest
          WHERE node_id = 'jp-a' AND server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&candidate_a.server_id)
    .bind(&candidate_a.profile_sha256)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(retained_performance, Some(20_000_000));

    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '61 minutes'
          WHERE server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&candidate_a.server_id)
    .bind(&candidate_a.profile_sha256)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE vpngate_candidate_probe_latest
            SET last_success_received_at = now() - interval '7 hours'
          WHERE server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&candidate_a.server_id)
    .bind(&candidate_a.profile_sha256)
    .execute(db.pool())
    .await
    .unwrap();
    let performance_due = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        performance_due.probe_assignments[0].candidates[0].probe_mode,
        VpngateProbeMode::Performance,
        "a profile receives a full measurement after the configured performance interval"
    );
    let failed_sample = |candidate: &VpngateCandidate| VpngateProbeSample {
        server_id: candidate.server_id.clone(),
        profile_sha256: candidate.profile_sha256.clone(),
        status: VpngateProbeStatus::Failed,
        exit_ip: None,
        exit_country_code: None,
        connect_ms: None,
        download_bps: None,
        ip_scores: Vec::new(),
        ip_networks: Vec::new(),
        error_code: Some("catalogue-probe-failed".to_owned()),
        error_detail: Some("test failure".to_owned()),
        probed_at_unix_secs: probed_at + 2,
    };
    db.store
        .record_vpngate_probe_report(
            "jp-a",
            VpngateProbeReport {
                catalog_generation: desired_a.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![failed_sample(&candidate_a)],
            },
        )
        .await
        .unwrap();
    let next_a = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-a", &origins)
        .await
        .unwrap()
        .unwrap();
    let unchanged_b = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-b", &origins)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        next_a.probe_assignments[0].candidates[0].server_id, candidate_a.server_id,
        "a failure immediately keeps the profile at the front for priority review"
    );
    assert_eq!(
        next_a.probe_assignments[0].candidates[0].probe_mode,
        VpngateProbeMode::Performance,
        "the first successful recovery attempt must refresh single-stream performance"
    );
    assert_eq!(
        unchanged_b.probe_assignments[0].candidates[0].server_id, candidate_b.server_id,
        "another same-region machine keeps its disjoint shard"
    );

    db.store
        .record_vpngate_probe_report(
            "jp-b",
            VpngateProbeReport {
                catalog_generation: desired_b.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![failed_sample(&candidate_b)],
            },
        )
        .await
        .unwrap();
    let next_b = db
        .store
        .vpngate_agent_desired_with_probe_origins("jp-b", &origins)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        next_b.probe_assignments[0].candidates[0].server_id, candidate_b.server_id,
        "each shard performs priority review for its own failed profile"
    );

    sqlx::query(
        "UPDATE node_agent_state
            SET runtime_reported_at = now() - interval '3 minutes'
          WHERE node_id = 'jp-b'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let unaffected = db
        .store
        .vpngate_agent_desired_with_probe_origins(
            "jp-a",
            &[VpngateProbeNodeOrigin {
                node_id: "jp-a".to_owned(),
                country_code: "JP".to_owned(),
            }],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unaffected.probe_assignments[0].candidates[0].server_id, candidate_b.server_id,
        "an offline peer's shard is immediately reassigned to a remaining machine"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn vpngate_sync_keeps_history_while_replacing_only_the_current_projection() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let admin = AdminContext::system_admin("vpngate-test");
    db.store
        .create_tenant(
            &admin,
            CreateTenantRequest {
                id: "platform.acme".to_owned(),
                name: "Platform".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO nodes
            (id, tenant_id, name, overlay_addr, wg_private_key, wg_public_key,
             wg_listen_port, egress_allowed, dns_kind)
         VALUES ('edge', 'platform.acme', 'Edge', '10.88.0.10', 'private', 'public',
                 51820, TRUE, 'system')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_agent_state
            (node_id, agent_protocol_version, runtime_versions, runtime_reported_at)
         VALUES ('edge', $1, '{\"agent\":\"build-without-openvpn\"}'::jsonb, now())",
    )
    .bind(i32::try_from(AGENT_PROTOCOL_VERSION + 1).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    db.store
        .upsert_app(
            &admin,
            CreateAppRequest {
                id: "app-2222".to_owned(),
                label: "VPN Gate storage".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();
    db.store
        .upsert_chain(
            &admin,
            "app-2222",
            CreateChainRequest {
                id: "chn-2222-2222".to_owned(),
                tenant_id: "platform.acme".to_owned(),
                name: "VPN Gate storage".to_owned(),
                subscription_country: None,
                note: None,
            },
        )
        .await
        .unwrap();
    // Exercise storage/materialization for both the new manual field and legacy automatic pools.
    for manual_ids in [
        vec!["vpn-jp".to_owned(), "vpn-jp-risky".to_owned()],
        Vec::new(),
    ] {
        db.store
            .apply_draft(
                &admin,
                vec![
                    ModelOp::UpsertExternalOutbound {
                        outbound: UpsertExternalOutboundRequest {
                            id: "vpngate-1111-1111".to_owned(),
                            tenant_id: "platform.acme".to_owned(),
                            name: "VPN Gate Japan".to_owned(),
                            address: "managed.vpngate.invalid".to_owned(),
                            port: 1,
                            protocol: ExternalOutboundProtocol::Vpngate {
                                country_code: "JP".to_owned(),
                                server_id: None,
                                server_ids: manual_ids.clone(),
                                max_connect_ms: 12_000,
                                min_download_bps: 2_000_000,
                                max_candidates: if manual_ids.is_empty() { 10 } else { 2 },
                            },
                            security: ExternalOutboundSecurity::None,
                            note: None,
                        },
                    },
                    ModelOp::PutStep {
                        app_id: "app-2222".to_owned(),
                        chain_id: "chn-2222-2222".to_owned(),
                        node_id: "edge".to_owned(),
                        step: PutStepRequest {
                            accept: None,
                            hop_in: None,
                            rules: vec![Rule {
                                dest_match: DestMatch::Any,
                                action: Action::Proxy {
                                    outbound: "vpngate-1111-1111".to_owned(),
                                },
                            }],
                            note: None,
                        },
                    },
                ],
                None,
            )
            .await
            .unwrap();
        let roundtrip = db.store.materialize_snapshot(None).await.unwrap();
        let ExternalOutboundProtocol::Vpngate { server_ids, .. } =
            &roundtrip.external_outbounds[0].protocol
        else {
            panic!("VPN Gate protocol")
        };
        assert_eq!(server_ids, &manual_ids);
    }
    let snapshot = db.store.materialize_snapshot(None).await.unwrap();
    assert!(matches!(
        &snapshot.external_outbounds[0].protocol,
        ExternalOutboundProtocol::Vpngate {
            country_code,
            server_id: None,
            server_ids: _,
            max_connect_ms: 12_000,
            min_download_bps: 2_000_000,
            max_candidates: 10,
        } if country_code == "JP"
    ));
    let initial = db.store.vpngate_overview().await.unwrap();
    assert!(initial.manual_pools_supported);
    assert_eq!(initial.status.current_servers, 0);
    assert!(initial.sync_history.is_empty());
    assert!(db
        .store
        .vpngate_runtime_views(&admin)
        .await
        .unwrap()
        .is_empty());

    let first = db
        .store
        .claim_vpngate_sync("test-worker", "startup")
        .await
        .unwrap()
        .expect("fresh database is immediately due");
    db.store
        .complete_vpngate_sync(
            "test-worker",
            &first,
            VpngateSyncBatch {
                content_sha256: "a".repeat(64),
                fetched_rows: 4,
                rejected_rows: 0,
                servers: vec![
                    server("vpn-jp", "JP", "192.0.2.10", 100),
                    server("vpn-jp-risky", "JP", "192.0.2.11", 110),
                    server("vpn-kr", "KR", "192.0.2.20", 90),
                    // Simulates data accepted before ZZ became an explicitly excluded provider
                    // bucket. Read paths and probe scheduling must remain safe with that history.
                    server("vpn-unknown", "ZZ", "192.0.2.30", 80),
                ],
            },
        )
        .await
        .unwrap();
    let first_view = db.store.vpngate_overview().await.unwrap();
    assert_eq!(first_view.status.current_servers, 3);
    assert_eq!(first_view.status.retained_observations, 3);
    assert_eq!(first_view.sync_history.len(), 1);
    assert_eq!(first_view.sync_history[0].current_servers, 3);
    assert_eq!(first_view.sync_history[0].first_seen_servers, 3);
    assert_eq!(first_view.sync_history[0].accepted_rows, 4);
    assert_eq!(first_view.sync_history[0].rejected_rows, 0);
    assert_eq!(first_view.countries.len(), 2);
    assert!(db
        .store
        .vpngate_country_servers("ZZ")
        .await
        .unwrap()
        .is_empty());

    // A concrete-node target is still revisioned intent, but its operational desired state must
    // contain only that server. This is what makes "fixed" fail closed instead of silently falling
    // back to another relay from the same country.
    let mut pinned = snapshot.clone();
    pinned.external_outbounds[0].protocol = ExternalOutboundProtocol::Vpngate {
        country_code: "JP".to_owned(),
        server_id: Some("vpn-jp".to_owned()),
        server_ids: Vec::new(),
        max_connect_ms: 15_000,
        min_download_bps: 1_000_000,
        max_candidates: 1,
    };
    let mut korea = pinned.external_outbounds[0].clone();
    korea.id = "vpngate-2222-2222".to_owned();
    korea.name = "VPN Gate Korea".to_owned();
    korea.protocol = ExternalOutboundProtocol::Vpngate {
        country_code: "KR".to_owned(),
        server_id: None,
        server_ids: Vec::new(),
        max_connect_ms: 15_000,
        min_download_bps: 1_000_000,
        max_candidates: 10,
    };
    pinned.external_outbounds.push(korea);
    pinned.apps.push(AppView {
        id: "app".to_owned(),
        label: "Pinned VPN Gate".to_owned(),
        chains: Vec::new(),
        ingresses: Vec::new(),
        fronts: Vec::new(),
        steps: vec![Step {
            chain: "chain".to_owned(),
            node: "edge".to_owned(),
            accept: None,
            hop_in: None,
            rules: vec![Rule {
                dest_match: DestMatch::Any,
                action: Action::Proxy {
                    outbound: "vpngate-1111-1111".to_owned(),
                },
            }],
        }],
        grants: Vec::new(),
    });
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(pinned.revision).unwrap())
        .bind(serde_json::to_value(&pinned).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let client_snapshot_id: i64 = sqlx::query_scalar(
        "SELECT head_snapshot_id FROM subscription_client_state WHERE id = TRUE",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO subscription_serving_state
            (id, topology_revision_id, permissions_revision_id, client_snapshot_id, generation)
         VALUES (TRUE, $1, $1, $2, 1)",
    )
    .bind(i64::try_from(pinned.revision).unwrap())
    .bind(client_snapshot_id)
    .execute(db.pool())
    .await
    .unwrap();
    let unavailable = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("a serving snapshot produces a complete empty desired state");
    assert!(
        unavailable.pools.is_empty(),
        "a node without a reported OpenVPN binary must not receive VPN Gate profiles"
    );
    sqlx::query(
        "UPDATE node_agent_state
            SET runtime_versions = runtime_versions ||
                '{\"openvpn\":\"OpenVPN 2.6.12 x86_64-pc-linux-gnu\",\"vpngate_catalog_probe_workers\":2}'::jsonb,
                runtime_reported_at = now()
          WHERE node_id = 'edge'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let selection = db
        .store
        .update_vpngate_probe_node(
            &admin,
            "edge",
            UpdateVpngateProbeNode {
                enabled: true,
                workers: Some(2),
            },
        )
        .await
        .unwrap();
    assert!(selection.enabled);
    assert_eq!(selection.workers, Some(2));
    let intelligence_selection = db
        .store
        .update_vpngate_intelligence_node(
            &admin,
            "edge",
            UpdateVpngateIntelligenceNode { enabled: true },
        )
        .await
        .unwrap();
    assert!(intelligence_selection.enabled);
    let desired = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("a serving snapshot produces VPN Gate desired state");
    assert_eq!(desired.pools.len(), 1);
    assert_eq!(desired.pools[0].max_candidates, 1);
    assert_eq!(desired.pools[0].candidates.len(), 1);
    assert_eq!(desired.pools[0].candidates[0].server_id, "vpn-jp");
    assert_eq!(desired.probe_assignments.len(), 1);
    assert_eq!(desired.probe_assignments[0].country_code, "JP");
    assert_eq!(desired.probe_assignments[0].candidates.len(), 2);

    let region_filtered = db
        .store
        .vpngate_agent_desired_with_probe_origins(
            "edge",
            &[
                VpngateProbeNodeOrigin {
                    node_id: "edge".to_owned(),
                    country_code: "US".to_owned(),
                },
                VpngateProbeNodeOrigin {
                    node_id: "asia-probe".to_owned(),
                    country_code: "JP".to_owned(),
                },
            ],
        )
        .await
        .unwrap()
        .expect("the runtime state remains available when regional probing is enabled");
    assert!(
        region_filtered.probe_assignments.is_empty(),
        "an American probe machine must not receive retained Asian catalogue candidates"
    );

    let safe_candidate = desired.probe_assignments[0]
        .candidates
        .iter()
        .find(|candidate| candidate.server_id == "vpn-jp")
        .unwrap()
        .clone();
    let risky_candidate = desired.probe_assignments[0]
        .candidates
        .iter()
        .find(|candidate| candidate.server_id == "vpn-jp-risky")
        .unwrap()
        .clone();
    let safe_catalog_sample = VpngateProbeSample {
        server_id: safe_candidate.server_id.clone(),
        profile_sha256: safe_candidate.profile_sha256.clone(),
        status: VpngateProbeStatus::Succeeded,
        exit_ip: Some("198.51.100.20".to_owned()),
        exit_country_code: None,
        connect_ms: Some(820),
        download_bps: Some(42_000_000),
        ip_scores: Vec::new(),
        ip_networks: Vec::new(),
        error_code: None,
        error_detail: None,
        probed_at_unix_secs: 1_800_000_000,
    };
    let risky_catalog_sample = VpngateProbeSample {
        server_id: risky_candidate.server_id.clone(),
        profile_sha256: risky_candidate.profile_sha256.clone(),
        status: VpngateProbeStatus::Succeeded,
        exit_ip: Some("198.51.100.21".to_owned()),
        exit_country_code: None,
        connect_ms: Some(300),
        download_bps: Some(100_000_000),
        ip_scores: Vec::new(),
        ip_networks: Vec::new(),
        error_code: None,
        error_detail: None,
        probed_at_unix_secs: 1_800_000_001,
    };
    db.store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: desired.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![safe_catalog_sample, risky_catalog_sample],
            },
        )
        .await
        .unwrap();
    // A second historical result from the same Agent remains in the short diagnostic store, but
    // neither current selection nor the directory's current evidence count may read it.
    db.store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: desired.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: safe_candidate.server_id.clone(),
                    profile_sha256: safe_candidate.profile_sha256.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.20".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(50),
                    download_bps: Some(500_000_000),
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: 1_799_999_999,
                }],
            },
        )
        .await
        .unwrap();

    // Desired-state polls may spend minutes being processed by an OpenVPN catalogue batch. They
    // must not acquire an IP-intelligence lease; the dedicated assignment endpoint is the only
    // authority that advances that queue.
    db.store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("catalogue desired state remains available");
    let desired_poll_leases: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM vpngate_exit_reputations
          WHERE lease_owner IS NOT NULL OR lease_generation <> 0",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(desired_poll_leases, 0);

    for _ in 0..2 {
        let reputation = db
            .store
            .claim_vpngate_exit_intelligence("edge")
            .await
            .unwrap()
            .expect("a newly discovered exit is immediately due for distributed intelligence");
        let risk = match reputation.exit_ip.as_str() {
            "198.51.100.20" => 8,
            "198.51.100.21" => 60,
            unexpected => panic!("unexpected reputation claim for {unexpected}"),
        };
        db.store
            .record_vpngate_ip_intelligence_report(
                "edge",
                &intelligence_report(&reputation.exit_ip, reputation.lease_generation, risk),
            )
            .await
            .unwrap();
    }

    pinned.external_outbounds[0].protocol = ExternalOutboundProtocol::Vpngate {
        country_code: "JP".to_owned(),
        server_id: None,
        server_ids: Vec::new(),
        max_connect_ms: 15_000,
        min_download_bps: 1_000_000,
        max_candidates: 2,
    };
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(pinned.revision).unwrap())
        .bind(serde_json::to_value(&pinned).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '31 minutes'
          WHERE last_outcome_status = 'succeeded'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let ranked_desired = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("distributed intelligence updates operational desired state");
    assert_eq!(
        ranked_desired
            .probe_assignments
            .iter()
            .map(|assignment| assignment.candidates.len())
            .sum::<usize>(),
        2
    );
    assert!(ranked_desired
        .probe_assignments
        .iter()
        .any(|assignment| assignment.country_code == "KR" && assignment.candidates.len() == 1));
    assert_eq!(ranked_desired.pools[0].candidates.len(), 2);
    assert_eq!(ranked_desired.pools[0].candidates[0].server_id, "vpn-jp");
    assert_eq!(
        ranked_desired.pools[0].candidates[0]
            .verified_exit_ip
            .as_deref(),
        Some("198.51.100.20")
    );
    assert_eq!(
        ranked_desired.pools[0].candidates[0].verified_ip_scores,
        scores(8)
    );
    assert_eq!(
        ranked_desired.pools[0].candidates[1].verified_ip_scores,
        scores(60)
    );

    // ISP exclusions remove both the active shortlist and its ranked reserve, but keep the
    // catalogue and probe evidence visible. Explicitly selected pools must not bypass the rule.
    for (exit_ip, isp) in [
        ("198.51.100.20", "OPTAGE"),
        ("198.51.100.21", "Chubu Telecommunications Company, Inc."),
    ] {
        let mut ip_networks = networks();
        ip_networks[2].isp = Some(isp.to_owned());
        sqlx::query(
            "UPDATE vpngate_exit_reputations SET ip_networks = $2 WHERE exit_ip = $1::INET",
        )
        .bind(exit_ip)
        .bind(serde_json::to_value(ip_networks).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
        let filtered = db
            .store
            .vpngate_agent_desired("edge")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            filtered.pools[0].candidates.len(),
            if isp == "OPTAGE" { 1 } else { 0 }
        );
    }
    let directory = db.store.vpngate_country_servers("JP").await.unwrap();
    assert_eq!(
        directory.len(),
        2,
        "excluded ISPs remain in the audit directory"
    );
    assert!(directory
        .iter()
        .all(|server| server.candidate_rank.is_none()));
    assert!(directory.iter().all(|server| !server.active));
    let country = db
        .store
        .vpngate_overview()
        .await
        .unwrap()
        .countries
        .into_iter()
        .find(|country| country.country_code == "JP")
        .unwrap();
    assert_eq!(country.candidate_servers, 0);

    let mut manual_isp = pinned.clone();
    if let ExternalOutboundProtocol::Vpngate { server_ids, .. } =
        &mut manual_isp.external_outbounds[0].protocol
    {
        *server_ids = vec!["vpn-jp".to_owned(), "vpn-jp-risky".to_owned()];
    }
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(manual_isp.revision).unwrap())
        .bind(serde_json::to_value(&manual_isp).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let manual_filtered = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap();
    assert!(manual_filtered.pools[0].candidates.is_empty());
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(pinned.revision).unwrap())
        .bind(serde_json::to_value(&pinned).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE vpngate_exit_reputations
            SET ip_networks = $1
          WHERE exit_ip IN ('198.51.100.20', '198.51.100.21')",
    )
    .bind(serde_json::to_value(networks()).unwrap())
    .execute(db.pool())
    .await
    .unwrap();

    // Changing operational policy immediately re-evaluates retained provider evidence. The
    // provider scores are never collapsed into one cross-provider maximum.
    let mut all_sources_policy = VpngateAdmissionPolicy::default();
    all_sources_policy
        .provider_rules
        .iter_mut()
        .find(|rule| rule.provider == VpngateIpProvider::Proxycheck)
        .unwrap()
        .maximum_score = 50;
    db.store
        .update_vpngate_admission_policy(&admin, all_sources_policy.clone())
        .await
        .unwrap();
    let reevaluated = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reevaluated.pools[0].candidates.len(), 1);
    assert_eq!(reevaluated.pools[0].candidates[0].server_id, "vpn-jp");

    all_sources_policy.risk_decision_policy = VpngateRiskDecisionPolicy::AnyAvailablePass;
    db.store
        .update_vpngate_admission_policy(&admin, all_sources_policy)
        .await
        .unwrap();
    let any_source = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(any_source.pools[0].candidates.len(), 2);
    db.store
        .update_vpngate_admission_policy(&admin, VpngateAdmissionPolicy::default())
        .await
        .unwrap();

    // A fresh success remains eligible for five hours. A later failure starts a separate
    // twenty-minute review window: one or two failures keep it eligible, while the third failure
    // suspends it immediately and any later success restores qualification.
    for failure_count in 1..=3 {
        db.store
            .record_vpngate_probe_report(
                "edge",
                VpngateProbeReport {
                    catalog_generation: ranked_desired.catalog_generation,
                    country_code: "JP".to_owned(),
                    samples: vec![VpngateProbeSample {
                        server_id: risky_candidate.server_id.clone(),
                        profile_sha256: risky_candidate.profile_sha256.clone(),
                        status: VpngateProbeStatus::Failed,
                        exit_ip: None,
                        exit_country_code: None,
                        connect_ms: None,
                        download_bps: None,
                        ip_scores: Vec::new(),
                        ip_networks: Vec::new(),
                        error_code: Some("catalogue-probe-failed".to_owned()),
                        error_detail: Some("priority review failed".to_owned()),
                        probed_at_unix_secs: 1_800_000_010 + i64::from(failure_count),
                    }],
                },
            )
            .await
            .unwrap();
        let view = db
            .store
            .vpngate_country_servers("JP")
            .await
            .unwrap()
            .into_iter()
            .find(|server| server.id == risky_candidate.server_id)
            .unwrap();
        assert_eq!(view.consecutive_probe_failures, failure_count);
        assert_eq!(view.active, failure_count < 3);
        assert!(view.probe_eligible_until_unix_secs.is_some());
        let state_filter = if failure_count < 3 {
            VpngateDirectoryFilter::Reviewing
        } else {
            VpngateDirectoryFilter::Suspended
        };
        let filtered = db
            .store
            .vpngate_country_server_page(
                "JP",
                VpngateServerPageRequest {
                    filter: state_filter,
                    ..VpngateServerPageRequest::default()
                },
            )
            .await
            .unwrap();
        assert!(filtered
            .items
            .iter()
            .any(|server| server.id == risky_candidate.server_id));
        if failure_count < 3 {
            let review = db
                .store
                .vpngate_agent_desired("edge")
                .await
                .unwrap()
                .unwrap();
            let first_jp = review
                .probe_assignments
                .iter()
                .find(|assignment| assignment.country_code == "JP")
                .and_then(|assignment| assignment.candidates.first())
                .expect("a failed candidate is immediately assigned for priority review");
            assert_eq!(first_jp.server_id, risky_candidate.server_id);
        }
        if failure_count == 2 {
            sqlx::query(
                "UPDATE vpngate_candidate_probe_state
                    SET failure_streak_started_at = now() - interval '21 minutes'
                  WHERE server_id = $1 AND profile_sha256 = $2",
            )
            .bind(&risky_candidate.server_id)
            .bind(&risky_candidate.profile_sha256)
            .execute(db.pool())
            .await
            .unwrap();
            assert!(
                !db.store
                    .vpngate_country_servers("JP")
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|server| server.id == risky_candidate.server_id)
                    .unwrap()
                    .active
            );
            let reviewing = db
                .store
                .vpngate_country_server_page(
                    "JP",
                    VpngateServerPageRequest {
                        filter: VpngateDirectoryFilter::Reviewing,
                        ..VpngateServerPageRequest::default()
                    },
                )
                .await
                .unwrap();
            assert!(!reviewing
                .items
                .iter()
                .any(|server| server.id == risky_candidate.server_id));
            let suspended = db
                .store
                .vpngate_country_server_page(
                    "JP",
                    VpngateServerPageRequest {
                        filter: VpngateDirectoryFilter::Suspended,
                        ..VpngateServerPageRequest::default()
                    },
                )
                .await
                .unwrap();
            assert!(suspended
                .items
                .iter()
                .any(|server| server.id == risky_candidate.server_id));
            sqlx::query(
                "UPDATE vpngate_candidate_probe_state
                    SET failure_streak_started_at = last_outcome_received_at
                  WHERE server_id = $1 AND profile_sha256 = $2",
            )
            .bind(&risky_candidate.server_id)
            .bind(&risky_candidate.profile_sha256)
            .execute(db.pool())
            .await
            .unwrap();
        }
    }
    db.store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: ranked_desired.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: risky_candidate.server_id.clone(),
                    profile_sha256: risky_candidate.profile_sha256.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.21".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(300),
                    download_bps: Some(100_000_000),
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: 1_800_000_020,
                }],
            },
        )
        .await
        .unwrap();
    let recovered = db
        .store
        .vpngate_country_servers("JP")
        .await
        .unwrap()
        .into_iter()
        .find(|server| server.id == risky_candidate.server_id)
        .unwrap();
    assert_eq!(recovered.consecutive_probe_failures, 0);
    assert!(recovered.active);
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT failure_streak_started_at IS NULL
           FROM vpngate_candidate_probe_state
          WHERE server_id = $1 AND profile_sha256 = $2",
    )
    .bind(&risky_candidate.server_id)
    .bind(&risky_candidate.profile_sha256)
    .fetch_one(db.pool())
    .await
    .unwrap());

    // Explicit pools neither inherit the country shortlist nor replace a missing/wrong-country
    // selected ID with a healthy unselected relay. The existing Agent candidate protocol suffices.
    let mut manual = pinned.clone();
    for ids in [
        vec!["vpn-jp-risky", "vpn-jp"],
        vec!["missing", "vpn-kr"],
        vec!["vpn-jp-risky"],
    ] {
        if let ExternalOutboundProtocol::Vpngate {
            server_ids,
            max_candidates,
            ..
        } = &mut manual.external_outbounds[0].protocol
        {
            *server_ids = ids.iter().map(|id| (*id).to_owned()).collect();
            *max_candidates = u8::try_from(ids.len()).unwrap();
        }
        sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
            .bind(i64::try_from(manual.revision).unwrap())
            .bind(serde_json::to_value(&manual).unwrap())
            .execute(db.pool())
            .await
            .unwrap();
        let state = db
            .store
            .vpngate_agent_desired("edge")
            .await
            .unwrap()
            .unwrap();
        let actual: Vec<_> = state.pools[0]
            .candidates
            .iter()
            .map(|candidate| candidate.server_id.as_str())
            .collect();
        let mut expected: Vec<_> = ids
            .into_iter()
            .filter(|id| id.starts_with("vpn-jp"))
            .collect();
        expected.sort();
        assert_eq!(actual, expected);
    }
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(pinned.revision).unwrap())
        .bind(serde_json::to_value(&pinned).unwrap())
        .execute(db.pool())
        .await
        .unwrap();

    let selected_candidate = &ranked_desired.pools[0].candidates[0];
    db.store
        .record_vpngate_agent_report(
            "edge",
            VpngatePoolReport {
                topology_revision: ranked_desired.topology_revision,
                catalog_generation: ranked_desired.catalog_generation,
                outbound_id: "vpngate-1111-1111".to_owned(),
                runtime_status: "running".to_owned(),
                selected_server_id: Some(selected_candidate.server_id.clone()),
                applied_profile_sha256: Some(selected_candidate.profile_sha256.clone()),
                manual_switch_result: None,
                samples: vec![
                    VpngateProbeSample {
                        server_id: safe_candidate.server_id.clone(),
                        profile_sha256: safe_candidate.profile_sha256.clone(),
                        status: VpngateProbeStatus::Succeeded,
                        exit_ip: Some("198.51.100.20".to_owned()),
                        // Node-supplied classification is untrusted; the store must replace it
                        // with the globally cached Agent facts for this exact exit IP.
                        exit_country_code: Some("US".to_owned()),
                        connect_ms: Some(900),
                        download_bps: Some(40_000_000),
                        ip_scores: scores(99),
                        ip_networks: networks(),
                        error_code: None,
                        error_detail: None,
                        probed_at_unix_secs: 1_800_000_100,
                    },
                    VpngateProbeSample {
                        server_id: risky_candidate.server_id.clone(),
                        profile_sha256: risky_candidate.profile_sha256.clone(),
                        status: VpngateProbeStatus::Succeeded,
                        exit_ip: Some("198.51.100.21".to_owned()),
                        exit_country_code: Some("US".to_owned()),
                        connect_ms: Some(250),
                        download_bps: Some(120_000_000),
                        ip_scores: scores(99),
                        ip_networks: networks(),
                        error_code: None,
                        error_detail: None,
                        probed_at_unix_secs: 1_800_000_101,
                    },
                ],
            },
        )
        .await
        .unwrap();
    let measured = db.store.vpngate_country_servers("JP").await.unwrap();
    // The directory uses the same order as the automatic pool. A faster latest sample must not
    // jump ahead of the lower-risk candidate selected by the operational ranking.
    assert_eq!(measured[0].id, "vpn-jp");
    assert_eq!(measured[0].candidate_rank, Some(1));
    assert_eq!(measured[0].pareto_layer, Some(1));
    assert_eq!(measured[0].global_connect_ms, Some(820));
    assert_eq!(measured[0].global_download_bps, Some(42_000_000));
    assert_eq!(measured[0].successful_samples, 1);
    assert_eq!(measured[1].candidate_rank, Some(2));
    assert_eq!(measured[1].pareto_layer, Some(2));
    assert_eq!(measured[0].latest_connect_ms, Some(820));
    assert_eq!(measured[0].latest_download_bps, Some(42_000_000));
    assert_eq!(measured[0].latest_ip_scores, scores(8));
    assert_eq!(measured[0].latest_ip_networks, networks());
    assert_eq!(
        measured[0].latest_probe_status.as_deref(),
        Some("succeeded")
    );
    let runtimes = db.store.vpngate_runtime_views(&admin).await.unwrap();
    assert_eq!(runtimes[0].selected_server_id.as_deref(), Some("vpn-jp"));
    assert_eq!(runtimes[0].latest_connect_ms, Some(900));
    assert_eq!(runtimes[0].latest_download_bps, Some(40_000_000));
    assert_eq!(runtimes[0].latest_ip_scores, scores(8));
    assert_eq!(runtimes[0].latest_ip_networks, networks());

    // Older Agents used zero as a sentinel when periodically rechecking an already established
    // backend. Keep the successful route observation, but persist the backend's last real dial
    // measurement so the API never reports an impossible 0 ms connection.
    db.store
        .record_vpngate_agent_report(
            "edge",
            VpngatePoolReport {
                topology_revision: ranked_desired.topology_revision,
                catalog_generation: ranked_desired.catalog_generation,
                outbound_id: "vpngate-1111-1111".to_owned(),
                runtime_status: "running".to_owned(),
                selected_server_id: Some(safe_candidate.server_id.clone()),
                applied_profile_sha256: Some(safe_candidate.profile_sha256.clone()),
                manual_switch_result: None,
                samples: vec![VpngateProbeSample {
                    server_id: safe_candidate.server_id.clone(),
                    profile_sha256: safe_candidate.profile_sha256.clone(),
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.20".to_owned()),
                    exit_country_code: Some("JP".to_owned()),
                    connect_ms: Some(0),
                    download_bps: Some(45_000_000),
                    ip_scores: scores(99),
                    ip_networks: networks(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: 1_800_000_102,
                }],
            },
        )
        .await
        .unwrap();
    let stored_connect_ms = sqlx::query_scalar::<_, i32>(
        "SELECT connect_ms
           FROM vpngate_probe_samples
          WHERE node_id = 'edge' AND outbound_id = 'vpngate-1111-1111'
            AND server_id = 'vpn-jp'
          ORDER BY probed_at DESC, id DESC
          LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stored_connect_ms, 900);
    let refreshed_runtime = db.store.vpngate_runtime_views(&admin).await.unwrap();
    assert_eq!(refreshed_runtime[0].latest_connect_ms, Some(900));
    assert_eq!(refreshed_runtime[0].latest_download_bps, Some(45_000_000));

    // Automatic failover is visible on the live path before its durable summary. Read evidence
    // for the observed backend on this machine, never the old pointer or a catalogue sample.
    sqlx::query(
        "UPDATE vpngate_node_pool_state SET selected_server_id = $1 WHERE node_id = 'edge'",
    )
    .bind(&risky_candidate.server_id)
    .execute(db.pool())
    .await
    .unwrap();
    let mut observed = VpngateRuntimeSelection {
        node_id: "edge".to_owned(),
        outbound_id: "vpngate-1111-1111".to_owned(),
        selected_server_id: Some("vpn-jp".to_owned()),
    };
    let live_runtime = db
        .store
        .vpngate_runtime_views_observed(&admin, std::slice::from_ref(&observed))
        .await
        .unwrap();
    assert_eq!(
        live_runtime[0].selected_server_id.as_deref(),
        Some("vpn-jp")
    );
    assert_eq!(
        live_runtime[0].latest_exit_ip.as_deref(),
        Some("198.51.100.20")
    );
    assert_eq!(live_runtime[0].latest_download_bps, Some(45_000_000));
    let stale_switch = db
        .store
        .request_vpngate_pool_switch(
            &admin,
            "edge",
            "vpngate-1111-1111",
            RequestVpngatePoolSwitch {
                expected_server_id: risky_candidate.server_id.clone(),
            },
            Some(&observed),
        )
        .await;
    assert!(matches!(
        stale_switch,
        Err(brocade_store::StoreError::Conflict(_))
    ));
    observed.selected_server_id = None;
    let empty_runtime = db
        .store
        .vpngate_runtime_views_observed(&admin, std::slice::from_ref(&observed))
        .await
        .unwrap();
    assert!(empty_runtime[0].selected_server_id.is_none());
    assert!(empty_runtime[0].latest_exit_ip.is_none());
    let no_active = db
        .store
        .request_vpngate_pool_switch(
            &admin,
            "edge",
            "vpngate-1111-1111",
            RequestVpngatePoolSwitch {
                expected_server_id: "vpn-jp".to_owned(),
            },
            Some(&observed),
        )
        .await;
    assert!(matches!(
        no_active,
        Err(brocade_store::StoreError::Conflict(_))
    ));
    observed.selected_server_id = Some("vpn-jp".to_owned());

    // An unchanged reconcile report is intentionally de-duplicated by the Agent, so the pool
    // timestamp is state evidence rather than a heartbeat. A compatible protocol and fresh Agent
    // runtime report are what make it safe to enqueue an operational command.
    sqlx::query(
        "UPDATE vpngate_node_pool_state
            SET reported_at = now() - interval '1 hour'
          WHERE node_id = 'edge' AND outbound_id = 'vpngate-1111-1111'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let switch_request = db
        .store
        .request_vpngate_pool_switch(
            &admin,
            "edge",
            "vpngate-1111-1111",
            RequestVpngatePoolSwitch {
                expected_server_id: "vpn-jp".to_owned(),
            },
            Some(&observed),
        )
        .await
        .unwrap();
    assert_eq!(switch_request.status, "pending");
    let switch_desired = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap();
    let switch_command = switch_desired.pools[0]
        .manual_switch
        .as_ref()
        .expect("pending request is delivered through operational desired state");
    assert_eq!(switch_command.request_id, switch_request.request_id);
    assert_eq!(switch_command.previous_server_id, "vpn-jp");
    assert_eq!(switch_command.cooldown_secs, 600);
    let replacement = switch_desired.pools[0]
        .candidates
        .iter()
        .find(|candidate| candidate.server_id != "vpn-jp")
        .expect("automatic pool retains a replacement");
    let cooldown_until = 1_900_000_000;
    let applied_report = VpngatePoolReport {
        topology_revision: switch_desired.topology_revision,
        catalog_generation: switch_desired.catalog_generation,
        outbound_id: "vpngate-1111-1111".to_owned(),
        runtime_status: "degraded".to_owned(),
        selected_server_id: Some(replacement.server_id.clone()),
        applied_profile_sha256: Some(replacement.profile_sha256.clone()),
        manual_switch_result: Some(VpngateManualSwitchResult {
            request_id: switch_request.request_id,
            status: VpngateManualSwitchStatus::Applied,
            previous_server_id: "vpn-jp".to_owned(),
            selected_server_id: Some(replacement.server_id.clone()),
            cooldown_until_unix_secs: Some(cooldown_until),
            error_detail: None,
        }),
        samples: Vec::new(),
    };
    // A pending request cannot be acknowledged by a snapshot whose active backend already
    // differs from the claimed replacement. This keeps the first transition auditable even
    // though terminal results may be replayed after later automatic failover.
    let mut inconsistent_initial_report = applied_report.clone();
    inconsistent_initial_report.selected_server_id = Some(safe_candidate.server_id.clone());
    inconsistent_initial_report.applied_profile_sha256 =
        Some(safe_candidate.profile_sha256.clone());
    assert!(matches!(
        db.store
            .record_vpngate_agent_report("edge", inconsistent_initial_report)
            .await,
        Err(brocade_store::StoreError::InvalidData(_))
    ));
    db.store
        .record_vpngate_agent_report("edge", applied_report.clone())
        .await
        .unwrap();
    // Complete reports repeat their most recent terminal result; accepting it again keeps an
    // Agent reconnect idempotent instead of manufacturing another switch.
    db.store
        .record_vpngate_agent_report("edge", applied_report.clone())
        .await
        .unwrap();
    // The Agent keeps its last terminal result in complete snapshots. A later automatic
    // failover changes the live selection, not that historical result, so the replay must remain
    // valid once the request is already terminal.
    let mut after_automatic_failover = applied_report;
    after_automatic_failover.selected_server_id = Some(safe_candidate.server_id.clone());
    after_automatic_failover.applied_profile_sha256 = Some(safe_candidate.profile_sha256.clone());
    db.store
        .record_vpngate_agent_report("edge", after_automatic_failover.clone())
        .await
        .unwrap();
    let switched = db.store.vpngate_runtime_views(&admin).await.unwrap();
    assert_eq!(switched[0].switch_status.as_deref(), Some("applied"));
    assert_eq!(
        switched[0].switch_selected_server_id.as_deref(),
        Some(replacement.server_id.as_str())
    );
    assert_eq!(
        switched[0].switch_cooldown_until_unix_secs,
        Some(cooldown_until)
    );
    assert!(db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap()
        .pools[0]
        .manual_switch
        .is_none());

    // Deleting and recreating a logical pool cascades its switch audit row, but an Agent that
    // stayed online still repeats its last durable terminal acknowledgement. That orphaned audit
    // attachment has no database row left to mutate and must not suppress otherwise valid current
    // runtime state.
    sqlx::query("DELETE FROM vpngate_pool_switch_requests WHERE id = $1")
        .bind(i64::try_from(switch_request.request_id).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let orphaned_switch_receipt = db
        .store
        .record_vpngate_agent_report("edge", after_automatic_failover)
        .await
        .unwrap();
    assert!(orphaned_switch_receipt.current_state_updated);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT runtime_status FROM vpngate_node_pool_state
              WHERE node_id = 'edge' AND outbound_id = 'vpngate-1111-1111'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        "degraded"
    );

    let measured_overview = db.store.vpngate_overview().await.unwrap();
    assert_eq!(measured_overview.countries[0].measured_successful, 2);
    assert_eq!(measured_overview.countries[0].candidate_servers, 2);

    // Twenty minutes is only the failure-review window; a healthy success remains eligible.
    sqlx::query(
        "UPDATE vpngate_candidate_probe_latest
            SET last_success_received_at = now() - interval '21 minutes'
          WHERE server_id = 'vpn-jp-risky'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let still_fresh_overview = db.store.vpngate_overview().await.unwrap();
    assert_eq!(still_fresh_overview.countries[0].measured_successful, 2);
    assert_eq!(still_fresh_overview.countries[0].candidate_servers, 2);
    assert!(
        db.store
            .vpngate_country_servers("JP")
            .await
            .unwrap()
            .into_iter()
            .find(|server| server.id == "vpn-jp-risky")
            .unwrap()
            .active
    );

    // The five-hour floor must not create a serving gap before the configured six-hour
    // performance interval and thirty-minute connectivity interval have both elapsed.
    sqlx::query(
        "UPDATE vpngate_candidate_probe_latest
            SET last_success_received_at = now() - interval '5 hours 1 minute'
          WHERE server_id = 'vpn-jp-risky'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let scheduled_fresh_overview = db.store.vpngate_overview().await.unwrap();
    assert_eq!(scheduled_fresh_overview.countries[0].measured_successful, 2);
    assert_eq!(scheduled_fresh_overview.countries[0].candidate_servers, 2);
    assert!(
        db.store
            .vpngate_country_servers("JP")
            .await
            .unwrap()
            .into_iter()
            .find(|server| server.id == "vpn-jp-risky")
            .unwrap()
            .active
    );

    // A historical success remains visible evidence, but it cannot keep an automatic pool
    // candidate alive after the complete configured scheduling window.
    sqlx::query(
        "UPDATE vpngate_candidate_probe_latest
            SET last_success_received_at = now() - interval '6 hours 31 minutes'
          WHERE server_id = 'vpn-jp-risky'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let stale_overview = db.store.vpngate_overview().await.unwrap();
    assert_eq!(stale_overview.countries[0].measured_successful, 2);
    assert_eq!(stale_overview.countries[0].candidate_servers, 1);
    assert!(
        !db.store
            .vpngate_country_servers("JP")
            .await
            .unwrap()
            .into_iter()
            .find(|server| server.id == "vpn-jp-risky")
            .unwrap()
            .active
    );
    sqlx::query(
        "UPDATE vpngate_candidate_probe_latest
            SET last_success_received_at = now()
          WHERE server_id = 'vpn-jp-risky'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        db.store.vpngate_overview().await.unwrap().countries[0].candidate_servers,
        2
    );

    // Catalogue probing is fleet observation, not model intent. Remove the KR outbound before
    // replacing the current provider projection: the selected Agent must still receive the
    // retained KR server even though KR is now both unconfigured and absent from the feed.
    pinned.external_outbounds.truncate(1);
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(pinned.revision).unwrap())
        .bind(serde_json::to_value(&pinned).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    db.store.request_vpngate_sync(&admin).await.unwrap();
    let second = db
        .store
        .claim_vpngate_sync("test-worker", "manual")
        .await
        .unwrap()
        .expect("manual request is immediately due");
    db.store
        .complete_vpngate_sync(
            "test-worker",
            &second,
            VpngateSyncBatch {
                content_sha256: "b".repeat(64),
                fetched_rows: 1,
                rejected_rows: 0,
                // The retained server gets a new current profile. Serving initialization must
                // measure it before resuming the all-history loop.
                servers: vec![server("vpn-jp", "JP", "192.0.2.12", 110)],
            },
        )
        .await
        .unwrap();
    let second_view = db.store.vpngate_overview().await.unwrap();
    assert_eq!(second_view.status.current_servers, 1);
    assert_eq!(second_view.status.retained_servers, 3);
    assert_eq!(second_view.status.retained_observations, 4);
    assert_eq!(
        second_view
            .sync_history
            .iter()
            .map(|point| point.accepted_rows)
            .collect::<Vec<_>>(),
        vec![4, 1]
    );
    assert_eq!(
        second_view
            .sync_history
            .iter()
            .map(|point| point.current_servers)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );

    // A successful fleet-wide catalogue sync can finish after this Agent fetched its desired
    // state but before its reconcile report arrives. The older candidate generation still
    // describes the runtime the Agent actually applied, and pool identity is governed by the
    // unchanged topology. Persist it instead of leaving the carrier invisible indefinitely.
    let stale_catalog_receipt = db
        .store
        .record_vpngate_agent_report(
            "edge",
            VpngatePoolReport {
                topology_revision: ranked_desired.topology_revision,
                catalog_generation: ranked_desired.catalog_generation,
                outbound_id: "vpngate-1111-1111".to_owned(),
                runtime_status: "degraded".to_owned(),
                selected_server_id: Some(selected_candidate.server_id.clone()),
                applied_profile_sha256: Some(selected_candidate.profile_sha256.clone()),
                manual_switch_result: None,
                samples: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(stale_catalog_receipt.current_state_updated);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT runtime_status FROM vpngate_node_pool_state
              WHERE node_id = 'edge' AND outbound_id = 'vpngate-1111-1111'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        "degraded"
    );
    assert_eq!(
        second_view
            .sync_history
            .iter()
            .map(|point| point.first_seen_servers)
            .collect::<Vec<_>>(),
        vec![3, 0]
    );
    assert_eq!(second_view.countries.len(), 2);
    let japan = second_view
        .countries
        .iter()
        .find(|country| country.country_code == "JP")
        .unwrap();
    assert_eq!(japan.current_servers, 1);
    assert_eq!(japan.retained_servers, 2);
    let korea = second_view
        .countries
        .iter()
        .find(|country| country.country_code == "KR")
        .unwrap();
    assert_eq!(korea.current_servers, 0);
    assert_eq!(korea.retained_servers, 1);
    let retained_korea = db.store.vpngate_country_servers("KR").await.unwrap();
    assert_eq!(retained_korea.len(), 1);
    assert!(!retained_korea[0].seen_in_latest_sync);
    let retained_japan = db.store.vpngate_country_servers("JP").await.unwrap();
    assert_eq!(retained_japan.len(), 2);
    assert!(retained_japan
        .iter()
        .any(|server| server.id == "vpn-jp-risky" && !server.seen_in_latest_sync));
    let serving_priority = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("an unmeasured current profile is initialized first");
    let current_jp = serving_priority
        .probe_assignments
        .iter()
        .find(|assignment| assignment.country_code == "JP")
        .and_then(|assignment| {
            assignment
                .candidates
                .iter()
                .find(|candidate| candidate.server_id == "vpn-jp")
        })
        .expect("the unmeasured current JP profile has serving priority")
        .clone();
    let probed_now = sqlx::query_scalar::<_, i64>("SELECT EXTRACT(EPOCH FROM now())::BIGINT")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: serving_priority.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: current_jp.server_id,
                    profile_sha256: current_jp.profile_sha256,
                    status: VpngateProbeStatus::Succeeded,
                    exit_ip: Some("198.51.100.22".to_owned()),
                    exit_country_code: None,
                    connect_ms: Some(750),
                    download_bps: Some(45_000_000),
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: None,
                    error_detail: None,
                    probed_at_unix_secs: probed_now,
                }],
            },
        )
        .await
        .unwrap();
    let failed_reputation = db
        .store
        .claim_vpngate_exit_intelligence("edge")
        .await
        .unwrap()
        .expect("the new current exit is immediately due for distributed intelligence");
    assert_eq!(failed_reputation.exit_ip, "198.51.100.22");
    db.store
        .record_vpngate_ip_intelligence_report(
            "edge",
            &partial_intelligence_report(
                &failed_reputation.exit_ip,
                failed_reputation.lease_generation,
            ),
        )
        .await
        .unwrap();
    assert!(!sqlx::query_scalar::<_, bool>(
        "SELECT next_check_at = 'infinity'::timestamptz
           FROM vpngate_exit_reputations
          WHERE exit_ip = '198.51.100.22'::inet",
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '31 minutes'
          WHERE server_id = 'vpn-jp-risky'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let historical_desired = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("the selected Agent keeps receiving retained catalogue work");
    assert_eq!(historical_desired.pools.len(), 1);
    assert_eq!(historical_desired.pools[0].candidates.len(), 2);
    assert!(historical_desired.pools[0]
        .candidates
        .iter()
        .any(|candidate| candidate.server_id == "vpn-jp-risky"));
    let current_candidate = historical_desired.pools[0]
        .candidates
        .iter()
        .find(|candidate| candidate.server_id == "vpn-jp")
        .expect("one successful provider is enough to admit current evidence");
    assert_eq!(current_candidate.verified_ip_scores.len(), 1);
    let historical_candidate = historical_desired.pools[0]
        .candidates
        .iter()
        .find(|candidate| candidate.server_id == "vpn-jp-risky")
        .unwrap();
    let historical_receipt = db
        .store
        .record_vpngate_reconcile_report(
            "edge",
            VpngateReconcileReport {
                topology_revision: historical_desired.topology_revision,
                catalog_generation: historical_desired.catalog_generation,
                pools: vec![VpngatePoolReport {
                    topology_revision: historical_desired.topology_revision,
                    catalog_generation: historical_desired.catalog_generation,
                    outbound_id: "vpngate-1111-1111".to_owned(),
                    runtime_status: "running".to_owned(),
                    selected_server_id: Some(historical_candidate.server_id.clone()),
                    applied_profile_sha256: Some(historical_candidate.profile_sha256.clone()),
                    manual_switch_result: None,
                    samples: Vec::new(),
                }],
            },
        )
        .await
        .unwrap();
    assert!(historical_receipt.current_state_updated);
    let historical_kr = historical_desired
        .probe_assignments
        .iter()
        .find(|assignment| assignment.country_code == "KR")
        .and_then(|assignment| {
            assignment
                .candidates
                .iter()
                .find(|candidate| candidate.server_id == "vpn-kr")
        })
        .expect("the retained KR profile stays in the global probe queue")
        .clone();
    db.store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: historical_desired.catalog_generation,
                country_code: "KR".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: historical_kr.server_id,
                    profile_sha256: historical_kr.profile_sha256,
                    status: VpngateProbeStatus::Failed,
                    exit_ip: None,
                    exit_country_code: None,
                    connect_ms: None,
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: Some("catalogue-probe-failed".to_owned()),
                    error_detail: Some("test failure".to_owned()),
                    // Queue fairness uses Console receipt order, so a skewed Agent clock cannot
                    // pin this country at the front or back of the cycle.
                    probed_at_unix_secs: 1_700_000_000,
                }],
            },
        )
        .await
        .unwrap();
    let continuing = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("a completed batch immediately advances the retained queue");
    let continuing_jp = continuing
        .probe_assignments
        .iter()
        .find(|assignment| assignment.country_code == "JP")
        .expect("the retained JP profile returns after its successful cooldown");
    assert!(continuing_jp
        .candidates
        .iter()
        .any(|candidate| candidate.server_id == "vpn-jp-risky"));

    // Increasing this selected machine to three workers grows only its next batch. One bounded
    // desired response may span countries so small countries cannot leave most workers idle.
    db.store
        .update_vpngate_probe_node(
            &admin,
            "edge",
            UpdateVpngateProbeNode {
                enabled: true,
                workers: Some(3),
            },
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node_agent_state
            SET runtime_versions = runtime_versions ||
                '{\"vpngate_catalog_probe_workers\":16}'::jsonb
          WHERE node_id = 'edge'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE vpngate_candidate_probe_state
            SET last_outcome_received_at = now() - interval '31 minutes'
          WHERE last_outcome_status = 'succeeded'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let parallel = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .expect("parallel probe capability enables a cross-country batch");
    assert_eq!(parallel.probe_assignments.len(), 2);
    let parallel_ids = parallel
        .probe_assignments
        .iter()
        .flat_map(|assignment| &assignment.candidates)
        .map(|candidate| candidate.server_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(parallel_ids, ["vpn-jp", "vpn-jp-risky", "vpn-kr"].into());

    db.store.request_vpngate_sync(&admin).await.unwrap();
    let failed = db
        .store
        .claim_vpngate_sync("test-worker", "manual")
        .await
        .unwrap()
        .expect("third request is due");
    db.store
        .fail_vpngate_sync("test-worker", &failed, "fetch-failed", "upstream timeout")
        .await
        .unwrap();
    let after_failure = db.store.vpngate_overview().await.unwrap();
    assert_eq!(after_failure.status.current_servers, 1);
    assert_eq!(
        after_failure.status.last_error_code.as_deref(),
        Some("fetch-failed")
    );

    // A complete report may delete rows only when its identity set is exactly the current
    // desired set. An empty report received while one pool is still desired is harmless.
    let incomplete = db
        .store
        .record_vpngate_reconcile_report(
            "edge",
            VpngateReconcileReport {
                topology_revision: historical_desired.topology_revision,
                catalog_generation: historical_desired.catalog_generation,
                pools: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(!incomplete.current_state_updated);
    assert_eq!(
        db.store.vpngate_runtime_views(&admin).await.unwrap().len(),
        1
    );

    // Once the authoritative desired topology contains no pool, the Agent's successfully
    // reconciled empty set removes replaceable runtime pointers but keeps immutable probe history.
    let mut without_runtime_pool = pinned.clone();
    for app in &mut without_runtime_pool.apps {
        for step in app.steps.iter_mut().filter(|step| step.node == "edge") {
            for rule in &mut step.rules {
                if matches!(
                    &rule.action,
                    Action::Proxy { outbound } if outbound == "vpngate-1111-1111"
                ) {
                    rule.action = Action::Egress { send_through: None };
                }
            }
        }
    }
    sqlx::query("UPDATE model_snapshots SET snapshot = $2 WHERE revision_id = $1")
        .bind(i64::try_from(without_runtime_pool.revision).unwrap())
        .bind(serde_json::to_value(&without_runtime_pool).unwrap())
        .execute(db.pool())
        .await
        .unwrap();
    let empty_desired = db
        .store
        .vpngate_agent_desired("edge")
        .await
        .unwrap()
        .unwrap();
    assert!(empty_desired.pools.is_empty());
    let cleared = db
        .store
        .record_vpngate_reconcile_report(
            "edge",
            VpngateReconcileReport {
                topology_revision: empty_desired.topology_revision,
                catalog_generation: empty_desired.catalog_generation,
                pools: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(cleared.current_state_updated);
    assert!(db
        .store
        .vpngate_runtime_views(&admin)
        .await
        .unwrap()
        .is_empty());
    assert!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM vpngate_probe_samples WHERE node_id = 'edge'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap()
            > 0
    );

    let latest_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM vpngate_candidate_probe_latest")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(latest_before > 0);
    sqlx::query(
        "UPDATE vpngate_candidate_probe_samples
            SET received_at = now() - interval '8 days'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE vpngate_server_observations
            SET observed_at = now() - interval '3 days'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let removed = db.store.prune_vpngate_history(7, 2).await.unwrap();
    assert!(removed > 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM vpngate_candidate_probe_samples")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM vpngate_server_observations")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM vpngate_candidate_probe_latest")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        latest_before,
        "history retention must not remove the serving projection"
    );
    let directory_after_prune = db.store.vpngate_country_servers("JP").await.unwrap();
    let retained_measurement = directory_after_prune
        .iter()
        .find(|server| server.id == "vpn-jp")
        .expect("the retained directory still contains vpn-jp");
    assert!(retained_measurement.measured_nodes > 0);
    assert!(retained_measurement.latest_probe_status.is_some());
    assert!(retained_measurement.latest_probed_at_unix_secs.is_some());
    assert_eq!(
        db.store
            .vpngate_overview()
            .await
            .unwrap()
            .status
            .retained_observations,
        0
    );

    let retained_profile = sqlx::query_scalar::<_, String>(
        "SELECT profile_sha256 FROM vpngate_servers WHERE id = 'vpn-jp'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let after_prune = sqlx::query_scalar::<_, i64>("SELECT EXTRACT(EPOCH FROM now())::BIGINT")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let receipt = db
        .store
        .record_vpngate_probe_report(
            "edge",
            VpngateProbeReport {
                catalog_generation: historical_desired.catalog_generation,
                country_code: "JP".to_owned(),
                samples: vec![VpngateProbeSample {
                    server_id: "vpn-jp".to_owned(),
                    profile_sha256: retained_profile,
                    status: VpngateProbeStatus::Failed,
                    exit_ip: None,
                    exit_country_code: None,
                    connect_ms: None,
                    download_bps: None,
                    ip_scores: Vec::new(),
                    ip_networks: Vec::new(),
                    error_code: Some("catalogue-probe-failed".to_owned()),
                    error_detail: Some("retained directory identity".to_owned()),
                    probed_at_unix_secs: after_prune,
                }],
            },
        )
        .await
        .expect("retained directory identity stays reportable after observation retention");
    assert_eq!(receipt.accepted_samples, 1);
}
