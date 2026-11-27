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
    sqlx::query("INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind) VALUES('n2','platform.acme','Network provider','198.51.100.20','10.66.0.2','fixture-private','fixture-public',51821,10086,true,true,'system')").execute(db.pool()).await.unwrap();
    for node in ["n1", "n2"] {
        db.store
            .cvm_record_nic_capabilities(
                node,
                NicNodeCapabilities {
                    entry_protocol_rules: true,
                    ssh_ports: Vec::new(),
                    contract: NIC_CONTRACT_VERSION,
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
                    entry_host_l3: false,
                    entry_xray: false,
                    entry_phantun: false,
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unchanged_idle_nic_observations_do_not_rewrite_the_node_row() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let capabilities: NicNodeCapabilities = serde_json::from_value(
        sqlx::query_scalar("SELECT capabilities FROM cvm_nic_nodes WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
    )
    .unwrap();
    let before: (String, String) =
        sqlx::query_as("SELECT observed_at::text,xmin::text FROM cvm_nic_nodes WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.store
        .cvm_record_nic_capabilities("n1", capabilities)
        .await
        .unwrap();
    let after: (String, String) =
        sqlx::query_as("SELECT observed_at::text,xmin::text FROM cvm_nic_nodes WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before);

    let report = NicNodeReport {
        generation: 1,
        bindings: Vec::new(),
        diagnostics: None,
    };
    db.store
        .cvm_record_nic_report("n1", report.clone())
        .await
        .unwrap();
    let before: (String, String) = sqlx::query_as(
        "SELECT report_received_at::text,xmin::text FROM cvm_nic_nodes WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!db
        .store
        .cvm_record_nic_report("n1", report.clone())
        .await
        .unwrap());
    let after: (String, String) = sqlx::query_as(
        "SELECT report_received_at::text,xmin::text FROM cvm_nic_nodes WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(after, before);

    let revision: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let deployment: i64 = sqlx::query_scalar("INSERT INTO deployments(revision_id,status,active,kind) VALUES($1,'canceled',false,'config') RETURNING id")
        .bind(revision)
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO deployment_targets(deployment_id,node_id,status) VALUES($1,'n1','canceled')",
    )
    .bind(deployment)
    .execute(db.pool())
    .await
    .unwrap();
    let mut dispatch = db.pool().begin().await.unwrap();
    // Match the production deployment-isolation lock. The report must wait for this transaction
    // before deciding that the empty observation is a duplicate; otherwise it can miss the
    // uncommitted dispatch and leave the target waiting for the next periodic report.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(0x6272_6f63_6973_6f6c_i64)
        .execute(&mut *dispatch)
        .await
        .unwrap();
    sqlx::query("INSERT INTO deployment_target_state(deployment_id,node_id,wave,disruptive,desired_structure,desired_grants,dispatched_at) VALUES($1,'n1',1,false,'{}','{}',now())")
        .bind(deployment)
        .execute(&mut *dispatch)
        .await
        .unwrap();
    let store = db.store.clone();
    let reporter = tokio::spawn(async move { store.cvm_record_nic_report("n1", report).await });
    wait_for_advisory_waiters(db.pool(), 1).await;
    dispatch.commit().await.unwrap();
    reporter.await.unwrap().unwrap();
    let after_dispatch: (String, String, bool) = sqlx::query_as(
        "SELECT report_received_at::text,xmin::text,report_received_at>=(SELECT max(dispatched_at) FROM deployment_target_state WHERE node_id='n1') FROM cvm_nic_nodes WHERE node_id='n1'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(after_dispatch.1, after.1);
    assert!(after_dispatch.2);
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
    ({
        supersede_cvm_fixture(db).await;
        &db.store
    })
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
    let instance = ({
        supersede_cvm_fixture(db).await;
        &db.store
    })
    .create_cvm(&system_admin(), request)
    .await
    .unwrap();
    acknowledge_guest(db, &instance).await;
    acknowledge_network(db, "n1").await;
    publish_cvm_intent(db).await;
    db.store
        .cvm_instance(&actor_user("alice"), &instance.desired.id)
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_route_match_selects_temporary_mounts_on_the_compiled_draft() {
    use brocade_store::cvm::CvmRouteMatchRequest;

    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let publication = publish(&db).await;
    let instance = create(&db, "match-temp-nic-create-0001").await;
    supersede_cvm_fixture(&db).await;
    let id = &instance.desired.id;
    let revision = instance.desired.spec.routing.revision;
    let mut draft = json!({
        "request_id":"match-temp-nic-request-0001",
        "expected_generation":instance.desired.generation,
        "mounts":[{"draft_id":"draft-first","publication_id":publication.id,"transport":"wireguard"}],
        "routes":{"expected_revision":revision,"routes":{
            "revision":revision,"dns":[],"priorities":[{"interface_id":"draft-first","ipv4":9}],
            "routes":[{"destination":"203.0.113.0/24","interface_id":"draft-first"},{"destination":"0.0.0.0/0","interface_id":"draft-first"}]
        }}
    });
    let request = |draft| {
        serde_json::from_value::<CvmRouteMatchRequest>(
            json!({"destination":"203.0.113.9","protocol":"tcp","port":443,"network_edit":draft}),
        )
        .unwrap()
    };
    let result = db
        .store
        .match_cvm_route(&system_admin(), id, request(draft.clone()))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"kind":"route","route_index":0,"priority":9,
        "route":{"destination":"203.0.113.0/24","interface_id":"draft-first"}})
    );
    // An implicit mount preset can also win; the response still uses the draft NIC identity.
    draft.as_object_mut().unwrap().remove("routes");
    let result = db
        .store
        .match_cvm_route(&system_admin(), id, request(draft))
        .await
        .unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(
        result["route"],
        json!({"destination":"0.0.0.0/0","interface_id":"draft-first"})
    );
    let after = db.store.cvm_instance(&system_admin(), id).await.unwrap();
    assert_eq!(after.desired, instance.desired);
    assert_eq!(after.revision_id, instance.revision_id);
    let allocations: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_allocations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(allocations, 0);
}

async fn create_native(db: &TestPg, key: &str) -> CvmInstance {
    let mut capabilities = super::baremetal_tests::native_catalogue();
    capabilities.managed_networks = true;
    capabilities.configure_network = true;
    capabilities.hot_attach = true;
    capabilities.hot_detach = true;
    let identity = capabilities
        .host_system
        .as_ref()
        .unwrap()
        .identity
        .as_ref()
        .unwrap()
        .clone();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let instance = ({
        supersede_cvm_fixture(db).await;
        &db.store
    })
    .adopt_cvm_host(
        &system_admin(),
        CvmAdopt {
            request_id: key.into(),
            tenant_id: "platform.acme".into(),
            user_id: Some("alice".into()),
            node_id: "n1".into(),
            name: "Running host".into(),
            identity,
        },
    )
    .await
    .unwrap();
    acknowledge_guest(db, &instance).await;
    db.store
        .cvm_instance(&actor_user("alice"), &instance.desired.id)
        .await
        .unwrap()
}

fn guest_evidence(instance: &CvmInstance) -> InstanceReport {
    let mut report = if instance.desired.spec.execution.is_cube() {
        evidence(instance)
    } else {
        super::baremetal_tests::native_evidence(instance)
    };
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
    ({
        publish_cvm_intent(db).await;
        &db.store
    })
    .cvm_record_report("n1", guest_evidence(instance))
    .await
    .unwrap();
}
async fn acknowledge_network(db: &TestPg, node: &str) {
    let desired = ({
        publish_cvm_intent(db).await;
        &db.store
    })
    .cvm_nic_desired(node)
    .await
    .unwrap();
    db.store
        .cvm_record_nic_report(
            node,
            NicNodeReport {
                diagnostics: None,
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
async fn cvm_remote_nic_provider_names_follow_publications_and_owned_allocations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','22222222-2222-4222-8222-222222222222')").execute(db.pool()).await.unwrap();
    let p = publish(&db).await;
    let mut other = publication_spec();
    other.provider_node = "n1".into();
    other.resource_domain = "bob-only-network".into();
    other.grants[0].user_id = "bob".into();
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .write_cvm_nic_publication(
        &system_admin(),
        None,
        PublicationWrite {
            request_id: "bob-network-names-0001".into(),
            expected_revision: 0,
            spec: other,
        },
    )
    .await
    .unwrap();
    for actor in [
        actor_user("alice"),
        AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into())),
        AdminContext::new(
            "tenant",
            AdminRole::TenantAdmin,
            Some("platform.acme".into()),
        ),
    ] {
        let catalog = db.store.cvm_nic_catalog(&actor).await.unwrap();
        assert_eq!(
            catalog.provider_names.get("n2").map(String::as_str),
            Some("Network provider")
        );
        assert!(catalog.providers.is_empty());
        assert!(catalog.attachments.is_empty());
        assert!(!catalog.can_manage);
        let json = serde_json::to_value(&catalog).unwrap();
        assert!(json["provider_names"]
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_string));
    }
    assert!(!db
        .store
        .cvm_nic_catalog(&actor_user("alice"))
        .await
        .unwrap()
        .provider_names
        .contains_key("n1"));
    assert!(!db
        .store
        .cvm_nic_catalog(&actor_user("bob"))
        .await
        .unwrap()
        .provider_names
        .contains_key("n2"));
    let outsider = AdminContext::new(
        "outside",
        AdminRole::TenantAdmin,
        Some("platform.outside".into()),
    );
    assert!(db
        .store
        .cvm_nic_catalog(&outsider)
        .await
        .unwrap()
        .provider_names
        .is_empty());
    let instance = create(&db, "provider-name-container-0001").await;
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &instance.desired.id,
        mount_request(&instance, &p, "provider-name-mount-0001"),
    )
    .await
    .unwrap();
    let mut revoked = p.spec.clone();
    revoked.grants.clear();
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .write_cvm_nic_publication(
        &system_admin(),
        Some(&p.id),
        PublicationWrite {
            request_id: "provider-name-revoke-0001".into(),
            expected_revision: p.revision,
            spec: revoked,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET name='Renamed network provider' WHERE id='n2'")
        .execute(db.pool())
        .await
        .unwrap();
    let alice = db
        .store
        .cvm_nic_catalog(&actor_user("alice"))
        .await
        .unwrap();
    assert!(alice.publications.is_empty());
    assert_eq!(
        alice.provider_names.get("n2").map(String::as_str),
        Some("Renamed network provider")
    );
    assert!(alice.providers.is_empty());
    assert!(!db
        .store
        .cvm_nic_catalog(&actor_user("bob"))
        .await
        .unwrap()
        .provider_names
        .contains_key("n2"));
    assert!(db
        .store
        .cvm_nic_catalog(&outsider)
        .await
        .unwrap()
        .provider_names
        .is_empty());
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
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
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
    let actor = system_admin();
    let (a, b) = tokio::join!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .mount_cvm_nic(&actor, &a.desired.id, first.clone()),
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .mount_cvm_nic(&actor, &b.desired.id, second)
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
    let replay = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(&system_admin(), &a.desired.id, first)
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &a.desired.id,
        mount_request(&a, &p, "mount-dependencies-0001"),
    )
    .await
    .unwrap();
    acknowledge_guest(&db, &a).await;
    let nic = a.desired.spec.interfaces[1].id.clone();
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .command_cvm(
            &system_admin(),
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
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
    assert!(({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
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
    let current = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .set_cvm_routes(
        &system_admin(),
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
    let detached = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .command_cvm(
        &system_admin(),
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
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
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
    let changed = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .change_cvm_nic_transport(
        &system_admin(),
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
    let desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n1")
    .await
    .unwrap();
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
                diagnostics: None,
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
                    diagnostics: None,
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
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
    ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
    let p = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &a.desired.id,
        mount_request(&a, &p, "meter-mount-0001"),
    )
    .await
    .unwrap();
    acknowledge_guest(&db, &a).await;
    let target = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n1")
    .await
    .unwrap();
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
        diagnostics: None,
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
    let target = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n1")
    .await
    .unwrap();
    let mut duplicate = evidence;
    duplicate.generation = target.bindings[0].generation;
    db.store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                diagnostics: None,
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
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n1")
    .await
    .unwrap();
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
    for native in [false, true] {
        let Some(db) = TestPg::start_if_enabled().await else {
            return;
        };
        setup(&db).await;
        let p = publish(&db).await;
        let a = if native {
            create_native(&db, "version-native-0001").await
        } else {
            create(&db, "version-container-0001").await
        };
        let a = ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .mount_cvm_nic(
            &system_admin(),
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
        let p = ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
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
            ({
                supersede_cvm_fixture(&db).await;
                &db.store
            })
            .update_cvm_nic_version(&system_admin(), &a.desired.id, &nic_id, denied)
            .await,
            Err(StoreError::Conflict(_))
        ));
        if !native {
            assert!(matches!(
                ({
                    supersede_cvm_fixture(&db).await;
                    &db.store
                })
                .update_cvm_nic_version(
                    &system_admin(),
                    &a.desired.id,
                    &nic_id,
                    NicVersionChange {
                        request_id: "version-running-cube-denied".into(),
                        expected_generation: a.desired.generation,
                        publication_revision: p.revision,
                        reallocate: true,
                        transport: None
                    }
                )
                .await,
                Err(StoreError::Conflict(_))
            ));
        }
        let stopped = if native {
            a.clone()
        } else {
            ({
                supersede_cvm_fixture(&db).await;
                &db.store
            })
            .command_cvm(
                &system_admin(),
                &a.desired.id,
                CvmCommand {
                    request_id: "version-stop-0001".into(),
                    expected_generation: a.desired.generation,
                    change: CvmChange::Stop,
                },
            )
            .await
            .unwrap()
        };
        acknowledge_guest(&db, &stopped).await;
        let updated = ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .update_cvm_nic_version(
            &system_admin(),
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
        assert_eq!(
            updated.desired.power,
            if native {
                PowerState::Running
            } else {
                PowerState::Stopped
            }
        );
        let old = a.desired.spec.interfaces[1].allocated.as_ref().unwrap();
        let next = updated.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap();
        assert_eq!(old.mac, next.mac);
        assert_eq!(a.desired.spec.routing, updated.desired.spec.routing);
        assert_ne!(old.allocation_id, next.allocation_id);
        assert_ne!(old.ipv4, next.ipv4);
        let held: bool =
            sqlx::query_scalar("SELECT NOT released FROM cvm_nic_allocations WHERE id=$1")
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
        let released: bool =
            sqlx::query_scalar("SELECT released FROM cvm_nic_allocations WHERE id=$1")
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
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
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
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
    let next = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
    assert!(({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .update_cvm_nic_versions(
        &system_admin(),
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
    let updated = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .update_cvm_nic_versions(&system_admin(), &a.desired.id, request.clone())
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
    let replay = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .update_cvm_nic_versions(&system_admin(), &a.desired.id, request)
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
    let unavailable = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
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
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .mount_cvm_nic(
            &system_admin(),
            &a.desired.id,
            mount_request(&a, &unavailable, "port-conflict-mount-0001")
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    let p = publish(&db).await;
    let a = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .mount_cvm_nic(
        &system_admin(),
        &a.desired.id,
        mount_request(&a, &p, "port-valid-mount-0001"),
    )
    .await
    .unwrap();
    let desired = ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_nic_desired("n2")
    .await
    .unwrap();
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

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_mixed_release_groups_carrier_peers_and_fences_provider_and_consumer() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let instance = create(&db, "release-carrier-create-01").await;
    let p = publish(&db).await;
    let mounted = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &instance.desired.id,
            NicMountRequest {
                request_id: "release-carrier-mount-01".into(),
                expected_generation: instance.desired.generation,
                publication_id: p.id,
                use_dns_suggestions: false,
                transport: TransportKind::Wireguard,
            },
        )
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_nic_desired("n1")
        .await
        .unwrap()
        .bindings
        .is_empty());
    assert!(db
        .store
        .cvm_nic_desired("n2")
        .await
        .unwrap()
        .bindings
        .is_empty());
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0].generation,
        instance.desired.generation
    );
    let r = mounted.revision_id.unwrap();
    let plan = db.store.plan_deployment(&system_admin(), r).await.unwrap();
    let a = plan.targets.iter().find(|t| t.node_id == "n1").unwrap();
    let b = plan.targets.iter().find(|t| t.node_id == "n2").unwrap();
    assert_eq!(
        a.wave, b.wave,
        "both halves must be in the same executable wave"
    );
    assert!(
        a.prerequisites.contains(&"n2".to_owned()) && b.prerequisites.contains(&"n1".to_owned())
    );
    publish_cvm_intent(&db).await;
    assert_eq!(
        db.store.cvm_nic_desired("n1").await.unwrap().bindings.len(),
        1
    );
    assert_eq!(
        db.store.cvm_nic_desired("n2").await.unwrap().bindings.len(),
        1
    );
    acknowledge_guest(&db, &mounted).await;
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    acknowledge_network(&db, "n1").await;
    acknowledge_network(&db, "n2").await;
    let status:String=sqlx::query_scalar("SELECT status FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(r as i64).fetch_one(db.pool()).await.unwrap();
    assert_eq!(status, "succeeded");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forced_node_retirement_deletes_hosted_cvm_and_only_waits_for_live_peer_cleanup() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let instance = create(&db, "force-retire-create-0001").await;
    let publication = publish(&db).await;
    let mounted = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &instance.desired.id,
            mount_request(&instance, &publication, "force-retire-mount-0001"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &mounted).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }

    let retired = db.store.abandon_node(&system_admin(), "n1").await.unwrap();
    assert_eq!(
        retired.lifecycle.phase,
        brocade_store::NodeLifecyclePhase::Abandoned
    );
    let stored = db
        .store
        .cvm_instance(&system_admin(), &mounted.desired.id)
        .await
        .unwrap();
    assert!(!stored.reserved);
    assert_eq!(stored.desired.power, PowerState::Deleted);
    assert_eq!(stored.status, ApplyStatus::Unknown);
    assert_eq!(
        stored.operation_kind.as_deref(),
        Some("force_node_retirement")
    );
    assert!(db
        .store
        .cvm_nic_desired("n1")
        .await
        .unwrap()
        .bindings
        .is_empty());

    let deployment = retired
        .deployment_id
        .expect("the surviving provider receives cleanup work");
    let detail = db
        .store
        .deployment_detail(&system_admin(), deployment, false)
        .await
        .unwrap();
    assert!(detail.targets.iter().all(|target| target.node_id != "n1"));
    let provider = detail
        .targets
        .iter()
        .find(|target| target.node_id == "n2")
        .expect("provider is the only live cleanup peer");
    let frozen: (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT b.phase,s.cvm_revision_id,
                (SELECT max(revision_id) FROM cvm_revision_nodes WHERE node_id='n2'),
                (SELECT revision_id FROM cvm_serving_nodes WHERE node_id='n2')
           FROM cvm_nic_bindings b
           JOIN deployment_target_state s ON s.deployment_id=$1 AND s.node_id='n2'
          WHERE b.instance_id=$2 ORDER BY b.created_at DESC LIMIT 1",
    )
    .bind(deployment)
    .bind(&mounted.desired.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(frozen.0, "withdrawing", "live binding must be withdrawing");
    assert_eq!(
        frozen.1, frozen.2,
        "deployment must freeze the newest CVM revision: {frozen:?}"
    );
    assert_ne!(
        frozen.2, frozen.3,
        "provider must still owe the forced withdrawal: {frozen:?}"
    );
    if provider.disruptive && provider.wave > 1 {
        db.store
            .confirm_deployment_wave(&system_admin(), deployment, provider.wave, None)
            .await
            .unwrap();
    }
    let claimed = db
        .store
        .claim_desired_for_node("n2")
        .await
        .unwrap()
        .expect("provider claims forced-retirement cleanup");
    db.store
        .report_target_result(applied_report(&claimed))
        .await
        .unwrap();
    let desired = db.store.cvm_nic_desired("n2").await.unwrap();
    assert_eq!(desired.bindings.len(), 1);
    assert_eq!(desired.bindings[0].phase, BindingPhase::Withdrawing);
    assert!(db
        .store
        .cvm_record_nic_report(
            "n2",
            NicNodeReport {
                diagnostics: None,
                generation: desired.generation,
                bindings: desired
                    .bindings
                    .into_iter()
                    .map(|binding| NicBindingReport {
                        local_block: None,
                        traffic: None,
                        id: binding.id,
                        side: binding.side,
                        generation: binding.generation,
                        phase: binding.phase,
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
        .unwrap());

    let cleanup: (bool, bool, bool) = sqlx::query_as(
        "SELECT b.cleaned,a.retired,a.released
           FROM cvm_nic_bindings b
           JOIN cvm_nic_allocations a ON a.id=b.allocation_id
          WHERE b.instance_id=$1 ORDER BY b.created_at DESC LIMIT 1",
    )
    .bind(&mounted.desired.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(cleanup, (true, true, true));
    let workspace = db.store.cvm_workspace(&system_admin()).await.unwrap();
    assert!(workspace.pending_revision_id.is_none());
    assert!(workspace.instances.iter().all(|item| !item.reserved));

    let repeated = db.store.abandon_node(&system_admin(), "n1").await.unwrap();
    assert_eq!(
        repeated.lifecycle.lifecycle_epoch,
        retired.lifecycle.lifecycle_epoch
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forced_node_retirement_does_not_wait_for_an_isolated_cvm_peer() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let instance = create(&db, "force-retire-isolated-create-01").await;
    let publication = publish(&db).await;
    let mounted = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &instance.desired.id,
            mount_request(&instance, &publication, "force-retire-isolated-mount-01"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &mounted).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    db.store
        .isolate_node(
            &system_admin(),
            "n2",
            IsolateNodeRequest {
                acknowledge_uncertain: true,
            },
        )
        .await
        .unwrap();

    let retired = db.store.abandon_node(&system_admin(), "n1").await.unwrap();
    if let Some(deployment) = retired.deployment_id {
        let detail = db
            .store
            .deployment_detail(&system_admin(), deployment, false)
            .await
            .unwrap();
        assert!(detail.targets.iter().all(|target| target.node_id != "n2"));
    }
    assert!(db
        .store
        .cvm_nic_desired("n2")
        .await
        .unwrap()
        .bindings
        .is_empty());
    let cleanup: (bool, bool, bool) = sqlx::query_as(
        "SELECT b.cleaned,a.retired,a.released
           FROM cvm_nic_bindings b
           JOIN cvm_nic_allocations a ON a.id=b.allocation_id
          WHERE b.instance_id=$1 ORDER BY b.created_at DESC LIMIT 1",
    )
    .bind(&mounted.desired.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(cleanup, (true, true, false));
    assert!(db
        .store
        .cvm_workspace(&system_admin())
        .await
        .unwrap()
        .pending_revision_id
        .is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_mixed_pending_instance_does_not_freeze_or_rewind_another_instances_runtime_quota() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let b = create(&db, "runtime-quota-running-01").await;
    let baseline:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('node',t.node_id,'status',t.status,'cvm',s.cvm_revision_id,'ready',s.cvm_machine_ready) FROM deployment_targets t JOIN deployment_target_state s ON s.deployment_id=t.deployment_id AND s.node_id=t.node_id JOIN deployments d ON d.id=t.deployment_id WHERE d.active=TRUE").fetch_all(db.pool()).await.unwrap();
    assert!(
        baseline.is_empty(),
        "initial worker rounds must settle: {baseline:?}"
    );
    let mut spec = publication_spec();
    spec.limits.monthly_bytes = Some(1000);
    let p = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "runtime-quota-publish-01".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let b = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &b.desired.id,
            mount_request(&b, &p, "runtime-quota-mount-01"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &b).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let pending = db
        .store
        .create_cvm(&system_admin(), create_request("runtime-quota-pending-01"))
        .await
        .unwrap();
    let revision = pending.revision_id.unwrap();
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    let binding = &desired.bindings[0];
    db.store
        .cvm_record_nic_report(
            "n1",
            NicNodeReport {
                generation: desired.generation,
                diagnostics: None,
                bindings: vec![NicBindingReport {
                    id: binding.id.clone(),
                    side: binding.side,
                    generation: binding.generation,
                    phase: binding.phase,
                    configured: true,
                    reachable: Some(true),
                    rx_bytes: 0,
                    tx_bytes: 0,
                    error: None,
                    local_block: None,
                    traffic: Some(NicTrafficSample {
                        epoch: "f".repeat(64),
                        observed_at: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        uploaded_bytes: 600,
                        downloaded_bytes: 500,
                    }),
                }],
            },
        )
        .await
        .unwrap();
    let serving = db.store.cvm_desired("n1").await.unwrap();
    assert!(!serving.instances.iter().any(|i| i.id == pending.desired.id));
    let current = serving
        .instances
        .iter()
        .find(|i| i.id == b.desired.id)
        .unwrap();
    assert!(current.generation > b.desired.generation);
    assert!(
        current.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded
    );
    let generation = current.generation;
    assert_eq!(
        db.store
            .cvm_instance(&actor_user("alice"), &b.desired.id)
            .await
            .unwrap()
            .revision_id,
        b.revision_id
    );
    let actual: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        actual as u64, revision,
        "operational quota does not create an unrelated global revision"
    );
    publish_cvm_intent(&db).await;
    let serving = db.store.cvm_desired("n1").await.unwrap();
    assert_eq!(
        serving
            .instances
            .iter()
            .find(|i| i.id == b.desired.id)
            .unwrap()
            .generation,
        generation,
        "approving the inherited snapshot cannot rewind an approved runtime generation"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_mixed_transport_fallback_does_not_complete_the_requested_release() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let item = create(&db, "release-fallback-instance-01").await;
    let mut spec = publication_spec();
    spec.limits.monthly_bytes = Some(1000);
    let p = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "fallback-metered-publication-01".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let item = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &item.desired.id,
            mount_request(&item, &p, "release-fallback-mount-01"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &item).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let nic = item.desired.spec.interfaces[1].id.clone();
    let switch = db
        .store
        .change_cvm_nic_transport(
            &system_admin(),
            &item.desired.id,
            &nic,
            NicTransportChange {
                request_id: "release-fallback-switch-01".into(),
                expected_generation: item.desired.generation,
                transport: TransportKind::Xray,
                rebuild: false,
            },
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &switch).await;
    let desired = db.store.cvm_nic_desired("n2").await.unwrap();
    let failed = desired
        .bindings
        .iter()
        .find(|b| b.offer.kind == TransportKind::Xray)
        .unwrap();
    db.store
        .cvm_record_nic_report(
            "n2",
            NicNodeReport {
                generation: desired.generation,
                diagnostics: None,
                bindings: vec![NicBindingReport {
                    id: failed.id.clone(),
                    side: failed.side,
                    generation: failed.generation,
                    phase: failed.phase,
                    configured: false,
                    reachable: Some(false),
                    rx_bytes: 0,
                    tx_bytes: 0,
                    error: Some("test transport cannot connect".into()),
                    local_block: None,
                    traffic: None,
                }],
            },
        )
        .await
        .unwrap();
    let order:i64=sqlx::query_scalar("SELECT id FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(switch.revision_id.unwrap() as i64).fetch_one(db.pool()).await.unwrap();
    let detail = db
        .store
        .deployment_detail(&system_admin(), order, false)
        .await
        .unwrap();
    assert_eq!(detail.status, "halted");
    assert_eq!(target_status(db.pool(), order, "n1").await, "failed-dirty");
    assert!(
        matches!(
            db.store.retry_target(&system_admin(), order, "n1").await,
            Err(StoreError::Unsupported(_))
        ),
        "a fallback cannot be reported as successful application of the failed transport"
    );
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, true))
        .await
        .unwrap();
    let operation:String=sqlx::query_scalar("SELECT o.kind FROM cvm_instances i JOIN cvm_operations o ON o.id=i.desired->>'operation_id' WHERE i.id=$1").bind(&item.desired.id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(operation, "network_traffic_quota");
    assert!(
        matches!(
            db.store.retry_target(&system_admin(), order, "n1").await,
            Err(StoreError::Unsupported(_))
        ),
        "quota generations must not erase the prior transport fallback"
    );
    db.store
        .cancel_deployment(&system_admin(), order)
        .await
        .unwrap();
    let recovered = db
        .store
        .cvm_instance(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    acknowledge_guest(&db, &recovered).await;
    for node in ["n1", "n2", "n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let stopped = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "fallback-compensating-stop-01".into(),
                expected_generation: recovered.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &stopped).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let status:String=sqlx::query_scalar("SELECT status FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(stopped.revision_id.unwrap() as i64).fetch_one(db.pool()).await.unwrap();
    assert_eq!(
        status, "succeeded",
        "a new compensating revision must remain publishable"
    );
}

async fn cvm_published_mounted(db: &TestPg) -> CvmInstance {
    setup(db).await;
    let input: CvmArtifactInput = serde_json::from_str(include_str!(
        "../../../brocade-cvm-compile/tests/golden/input.json"
    ))
    .unwrap();
    let mut capabilities = catalogue();
    capabilities.artifact_compiler = Some(CVM_ARTIFACT_COMPILER.into());
    capabilities.cube_storage_backend = Some("plain".into());
    capabilities.images[0].sha256 = input.image_manifest.sha256.clone();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    db.store
        .cvm_record_image_manifest("n1", input.image_manifest)
        .await
        .unwrap();
    let b = create(db, "review-metering-create-01").await;
    let mut spec = publication_spec();
    spec.limits.monthly_bytes = Some(1000);
    let p = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            None,
            PublicationWrite {
                request_id: "review-metering-publication-01".into(),
                expected_revision: 0,
                spec,
            },
        )
        .await
        .unwrap();
    let b = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &b.desired.id,
            mount_request(&b, &p, "review-metering-mount-01"),
        )
        .await
        .unwrap();
    acknowledge_guest(db, &b).await;
    for n in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(db, n).await;
    }
    b
}
fn cvm_published_report(desired: NicDesired, sample: bool) -> NicNodeReport {
    NicNodeReport {
        generation: desired.generation,
        diagnostics: None,
        bindings: desired
            .bindings
            .into_iter()
            .map(|b| NicBindingReport {
                id: b.id,
                side: b.side,
                generation: b.generation,
                phase: b.phase,
                configured: true,
                reachable: Some(true),
                rx_bytes: 0,
                tx_bytes: 0,
                error: None,
                local_block: None,
                traffic: sample.then(|| NicTrafficSample {
                    epoch: "f".repeat(64),
                    observed_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                    uploaded_bytes: 600,
                    downloaded_bytes: 500,
                }),
            })
            .collect(),
    }
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_published_pending_delete_keeps_metering_serving_binding() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let b = cvm_published_mounted(&db).await;
    db.store
        .command_cvm(
            &system_admin(),
            &b.desired.id,
            CvmCommand {
                request_id: "review-pending-delete-01".into(),
                expected_generation: b.desired.generation,
                change: CvmChange::Delete { delete_data: false },
            },
        )
        .await
        .unwrap();
    let serving = db.store.cvm_desired("n1").await.unwrap();
    assert_eq!(serving.instances[0].power, PowerState::Running);
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    assert_eq!(desired.bindings[0].phase, BindingPhase::Active);
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, true))
        .await
        .unwrap();
    let total: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(uploaded+downloaded),0)::bigint FROM cvm_nic_monthly_usage",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        total, 1100,
        "an unapproved deletion must not stop accounting for the active data plane"
    );
    let serving = db.store.cvm_desired_page("n1", 0).await.unwrap();
    let running = &serving.instances[0];
    assert_eq!(running.power, PowerState::Running);
    assert!(
        running.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded
    );
    brocade_cvm_compile::verify(running.artifacts.as_ref().unwrap(), running).unwrap();
    let network = db.store.cvm_nic_desired("n1").await.unwrap();
    assert_eq!(network.bindings[0].phase, BindingPhase::Active);
    assert!(network.bindings[0].nic.quota_exceeded);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_published_pending_stop_does_not_freeze_monthly_quota_reset() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let b = cvm_published_mounted(&db).await;
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, true))
        .await
        .unwrap();
    let blocked = db
        .store
        .cvm_instance(&actor_user("alice"), &b.desired.id)
        .await
        .unwrap();
    assert!(
        blocked.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded
    );
    acknowledge_guest(&db, &blocked).await;
    for n in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, n).await;
    }
    db.store
        .command_cvm(
            &system_admin(),
            &b.desired.id,
            CvmCommand {
                request_id: "review-pending-stop-01".into(),
                expected_generation: blocked.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE cvm_nic_monthly_usage SET period=(date_trunc('month',now() AT TIME ZONE 'UTC')-interval '1 month')::date").execute(db.pool()).await.unwrap();
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, false))
        .await
        .unwrap();
    let latest = db
        .store
        .cvm_instance(&actor_user("alice"), &b.desired.id)
        .await
        .unwrap();
    assert!(
        !latest.desired.spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded,
        "live ledger reset must have happened"
    );
    let serving = db.store.cvm_desired("n1").await.unwrap();
    assert_eq!(
        serving.instances[0].power,
        PowerState::Running,
        "draft stop stays unpublished"
    );
    assert!(
        !serving.instances[0].spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded,
        "serving guest policy must receive the new month without publishing the stop"
    );
    let published_generation = serving.instances[0].generation;
    assert!(
        latest.desired.generation > published_generation,
        "pending intent must stay ahead of the execution already sent to Agent"
    );
    let page = db.store.cvm_desired_page("n1", 0).await.unwrap();
    brocade_cvm_compile::verify(
        page.instances[0].artifacts.as_ref().unwrap(),
        &page.instances[0],
    )
    .unwrap();
    publish_cvm_intent(&db).await;
    let approved = db.store.cvm_desired_page("n1", 0).await.unwrap();
    assert_eq!(approved.instances[0].power, PowerState::Stopped);
    assert_eq!(approved.instances[0].generation, latest.desired.generation);
    brocade_cvm_compile::verify(
        approved.instances[0].artifacts.as_ref().unwrap(),
        &approved.instances[0],
    )
    .unwrap();
    acknowledge_guest(&db, &latest).await;
    for node in ["n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let status:String=sqlx::query_scalar("SELECT status FROM deployments WHERE revision_id=$1 AND kind='config' ORDER BY id DESC LIMIT 1").bind(latest.revision_id.unwrap() as i64).fetch_one(db.pool()).await.unwrap();
    assert_eq!(status, "succeeded");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_published_empty_network_receipt_unblocks_next_container_release() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let b = cvm_published_mounted(&db).await;
    let deleted = db
        .store
        .command_cvm(
            &system_admin(),
            &b.desired.id,
            CvmCommand {
                request_id: "empty-network-delete-01".into(),
                expected_generation: b.desired.generation,
                change: CvmChange::Delete { delete_data: true },
            },
        )
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    let mut report = guest_evidence(&deleted);
    report.runtime_id = None;
    report.network_absent = true;
    report.volume_deleted = true;
    report.volume_retained = false;
    for nic in &mut report.interfaces {
        nic.attached = false;
    }
    db.store.cvm_record_report("n1", report).await.unwrap();
    let final_network = db.store.cvm_nic_desired("n1").await.unwrap();
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(final_network, true))
        .await
        .unwrap();
    let terminal = db
        .store
        .cvm_instance(&actor_user("alice"), &deleted.desired.id)
        .await
        .unwrap();
    assert_eq!(
        terminal.status,
        ApplyStatus::Applied,
        "a final traffic sample must not reopen an already deleted instance"
    );
    assert_eq!(terminal.desired.generation, deleted.desired.generation);
    for node in ["n1", "n2", "n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let evidence:serde_json::Value=sqlx::query_scalar("SELECT observed_after->'cvm' FROM deployment_target_state s JOIN deployments d ON d.id=s.deployment_id WHERE d.revision_id=$1 AND s.node_id='n1' AND d.kind='config'").bind(deleted.revision_id.unwrap() as i64).fetch_one(db.pool()).await.unwrap();
    assert!(
        evidence["instances"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == deleted.desired.id),
        "network cleanup must retain the earlier terminal deletion receipt"
    );
    let instance = db
        .store
        .create_cvm(
            &system_admin(),
            create_request("empty-network-next-create-01"),
        )
        .await
        .unwrap();
    acknowledge_guest(&db, &instance).await;
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    assert!(desired.bindings.is_empty() && desired.fences.is_empty());
    assert!(desired.generation > 0);
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM deployments WHERE revision_id=$1 AND kind='config'")
            .bind(instance.revision_id.unwrap() as i64)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(target_status(db.pool(), id, "n1").await, "converging");
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, false))
        .await
        .unwrap();
    assert_eq!(target_status(db.pool(), id, "n1").await, "succeeded");
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_published_quota_uses_approved_limit_until_new_nic_version_is_published() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let b = cvm_published_mounted(&db).await;
    let interface = &b.desired.spec.interfaces[1];
    let id = &interface.allocated.as_ref().unwrap().publication_id;
    let p = db
        .store
        .cvm_nic_catalog(&system_admin())
        .await
        .unwrap()
        .publications
        .into_iter()
        .find(|p| &p.id == id)
        .unwrap();
    let mut spec = p.spec;
    spec.limits.monthly_bytes = Some(2000);
    let next = db
        .store
        .write_cvm_nic_publication(
            &system_admin(),
            Some(&p.id),
            PublicationWrite {
                request_id: "quota-pending-limit-publish-01".into(),
                expected_revision: p.revision,
                spec,
            },
        )
        .await
        .unwrap();
    db.store
        .update_cvm_nic_versions(
            &system_admin(),
            &b.desired.id,
            NicVersionsChange {
                request_id: "quota-pending-limit-apply-01".into(),
                expected_generation: b.desired.generation,
                updates: vec![NicVersionUpdate {
                    interface_id: interface.id.clone(),
                    publication_revision: next.revision,
                    reallocate: false,
                    transport: None,
                }],
            },
        )
        .await
        .unwrap();
    let desired = db.store.cvm_nic_desired("n1").await.unwrap();
    db.store
        .cvm_record_nic_report("n1", cvm_published_report(desired, true))
        .await
        .unwrap();
    let serving = db.store.cvm_desired_page("n1", 0).await.unwrap();
    let approved = serving.instances[0].spec.interfaces[1]
        .allocated
        .as_ref()
        .unwrap();
    assert_eq!(approved.limits.monthly_bytes, Some(1000));
    assert!(approved.quota_exceeded);
    brocade_cvm_compile::verify(
        serving.instances[0].artifacts.as_ref().unwrap(),
        &serving.instances[0],
    )
    .unwrap();
    let draft = db
        .store
        .cvm_instance(&actor_user("alice"), &b.desired.id)
        .await
        .unwrap();
    let pending = draft.desired.spec.interfaces[1].allocated.as_ref().unwrap();
    assert_eq!(pending.limits.monthly_bytes, Some(2000));
    assert!(!pending.quota_exceeded);
    assert!(
        db.store.cvm_nic_desired("n1").await.unwrap().bindings[0]
            .nic
            .quota_exceeded
    );
    publish_cvm_intent(&db).await;
    let desired = db.store.cvm_desired_page("n1", 0).await.unwrap();
    assert_eq!(desired.instances[0].generation, draft.desired.generation);
    assert!(
        !desired.instances[0].spec.interfaces[1]
            .allocated
            .as_ref()
            .unwrap()
            .quota_exceeded
    );
    let network = db.store.cvm_nic_desired("n1").await.unwrap();
    let next_binding = network
        .bindings
        .iter()
        .find(|b| b.nic.publication_revision == next.revision)
        .unwrap();
    assert!(
        !next_binding.nic.quota_exceeded,
        "the newly approved limit applies to the replacement binding"
    );
    acknowledge_guest(&db, &draft).await;
    for node in ["n1", "n2", "n1", "n2", "n1", "n2"] {
        acknowledge_network(&db, node).await;
    }
    let network = db.store.cvm_nic_desired("n1").await.unwrap();
    assert!(network
        .bindings
        .iter()
        .all(|b| b.nic.publication_revision == next.revision && !b.nic.quota_exceeded));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_routes_restores_both_host_rules_with_the_guest_routes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    let mounted = cvm_published_mounted(&db).await;
    let before = db
        .store
        .cvm_instance(&actor_user("alice"), &mounted.desired.id)
        .await
        .unwrap();
    assert!(!before.desired.spec.routing.routes.is_empty());
    let mut routes = before.desired.spec.routing.clone();
    routes.routes.clear();
    routes.priorities.clear();
    let pending = db
        .store
        .set_cvm_routes(
            &system_admin(),
            &before.desired.id,
            RoutesWrite {
                request_id: "undo-host-routes-stage-01".into(),
                expected_generation: before.desired.generation,
                expected_revision: routes.revision,
                routes,
            },
        )
        .await
        .unwrap();
    let restored = db
        .store
        .command_cvm(
            &system_admin(),
            &pending.desired.id,
            CvmCommand {
                request_id: "undo-host-routes-restore-01".into(),
                expected_generation: pending.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: pending.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        restored.desired.spec.routing.routes,
        before.desired.spec.routing.routes
    );
    assert!(restored.desired.spec.routing.revision > pending.desired.spec.routing.revision);
    publish_cvm_intent(&db).await;
    for node in ["n1", "n2"] {
        let network = db.store.cvm_nic_desired(node).await.unwrap();
        let bindings: Vec<_> = network
            .bindings
            .iter()
            .filter(|b| b.instance_id == restored.desired.id)
            .collect();
        assert_eq!(bindings.len(), 1, "{node} must retain its actual endpoint");
        assert_eq!(
            bindings[0].routing, restored.desired.spec.routing,
            "{node} firewall must agree with restored Guest routes"
        );
        assert_eq!(
            bindings[0].nic.allocation_id,
            before.desired.spec.interfaces[1]
                .allocated
                .as_ref()
                .unwrap()
                .allocation_id
        );
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_undo_unpublished_mount_releases_only_new_allocation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    setup(&db).await;
    let item = create(&db, "undo-nic-instance-01").await;
    let network = publish(&db).await;
    let mounted = db
        .store
        .mount_cvm_nic(
            &system_admin(),
            &item.desired.id,
            mount_request(&item, &network, "undo-nic-mount-request-01"),
        )
        .await
        .unwrap();
    let nic = mounted
        .desired
        .spec
        .interfaces
        .iter()
        .find_map(|nic| nic.allocated.as_ref())
        .unwrap();
    let allocation = nic.allocation_id.clone();
    let restored = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "undo-nic-command-request-01".into(),
                expected_generation: mounted.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: mounted.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        restored.desired.spec.interfaces,
        item.desired.spec.interfaces
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT released FROM cvm_nic_allocations WHERE id=$1")
            .bind(&allocation)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM cvm_nic_resources WHERE allocation_id=$1 AND NOT released"
        )
        .bind(&allocation)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        db.store.cvm_desired("n1").await.unwrap().instances[0]
            .spec
            .interfaces,
        item.desired.spec.interfaces
    );
}
