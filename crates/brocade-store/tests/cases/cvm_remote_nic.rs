use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "cvm_entries.rs"]
mod entries_tests;

async fn setup(db: &TestPg) {
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    prepare(db).await;
    let mut policy = quota();
    policy.max_instances = 8;
    policy.max_vcpus = 8;
    policy.max_memory_bytes = 4 * 1024 * 1024 * 1024;
    policy.max_disk_bytes = 32 * 1024 * 1024 * 1024;
    policy.max_interfaces = 32;
    db.store
        .set_cvm_quota(&system_admin(), policy)
        .await
        .unwrap();
    sqlx::query("INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind) VALUES('n2','platform.acme','Network provider','198.51.100.20','10.66.0.2','fixture-private','fixture-public',51821,10086,true,true,'system')").execute(db.pool()).await.unwrap();
    for node in ["n1", "n2"] {
        db.store
            .cvm_record_nic_capabilities(
                node,
                NicNodeCapabilities {
                    contract: NIC_PROTOCOL_VERSION,
                    transports: vec![TransportKind::Wireguard, TransportKind::Xray],
                    consume: node == "n1",
                    provide: true,
                    max_bindings: 64,
                    addresses: vec!["198.51.100.20".parse().unwrap()],
                    host_ports: vec![NicHostPort {
                        protocol: L4Protocol::Tcp,
                        port: 22,
                    }],
                    xray_certificate_pem: Some("fixture-public-certificate".into()),
                    error: None,
                    entries: false,
                    route_service: false,
                },
            )
            .await
            .unwrap();
    }
}
fn publication_spec() -> NicPublicationSpec {
    NicPublicationSpec {
        name: "Reusable network".into(),
        provider_node: "n2".into(),
        resource_domain: "shared-provider-pool".into(),
        ipv4_pool: Some("10.120.0.0/24".parse().unwrap()),
        ipv6_pool: Some("fd12:3456::/120".parse().unwrap()),
        public_ipv4_pool: None,
        public_ipv6_pool: None,
        shared_ipv4: Some("198.51.100.20".parse().unwrap()),
        shared_ipv6: Some("2001:db8::20".parse().unwrap()),
        address_mode: NicAddressMode::SharedNat,
        destinations: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
        route_presets: vec!["0.0.0.0/0".parse().unwrap()],
        dns_suggestions: vec![],
        transports: vec![
            TransportOffer {
                listen_side: TransportListenSide::Provider,
                kind: TransportKind::Wireguard,
                endpoint: "198.51.100.20".parse().unwrap(),
                port_start: 30000,
                port_end: 30100,
                mtu: 1400,
            },
            TransportOffer {
                listen_side: TransportListenSide::Provider,
                kind: TransportKind::Xray,
                endpoint: "198.51.100.20".parse().unwrap(),
                port_start: 31000,
                port_end: 31100,
                mtu: 1400,
            },
        ],
        grants: vec![NetworkOwner {
            tenant_id: "platform.acme".into(),
            user_id: "alice".into(),
        }],
        limits: NicLimits {
            max_attachments: 32,
            ..Default::default()
        },
    }
}
async fn publish(db: &TestPg) -> NicPublication {
    db.store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "network-publication-request-0001".into(),
                expected_revision: 0,
                spec: publication_spec(),
            },
        )
        .await
        .unwrap()
}
async fn create(db: &TestPg, key: &str) -> CvmInstance {
    let mut request = create_request(key);
    request.interfaces = vec![contact()];
    let instance = db
        .store
        .create_cvm(&actor_user("alice"), request)
        .await
        .unwrap();
    acknowledge_guest(db, &instance).await;
    db.store
        .cvm_instance(&actor_user("alice"), &instance.desired.id)
        .await
        .unwrap()
}
fn guest_evidence(instance: &CvmInstance) -> InstanceReport {
    let mut report = evidence(instance);
    if let Some(network) = report.network.as_mut() {
        network.default_ipv4_interface = instance
            .desired
            .spec
            .routing
            .default_interface(true)
            .map(str::to_owned);
        network.default_ipv6_interface = instance
            .desired
            .spec
            .routing
            .default_interface(false)
            .map(str::to_owned);
    }
    for observed in &mut report.interfaces {
        if let Some(nic) = instance
            .desired
            .spec
            .interfaces
            .iter()
            .find(|n| n.id == observed.id)
            .and_then(|n| n.allocated.as_ref())
        {
            observed.mac = nic.mac.clone();
            observed.addresses = [nic.ipv4, nic.ipv6]
                .into_iter()
                .flatten()
                .map(|ip| format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 }))
                .collect();
        }
    }
    if instance.desired.power == PowerState::Stopped {
        report.runtime_id = None;
        report.network_absent = true;
        report.volume_retained = true;
        for nic in &mut report.interfaces {
            nic.attached = false;
        }
    }
    report.detached_interfaces = instance.desired.detached_interfaces.clone();
    report
}
async fn acknowledge_guest(db: &TestPg, instance: &CvmInstance) {
    db.store
        .cvm_record_report("n1", guest_evidence(instance))
        .await
        .unwrap();
}
async fn acknowledge_network(db: &TestPg, node: &str) {
    let desired = db.store.cvm_nic_desired(node).await.unwrap();
    db.store
        .cvm_record_nic_report(
            node,
            NicNodeReport {
                generation: desired.generation,
                bindings: desired
                    .bindings
                    .into_iter()
                    .map(|b| NicBindingReport {
                        local_block: None,
                        traffic: None,
                        id: b.id,
                        side: b.side,
                        generation: b.generation,
                        phase: b.phase,
                        configured: true,
                        reachable: Some(true),
                        rx_bytes: 0,
                        tx_bytes: 0,
                        error: None,
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
}
fn mount_request(instance: &CvmInstance, p: &NicPublication, key: &str) -> NicMountRequest {
    NicMountRequest {
        use_dns_suggestions: false,
        request_id: key.into(),
        expected_generation: instance.desired.generation,
        publication_id: p.id.clone(),
        transport: TransportKind::Wireguard,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_reuse_concurrent_allocations_replay_and_scope() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    assert!(db
        .store
        .cvm_nic_catalog(&actor_user("bob"))
        .await
        .unwrap()
        .publications
        .is_empty());
    assert!(matches!(
        db.store
            .write_cvm_nic_publication(
                &actor_user("alice"),
                None,
                PublicationWrite {
                    request_id: "unauthorized-publish-0001".into(),
                    expected_revision: 0,
                    spec: publication_spec()
                }
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let a = create(&db, "container-network-a-0001").await;
    let b = create(&db, "container-network-b-0001").await;
    let first = mount_request(&a, &p, "mount-network-a-0001");
    let second = mount_request(&b, &p, "mount-network-b-0001");
    let actor = actor_user("alice");
    let (a, b) = tokio::join!(
        db.store.mount_cvm_nic(&actor, &a.desired.id, first.clone()),
        db.store.mount_cvm_nic(&actor, &b.desired.id, second)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let nic_a = &a.desired.spec.interfaces[1];
    let nic_b = &b.desired.spec.interfaces[1];
    assert_ne!(nic_a.id, nic_b.id);
    assert_ne!(
        nic_a.allocated.as_ref().unwrap().ipv4,
        nic_b.allocated.as_ref().unwrap().ipv4
    );
    assert_ne!(
        nic_a.allocated.as_ref().unwrap().mac,
        nic_b.allocated.as_ref().unwrap().mac
    );
    let replay = db
        .store
        .mount_cvm_nic(&actor, &a.desired.id, first)
        .await
        .unwrap();
    assert_eq!(replay.desired.spec.interfaces, a.desired.spec.interfaces);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_allocations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    let public = db.store.cvm_nic_state(&actor, &a.desired.id).await.unwrap();
    let public = serde_json::to_string(&public).unwrap();
    assert!(!public.contains("credential") && !public.contains("sealed"));
    assert!(db
        .store
        .cvm_nic_state(&actor_user("bob"), &a.desired.id)
        .await
        .is_err());
    let raw: String = sqlx::query_scalar("SELECT sealed FROM cvm_nic_bindings LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(raw.starts_with("v1."));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_dependencies_two_sided_cleanup_and_admission() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    let a = create(&db, "container-dependencies-0001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "mount-dependencies-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    let nic = a.desired.spec.interfaces[1].id.clone();
    assert!(matches!(
        db.store
            .command_cvm(
                &actor_user("alice"),
                &a.desired.id,
                CvmCommand {
                    request_id: "dependent-detach-0001".into(),
                    expected_generation: a.desired.generation,
                    change: CvmChange::Detach {
                        interface_id: nic.clone()
                    }
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    db.store
        .set_cvm_nic_admission(
            &system_admin(),
            &p.id,
            PublicationAdmission {
                request_id: "stop-admission-00001".into(),
                expected_revision: p.revision,
                admitting: false,
            },
        )
        .await
        .unwrap();
    let current = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert_eq!(current.desired.spec, a.desired.spec);
    assert!(db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&current, &p, "mount-stopped-00001")
        )
        .await
        .is_err());
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    let mut routes = current.desired.spec.routing.clone();
    routes.remove(&nic);
    let current = db
        .store
        .set_cvm_routes(
            &actor_user("alice"),
            &a.desired.id,
            RoutesWrite {
                request_id: "remove-route-reference-0001".into(),
                expected_generation: current.desired.generation,
                expected_revision: routes.revision,
                routes,
            },
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &current).await;
    let detached = db
        .store
        .command_cvm(
            &actor_user("alice"),
            &a.desired.id,
            CvmCommand {
                request_id: "detach-network-after-routes-0001".into(),
                expected_generation: current.desired.generation,
                change: CvmChange::Detach { interface_id: nic },
            },
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &detached).await;
    acknowledge_network(&db, "n1").await;
    let held: bool = sqlx::query_scalar("SELECT NOT released FROM cvm_nic_allocations LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(held, "one endpoint cannot release the allocation");
    acknowledge_network(&db, "n2").await;
    let released: bool = sqlx::query_scalar("SELECT released FROM cvm_nic_allocations LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(released);
    db.store
        .delete_cvm_nic_publication(
            &system_admin(),
            &p.id,
            PublicationAdmission {
                request_id: "delete-publication-cleaned-0001".into(),
                expected_revision: p.revision,
                admitting: false,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_switch_failure_preserves_identity_allocation_and_routes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    let a = create(&db, "container-switch-00001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "mount-switch-00001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let nic = a.desired.spec.interfaces[1].clone();
    let allocation = nic.allocated.as_ref().unwrap();
    let changed = db
        .store
        .change_cvm_nic_transport(
            &actor_user("alice"),
            &a.desired.id,
            &nic.id,
            NicTransportChange {
                rebuild: false,
                request_id: "switch-to-xray-00001".into(),
                expected_generation: a.desired.generation,
                transport: TransportKind::Xray,
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.desired.spec.routing, a.desired.spec.routing);
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    let new = desired
        .bindings
        .iter()
        .find(|b| b.nic.transport == TransportKind::Xray)
        .unwrap();
    let failed = NicBindingReport {
        local_block: None,
        traffic: None,
        id: new.id.clone(),
        side: new.side,
        generation: new.generation,
        phase: new.phase,
        configured: false,
        reachable: Some(false),
        rx_bytes: 0,
        tx_bytes: 0,
        error: Some("fixture: new transport unavailable".into()),
    };
    db.store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                generation: desired.generation,
                bindings: vec![failed.clone()],
            },
        )
        .await
        .unwrap();
    let recovered = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    let actual = recovered.desired.spec.interfaces[1]
        .allocated
        .as_ref()
        .unwrap();
    assert_eq!(actual.transport, TransportKind::Wireguard);
    assert_eq!(actual.mac, allocation.mac);
    assert_eq!(actual.allocation_id, allocation.allocation_id);
    assert_eq!(actual.ipv4, allocation.ipv4);
    assert_eq!(recovered.desired.spec.routing, a.desired.spec.routing);
    assert!(
        !db.store
            .cvm_record_nic_report(
                "n1",
                NicNodeReport {
                    generation: desired.generation,
                    bindings: vec![failed]
                }
            )
            .await
            .unwrap(),
        "late failed generation must not replay recovery"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_revocation_blocks_but_retirement_removes_references() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    let a = create(&db, "container-retirement-0001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "mount-retirement-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    sqlx::query(
        "UPDATE cvm_nic_nodes SET observed_at=now()-interval '5 minutes' WHERE node_id='n2'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut updated = p.spec.clone();
    updated.grants.clear();
    db.store
        .write_cvm_nic_publication(
            &system_admin(),
            Some(&p.id),
            PublicationWrite {
                request_id: "revoke-network-grant-0001".into(),
                expected_revision: p.revision,
                spec: updated,
            },
        )
        .await
        .unwrap();
    let blocked = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert!(
        blocked.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .blocked
    );
    assert_eq!(blocked.desired.spec.routing, a.desired.spec.routing);
    db.store
        .update_node_status(
            &system_admin(),
            "n2",
            brocade_store::UpdateNodeStatusRequest {
                status: "retired".into(),
            },
        )
        .await
        .unwrap();
    let retired = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert_eq!(retired.desired.spec.interfaces.len(), 1);
    assert!(retired.desired.spec.routing.routes.is_empty());
    assert!(retired.desired.spec.routing.priorities.is_empty());
    assert_eq!(
        retired.desired.detached_interfaces,
        vec![a.desired.spec.interfaces[1].id.clone()]
    );
    assert!(db
        .store
        .cvm_nic_catalog(&system_admin())
        .await
        .unwrap()
        .publications
        .is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_monthly_usage_counts_consumer_once_and_blocks_without_losing_identity() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let mut spec = publication_spec();
    spec.limits.monthly_bytes = Some(1000);
    let p = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "meter-publication-0001".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let a = create(&db, "meter-container-0001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "meter-mount-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    let target = db.store.cvm_nic_desired("n1").await.unwrap();
    let b = &target.bindings[0];
    let sample = NicTrafficSample {
        epoch: "a".repeat(64),
        observed_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        uploaded_bytes: 600,
        downloaded_bytes: 500,
    };
    let evidence = NicBindingReport {
        local_block: None,
        traffic: Some(sample),
        id: b.id.clone(),
        side: b.side,
        generation: b.generation,
        phase: b.phase,
        configured: true,
        reachable: Some(true),
        rx_bytes: 99999,
        tx_bytes: 99999,
        error: None,
    };
    let report = NicNodeReport {
        generation: target.generation,
        bindings: vec![evidence.clone()],
    };
    db.store
        .cvm_record_nic_report("n1", report.clone())
        .await
        .unwrap();
    db.store.cvm_record_nic_report("n1", report).await.unwrap();
    let next = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    let old = a.desired.spec.interfaces[1].allocated.as_ref().unwrap();
    let actual = next.desired.spec.interfaces[1].allocated.as_ref().unwrap();
    assert!(actual.quota_exceeded);
    assert!(!actual.blocked);
    assert_eq!(old.allocation_id, actual.allocation_id);
    assert_eq!(a.desired.spec.routing, next.desired.spec.routing);
    let state = db
        .store
        .cvm_nic_state(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    let usage = &state.usage[&a.desired.spec.interfaces[1].id];
    assert_eq!(usage.uploaded_bytes + usage.downloaded_bytes, 1100);
    assert!(usage.period.ends_with("-01"));
    let readings = db.store.cvm_readings(&actor_user("alice")).await.unwrap();
    let reading = readings
        .instances
        .iter()
        .find(|r| r.id == a.desired.id)
        .unwrap();
    assert_eq!(reading.month_used_bytes, Some(1100));
    assert_eq!(reading.month_limit_bytes, Some(1000));
    assert!(!reading.month_coverage_gap);
    assert_eq!(
        reading.month_observed_at_unix_millis,
        Some(usage.observed_at)
    );
    assert!(db
        .store
        .cvm_readings(&actor_user("bob"))
        .await
        .unwrap()
        .instances
        .is_empty());
    // A sample with the same epoch is not counted again by a staged binding.
    let target = db.store.cvm_nic_desired("n1").await.unwrap();
    let mut duplicate = evidence;
    duplicate.generation = target.bindings[0].generation;
    db.store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                generation: target.generation,
                bindings: vec![duplicate],
            },
        )
        .await
        .unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT sum(uploaded+downloaded)::bigint FROM cvm_nic_monthly_usage")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(total, 1100);
    // Moving historical usage out of the current UTC cycle models month rollover.
    sqlx::query("UPDATE cvm_nic_monthly_usage SET period=(period-interval '1 month')::date")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.cvm_nic_desired("n1").await.unwrap();
    let reset = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert!(
        !reset.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_version_reallocation_is_explicit_and_retains_old_resources_until_cleanup() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    let a = create(&db, "version-container-0001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "version-mount-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    for _ in 0..2 {
        acknowledge_network(&db, "n1").await;
        acknowledge_network(&db, "n2").await;
    }
    let mut spec = p.spec;
    spec.ipv4_pool = Some("10.121.0.0/24".parse().unwrap());
    spec.dns_suggestions = vec!["1.1.1.1".parse().unwrap()];
    let p = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            Some(&p.id),
            PublicationWrite {
                request_id: "version-publish-0002".into(),
                expected_revision: p.revision,
                spec,
            },
        )
        .await
        .unwrap();
    let a = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    let nic_id = a.desired.spec.interfaces[1].id.clone();
    assert_eq!(
        a.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .publication_revision,
        1
    );
    let denied = NicVersionChange {
        request_id: "version-change-denied-0001".into(),
        expected_generation: a.desired.generation,
        publication_revision: p.revision,
        reallocate: false,
        transport: None,
    };
    assert!(matches!(
        db.store
            .update_cvm_nic_version(&actor_user("alice"), &a.desired.id, &nic_id, denied)
            .await,
        Err(StoreError::Conflict(_))
    ));
    let stopped = db
        .store
        .command_cvm(
            &actor_user("alice"),
            &a.desired.id,
            CvmCommand {
                request_id: "version-stop-0001".into(),
                expected_generation: a.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &stopped).await;
    let updated = db
        .store
        .update_cvm_nic_version(
            &actor_user("alice"),
            &a.desired.id,
            &nic_id,
            NicVersionChange {
                request_id: "version-change-0002".into(),
                expected_generation: stopped.desired.generation,
                publication_revision: p.revision,
                reallocate: true,
                transport: None,
            },
        )
        .await
        .unwrap();
    let old = a.desired.spec.interfaces[1].allocated.as_ref().unwrap();
    let next = updated.desired.spec.interfaces[1]
        .allocated
        .as_ref()
        .unwrap();
    assert_eq!(old.mac, next.mac);
    assert_eq!(a.desired.spec.routing, updated.desired.spec.routing);
    assert_ne!(old.allocation_id, next.allocation_id);
    assert_ne!(old.ipv4, next.ipv4);
    let held: bool = sqlx::query_scalar("SELECT NOT released FROM cvm_nic_allocations WHERE id=$1")
        .bind(&old.allocation_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(held);
    acknowledge_guest(&db, &updated).await;
    for _ in 0..4 {
        acknowledge_network(&db, "n1").await;
        acknowledge_network(&db, "n2").await;
    }
    let released: bool = sqlx::query_scalar("SELECT released FROM cvm_nic_allocations WHERE id=$1")
        .bind(&old.allocation_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(released);
    let current = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert_eq!(
        current.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .allocation_id,
        next.allocation_id
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_bulk_versions_are_atomic_within_one_container() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let p = publish(&db).await;
    let a = create(&db, "bulk-container-0001").await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "bulk-mount-first-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    for _ in 0..2 {
        acknowledge_network(&db, "n1").await;
        acknowledge_network(&db, "n2").await;
    }
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "bulk-mount-second-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &a).await;
    for _ in 0..2 {
        acknowledge_network(&db, "n1").await;
        acknowledge_network(&db, "n2").await;
    }
    let mut spec = p.spec;
    spec.name = "Updated reusable network".into();
    let next = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            Some(&p.id),
            PublicationWrite {
                request_id: "bulk-publish-0002".into(),
                expected_revision: p.revision,
                spec,
            },
        )
        .await
        .unwrap();
    let updates = a
        .desired
        .spec
        .interfaces
        .iter()
        .filter(|nic| nic.allocated.is_some())
        .map(|nic| NicVersionUpdate {
            interface_id: nic.id.clone(),
            publication_revision: next.revision,
            reallocate: false,
            transport: None,
        })
        .collect::<Vec<_>>();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_bindings")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let mut wrong = updates.clone();
    wrong[1].interface_id = "nic-outside-container".into();
    assert!(db
        .store
        .update_cvm_nic_versions(
            &actor_user("alice"),
            &a.desired.id,
            NicVersionsChange {
                request_id: "bulk-invalid-0001".into(),
                expected_generation: a.desired.generation,
                updates: wrong
            }
        )
        .await
        .is_err());
    assert_eq!(
        count,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cvm_nic_bindings")
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    let request = NicVersionsChange {
        request_id: "bulk-apply-0002".into(),
        expected_generation: a.desired.generation,
        updates,
    };
    let updated = db
        .store
        .update_cvm_nic_versions(&actor_user("alice"), &a.desired.id, request.clone())
        .await
        .unwrap();
    assert_eq!(updated.desired.generation, a.desired.generation + 1);
    assert_eq!(updated.desired.spec.routing, a.desired.spec.routing);
    for (before, after) in a
        .desired
        .spec
        .interfaces
        .iter()
        .zip(&updated.desired.spec.interfaces)
    {
        if let (Some(before), Some(after)) = (&before.allocated, &after.allocated) {
            assert_eq!(before.allocation_id, after.allocation_id);
            assert_eq!(before.mac, after.mac);
            assert_eq!(after.publication_revision, next.revision);
        }
    }
    let replay = db
        .store
        .update_cvm_nic_versions(&actor_user("alice"), &a.desired.id, request)
        .await
        .unwrap();
    assert_eq!(replay.desired.generation, updated.desired.generation);
    let catalog = db.store.cvm_nic_catalog(&system_admin()).await.unwrap();
    assert_eq!(catalog.attachments.iter().filter(|n| n.attached).count(), 2);
    assert!(db
        .store
        .cvm_nic_catalog(&actor_user("alice"))
        .await
        .unwrap()
        .attachments
        .is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_remote_nic_ports_conflict_with_static_services_in_both_write_directions() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let mut spec = publication_spec();
    spec.transports
        .retain(|t| t.kind == TransportKind::Wireguard);
    spec.transports[0].port_start = 51821;
    spec.transports[0].port_end = 51821;
    let unavailable = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "static-conflict-publication-0001".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let a = create(&db, "port-conflict-container-0001").await;
    assert!(matches!(
        db.store
            .mount_cvm_nic(
                &actor_user("alice"),
                &a.desired.id,
                mount_request(&a, &unavailable, "port-conflict-mount-0001")
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    let p = publish(&db).await;
    let a = db
        .store
        .mount_cvm_nic(
            &actor_user("alice"),
            &a.desired.id,
            mount_request(&a, &p, "port-valid-mount-0001"),
        )
        .await
        .unwrap();
    let desired = db.store.cvm_nic_desired("n2").await.unwrap();
    let port = desired.bindings[0].port;
    let request: brocade_store::UpdateNodeRequest =
        serde_json::from_value(json!({"wg_listen_port":port})).unwrap();
    assert!(matches!(
        db.store.update_node(&system_admin(), "n2", request).await,
        Err(StoreError::Conflict(_))
    ));
    let stored: i32 = sqlx::query_scalar("SELECT wg_listen_port FROM nodes WHERE id='n2'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, 51821);
    let unchanged = db
        .store
        .cvm_instance(&actor_user("alice"), &a.desired.id)
        .await
        .unwrap();
    assert_eq!(unchanged.desired.generation, a.desired.generation);
}
