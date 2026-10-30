use super::*;
use brocade_deployment::unlock_probe::{
    UnlockCode, UnlockProbeReport, UnlockResult, UnlockStatus, LEGACY_UNLOCK_SERVICES,
    UNLOCK_SERVICES,
};
use brocade_store::{UnlockProbeState, UnlockProbeTrigger};
fn report(id: &str) -> UnlockProbeReport {
    UnlockProbeReport {
        family: None,
        probe_id: id.into(),
        path_error: None,
        results: LEGACY_UNLOCK_SERVICES
            .into_iter()
            .map(|service| UnlockResult {
                service,
                status: UnlockStatus::Available,
                code: UnlockCode::WebAvailable,
                client_dns: false,
                youtube: None,
                dns_path: None,
                connection_observations: Vec::new(),
                connection_requests: Default::default(),
                dns_parts: Vec::new(),
                region: Some(
                    if service == brocade_deployment::unlock_probe::UnlockService::Netflix {
                        "JP"
                    } else if service == brocade_deployment::unlock_probe::UnlockService::Reddit {
                        "HKG"
                    } else {
                        "TW"
                    }
                    .into(),
                ),
            })
            .collect(),
    }
}
async fn setup(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(47))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_dns_provenance_is_reconciled_persisted_and_idempotent() {
    use brocade_core::model::*;
    use brocade_deployment::unlock_probe::*;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let mut snapshot = db.store.materialize_snapshot(None).await.unwrap();
    for node in &mut snapshot.nodes {
        node.domain_strategy = DomainStrategy::UseIp;
    }
    let resolution = EgressDnsResolution {
        address: "198.51.100.53".into(),
        port: 53,
        transport: EgressDnsTransport::Tcp,
        address_strategy: EgressDnsAddressStrategy::UseIp,
        fallback: EgressDnsFallback::Machine,
    };
    snapshot.node_egress_dns = snapshot
        .nodes
        .iter()
        .map(|node| NodeEgressDnsPolicy {
            node: node.id.clone(),
            position: 0,
            selector: DestMatch::Geosite(vec!["category-ai-!cn".into()]),
            resolution: resolution.clone(),
        })
        .collect();
    let tag =
        brocade_core::physical::node::egress_dns_policy_tag(&snapshot.node_egress_dns[0]).unwrap();
    let compiled = brocade_core::compile::compile(&snapshot);
    let exit = compiled
        .project_probe("n1")
        .unwrap()
        .targets
        .into_iter()
        .find(|target| target.chain_id == "chn-a1b2-c3d4")
        .unwrap()
        .exit_nodes[0]
        .clone();
    let node_plan = compiled.project_node(&exit).unwrap();
    let config =
        brocade_core::format::json::xray(&brocade_core::artifacts::xray::build(&node_plan));
    let profile = UnlockProbeProfile::from_config(&serde_json::from_str(&config).unwrap()).unwrap();
    seed_subscription_serving_snapshot(&db, snapshot).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(57))
        .await
        .unwrap();
    let queued = db
        .store
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            true,
            Some(IpFamily::V6),
        )
        .await
        .unwrap()
        .unwrap();
    let before_capability = db
        .store
        .claim_unlock_probe("n1", 57)
        .await
        .unwrap()
        .unwrap();
    assert!(!before_capability.connection_trace);
    sqlx::query("INSERT INTO node_agent_state(node_id,last_poll_at,runtime_reported_at,agent_protocol_version,runtime_versions) SELECT id,now(),now(),57,'{\"unlock_connection_trace\":true}'::jsonb FROM nodes ON CONFLICT(node_id) DO UPDATE SET last_poll_at=now(),runtime_reported_at=now(),agent_protocol_version=57,runtime_versions=EXCLUDED.runtime_versions")
        .execute(db.pool()).await.unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 57)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, queued.id);
    assert!(target.connection_trace);
    assert!(
        !db.store
            .claim_unlock_probe("n1", 56)
            .await
            .unwrap()
            .unwrap()
            .connection_trace
    );
    let mut completed = report(&queued.id);
    completed.family = Some(IpFamily::V6);
    let gemini = completed
        .results
        .iter_mut()
        .find(|r| r.service == UnlockService::Gemini)
        .unwrap();
    gemini.connection_requests.insert(UnlockPart::Service, 1);
    gemini.connection_observations = vec![UnlockConnectionObservation {
        part: UnlockPart::Service,
        connection: UnlockConnectionTrace {
            version: 2,
            id: "00000000000000000000000000000001".into(),
            domain: "gemini.google.com".into(),
            exit,
            outbound: "out:forward:probe-v6".into(),
            profile: profile.fingerprint,
            normal_strategy: "UseIP".into(),
            family: Some(IpFamily::V6),
            connected: true,
            complete: true,
            error: None,
            lookups: vec![UnlockDnsTrace {
                version: 1,
                complete: true,
                domain: "gemini.google.com".into(),
                strategy: "FORCE_IP6".into(),
                steps: vec![
                    UnlockDnsStep {
                        tag,
                        scoped: true,
                        outcome: UnlockDnsOutcome::Empty,
                        other: true,
                        winner: None,
                        members: Vec::new(),
                    },
                    UnlockDnsStep {
                        tag: "xray.system.opaque".into(),
                        scoped: false,
                        outcome: UnlockDnsOutcome::Answer,
                        other: false,
                        winner: None,
                        members: Vec::new(),
                    },
                ],
            }],
        },
    }];
    completed
        .results
        .iter_mut()
        .find(|r| r.service == UnlockService::Chatgpt)
        .unwrap()
        .dns_path = Some(UnlockDnsPath {
        exit: "forged-exit".into(),
        normal_family: Some(IpFamily::V4),
        fallback: EgressDnsFallback::Machine,
        rules: vec![],
        answer_from: None,
        reason: None,
    });
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();
    db.store.report_unlock_probe("n1", completed).await.unwrap();
    let restarted = PgStore::from_pool(db.pool().clone());
    let saved = restarted
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            false,
            Some(IpFamily::V6),
        )
        .await
        .unwrap()
        .unwrap();
    let path = saved
        .results
        .iter()
        .find(|r| r.service == UnlockService::Gemini)
        .unwrap()
        .dns_path
        .as_ref()
        .unwrap();
    assert_ne!(path.exit, "forged-exit");
    assert_eq!(path.normal_family, Some(IpFamily::V4));
    assert_eq!(path.rules[0].resolution, resolution);
    assert!(saved
        .results
        .iter()
        .find(|r| r.service == UnlockService::Chatgpt)
        .unwrap()
        .dns_path
        .is_none());
    sqlx::query(
        "UPDATE unlock_probe_jobs SET created_at=now()-interval '61 seconds' WHERE job_id=$1",
    )
    .bind(&queued.id)
    .execute(db.pool())
    .await
    .unwrap();
    let next = restarted
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            true,
            Some(IpFamily::V6),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.previous_results, saved.results);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_extended_services_negotiate_by_agent_protocol_and_preserve_legacy_results() {
    use brocade_deployment::unlock_probe::IpFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(54))
        .await
        .unwrap();
    let view = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", true, Some(IpFamily::V4))
        .await
        .unwrap()
        .unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 54)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, view.id);
    assert_eq!(target.services(), UNLOCK_SERVICES);
    let mut expanded = report(&target.probe_id);
    expanded.family = Some(IpFamily::V4);
    for service in UNLOCK_SERVICES
        .into_iter()
        .skip(LEGACY_UNLOCK_SERVICES.len())
    {
        expanded.results.push(UnlockResult {
            service,
            status: UnlockStatus::Available,
            code: UnlockCode::WebAvailable,
            region: Some("GB".into()),
            client_dns: false,
            youtube: None,
            dns_path: None,
            connection_observations: Vec::new(),
            connection_requests: Default::default(),
            dns_parts: Vec::new(),
        });
    }
    db.store.report_unlock_probe("n1", expanded).await.unwrap();
    let done = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.results.len(), 12);
    assert_eq!(
        done.results
            .iter()
            .find(|r| r.service == brocade_deployment::unlock_probe::UnlockService::HboMax)
            .unwrap()
            .region
            .as_deref(),
        Some("GB")
    );
    for protocol in [53, 52] {
        // Keep this legacy service negotiation fixture focused on its explicit v4 job.
        sqlx::query("DELETE FROM unlock_probe_jobs WHERE address_family='v6'")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(
            "UPDATE unlock_probe_jobs SET created_at=now()-interval '2 minutes' WHERE node_id='n1'",
        )
        .execute(db.pool())
        .await
        .unwrap();
        db.store
            .record_node_poll_with_protocol("n1", None, Some(protocol))
            .await
            .unwrap();
        db.store
            .node_unlock_probe_on_family(&system_admin(), "n1", true, Some(IpFamily::V4))
            .await
            .unwrap();
        let old = db
            .store
            .claim_unlock_probe("n1", u32::try_from(protocol).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.services(), LEGACY_UNLOCK_SERVICES);
        assert_eq!(old.services.is_none(), protocol < 53);
        let mut legacy = report(&old.probe_id);
        legacy.family = Some(IpFamily::V4);
        db.store.report_unlock_probe("n1", legacy).await.unwrap();
        let done = db
            .store
            .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V4))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.results.len(), 9);
        assert_eq!(
            done.previous_results.len(),
            if protocol == 53 { 12 } else { 9 }
        );
    }
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_schedule_runs_without_browser_persists_and_manual_resets_one_day() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let target = db
        .store
        .claim_unlock_probe("n1", 46)
        .await
        .unwrap()
        .unwrap();
    assert!(target.chain.is_none());
    assert_eq!(
        db.store
            .claim_unlock_probe("n1", 46)
            .await
            .unwrap()
            .unwrap(),
        target
    );
    db.store
        .report_unlock_probe("n1", report(&target.probe_id))
        .await
        .unwrap();
    db.store
        .report_unlock_probe("n1", report(&target.probe_id))
        .await
        .unwrap();
    let restarted = PgStore::from_pool(db.pool().clone());
    let view = restarted
        .node_unlock_probe(&system_admin(), "n1", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.trigger, UnlockProbeTrigger::Scheduled);
    assert_eq!(view.interval_secs, 86_400);
    assert!((86_399..=86_401).contains(&(view.next_run_at - view.completed_at.unwrap())));
    assert_eq!(
        view.results
            .iter()
            .find(|r| r.service == brocade_deployment::unlock_probe::UnlockService::Netflix)
            .unwrap()
            .region
            .as_deref(),
        Some("JP")
    );
    assert_eq!(
        view.results
            .iter()
            .find(|r| r.service == brocade_deployment::unlock_probe::UnlockService::Reddit)
            .unwrap()
            .region
            .as_deref(),
        Some("HKG")
    );
    assert!(restarted
        .claim_unlock_probe("n1", 46)
        .await
        .unwrap()
        .is_none());
    assert!(restarted
        .node_unlock_probe(&system_admin(), "n1", true)
        .await
        .is_err());
    sqlx::query(
        "UPDATE unlock_probe_jobs SET created_at=now()-interval '2 minutes' WHERE node_id='n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let manual = restarted
        .node_unlock_probe(&system_admin(), "n1", true)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(manual.id, view.id);
    assert_eq!(manual.trigger, UnlockProbeTrigger::Manual);
    assert_eq!(manual.previous_results, view.results);
    let target = restarted
        .claim_unlock_probe("n1", 46)
        .await
        .unwrap()
        .unwrap();
    restarted
        .report_unlock_probe("n1", report(&target.probe_id))
        .await
        .unwrap();
    let done = restarted
        .node_unlock_probe(&system_admin(), "n1", false)
        .await
        .unwrap()
        .unwrap();
    assert!((86_399..=86_401).contains(&(done.next_run_at - done.completed_at.unwrap())));
    sqlx::query("UPDATE control_state SET unlock_detection_next_run_at=now()-interval '1 second' WHERE id=TRUE")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE unlock_probe_discovery SET next_discovery_at=now()-interval '1 second'")
        .execute(db.pool())
        .await
        .unwrap();
    let next = restarted
        .claim_unlock_probe("n1", 46)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.probe_id, target.probe_id);
    let observer = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(matches!(
        restarted.node_unlock_probe(&observer, "n1", true).await,
        Err(StoreError::Forbidden(_))
    ));
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_chain_uses_serving_is_protocol_gated_and_fences_late_results() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(46))
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
            .await,
        Err(StoreError::Unsupported(_))
    ));
    db.store
        .record_node_poll_with_protocol("n1", None, Some(47))
        .await
        .unwrap();
    let view = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.chain_id.as_deref(), Some("chn-a1b2-c3d4"));
    let target = db
        .store
        .claim_unlock_probe("n1", 47)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, view.id);
    let chain = target.chain.unwrap();
    assert_eq!(chain.chain_id, "chn-a1b2-c3d4");
    assert_eq!(chain.dial_host, "127.0.0.1");
    assert!(!chain.uuid.is_empty());
    let persisted: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(j) FROM unlock_probe_jobs j WHERE chain_id IS NOT NULL",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        !persisted.to_string().contains(&chain.uuid),
        "credentials stay out of the schedule row"
    );
    sqlx::query("UPDATE subscription_serving_state SET generation=generation+1 WHERE id=TRUE")
        .execute(db.pool())
        .await
        .unwrap();
    let still_running = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        still_running.state,
        UnlockProbeState::Running,
        "authorization-only generation changes must not invalidate topology diagnostics"
    );
    sqlx::query("UPDATE nodes SET dns_servers='[\"9.9.9.9\"]'::jsonb WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let newer = commit_direct_fixture_revision(&db, "unlock DNS changed").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1,generation=generation+1 WHERE id=TRUE").bind(newer as i64)
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .report_unlock_probe("n1", report(&view.id))
        .await
        .unwrap();
    let obsolete = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(obsolete.state, UnlockProbeState::Obsolete);
    assert!(obsolete.results.is_empty());
    let other = AdminContext::new("other", AdminRole::Editor, Some("other".into()));
    assert!(matches!(
        db.store
            .chain_unlock_probe(&other, "app-main", "chn-a1b2-c3d4", false)
            .await,
        Err(StoreError::NotFound(_))
    ));
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_chain_preserves_client_dns_condition_in_stored_results() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    let queued = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 48)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, queued.id);
    let mut completed = report(&target.probe_id);
    let youtube = completed
        .results
        .iter_mut()
        .find(|r| r.service == brocade_deployment::unlock_probe::UnlockService::YoutubePremium)
        .unwrap();
    youtube.client_dns = true;
    youtube.region = Some("HK".into());
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();
    let restarted = PgStore::from_pool(db.pool().clone());
    let saved = restarted
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.state, UnlockProbeState::Complete);
    assert_eq!(saved.results, completed.results);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_chain_persists_independent_youtube_observations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    let queued = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 49)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, queued.id);
    let mut completed = report(&target.probe_id);
    let result = completed
        .results
        .iter_mut()
        .find(|r| r.service == brocade_deployment::unlock_probe::UnlockService::YoutubePremium)
        .unwrap();
    result.status = UnlockStatus::UnsupportedRegion;
    result.code = UnlockCode::RegionUnsupported;
    result.region = Some("MO".into());
    result.youtube=Some(serde_json::from_value(serde_json::json!({
        "webpage":{"status":"available","code":"web_available","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
        "premium":{"status":"unsupported_region","code":"region_unsupported","http_status":200,"page_region":"US","topbar_region":null,"privacy_region":"MO"},
        "cdn":{"http_status":200,"node":"hkg12s15","airport":"HKG","country":"HK","error":null,"elapsed_ms":1200}
    })).unwrap());
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();
    let restarted = PgStore::from_pool(db.pool().clone());
    let saved = restarted
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.results, completed.results);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_premium_country_policy_covers_old_reads_new_reports_and_replays() {
    use brocade_deployment::unlock_probe::UnlockService;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    let target = db
        .store
        .claim_unlock_probe("n1", 49)
        .await
        .unwrap()
        .unwrap();
    let mut completed = report(&target.probe_id);
    let youtube = completed
        .results
        .iter_mut()
        .find(|r| r.service == UnlockService::YoutubePremium)
        .unwrap();
    youtube.region = Some("CN".into());
    youtube.youtube = Some(serde_json::from_value(serde_json::json!({
        "webpage":{"status":"available","code":"web_available","http_status":200,"page_region":"HK","topbar_region":"HK","privacy_region":"CN"},
        "premium":{"status":"available","code":"web_available","http_status":200,"page_region":"HK","topbar_region":"HK","privacy_region":"CN"},
        "cdn":{"http_status":200,"node":"hkg12s15","airport":"HKG","country":"HK","error":null,"elapsed_ms":1200}
    })).unwrap());
    assert!(completed.valid());
    let raw = serde_json::to_value(&completed.results).unwrap();
    let mut expected = completed.results.clone();
    for result in &mut expected {
        result.apply_youtube_premium_region_policy();
    }
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();
    let persisted: serde_json::Value =
        sqlx::query_scalar("SELECT results FROM unlock_probe_jobs WHERE job_id=$1")
            .bind(&target.probe_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(persisted, serde_json::to_value(&expected).unwrap());
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();

    // Existing observations are corrected on read, without rewriting their stored evidence.
    sqlx::query("UPDATE unlock_probe_jobs SET results=$2,previous_results=$2 WHERE job_id=$1")
        .bind(&target.probe_id)
        .bind(&raw)
        .execute(db.pool())
        .await
        .unwrap();
    let saved = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.results, expected);
    assert_eq!(saved.previous_results, expected);
    db.store
        .report_unlock_probe("n1", completed.clone())
        .await
        .unwrap();
    let persisted: serde_json::Value =
        sqlx::query_scalar("SELECT results FROM unlock_probe_jobs WHERE job_id=$1")
            .bind(&target.probe_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(persisted, raw);

    // Country normalization must not repair an invalid, contradictory report before validation.
    let youtube = completed
        .results
        .iter_mut()
        .find(|r| r.service == UnlockService::YoutubePremium)
        .unwrap();
    youtube.youtube.as_mut().unwrap().premium.status = UnlockStatus::Failed;
    youtube.youtube.as_mut().unwrap().premium.code = UnlockCode::NetworkError;
    assert!(!completed.valid());
    assert!(db.store.report_unlock_probe("n1", completed).await.is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_auto_discovers_chains_without_ui_and_stale_claims_expire() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    let target = db
        .store
        .claim_unlock_probe("n1", 47)
        .await
        .unwrap()
        .unwrap();
    assert!(db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .is_some());
    sqlx::query(
        "UPDATE unlock_probe_jobs SET claimed_at=now()-interval '16 minutes' WHERE job_id=$1",
    )
    .bind(&target.probe_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db
        .store
        .report_unlock_probe("n1", report(&target.probe_id))
        .await
        .is_err());
    let state: String = sqlx::query_scalar("SELECT state FROM unlock_probe_jobs WHERE job_id=$1")
        .bind(&target.probe_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    // Rejected late reports roll back their transaction; a read performs the visible expiration.
    if target.chain.is_some() {
        assert_eq!(
            db.store
                .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
                .await
                .unwrap()
                .unwrap()
                .state,
            UnlockProbeState::Expired
        );
    } else {
        assert_eq!(
            db.store
                .node_unlock_probe(&system_admin(), "n1", false)
                .await
                .unwrap()
                .unwrap()
                .state,
            UnlockProbeState::Expired
        );
    }
    assert!(matches!(state.as_str(), "running" | "expired"));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_dual_stack_jobs_are_independent_and_reject_cross_family_reports() {
    use brocade_deployment::unlock_probe::IpFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("UPDATE nodes SET public_ipv6='2606:4700::1234' WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(50))
        .await
        .unwrap();
    let first = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.family, Some(IpFamily::V4));
    sqlx::query(
        "UPDATE unlock_probe_jobs SET claimed_at=now()-interval '780 seconds' WHERE job_id=$1",
    )
    .bind(&first.probe_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        db.store
            .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V4))
            .await
            .unwrap()
            .unwrap()
            .state,
        UnlockProbeState::Running,
        "the initial attempt and ten retries must retain their lease"
    );

    assert!(
        db.store
            .claim_unlock_probe("n1", 49)
            .await
            .unwrap()
            .is_none(),
        "old Agents must not claim either family"
    );
    let mut wrong = report(&first.probe_id);
    wrong.family = Some(IpFamily::V6);
    assert!(matches!(
        db.store.report_unlock_probe("n1", wrong).await,
        Err(StoreError::InvalidData(_))
    ));
    let mut v4 = report(&first.probe_id);
    v4.family = first.family;
    db.store.report_unlock_probe("n1", v4).await.unwrap();
    let second = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.family, Some(IpFamily::V6));
    assert_ne!(first.probe_id, second.probe_id);
    let mut v6 = report(&second.probe_id);
    v6.family = second.family;
    for item in &mut v6.results {
        item.region = Some("US".into());
    }
    db.store.report_unlock_probe("n1", v6).await.unwrap();
    let view4 = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V4))
        .await
        .unwrap()
        .unwrap();
    let view6 = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V6))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view4.family, Some(IpFamily::V4));
    assert_eq!(view6.family, Some(IpFamily::V6));
    assert_eq!(
        view4
            .results
            .iter()
            .find(|item| item.service == brocade_deployment::unlock_probe::UnlockService::Netflix)
            .unwrap()
            .region
            .as_deref(),
        Some("JP")
    );
    assert!(view6
        .results
        .iter()
        .all(|item| item.region.as_deref() == Some("US")));
    sqlx::query("UPDATE unlock_probe_jobs SET created_at=now()-interval '2 minutes' WHERE address_family='v4'").execute(db.pool()).await.unwrap();
    let next4 = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", true, Some(IpFamily::V4))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next4.id, view4.id);
    assert!(next4.results.is_empty());
    assert!(!next4.previous_results.is_empty());
    assert!(
        db.store
            .node_unlock_probe_on_family(&system_admin(), "n1", true, Some(IpFamily::V6))
            .await
            .is_err(),
        "cooldown is independent for IPv6"
    );
    let preserved6 = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(IpFamily::V6))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(preserved6.id, view6.id);
    assert_eq!(preserved6.next_run_at, view6.next_run_at);
    assert_eq!(preserved6.results, view6.results);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_dual_stack_never_migrates_automatic_results_into_a_family() {
    use brocade_deployment::unlock_probe::IpFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("UPDATE nodes SET public_ipv6='2606:4700::1234' WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let automatic = db
        .store
        .claim_unlock_probe("n1", 46)
        .await
        .unwrap()
        .unwrap();
    db.store
        .report_unlock_probe("n1", report(&automatic.probe_id))
        .await
        .unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(50))
        .await
        .unwrap();
    sqlx::query("UPDATE unlock_probe_discovery SET next_discovery_at=now()-interval '1 second'")
        .execute(db.pool())
        .await
        .unwrap();
    let selected = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert!(selected.family.is_some());
    let legacy = db
        .store
        .node_unlock_probe(&system_admin(), "n1", false)
        .await
        .unwrap()
        .unwrap();
    assert!(legacy.family.is_none());
    assert!(!legacy.results.is_empty());
    for family in [IpFamily::V4, IpFamily::V6] {
        let view = db
            .store
            .node_unlock_probe_on_family(&system_admin(), "n1", false, Some(family))
            .await
            .unwrap()
            .unwrap();
        assert!(view.results.is_empty());
        assert!(view.previous_results.is_empty());
        assert_ne!(view.id, automatic.probe_id);
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_chain_family_dispatch_does_not_depend_on_host_runtime_freshness() {
    use brocade_deployment::unlock_probe::IpFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("UPDATE nodes SET public_ipv6='2606:4700::1234' WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    seed_subscription_serving(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(50))
        .await
        .unwrap();
    let queued = db
        .store
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            true,
            Some(IpFamily::V4),
        )
        .await
        .unwrap()
        .unwrap();
    let unknown = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unknown.probe_id, queued.id);
    assert_eq!(
        unknown.family_ready,
        Some(true),
        "actual path attempts must not be suppressed by missing runtime observations"
    );
    sqlx::query("UPDATE node_agent_state SET runtime_versions='{\"unlock_ip_families\":true}'::jsonb,runtime_reported_at=now() WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let ready = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.family_ready, Some(true));
    assert_eq!(ready.probe_id, queued.id);
    sqlx::query("UPDATE node_agent_state SET runtime_reported_at=now()-interval '2 minutes' WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let stale = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.family_ready, Some(true));
    sqlx::query("UPDATE node_agent_state SET agent_protocol_version=49,runtime_reported_at=now() WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let old = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.family_ready, Some(true));
    sqlx::query("UPDATE node_agent_state SET agent_protocol_version=50,runtime_versions='{}'::jsonb WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    let missing = db
        .store
        .claim_unlock_probe("n1", 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(missing.family_ready, Some(true));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_ipv6_requests_are_not_suppressed_by_host_address_observations() {
    use brocade_deployment::unlock_probe::IpFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(55))
        .await
        .unwrap();
    let caps = db
        .store
        .node_unlock_capabilities(&system_admin(), "n1")
        .await
        .unwrap();
    assert_eq!(caps.ipv6_available, None);
    assert_eq!(caps.ipv6_reason, None);
    let job = db
        .store
        .node_unlock_probe_on_family(&system_admin(), "n1", true, Some(IpFamily::V6))
        .await
        .unwrap()
        .unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 55)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, job.id);
    assert_eq!(target.family, Some(IpFamily::V6));
    assert!(
        !target.ipv6_egress_check,
        "new dispatch never enables the legacy host-route inference"
    );
    assert!(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM unlock_probe_jobs WHERE address_family='v4')"
    )
    .fetch_one(db.pool())
    .await
    .unwrap());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_automatic_chain_isolated_from_family_jobs_requires_v52_and_keeps_retry_lease() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(51))
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
            .await,
        Err(StoreError::Unsupported(_))
    ));
    db.store
        .record_node_poll_with_protocol("n1", None, Some(52))
        .await
        .unwrap();
    let view = db
        .store
        .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    assert!(view.automatic);
    assert_eq!(view.family, None);
    assert!(db
        .store
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            false,
            Some(IpFamily::V4)
        )
        .await
        .unwrap()
        .is_none());
    assert!(db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .is_none());
    sqlx::query("INSERT INTO unlock_probe_discovery(node_id,next_discovery_at) VALUES('n1',now()+interval '1 hour')").execute(db.pool()).await.unwrap();
    assert!(db
        .store
        .claim_unlock_probe("n1", 51)
        .await
        .unwrap()
        .is_none());
    let target = db
        .store
        .claim_unlock_probe("n1", 52)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, view.id);
    assert!(target.chain.is_some());
    assert_eq!(target.family, None);
    sqlx::query(
        "UPDATE unlock_probe_jobs SET claimed_at=now()-interval '780 seconds' WHERE job_id=$1",
    )
    .bind(&view.id)
    .execute(db.pool())
    .await
    .unwrap();
    let running = db
        .store
        .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.state, UnlockProbeState::Running);
    let mut wrong = report(&view.id);
    wrong.family = Some(IpFamily::V4);
    assert!(db.store.report_unlock_probe("n1", wrong).await.is_err());
    db.store
        .report_unlock_probe("n1", report(&view.id))
        .await
        .unwrap();
    let done = db
        .store
        .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert!(done.automatic);
    assert_eq!(done.state, UnlockProbeState::Complete);
    assert_eq!(done.interval_secs, 86_400);
    let other = AdminContext::new("other", AdminRole::Editor, Some("other".into()));
    assert!(matches!(
        db.store
            .chain_unlock_probe_automatic(&other, "app-main", "chn-a1b2-c3d4", false)
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_external_chain_schedules_one_automatic_original_path_instead_of_two_family_failures(
) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    setup(&db).await;
    let mut snapshot = db.store.materialize_snapshot(None).await.unwrap();
    snapshot
        .external_outbounds
        .push(brocade_core::model::ExternalOutbound {
            id: "proxy-auto".into(),
            tenant: "platform.acme".into(),
            name: "Automatic proxy".into(),
            address: "proxy.example.com".into(),
            port: 1080,
            protocol: ExternalOutboundProtocol::Socks5 {
                username: None,
                credential: String::new(),
            },
            security: ExternalOutboundSecurity::None,
            target_strategy: None,
            bindings: Vec::new(),
        });
    let app = snapshot
        .apps
        .iter_mut()
        .find(|a| a.id == "app-main")
        .unwrap();
    let step = app
        .steps
        .iter_mut()
        .find(|s| s.chain == "chn-a1b2-c3d4" && s.node == "n1")
        .unwrap();
    step.rules = vec![Rule {
        dest_match: DestMatch::Any,
        action: Action::Proxy {
            outbound: "proxy-auto".into(),
        },
    }];
    seed_subscription_serving_snapshot(&db, snapshot).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(52))
        .await
        .unwrap();
    let caps = db
        .store
        .chain_unlock_capabilities(&system_admin(), "app-main", "chn-a1b2-c3d4")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(caps).unwrap()["family_mode"],
        "automatic"
    );
    let target = db
        .store
        .claim_unlock_probe("n1", 52)
        .await
        .unwrap()
        .unwrap();
    assert!(target.chain.is_some());
    assert_eq!(target.family, None);
    assert_eq!(target.family_ready, None);
    let jobs: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT scope_key,address_family FROM unlock_probe_jobs WHERE chain_id='chn-a1b2-c3d4'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        jobs,
        vec![("chain:app-main/chn-a1b2-c3d4:auto".into(), None)]
    );
    db.store
        .report_unlock_probe("n1", report(&target.probe_id))
        .await
        .unwrap();
    let view = db
        .store
        .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert!(view.automatic);
    assert_eq!(view.state, UnlockProbeState::Complete);
    assert_eq!(view.path_error, None);
    {
        let page = db
            .store
            .chain_unlock_probe_page(&system_admin(), None, 100)
            .await
            .unwrap();
        let item = page
            .chains
            .into_iter()
            .find(|item| item.chain_id == "chn-a1b2-c3d4")
            .unwrap();
        assert_eq!(
            serde_json::to_value(&item.capabilities).unwrap()["family_mode"],
            "automatic"
        );
        let bulk = item.probes.automatic.unwrap();
        assert!(bulk.automatic);
        assert_eq!(bulk.family, None);
        assert_eq!(bulk.results, view.results);
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_single_stack_observations_do_not_replace_forced_family_path_attempts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(55))
        .await
        .unwrap();
    sqlx::query("UPDATE node_agent_state SET runtime_versions='{\"unlock_ip_families\":false}'::jsonb,runtime_reported_at=now()-interval '5 minutes' WHERE node_id='n1'").execute(db.pool()).await.unwrap();
    let caps = db
        .store
        .chain_unlock_capabilities(&system_admin(), "app-main", "chn-a1b2-c3d4")
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(caps).unwrap()["family_mode"], "split");
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    let families: Vec<Option<String>> = sqlx::query_scalar("SELECT address_family FROM unlock_probe_jobs WHERE chain_id='chn-a1b2-c3d4' ORDER BY address_family").fetch_all(db.pool()).await.unwrap();
    assert_eq!(families, [Some("v4".into()), Some("v6".into())]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM unlock_probe_jobs WHERE scope_key LIKE '%:auto'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_unchanged_paths_keep_completed_results_across_revisions_in_every_mode() {
    for mode in ["legacy", "automatic", "v4", "v6"] {
        let Some(db) = TestPg::start_if_enabled().await else {
            return;
        };
        setup(&db).await;
        sqlx::query("UPDATE nodes SET public_ipv6='2606:4700::1234' WHERE id='n1'")
            .execute(db.pool())
            .await
            .unwrap();
        seed_subscription_serving(&db).await;
        db.store
            .record_node_poll_with_protocol("n1", None, Some(55))
            .await
            .unwrap();
        let family = match mode {
            "v4" => Some(brocade_core::model::IpFamily::V4),
            "v6" => Some(brocade_core::model::IpFamily::V6),
            _ => None,
        };
        let queued = if mode == "automatic" {
            db.store
                .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
                .await
        } else {
            db.store
                .chain_unlock_probe_on_family(
                    &system_admin(),
                    "app-main",
                    "chn-a1b2-c3d4",
                    true,
                    family,
                )
                .await
        }
        .unwrap()
        .unwrap();
        let target = db
            .store
            .claim_unlock_probe("n1", 55)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(target.probe_id, queued.id);
        let mut outcome = report(&queued.id);
        outcome.family = family;
        db.store
            .report_unlock_probe("n1", outcome.clone())
            .await
            .unwrap();
        let baseline: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(j) FROM unlock_probe_jobs j WHERE job_id=$1")
                .bind(&queued.id)
                .fetch_one(db.pool())
                .await
                .unwrap();

        let expected: Vec<brocade_deployment::unlock_probe::UnlockResult> =
            serde_json::from_value(baseline["results"].clone()).unwrap();
        let mut service_facts = expected.clone();
        for result in &mut service_facts {
            if family.is_some() {
                assert!(!result.dns_parts.is_empty(), "missing evidence is explicit");
                assert!(result
                    .dns_parts
                    .iter()
                    .all(|part| part.kind
                        == brocade_deployment::unlock_probe::UnlockDnsKind::Unknown));
            }
            result.dns_parts.clear();
        }
        assert_eq!(
            service_facts, outcome.results,
            "optional evidence preserves service facts"
        );

        sqlx::query("UPDATE nodes SET name='Only display name changed' WHERE id='n1'")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE chains SET name='Display-only chain rename' WHERE id='chn-a1b2-c3d4'")
            .execute(db.pool())
            .await
            .unwrap();
        let revision = commit_direct_fixture_revision(&db, "display-only revision").await;
        sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1,generation=generation+1 WHERE id=TRUE").bind(revision as i64).execute(db.pool()).await.unwrap();
        // Existing Consoles may already have marked the unchanged completed report obsolete.
        sqlx::query("UPDATE unlock_probe_jobs SET state='obsolete' WHERE job_id=$1")
            .bind(&queued.id)
            .execute(db.pool())
            .await
            .unwrap();
        if let Some(family) = family {
            let page = db
                .store
                .chain_unlock_probe_page(&system_admin(), None, 100)
                .await
                .unwrap();
            let probe = page
                .chains
                .into_iter()
                .find(|item| item.chain_id == "chn-a1b2-c3d4")
                .unwrap()
                .probes;
            let probe = match family {
                brocade_core::model::IpFamily::V4 => probe.v4,
                brocade_core::model::IpFamily::V6 => probe.v6,
            }
            .unwrap();
            assert_eq!(
                probe.state,
                UnlockProbeState::Complete,
                "bulk read repairs unchanged path: {mode}"
            );
            assert_eq!(probe.results, expected);
        }
        let current = if mode == "automatic" {
            db.store
                .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
                .await
        } else {
            db.store
                .chain_unlock_probe_on_family(
                    &system_admin(),
                    "app-main",
                    "chn-a1b2-c3d4",
                    false,
                    family,
                )
                .await
        }
        .unwrap()
        .unwrap();
        assert_eq!(current.state, UnlockProbeState::Complete, "{mode}");
        assert_eq!(current.id, queued.id);
        assert_eq!(current.results, expected);
        assert_eq!(
            current.configuration_revision, queued.configuration_revision,
            "preserve measured provenance"
        );
        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(j) FROM unlock_probe_jobs j WHERE job_id=$1")
                .bind(&queued.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            stored, baseline,
            "no fake re-test, retimestamp or revision rewrite: {mode}"
        );

        sqlx::query("UPDATE nodes SET dns_servers='[\"9.9.9.9\"]'::jsonb WHERE id='n1'")
            .execute(db.pool())
            .await
            .unwrap();
        let changed = commit_direct_fixture_revision(&db, "effective DNS changed").await;
        sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1 WHERE id=TRUE")
            .bind(changed as i64)
            .execute(db.pool())
            .await
            .unwrap();
        if let Some(family) = family {
            let page = db
                .store
                .chain_unlock_probe_page(&system_admin(), None, 100)
                .await
                .unwrap();
            let probe = page
                .chains
                .into_iter()
                .find(|item| item.chain_id == "chn-a1b2-c3d4")
                .unwrap()
                .probes;
            let probe = match family {
                brocade_core::model::IpFamily::V4 => probe.v4,
                brocade_core::model::IpFamily::V6 => probe.v6,
            }
            .unwrap();
            assert_eq!(
                probe.state,
                UnlockProbeState::Obsolete,
                "bulk read detects changed path: {mode}"
            );
            assert_eq!(probe.results, expected);
        }
        let obsolete = if mode == "automatic" {
            db.store
                .chain_unlock_probe_automatic(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
                .await
        } else {
            db.store
                .chain_unlock_probe_on_family(
                    &system_admin(),
                    "app-main",
                    "chn-a1b2-c3d4",
                    false,
                    family,
                )
                .await
        }
        .unwrap()
        .unwrap();
        assert_eq!(
            obsolete.state,
            UnlockProbeState::Obsolete,
            "actual DNS change: {mode}"
        );
        assert_eq!(obsolete.results, expected, "keep last real observations");
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_unchanged_running_path_survives_claim_discovery_and_report_across_revisions() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    let queued = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE nodes SET name='Unrelated display change' WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let revision = commit_direct_fixture_revision(&db, "same path before claim").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1,generation=generation+1 WHERE id=TRUE").bind(revision as i64).execute(db.pool()).await.unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 47)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        target.probe_id, queued.id,
        "discovery and claim reuse the equivalent pending path"
    );
    let repeated = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated.id, queued.id,
        "equivalent running job is reused despite manual cooldown"
    );
    sqlx::query("UPDATE chains SET name='Another display change' WHERE id='chn-a1b2-c3d4'")
        .execute(db.pool())
        .await
        .unwrap();
    let revision = commit_direct_fixture_revision(&db, "same path before report").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1 WHERE id=TRUE")
        .bind(revision as i64)
        .execute(db.pool())
        .await
        .unwrap();
    db.store
        .report_unlock_probe("n1", report(&queued.id))
        .await
        .unwrap();
    let completed = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, UnlockProbeState::Complete);
    assert_eq!(
        completed.configuration_revision,
        queued.configuration_revision
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_freshness_checks_downstream_configuration_but_ignores_unrelated_nodes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    for (id, address, key) in [
        ("n2", "10.66.0.2", "peer-two"),
        ("n3", "10.66.0.3", "peer-three"),
    ] {
        sqlx::query("INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers) VALUES($1,'platform.acme',$1,$1||'.example.net',$2::inet,$3||'-private',$3||'-public',51820,10085,true,true,'servers','[\"1.1.1.1\"]'::jsonb)")
            .bind(id).bind(address).bind(key).execute(db.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO steps(chain_id,node_id,rules,accept_uuid,accept_label,hop_in_port,hop_in_wire) VALUES('chn-a1b2-c3d4','n2','[{\"m\":{\"t\":\"any\"},\"a\":{\"t\":\"egress\",\"send_through\":null}}]'::jsonb,'3f7b1f5e-9a2c-4c1d-8b3a-6d5e4f2c1b0a','chn-a1b2-c3d4@n2',20001,'{\"t\":\"none\"}'::jsonb)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE steps SET rules=$1 WHERE node_id='n1'")
        .bind(json!([{"m":{"t":"any"},"a":{"t":"forward","to":"n2","dial":{"t":"overlay"},"pool":{"t":"none"}}}]))
        .execute(db.pool()).await.unwrap();
    seed_subscription_serving(&db).await;
    let view = db
        .store
        .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", true)
        .await
        .unwrap()
        .unwrap();
    let target = db
        .store
        .claim_unlock_probe("n1", 47)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.probe_id, view.id);
    db.store
        .report_unlock_probe("n1", report(&view.id))
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET dns_servers='[\"9.9.9.9\"]'::jsonb WHERE id='n3'")
        .execute(db.pool())
        .await
        .unwrap();
    let unrelated = commit_direct_fixture_revision(&db, "unrelated node DNS").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1 WHERE id=TRUE")
        .bind(unrelated as i64)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        db.store
            .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
            .await
            .unwrap()
            .unwrap()
            .state,
        UnlockProbeState::Complete
    );
    let before = db.store.materialize_snapshot(None).await.unwrap();
    sqlx::query("UPDATE nodes SET dns_servers='[\"8.8.8.8\"]'::jsonb WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    let changed = commit_direct_fixture_revision(&db, "downstream node DNS").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1 WHERE id=TRUE")
        .bind(changed as i64)
        .execute(db.pool())
        .await
        .unwrap();
    let after = db.store.materialize_snapshot(None).await.unwrap();
    let head = |snapshot: &brocade_core::model::ModelSnapshot| {
        brocade_core::artifacts::xray::build(
            &brocade_core::compile::compile(snapshot)
                .project_node("n1")
                .unwrap(),
        )
    };
    assert_eq!(
        head(&before),
        head(&after),
        "head alone cannot prove the entire path unchanged"
    );
    assert_eq!(
        db.store
            .chain_unlock_probe(&system_admin(), "app-main", "chn-a1b2-c3d4", false)
            .await
            .unwrap()
            .unwrap()
            .state,
        UnlockProbeState::Obsolete
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_manual_mode_refreshes_changed_dns_and_full_sweeps_include_fresh_scopes() {
    use brocade_store::{UnlockDetectionMode, UpdateDeploymentSettings};
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    assert_eq!(
        db.store
            .deployment_settings()
            .await
            .unwrap()
            .unlock_detection_mode,
        UnlockDetectionMode::Scheduled
    );
    let manual = UpdateDeploymentSettings {
        unlock_detection_mode: Some(UnlockDetectionMode::Manual),
        unlock_detection_interval_hours: Some(6),
        ..Default::default()
    };
    let editor = AdminContext::new("editor", AdminRole::Editor, Some("platform.acme".into()));
    assert!(matches!(
        db.store.update_deployment_settings(&editor, manual).await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store.refresh_all_unlock_probes(&editor).await,
        Err(StoreError::Forbidden(_))
    ));
    db.store
        .update_deployment_settings(&system_admin(), manual)
        .await
        .unwrap();
    for hours in [0, 721] {
        assert!(matches!(
            db.store
                .update_deployment_settings(
                    &system_admin(),
                    UpdateDeploymentSettings {
                        unlock_detection_interval_hours: Some(hours),
                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::InvalidData(_))
        ));
    }
    assert_eq!(
        db.store
            .deployment_settings()
            .await
            .unwrap()
            .unlock_detection_interval_hours,
        6
    );
    db.store
        .record_node_poll_with_protocol("n1", None, Some(55))
        .await
        .unwrap();
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    // Complete all discovered scopes as fresh fixtures; no probe execution is involved in this scheduler test.
    sqlx::query("UPDATE unlock_probe_jobs SET state='complete',completed_at=now(),next_run_at=now()+interval '6 hours'").execute(db.pool()).await.unwrap();
    let jobs = "SELECT jsonb_object_agg(scope_key,job_id) FROM unlock_probe_jobs";
    let before: serde_json::Value = sqlx::query_scalar(jobs).fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        before.as_object().unwrap().len(),
        4,
        "node and chain, both families"
    );
    let discover = "UPDATE unlock_probe_discovery SET next_discovery_at=now()-interval '1 second'";
    // Per-job clocks cannot silently turn manual mode into a recurring schedule.
    sqlx::query("UPDATE unlock_probe_jobs SET next_run_at=now()-interval '1 day'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(discover).execute(db.pool()).await.unwrap();
    assert!(db
        .store
        .claim_unlock_probe("n1", 55)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(jobs)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
    // A published machine DNS change refreshes its direct scope and chains using that machine.
    sqlx::query("UPDATE nodes SET dns_servers='[\"9.9.9.9\"]'::jsonb WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let revision = commit_direct_fixture_revision(&db, "DNS changed in manual mode").await;
    sqlx::query("UPDATE subscription_serving_state SET topology_revision_id=$1 WHERE id=TRUE")
        .bind(revision as i64)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(discover).execute(db.pool()).await.unwrap();
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    let changed: serde_json::Value = sqlx::query_scalar(jobs).fetch_one(db.pool()).await.unwrap();
    for (scope, id) in before.as_object().unwrap() {
        assert_ne!(changed[scope], *id, "DNS must invalidate {scope}");
    }
    sqlx::query("UPDATE unlock_probe_jobs SET state='complete',completed_at=now(),next_run_at=now()+interval '6 hours'").execute(db.pool()).await.unwrap();
    // A global request forces fresh scopes too, without altering the model or recurring timer.
    let policy = db
        .store
        .refresh_all_unlock_probes(&system_admin())
        .await
        .unwrap();
    assert_eq!(policy.mode, UnlockDetectionMode::Manual);
    assert_eq!(policy.next_run_at, None);
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    let full: serde_json::Value = sqlx::query_scalar(jobs).fetch_one(db.pool()).await.unwrap();
    for (scope, id) in changed.as_object().unwrap() {
        assert_ne!(full[scope], *id, "full sweep must refresh {scope}");
    }
    // Repeated requests coalesce while existing scope attempts remain in flight.
    db.store
        .refresh_all_unlock_probes(&system_admin())
        .await
        .unwrap();
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(jobs)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        full
    );
    let current: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(current, revision as i64);
    // Advancing a scheduled cycle refreshes every scope even though all results are still fresh.
    db.store
        .update_deployment_settings(
            &system_admin(),
            UpdateDeploymentSettings {
                unlock_detection_mode: Some(UnlockDetectionMode::Scheduled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE unlock_probe_jobs SET state='complete',completed_at=now(),created_at=now(),next_run_at=now()+interval '6 hours'").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE control_state SET unlock_detection_requested_at=now()-interval '1 day',unlock_detection_next_run_at=now()-interval '1 second' WHERE id=TRUE").execute(db.pool()).await.unwrap();
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    let scheduled: serde_json::Value = sqlx::query_scalar(jobs).fetch_one(db.pool()).await.unwrap();
    for (scope, id) in full.as_object().unwrap() {
        assert_ne!(
            scheduled[scope], *id,
            "scheduled cycle must refresh {scope}"
        );
    }
    let next:i64=sqlx::query_scalar("SELECT extract(epoch FROM unlock_detection_next_run_at-now())::bigint FROM control_state WHERE id=TRUE").fetch_one(db.pool()).await.unwrap();
    assert!((21598..=21601).contains(&next));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_not_ready_retries_are_bounded_and_preserve_previous_service_facts() {
    use brocade_deployment::unlock_probe::{IpFamily, UnlockPathError};
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    db.store
        .record_node_poll_with_protocol("n1", None, Some(55))
        .await
        .unwrap();
    let first = db
        .store
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            true,
            Some(IpFamily::V6),
        )
        .await
        .unwrap()
        .unwrap();
    let previous = serde_json::to_value(report(&first.id).results).unwrap();
    sqlx::query("UPDATE unlock_probe_jobs SET previous_results=$1,previous_completed_at=now()-interval '5 minutes' WHERE job_id=$2").bind(&previous).bind(&first.id).execute(db.pool()).await.unwrap();
    let mut stale_id: Option<String> = None;
    for (attempt, delay) in [60_i64, 120, 240, 480, 900, 86400].into_iter().enumerate() {
        // Exclude other independent periodic tasks from this fixture's claim selection.
        sqlx::query(
            "DELETE FROM unlock_probe_jobs WHERE scope_key<>'chain:app-main/chn-a1b2-c3d4:v6'",
        )
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE unlock_probe_discovery SET next_discovery_at=now()-interval '1 second'",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let target = loop {
            let candidate = db
                .store
                .claim_unlock_probe("n1", 55)
                .await
                .unwrap()
                .unwrap();
            if candidate.family == Some(IpFamily::V6)
                && candidate
                    .chain
                    .as_ref()
                    .is_some_and(|c| c.chain_id == "chn-a1b2-c3d4")
            {
                break candidate;
            }
            let mut accepted = report(&candidate.probe_id);
            accepted.family = candidate.family;
            db.store.report_unlock_probe("n1", accepted).await.unwrap();
        };
        assert_eq!(target.family, Some(IpFamily::V6));
        if attempt == 0 {
            assert_eq!(target.probe_id, first.id);
        }
        let mut failed = report(&target.probe_id);
        failed.family = target.family;
        failed.path_error = Some(UnlockPathError::FamilyUnavailable);
        for result in &mut failed.results {
            result.status = UnlockStatus::Failed;
            result.code = UnlockCode::NetworkError;
            result.region = None;
        }
        db.store
            .report_unlock_probe("n1", failed.clone())
            .await
            .unwrap();
        db.store.report_unlock_probe("n1", failed).await.unwrap();
        let state:(i16,i64)=sqlx::query_as("SELECT readiness_failures,extract(epoch FROM next_run_at-completed_at)::bigint FROM unlock_probe_jobs WHERE job_id=$1").bind(&target.probe_id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(state, (attempt as i16 + 1, delay));
        let retained: serde_json::Value =
            sqlx::query_scalar("SELECT previous_results FROM unlock_probe_jobs WHERE job_id=$1")
                .bind(&target.probe_id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            retained, previous,
            "readiness failures must not replace last real service facts"
        );
        if let Some(old) = stale_id.take() {
            let mut late = report(&old);
            late.family = target.family;
            assert!(db.store.report_unlock_probe("n1", late).await.is_err());
        }
        stale_id = Some(target.probe_id);
        if attempt < 5 {
            sqlx::query("UPDATE unlock_probe_jobs SET next_run_at=now()-interval '1 second' WHERE scope_key='chain:app-main/chn-a1b2-c3d4:v6'").execute(db.pool()).await.unwrap();
        }
    }
    // Exhaustion returns to the daily schedule instead of retrying indefinitely each minute.
    let due:bool=sqlx::query_scalar("SELECT next_run_at>now()+interval '23 hours' FROM unlock_probe_jobs WHERE scope_key='chain:app-main/chn-a1b2-c3d4:v6'").fetch_one(db.pool()).await.unwrap();
    assert!(due);
    sqlx::query("UPDATE unlock_probe_jobs SET created_at=now()-interval '61 seconds' WHERE scope_key='chain:app-main/chn-a1b2-c3d4:v6'").execute(db.pool()).await.unwrap();
    db.store
        .chain_unlock_probe_on_family(
            &system_admin(),
            "app-main",
            "chn-a1b2-c3d4",
            true,
            Some(IpFamily::V6),
        )
        .await
        .unwrap();
    let reset:i16=sqlx::query_scalar("SELECT readiness_failures FROM unlock_probe_jobs WHERE scope_key='chain:app-main/chn-a1b2-c3d4:v6'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(reset, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_published_dns_change_wakes_discovery_immediately_in_manual_mode() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    seed_subscription_serving(&db).await;
    db.store
        .update_deployment_settings(
            &system_admin(),
            brocade_store::UpdateDeploymentSettings {
                unlock_detection_mode: Some(brocade_store::UnlockDetectionMode::Manual),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.store.claim_unlock_probe("n1", 55).await.unwrap();
    sqlx::query("UPDATE unlock_probe_jobs SET state='complete',completed_at=now()")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE unlock_probe_discovery SET next_discovery_at=now()+interval '1 day'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET dns_servers='[\"9.9.9.9\"]'::jsonb WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let revision = commit_direct_fixture_revision(&db, "publish DNS change").await;
    let mut listener = sqlx::postgres::PgListener::connect_with(db.pool())
        .await
        .unwrap();
    listener.listen("brocade_agent_change").await.unwrap();
    let _created = db
        .store
        .create_deployment(
            &system_admin(),
            create_deployment_request(revision, "unlock-dns-publish"),
        )
        .await
        .unwrap();
    for node in ["n1", "n2"] {
        if let Some(desired) = db.store.claim_desired_for_node(node).await.unwrap() {
            db.store
                .report_target_result(applied_report(&desired))
                .await
                .unwrap();
        }
    }
    let due: bool = sqlx::query_scalar(
        "SELECT next_discovery_at<=now() FROM unlock_probe_discovery WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        due,
        "effective configuration must invalidate discovery without waiting for its clock"
    );
    let wait = async {
        loop {
            let signal = listener.recv().await.unwrap();
            let payload: serde_json::Value = serde_json::from_str(signal.payload()).unwrap();
            if payload["domain"] == "unlock_probe" {
                return;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), wait)
        .await
        .expect("configuration activation must send an unlock wake");
}
