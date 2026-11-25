use super::*;

pub(super) fn native_catalogue() -> Capabilities {
    let mut capabilities = super::catalogue();
    capabilities.backend = "cube-test-contract".into();
    capabilities.host_system = Some(HostSystemInventory {
        os: "Debian 13".into(),
        primary_device: "eth0".into(),
        identity: Some(HostIdentity {
            machine: "a".repeat(64),
            root_filesystem: "b".repeat(64),
        }),
        terminal_user: Some("root".into()),
        resources: Some(Resources {
            vcpus: 2,
            memory_bytes: 256 * 1024 * 1024,
            root_disk_bytes: 6 * 1024 * 1024 * 1024,
        }),
        terminal_ready: true,
    });
    capabilities.network_sources.push(NetworkSource {
        id: "host".into(),
        name: "Host".into(),
        kind: NetworkKind::Public,
        description: String::new(),
        max_attachments: Some(1),
        supports_ipv6: Some(true),
    });
    capabilities
}
fn adopt(key: &str) -> CvmAdopt {
    CvmAdopt {
        request_id: key.into(),
        tenant_id: "platform.acme".into(),
        user_id: Some("alice".into()),
        node_id: "n1".into(),
        name: "existing OS".into(),
        identity: native_catalogue().host_system.unwrap().identity.unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn baremetal_chain_egress_uses_owner_credentials_and_preserves_undo_identity() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    prepare_native(&db).await;
    supersede_cvm_fixture(&db).await;
    let owner = system_admin();
    let item = db
        .store
        .adopt_cvm_host(&owner, adopt("native-chain-adopt-01"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", native_evidence(&item))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let unavailable = db
        .store
        .cvm_chain_catalog(&owner, Some(&item.desired.id))
        .await
        .unwrap();
    assert!(!unavailable.supported);
    assert!(unavailable.reason.unwrap().contains("更新 Agent"));
    let mut caps = native_catalogue();
    caps.managed_networks = true;
    caps.configure_network = true;
    caps.chain_egress = true;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    assert!(
        db.store
            .cvm_chain_catalog(&owner, Some(&item.desired.id))
            .await
            .unwrap()
            .supported
    );
    let routes: ContainerRoutes = serde_json::from_value(json!({"revision":0,"priorities":[{"chain_id":"chn-a1b2-c3d4","ipv4":10}],"routes":[{"destination":"0.0.0.0/0","chain_id":"chn-a1b2-c3d4"}],"dns":[]})).unwrap();
    let saved = db
        .store
        .set_cvm_routes(
            &owner,
            &item.desired.id,
            RoutesWrite {
                request_id: "native-chain-routes-01".into(),
                expected_generation: item.desired.generation,
                expected_revision: item.desired.spec.routing.revision,
                routes,
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.desired.spec.execution, item.desired.spec.execution);
    assert_eq!(saved.desired.spec.interfaces, item.desired.spec.interfaces);
    let undone = db
        .store
        .command_cvm(
            &owner,
            &item.desired.id,
            CvmCommand {
                request_id: "native-chain-undo-01".into(),
                expected_generation: saved.desired.generation,
                change: CvmChange::UndoPending {
                    expected_revision: saved.revision_id.unwrap(),
                },
            },
        )
        .await
        .unwrap();
    let mut restored_routes = item.desired.spec.routing.clone();
    restored_routes.revision = undone.desired.spec.routing.revision;
    assert_eq!(undone.desired.spec.routing, restored_routes);
    assert_eq!(undone.desired.spec.execution, item.desired.spec.execution);
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", native_evidence(&undone))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let mut routes = saved.desired.spec.routing.clone();
    routes.revision = undone.desired.spec.routing.revision;
    let saved = db
        .store
        .set_cvm_routes(
            &owner,
            &item.desired.id,
            RoutesWrite {
                request_id: "native-chain-routes-02".into(),
                expected_generation: undone.desired.generation,
                expected_revision: undone.desired.spec.routing.revision,
                routes,
            },
        )
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    let desired = db.store.cvm_desired("n1").await.unwrap();
    assert_eq!(desired.chain_clients.len(), 1);
    let client = &desired.chain_clients[0];
    assert_eq!(client.user_id, "alice");
    assert_eq!(client.instance_id, item.desired.id);
    let content: serde_json::Value = serde_json::from_str(&client.content).unwrap();
    assert_eq!(
        content["outbounds"][0]["streamSettings"]["sockopt"]["mark"],
        CHAIN_CLIENT_SOCKET_MARK
    );
    let mut report = native_evidence(&saved);
    report.network.as_mut().unwrap().chain_egress_configured = true;
    report.network.as_mut().unwrap().routing_revision = saved.desired.spec.routing.revision;
    report.chain_egress = vec![ChainClientReceipt {
        chain_id: client.chain_id.clone(),
        sha256: client.sha256.clone(),
        guard_sha256: brocade_core::hash::sha256_hex(client.guard.as_bytes()),
        configured: true,
        error: None,
    }];
    db.store.cvm_record_report("n1", report).await.unwrap();
    assert_eq!(
        db.store
            .cvm_instance(&owner, &item.desired.id)
            .await
            .unwrap()
            .status,
        ApplyStatus::Applied
    );
    let allocations: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_allocations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let volumes: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_volumes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!((allocations, volumes), (0, 0));
}
pub(super) fn native_evidence(item: &CvmInstance) -> InstanceReport {
    let mut report = evidence(item);
    let boot = "12345678-1234-1234-1234-123456789abc";
    report.host = Some(HostEvidence {
        identity: item.desired.spec.execution.host_identity().unwrap().clone(),
        boot_id: boot.into(),
    });
    report.runtime_id = Some(format!(
        "host-{}-{boot}",
        &brocade_core::hash::sha256_hex(item.desired.id.as_bytes())[..16]
    ));
    report.volume_id = None;
    if item.desired.power == PowerState::Deleted {
        report.runtime_id = None;
        report.access_address = None;
        report.interfaces.clear();
        report.network_absent = true;
    }
    report
}
async fn prepare_native(db: &TestPg) {
    prepare(db).await;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: native_catalogue(),
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn physical_host_adoption_is_scoped_idempotent_and_has_no_volume() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare_native(&db).await;
    for actor in [
        actor_user("alice"),
        AdminContext::new(
            "tenant",
            AdminRole::TenantAdmin,
            Some("platform.acme".into()),
        ),
    ] {
        assert!(matches!(
            ({
                supersede_cvm_fixture(&db).await;
                &db.store
            })
            .adopt_cvm_host(&actor, adopt("baremetal-adopt-denied-01"))
            .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    let owner = system_admin();
    let first = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .adopt_cvm_host(&owner, adopt("baremetal-adopt-replay-01"))
    .await
    .unwrap();
    let replay = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .adopt_cvm_host(&owner, adopt("baremetal-adopt-replay-01"))
    .await
    .unwrap();
    assert_eq!(first.desired, replay.desired);
    assert!(first.desired.spec.image.is_none());
    let volumes: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_volumes WHERE instance_id=$1")
        .bind(&first.desired.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(volumes, 0);
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .adopt_cvm_host(&owner, adopt("baremetal-adopt-second-01"))
        .await,
        Err(StoreError::Conflict(_))
    ));
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", native_evidence(&first))
    .await
    .unwrap();
    let mut routes = first.desired.spec.routing.clone();
    routes.dns = vec!["1.1.1.1".parse().unwrap()];
    assert!(matches!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .set_cvm_routes(
            &owner,
            &first.desired.id,
            RoutesWrite {
                request_id: "baremetal-dns-rejected-01".into(),
                expected_generation: first.desired.generation,
                expected_revision: routes.revision,
                routes
            }
        )
        .await,
        Err(StoreError::Unsupported(_))
    ));
    assert_eq!(
        db.store
            .cvm_instance(&owner, &first.desired.id)
            .await
            .unwrap()
            .desired,
        first.desired
    );
    let visible = db.store.cvm_workspace(&actor_user("alice")).await.unwrap();
    assert_eq!(
        visible.usage[0].disk_bytes,
        first.desired.spec.resources.root_disk_bytes
    );
    let wrong = actor_user("bob");
    assert!(db
        .store
        .cvm_instance(&wrong, &first.desired.id)
        .await
        .is_err());
    for (index, change) in [
        CvmChange::Start,
        CvmChange::Stop,
        CvmChange::Restart,
        CvmChange::Delete { delete_data: true },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            ({
                supersede_cvm_fixture(&db).await;
                &db.store
            })
            .command_cvm(
                &owner,
                &first.desired.id,
                CvmCommand {
                    request_id: format!("baremetal-invalid-op-{index:04}"),
                    expected_generation: 1,
                    change
                }
            )
            .await,
            Err(StoreError::Unsupported(_))
        ));
    }
    let cube = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .create_cvm(&system_admin(), create_request("baremetal-cube-coexist-01"))
    .await
    .unwrap();
    assert!(cube.desired.spec.execution.is_cube());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn baremetal_concurrent_claims_and_release_receipts_preserve_ownership() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare_native(&db).await;
    let owner = system_admin();
    let (a, b) = tokio::join!(
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .adopt_cvm_host(&owner, adopt("baremetal-concurrent-one")),
        ({
            supersede_cvm_fixture(&db).await;
            &db.store
        })
        .adopt_cvm_host(&owner, adopt("baremetal-concurrent-two"))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let item = a.or(b).unwrap();
    let mut bad = native_evidence(&item);
    bad.volume_deleted = true;
    assert!(({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", bad)
    .await
    .is_err());
    let mut bad = native_evidence(&item);
    bad.host.as_mut().unwrap().identity.machine = "c".repeat(64);
    assert!(({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", bad)
    .await
    .is_err());
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", native_evidence(&item))
    .await
    .unwrap();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: Capabilities::default(),
                terminal_sessions_used: None,
                error: Some("offline".into()),
            },
        )
        .await
        .unwrap();
    let released = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .command_cvm(
        &owner,
        &item.desired.id,
        CvmCommand {
            request_id: "baremetal-release-000001".into(),
            expected_generation: 1,
            change: CvmChange::ReleaseHost,
        },
    )
    .await
    .unwrap();
    assert!(!released.desired.delete_data);
    assert!(released.reserved);
    let mut bad = native_evidence(&released);
    bad.network_absent = false;
    assert!(({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", bad)
    .await
    .is_err());
    ({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", native_evidence(&released))
    .await
    .unwrap();
    assert!(!({
        publish_cvm_intent(&db).await;
        &db.store
    })
    .cvm_record_report("n1", native_evidence(&item))
    .await
    .unwrap());
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: native_catalogue(),
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let next = ({
        supersede_cvm_fixture(&db).await;
        &db.store
    })
    .adopt_cvm_host(&owner, adopt("baremetal-rebind-00000001"))
    .await
    .unwrap();
    assert_ne!(next.desired.id, item.desired.id);
}
