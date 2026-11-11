use super::*;
use brocade_deployment::cvm::*;
use brocade_store::cvm::*;

#[path = "cvm_network_quota.rs"]
mod network_quota_tests;
#[path = "cvm_remote_nic.rs"]
mod remote_nic_tests;
#[path = "cvm_host_policy.rs"]
mod host_policy_tests;

fn catalogue() -> Capabilities {
    Capabilities {
        managed_networks: true,
        network_quota: None,
        route_services: false,
        primary_ip_delivery: false,
        guest_terminal: true,
        guest_terminal_persistent: true,
        backend: "test-cube-contract".into(),
        resource_limits: Some(ResourceLimits {
            min_vcpus: 1,
            max_vcpus: 8,
            min_memory_bytes: 128 * 1024 * 1024,
            max_memory_bytes: 4 * 1024 * 1024 * 1024,
            memory_step_bytes: 64 * 1024 * 1024,
            min_root_disk_bytes: 3 * 1024 * 1024 * 1024,
            max_root_disk_bytes: 32 * 1024 * 1024 * 1024,
            root_disk_step_bytes: 1024 * 1024 * 1024,
        }),
        create: true,
        stop: true,
        start: true,
        delete: true,
        stable_storage: true,
        retain_on_delete: true,
        restart: true,
        reinstall: true,
        configure: true,
        configure_network: true,
        hot_attach: true,
        hot_detach: true,
        max_interfaces: 4,
        allocatable_vcpus: 8,
        allocatable_memory_bytes: 4 * 1024 * 1024 * 1024,
        allocatable_disk_bytes: 50 * 1024 * 1024 * 1024,
        images: vec![Image {
            id: "debian13".into(),
            name: "Debian".into(),
            version: "13".into(),
            architecture: "x86_64".into(),
            sha256: "a".repeat(64),
            boot_mode: "cube-oci".into(),
        }],
        network_sources: vec![
            NetworkSource {
                id: "public".into(),
                name: "Public".into(),
                kind: NetworkKind::Public,
                description: "mapped".into(),
                max_attachments: None,
                supports_ipv6: None,
            },
            NetworkSource {
                id: "internal".into(),
                name: "Own network".into(),
                kind: NetworkKind::Internal,
                description: "Brocade internal transport".into(),
                max_attachments: None,
                supports_ipv6: None,
            },
        ],
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_first_release_initializes_shared_schema() {
    let Some(db) = TestPg::start_if_enabled().await else { return; };
    db.store.migrate().await.unwrap();
    db.store.migrate().await.unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert!(check.pending_versions.is_empty());
    db.store.ensure_cvm_released().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'cvm_%'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(count, 33);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_lifecycle_epochs_and_shutdown_configuration_preserve_boundaries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let actor = actor_user("alice");
    let mut item = db
        .store
        .create_cvm(&actor, create_request("lifecycle-create-0001"))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let restart = CvmCommand {
        request_id: "lifecycle-restart-0001".into(),
        expected_generation: item.desired.generation,
        change: CvmChange::Restart,
    };
    item = db
        .store
        .command_cvm(&actor, &item.desired.id, restart.clone())
        .await
        .unwrap();
    assert_eq!(item.desired.restart_epoch, 1);
    let replay = db
        .store
        .command_cvm(&actor, &item.desired.id, restart)
        .await
        .unwrap();
    assert_eq!(replay.desired, item.desired);
    let mut stale = evidence(&item);
    stale.restart_epoch = 0;
    assert!(db.store.cvm_record_report("n1", stale).await.is_err());
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let configure = |generation, cpu| CvmCommand {
        request_id: format!("lifecycle-configure-{generation}-{cpu}"),
        expected_generation: generation,
        change: CvmChange::Configure {
            name: "edited".into(),
            vcpus: cpu,
            memory_bytes: 256 * 1024 * 1024,
        },
    };
    assert!(db
        .store
        .command_cvm(
            &actor,
            &item.desired.id,
            configure(item.desired.generation, 2)
        )
        .await
        .is_err());
    item = db
        .store
        .command_cvm(
            &actor,
            &item.desired.id,
            CvmCommand {
                request_id: "lifecycle-stop-00001".into(),
                expected_generation: item.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    let mut stopped = evidence(&item);
    stopped.runtime_id = None;
    stopped.interfaces.clear();
    stopped.network = None;
    stopped.network_absent = true;
    stopped.volume_retained = true;
    db.store.cvm_record_report("n1", stopped).await.unwrap();
    assert!(db
        .store
        .command_cvm(
            &actor,
            &item.desired.id,
            configure(item.desired.generation, 3)
        )
        .await
        .is_err());
    item = db
        .store
        .command_cvm(
            &actor,
            &item.desired.id,
            configure(item.desired.generation, 2),
        )
        .await
        .unwrap();
    assert_eq!(item.desired.spec.resources.vcpus, 2);
    assert_eq!(item.desired.power, PowerState::Stopped);
    assert_eq!(item.desired.spec.image.id, "debian13");
    let mut stopped = evidence(&item);
    stopped.runtime_id = None;
    stopped.interfaces.clear();
    stopped.network = None;
    stopped.network_absent = true;
    stopped.volume_retained = true;
    db.store.cvm_record_report("n1", stopped).await.unwrap();
    let reinstall = CvmCommand {
        request_id: "lifecycle-reinstall-0001".into(),
        expected_generation: item.desired.generation,
        change: CvmChange::Reinstall {
            image_id: "debian13".into(),
            ssh_public_key: item.desired.spec.ssh_public_key.clone(),
        },
    };
    assert!(matches!(
        db.store
            .command_cvm(&actor_user("bob"), &item.desired.id, reinstall.clone())
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let reset = db
        .store
        .command_cvm(&actor, &item.desired.id, reinstall)
        .await
        .unwrap();
    assert_eq!(reset.desired.reinstall_epoch, 1);
    assert_eq!(reset.desired.spec.resources, item.desired.spec.resources);
    assert_eq!(reset.desired.spec.interfaces, item.desired.spec.interfaces);
    let mut wrong = evidence(&reset);
    wrong.reinstall_epoch = 0;
    assert!(db.store.cvm_record_report("n1", wrong).await.is_err());
}
fn quota() -> CvmQuota {
    CvmQuota {
        host_network_quotas: vec![],
        tenant_id: "platform.acme".into(),
        user_id: "alice".into(),
        enabled: true,
        allowed_nodes: vec!["n1".into()],
        max_instances: 1,
        max_vcpus: 2,
        max_memory_bytes: 256 * 1024 * 1024,
        max_disk_bytes: 6 * 1024 * 1024 * 1024,
        max_interfaces: 4,
    }
}

fn contact() -> InterfaceRequest {
    InterfaceRequest {
        source_id: "public".into(),
        purpose: InterfacePurpose::Contact,
        default_ipv4: false,
        default_ipv6: false,
    }
}

fn create_request(key: &str) -> CvmCreate {
    CvmCreate {
        request_id: key.into(),
        tenant_id: "platform.acme".into(),
        user_id: "alice".into(),
        node_id: "n1".into(),
        name: "my-cvm".into(),
        image_id: "debian13".into(),
        resources: Resources { vcpus: 1, memory_bytes: 128 * 1024 * 1024, root_disk_bytes: 3 * 1024 * 1024 * 1024 },
        ssh_public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEiBcpQEzqzjADp8VXvOigHnqLh5L5xP76cwoCD8YMK5 fixture".into(),
        interfaces: vec![contact()],
    }
}
fn actor_user(user: &str) -> AdminContext {
    AdminContext::from_authenticated(&brocade_store::AuthenticatedAdmin {
        operator_id: format!("user-{user}"),
        role: AdminRole::User,
        tenant_scope: Some("platform.acme".into()),
        token_prefix: None,
        self_user: Some(brocade_store::AuthenticatedUser {
            tenant_id: "platform.acme".into(),
            user_id: user.into(),
        }),
    })
}

fn evidence(instance: &CvmInstance) -> InstanceReport {
    let d = &instance.desired;
    InstanceReport {
        restart_epoch: d.restart_epoch,
        reinstall_epoch: d.reinstall_epoch,
        id: d.id.clone(),
        operation_id: d.operation_id.clone(),
        generation: d.generation,
        spec_sha256: d.spec_sha256.clone(),
        status: ApplyStatus::Applied,
        power: Some(d.power),
        runtime_id: Some("cube-runtime-01".into()),
        volume_id: Some("cube-volume-01".into()),
        access_address: (d.power == PowerState::Running).then(|| "192.0.2.10".into()),
        network: Some(NetworkObservation {
            routing_revision: d.spec.routing.revision,
            contact_route_preserved: true,
            business_fallback_blocked: true,
            default_ipv4_interface: d
                .spec
                .interfaces
                .iter()
                .find(|nic| nic.connection.default_ipv4)
                .map(|nic| nic.id.clone()),
            default_ipv6_interface: d
                .spec
                .interfaces
                .iter()
                .find(|nic| nic.connection.default_ipv6)
                .map(|nic| nic.id.clone()),
        }),
        interfaces: d
            .spec
            .interfaces
            .iter()
            .enumerate()
            .map(|(i, nic)| InterfaceObservation {
                id: nic.id.clone(),
                device: format!("eth{i}"),
                mac: format!("02:00:00:00:00:{i:02x}"),
                addresses: vec![format!("10.88.{i}.2/24")],
                attached: true,
            })
            .collect(),
        detached_interfaces: vec![],
        network_absent: false,
        volume_retained: false,
        volume_deleted: false,
        error: None,
    }
}
async fn prepare(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: catalogue(),
                error: None,
            },
        )
        .await
        .unwrap();
    db.store
        .set_cvm_quota(&system_admin(), quota())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_idempotency_capacity_and_owner_boundaries() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let request = create_request("request-create-00000001");
    let a = db
        .store
        .create_cvm(&actor_user("alice"), request.clone())
        .await
        .unwrap();
    let replay = db
        .store
        .create_cvm(&actor_user("alice"), request)
        .await
        .unwrap();
    assert_eq!(a.desired.id, replay.desired.id);
    assert!(matches!(
        db.store
            .create_cvm(
                &actor_user("bob"),
                create_request("request-crossuser-00001")
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("request-create-00000002")
        )
        .await
        .is_err());
    assert!(db
        .store
        .cvm_instance(&actor_user("bob"), &a.desired.id)
        .await
        .is_err());
    assert!(db
        .store
        .cvm_workspace(&actor_user("bob"))
        .await
        .unwrap()
        .instances
        .is_empty());
    assert_eq!(
        db.store
            .cvm_workspace(&actor_user("alice"))
            .await
            .unwrap()
            .instances
            .len(),
        1
    );
    assert_eq!(a.desired.spec.resources, create_request("unused").resources);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_instances")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_concurrent_requests_only_reserve_the_last_slot_once() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let actor = actor_user("alice");
    let (a, b) = tokio::join!(
        db.store
            .create_cvm(&actor, create_request("concurrent-request-000001")),
        db.store
            .create_cvm(&actor, create_request("concurrent-request-000002"))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let volumes: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_volumes")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(volumes, 1);
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_requires_real_interface_and_version_evidence_and_retains_reservations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("evidence-request-000001"))
        .await
        .unwrap();
    let mut bad = evidence(&item);
    bad.interfaces.clear();
    assert!(db.store.cvm_record_report("n1", bad).await.is_err());
    assert!(db
        .store
        .cvm_record_report("other-machine", evidence(&item))
        .await
        .is_err());
    assert!(db
        .store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap());
    let mut q = quota();
    q.enabled = false;
    assert!(db.store.set_cvm_quota(&system_admin(), q).await.is_err());
    let before = db
        .store
        .cvm_instance(&system_admin(), &item.desired.id)
        .await
        .unwrap();
    let attached = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "attach-request-0000001".into(),
                expected_generation: 1,
                change: CvmChange::Restart,
            },
        )
        .await
        .unwrap();
    assert_eq!(attached.desired.spec.interfaces.len(), 1);
    assert_eq!(attached.desired.generation, 2);
    assert!(!db
        .store
        .cvm_record_report("n1", evidence(&before))
        .await
        .unwrap());
    assert!(db
        .store
        .cvm_record_report("n1", evidence(&attached))
        .await
        .unwrap());
    // A managed contact interface can never be detached.
    assert!(db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "contact-detach-denied-0001".into(),
                expected_generation: attached.desired.generation,
                change: CvmChange::Detach {
                    interface_id: attached.desired.spec.interfaces[0].id.clone()
                }
            }
        )
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_delete_needs_cleanup_proof_and_keeps_retained_disk_charged() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("delete-create-0000001"))
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let deleted = db
        .store
        .command_cvm(
            &system_admin(),
            &item.desired.id,
            CvmCommand {
                request_id: "delete-request-000001".into(),
                expected_generation: 1,
                change: CvmChange::Delete { delete_data: false },
            },
        )
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_record_report("n1", evidence(&deleted))
        .await
        .is_err());
    let mut report = evidence(&deleted);
    report.runtime_id = None;
    report.interfaces.clear();
    report.network_absent = true;
    report.volume_retained = true;
    db.store.cvm_record_report("n1", report).await.unwrap();
    let mut q = quota();
    q.max_disk_bytes = 0;
    assert!(db.store.set_cvm_quota(&system_admin(), q).await.is_err());
    assert!(db
        .store
        .cvm_desired("n1")
        .await
        .unwrap()
        .instances
        .is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_one_image_is_independent_of_cpu_memory_and_disk_resources() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let mut q = quota();
    q.max_instances = 2;
    q.max_vcpus = 4;
    q.max_memory_bytes = 1024 * 1024 * 1024;
    q.max_disk_bytes = 20 * 1024 * 1024 * 1024;
    db.store.set_cvm_quota(&system_admin(), q).await.unwrap();
    let small = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("independent-image-small-01"),
        )
        .await
        .unwrap();
    let mut request = create_request("independent-image-large-01");
    request.resources = Resources {
        vcpus: 3,
        memory_bytes: 256 * 1024 * 1024,
        root_disk_bytes: 8 * 1024 * 1024 * 1024,
    };
    let large = db
        .store
        .create_cvm(&actor_user("alice"), request)
        .await
        .unwrap();
    assert_eq!(small.desired.spec.image, large.desired.spec.image);
    assert_eq!(large.desired.spec.resources.vcpus, 3);
    assert_eq!(large.desired.spec.resources.memory_bytes, 256 * 1024 * 1024);
    assert_eq!(
        large.desired.spec.resources.root_disk_bytes,
        8 * 1024 * 1024 * 1024
    );
    let spec = serde_json::to_value(large.desired.spec).unwrap();
    assert!(spec.get("template").is_none());
    for field in ["vcpus", "memory_bytes", "root_disk_bytes", "profile_id"] {
        assert!(spec["image"].get(field).is_none());
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_tenant_admin_sees_explicitly_granted_shared_pool_and_readonly_detail_is_redacted() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    sqlx::query("INSERT INTO tenants(id,name) VALUES ('platform','Shared pool owner')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET tenant_id='platform' WHERE id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let admin = AdminContext::new(
        "tenant-admin",
        AdminRole::TenantAdmin,
        Some("platform.acme".into()),
    );
    let workspace = db.store.cvm_workspace(&admin).await.unwrap();
    assert_eq!(workspace.hosts.len(), 1);
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("shared-pool-create-00001"),
        )
        .await
        .unwrap();
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    let reader = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    let detail = db
        .store
        .cvm_instance(&reader, &item.desired.id)
        .await
        .unwrap();
    assert!(detail.desired.spec.ssh_public_key.is_empty());
    let report = detail.report.unwrap();
    assert!(
        report.runtime_id.is_none()
            && report.volume_id.is_none()
            && report.access_address.is_none()
    );
    assert!(report
        .interfaces
        .iter()
        .all(|nic| nic.addresses.is_empty() && nic.mac.is_empty()));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_guest_reads_are_scoped_redacted_and_cannot_write() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("guest-create-00000001"),
        )
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap());
    let guest = AdminContext::new(
        brocade_store::PUBLIC_OPERATOR_ID,
        AdminRole::Readonly,
        Some("platform".into()),
    );
    let workspace = db.store.cvm_workspace(&guest).await.unwrap();
    assert!(!workspace.can_write && !workspace.can_manage_quotas && workspace.redacted);
    assert_eq!(workspace.instances.len(), 1);
    assert_eq!(workspace.hosts.len(), 1);
    assert!(!workspace.hosts[0].can_manage_terminal);
    assert_eq!(workspace.quotas.len(), 1);
    assert!(workspace
        .owners
        .iter()
        .all(|owner| owner.tenant_id.starts_with("platform.")));
    for instance in [
        workspace.instances[0].clone(),
        db.store
            .cvm_instance(&guest, &item.desired.id)
            .await
            .unwrap(),
    ] {
        assert!(instance.desired.spec.ssh_public_key.is_empty());
        let report = instance.report.unwrap();
        assert!(
            report.runtime_id.is_none()
                && report.volume_id.is_none()
                && report.access_address.is_none()
        );
        assert!(report
            .interfaces
            .iter()
            .all(|nic| nic.addresses.is_empty() && nic.mac.is_empty()));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert_eq!(
        db.store
            .cvm_record_observations(
                "n1",
                vec![InstanceObservation {
                    id: item.desired.id.clone(),
                    generation: item.desired.generation,
                    observed_at_unix_millis: now,
                    runtime_id: Some("cube-runtime-01".into()),
                    runtime_state: ObservedRuntimeState::Running,
                    guest_state: Reachability::Reachable,
                    ssh_state: Reachability::Reachable,
                    metrics: None,
                    error: None,
                    services: vec![],
                }]
            )
            .await
            .unwrap(),
        1
    );
    let monitoring = db
        .store
        .cvm_monitoring(&guest, &item.desired.id)
        .await
        .unwrap();
    assert!(monitoring.health.unwrap().runtime_id.is_none());
    assert_eq!(
        db.store
            .cvm_monitoring(&actor_user("alice"), &item.desired.id)
            .await
            .unwrap()
            .health
            .unwrap()
            .runtime_id
            .as_deref(),
        Some("cube-runtime-01")
    );
    assert!(db
        .store
        .cvm_nic_state(&guest, &item.desired.id)
        .await
        .is_ok());
    assert!(matches!(
        db.store
            .create_cvm(&guest, create_request("guest-denied-00000001"))
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store
            .command_cvm(
                &guest,
                &item.desired.id,
                CvmCommand {
                    request_id: "guest-command-0000001".into(),
                    expected_generation: item.desired.generation,
                    change: CvmChange::Stop,
                }
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store.set_cvm_quota(&guest, quota()).await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store.cvm_terminal_target(&guest, &item.desired.id).await,
        Err(StoreError::Forbidden(_))
    ));

    let outside = AdminContext::new(
        brocade_store::PUBLIC_OPERATOR_ID,
        AdminRole::Readonly,
        Some("platform.other".into()),
    );
    let workspace = db.store.cvm_workspace(&outside).await.unwrap();
    assert!(
        workspace.instances.is_empty()
            && workspace.hosts.is_empty()
            && workspace.quotas.is_empty()
            && workspace.owners.is_empty()
    );
    assert!(matches!(
        db.store.cvm_instance(&outside, &item.desired.id).await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store.cvm_monitoring(&outside, &item.desired.id).await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(matches!(
        db.store.cvm_nic_state(&outside, &item.desired.id).await,
        Err(StoreError::Forbidden(_))
    ));
}

fn deleted_evidence(instance: &CvmInstance) -> InstanceReport {
    let mut report = evidence(instance);
    report.runtime_id = None;
    report.volume_id = None;
    report.access_address = None;
    report.interfaces.clear();
    report.network_absent = true;
    report.volume_deleted = true;
    report
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_cleanup_survives_inventory_failure_and_expiry_without_new_allocations() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let actor = system_admin();
    let mut caps = catalogue();
    caps.stable_storage = false;
    caps.retain_on_delete = false;
    caps.restart = false;
    caps.reinstall = false;
    caps.configure = false;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps.clone(),
                error: None,
            },
        )
        .await
        .unwrap();
    for (index, failure) in [true, false].into_iter().enumerate() {
        let item = db
            .store
            .create_cvm(
                &actor,
                create_request(&format!("cleanup-create-{index:08}")),
            )
            .await
            .unwrap();
        db.store
            .cvm_record_report("n1", evidence(&item))
            .await
            .unwrap();
        if failure {
            db.store
                .cvm_record_inventory(
                    "n1",
                    InventoryReport {
                        capabilities: Capabilities::default(),
                        error: Some("bridge unavailable".into()),
                    },
                )
                .await
                .unwrap();
        } else {
            sqlx::query(
                "UPDATE cvm_hosts SET observed_at=now()-interval '10 minutes' WHERE node_id='n1'",
            )
            .execute(db.pool())
            .await
            .unwrap();
        }
        let workspace = db.store.cvm_workspace(&actor).await.unwrap();
        assert!(!workspace.hosts[0].fresh);
        assert!(workspace.hosts[0].capabilities.delete);
        let error = db
            .store
            .create_cvm(
                &actor,
                create_request(&format!("cleanup-denied-{index:08}")),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("机器能力已过期"));
        assert!(
            db.store
                .command_cvm(
                    &actor,
                    &item.desired.id,
                    CvmCommand {
                        request_id: format!("cleanup-retain-{index:08}"),
                        expected_generation: 1,
                        change: CvmChange::Delete { delete_data: false },
                    }
                )
                .await
                .is_err(),
            "the last verified retention policy must remain enforced"
        );
        let deleted = db
            .store
            .command_cvm(
                &actor,
                &item.desired.id,
                CvmCommand {
                    request_id: format!("cleanup-delete-{index:08}"),
                    expected_generation: 1,
                    change: CvmChange::Delete { delete_data: true },
                },
            )
            .await
            .unwrap();
        db.store
            .cvm_record_report("n1", deleted_evidence(&deleted))
            .await
            .unwrap();
        db.store
            .cvm_record_inventory(
                "n1",
                InventoryReport {
                    capabilities: caps.clone(),
                    error: None,
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_terminal_requires_write_scope_and_current_running_incarnation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    db.store
        .set_cvm_terminal_policy(
            &system_admin(),
            "n1",
            TerminalPolicyUpdate {
                enabled: true,
                expected_revision: 0,
            },
        )
        .await
        .unwrap();
    let instance = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("terminal-create-00001"),
        )
        .await
        .unwrap();
    let id = &instance.desired.id;
    assert!(db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .is_err());
    assert!(db
        .store
        .cvm_record_report("n1", evidence(&instance))
        .await
        .unwrap());
    let (node, target) = db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .unwrap();
    assert_eq!(node, "n1");
    assert_eq!(target.runtime_id, "cube-runtime-01");
    assert!(matches!(
        db.store.cvm_terminal_target(&actor_user("bob"), id).await,
        Err(StoreError::Forbidden(_))
    ));
    let reader = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    assert!(matches!(
        db.store.cvm_terminal_target(&reader, id).await,
        Err(StoreError::Forbidden(_))
    ));
    let other_admin = AdminContext::new(
        "other-admin",
        AdminRole::TenantAdmin,
        Some("platform.other".into()),
    );
    assert!(db
        .store
        .cvm_terminal_target(&other_admin, id)
        .await
        .is_err());
    // A network observation error must not prevent maintenance through vsock.
    sqlx::query("UPDATE cvm_hosts SET error='network unavailable' WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .is_ok());
    sqlx::query(
        "UPDATE cvm_hosts SET capabilities=capabilities-'guest_terminal' WHERE node_id='n1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .is_err());
    sqlx::query("UPDATE cvm_hosts SET capabilities=jsonb_set(capabilities,'{guest_terminal}','false') WHERE node_id='n1'")
        .execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.store.cvm_terminal_target(&actor_user("alice"), id).await,
        Err(StoreError::Unsupported(_))
    ));
    sqlx::query("UPDATE cvm_hosts SET capabilities=jsonb_set(capabilities,'{guest_terminal}','true') WHERE node_id='n1'").execute(db.pool()).await.unwrap();
    sqlx::query(
        "UPDATE cvm_instances SET desired=jsonb_set(desired,'{restart_epoch}','1') WHERE id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .is_err());
    sqlx::query(
        "UPDATE cvm_instances SET desired=jsonb_set(desired,'{restart_epoch}','0') WHERE id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE cvm_instances SET desired=jsonb_set(desired,'{power}','\"stopped\"') WHERE id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(db
        .store
        .cvm_terminal_target(&actor_user("alice"), id)
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_independent_observations_survive_failed_inventory_and_preserve_intents() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("independent-monitor-001"),
        )
        .await
        .unwrap();
    let mut report = evidence(&item);
    report.status = ApplyStatus::Unknown;
    report.error = Some("network configuration failed".into());
    db.store.cvm_record_report("n1", report).await.unwrap();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: catalogue(),
                error: Some("gateway unavailable".into()),
            },
        )
        .await
        .unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(i) FROM cvm_instances i WHERE id=$1")
            .bind(&item.desired.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let targets = db.store.cvm_observation_targets("n1").await.unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, item.desired.id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let base = now / 60_000 * 60_000 - 60_000 + 5_000;
    let mut m = GuestMetrics {
        diagnostics: Some(GuestDiagnostics {
            cpus: vec![CpuCounters {
                cpu: -1,
                ticks: [100, 0, 100, 800, 0, 0, 0, 0],
            }],
            vm: [("oom_kill".into(), 0), ("pgmajfault".into(), 3)].into(),
            tcp: [("CurrEstab".into(), 2), ("RetransSegs".into(), 2)].into(),
            ..Default::default()
        }),
        observed_at_unix_millis: base,
        boot_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
        uptime_millis: 100_000,
        cpu_total_ticks: 1000,
        cpu_busy_ticks: 200,
        memory_total_bytes: 100_000_000,
        memory_available_bytes: 40_000_000,
        root_total_bytes: 3_000_000_000,
        root_used_bytes: 1_000_000_000,
        interfaces: vec![InterfaceCounters {
            id: item.desired.spec.interfaces[0].id.clone(),
            device: "eth0".into(),
            ifindex: 2,
            rx_bytes: 1000,
            tx_bytes: 2000,
            rx_packets: Some(10),
            tx_packets: Some(20),
            rx_dropped: Some(0),
            tx_dropped: Some(0),
            rx_errors: Some(0),
            tx_errors: Some(0),
            carrier: Some(true),
        }],
    };
    let observation = |m: GuestMetrics| InstanceObservation {
        id: item.desired.id.clone(),
        generation: item.desired.generation,
        observed_at_unix_millis: m.observed_at_unix_millis,
        runtime_id: Some("cube-runtime-01".into()),
        runtime_state: ObservedRuntimeState::Running,
        guest_state: Reachability::Reachable,
        ssh_state: Reachability::Reachable,
        metrics: Some(m),
        error: None,
        services: vec![],
    };
    assert_eq!(
        db.store
            .cvm_record_observations("n1", vec![observation(m.clone())])
            .await
            .unwrap(),
        1
    );
    for (busy, available) in [(20, 20_000_000), (60, 30_000_000)] {
        m.observed_at_unix_millis += 15_000;
        m.uptime_millis += 15_000;
        m.cpu_total_ticks += 100;
        m.cpu_busy_ticks += busy;
        m.memory_available_bytes = available;
        m.interfaces[0].rx_packets = Some(m.interfaces[0].rx_packets.unwrap() + 30);
        m.interfaces[0].rx_bytes += 15000;
        let d = m.diagnostics.as_mut().unwrap();
        d.cpus[0].ticks[0] += busy;
        d.cpus[0].ticks[3] += 100 - busy;
        *d.vm.get_mut("oom_kill").unwrap() += 1;
        *d.tcp.get_mut("RetransSegs").unwrap() += 2;
        assert_eq!(
            db.store
                .cvm_record_observations("n1", vec![observation(m.clone())])
                .await
                .unwrap(),
            1
        );
    }
    assert_eq!(
        db.store
            .cvm_record_observations("n1", vec![observation(m.clone())])
            .await
            .unwrap(),
        0
    );
    let view = db
        .store
        .cvm_monitoring_range(&actor_user("alice"), &item.desired.id, 86400)
        .await
        .unwrap();
    assert_eq!(view.state, "live");
    assert_eq!(view.samples.len(), 1);
    let minute = &view.samples[0];
    assert_eq!(minute.cpu_usage_basis_points, Some(4000));
    assert_eq!(minute.cpu_peak_basis_points, Some(6000));
    assert_eq!(minute.memory_available_min_bytes, Some(20_000_000));
    assert_eq!(minute.interfaces[0].rx_pps, Some(2.0));
    assert_eq!(minute.diagnostics.as_ref().unwrap().oom_kills, Some(2));
    assert_eq!(minute.diagnostics.as_ref().unwrap().tcp["RetransSegs"], 4);
    let readings = db.store.cvm_readings(&actor_user("alice")).await.unwrap();
    assert_eq!(readings.instances.len(), 1);
    let card = &readings.instances[0];
    assert_eq!(card.id, item.desired.id);
    assert_eq!(card.state, "live");
    assert_eq!(
        card.latest.as_ref().unwrap().cpu_usage_basis_points,
        Some(6000)
    );
    assert_eq!(card.samples.len(), 1);
    assert!(
        card.month_used_bytes.is_none(),
        "management NAT is not business NIC metering"
    );
    let hidden = db.store.cvm_readings(&actor_user("bob")).await.unwrap();
    assert!(hidden.instances.is_empty());
    let workspace = db.store.cvm_workspace(&actor_user("alice")).await.unwrap();
    assert_eq!(workspace.usage.len(), 1);
    assert_eq!(workspace.usage[0].instances, 1);
    assert_eq!(
        workspace.usage[0].disk_bytes,
        item.desired.spec.resources.root_disk_bytes
    );
    assert_eq!(workspace.usage[0].interfaces, 1);
    assert_eq!(view.latest.unwrap().cpu_usage_basis_points, Some(6000));
    assert!(view.events.iter().any(|e| e.kind == "oom"));
    assert!(matches!(
        db.store
            .cvm_monitoring(&actor_user("bob"), &item.desired.id)
            .await,
        Err(StoreError::Forbidden(_))
    ));
    assert!(db
        .store
        .cvm_monitoring_range(&actor_user("alice"), &item.desired.id, 17)
        .await
        .is_err());
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(i) FROM cvm_instances i WHERE id=$1")
            .bind(&item.desired.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "monitoring must not acknowledge, reserve or mutate any lifecycle intent"
    );
    let mut failed = observation(m.clone());
    failed.observed_at_unix_millis += 1000;
    failed.metrics = None;
    failed.guest_state = Reachability::Unreachable;
    failed.error = Some("Guest 采样通道无响应".into());
    db.store
        .cvm_record_observations("n1", vec![failed])
        .await
        .unwrap();
    let unavailable = db
        .store
        .cvm_monitoring(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert_eq!(unavailable.state, "unavailable");
    assert!(unavailable
        .events
        .iter()
        .any(|e| e.kind == "guest_unreachable"));
    assert!(unavailable.latest.is_some());
    m.observed_at_unix_millis += 15_000;
    m.uptime_millis += 15_000;
    db.store
        .cvm_record_observations("n1", vec![observation(m.clone())])
        .await
        .unwrap();
    let recovered = db
        .store
        .cvm_monitoring(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert!(recovered.latest.unwrap().has_gap);
    assert!(recovered.events.iter().any(|e| e.kind == "guest_recovered"));
    m.observed_at_unix_millis += 15000;
    m.uptime_millis = 1000;
    m.boot_id = "bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee".into();
    db.store
        .cvm_record_observations("n1", vec![observation(m)])
        .await
        .unwrap();
    let rebooted = db
        .store
        .cvm_monitoring(&actor_user("alice"), &item.desired.id)
        .await
        .unwrap();
    assert!(rebooted.latest.unwrap().cpu_usage_basis_points.is_none());
    assert!(rebooted.events.iter().any(|e| e.kind == "reboot"));
    let mut future = rebooted.health.unwrap().metrics.unwrap();
    future.observed_at_unix_millis = now + 30_000;
    future.uptime_millis += 30_000;
    db.store
        .cvm_record_observations("n1", vec![observation(future.clone())])
        .await
        .unwrap();
    let skewed = db
        .store
        .cvm_monitoring_range(&actor_user("alice"), &item.desired.id, 86400)
        .await
        .unwrap();
    assert_eq!(
        skewed.range_end_unix_millis - skewed.range_start_unix_millis,
        86_400_000
    );
    assert!(skewed.range_end_unix_millis >= future.observed_at_unix_millis);
    assert!(
        skewed
            .samples
            .iter()
            .any(|p| p.observed_at_unix_millis == future.observed_at_unix_millis),
        "an accepted near-future reading must not disappear from history"
    );
    assert!(skewed.issues.iter().any(|i| i.kind == "clock"));
    // Expiration is a separate maintenance path, including when nobody uploads.
    sqlx::query("UPDATE cvm_observation_samples SET observed_at=$1")
        .bind((now - 90_000_001) as i64)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cvm_observation_events SET observed_at=observed_at-$1")
        .bind(90_000_001_i64)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.prune_cvm_observations().await.unwrap() > 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_observation_samples")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_terminal_policy_is_admin_owned_versioned_and_not_overwritten_by_inventory() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let owner = system_admin();
    let view = db.store.cvm_workspace(&owner).await.unwrap();
    assert!(!view.hosts[0].terminal_enabled);
    assert!(view.hosts[0].can_manage_terminal);
    assert!(matches!(
        db.store
            .set_cvm_terminal_policy(
                &actor_user("alice"),
                "n1",
                TerminalPolicyUpdate {
                    enabled: true,
                    expected_revision: 0
                }
            )
            .await,
        Err(StoreError::Forbidden(_))
    ));
    let other = AdminContext::new(
        "other",
        AdminRole::TenantAdmin,
        Some("platform.other".into()),
    );
    assert!(db
        .store
        .set_cvm_terminal_policy(
            &other,
            "n1",
            TerminalPolicyUpdate {
                enabled: true,
                expected_revision: 0
            }
        )
        .await
        .is_err());
    db.store
        .set_cvm_terminal_policy(
            &owner,
            "n1",
            TerminalPolicyUpdate {
                enabled: true,
                expected_revision: 0,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .set_cvm_terminal_policy(
                &owner,
                "n1",
                TerminalPolicyUpdate {
                    enabled: false,
                    expected_revision: 0
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: catalogue(),
                error: None,
            },
        )
        .await
        .unwrap();
    let view = db.store.cvm_workspace(&owner).await.unwrap();
    assert!(view.hosts[0].terminal_enabled);
    assert_eq!(view.hosts[0].terminal_revision, 1);
    db.store
        .set_cvm_terminal_policy(
            &owner,
            "n1",
            TerminalPolicyUpdate {
                enabled: false,
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
    assert!(!db.store.cvm_workspace(&owner).await.unwrap().hosts[0].terminal_enabled);
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cvm_terminal_policy_events WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(audit, 2);
    db.store
        .set_cvm_terminal_policy(
            &owner,
            "n1",
            TerminalPolicyUpdate {
                enabled: true,
                expected_revision: 2,
            },
        )
        .await
        .unwrap();
    let policy = db.store.cvm_terminal_policy("n1").await.unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.revision, 3);
    assert_eq!(policy.revoked_through, 2);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_quota_usage_keeps_storage_and_interfaces_until_cleanup() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(&actor_user("alice"), create_request("quota-ledger-00001"))
        .await
        .unwrap();
    // Compute and resource cleanup have separate durable ledgers. A released
    // compute reservation must not imply that disk or NIC cleanup succeeded.
    sqlx::query("UPDATE cvm_instances SET reserved=false WHERE id=$1")
        .bind(&item.desired.id)
        .execute(db.pool())
        .await
        .unwrap();
    let workspace = db.store.cvm_workspace(&actor_user("alice")).await.unwrap();
    let usage = &workspace.usage[0];
    assert_eq!(usage.instances, 0);
    assert_eq!(usage.vcpus, 0);
    assert_eq!(usage.memory_bytes, 0);
    assert_eq!(
        usage.disk_bytes,
        item.desired.spec.resources.root_disk_bytes
    );
    assert_eq!(usage.interfaces, 1);
    assert!(db
        .store
        .cvm_workspace(&actor_user("bob"))
        .await
        .unwrap()
        .usage
        .is_empty());
    sqlx::query("UPDATE cvm_volumes SET state='released' WHERE instance_id=$1")
        .bind(&item.desired.id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cvm_interfaces SET detached_confirmed=true WHERE instance_id=$1")
        .bind(&item.desired.id)
        .execute(db.pool())
        .await
        .unwrap();
    let workspace = db.store.cvm_workspace(&actor_user("alice")).await.unwrap();
    assert_eq!(workspace.usage[0].disk_bytes, 0);
    assert_eq!(workspace.usage[0].interfaces, 0);
}
