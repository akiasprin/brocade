use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_oversells_host_cpu_memory_disk_but_preserves_user_quota() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let mib = 1024 * 1024;
    let gib = 1024 * mib;
    let mut caps = catalogue();
    caps.allocatable_vcpus = 1;
    caps.allocatable_memory_bytes = 128 * mib;
    caps.allocatable_disk_bytes = 3 * gib;
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                error: None,
            },
        )
        .await
        .unwrap();
    let mut q = quota();
    q.max_instances = 4;
    q.max_vcpus = 6;
    q.max_memory_bytes = gib;
    q.max_disk_bytes = 16 * gib;
    db.store.set_cvm_quota(&system_admin(), q).await.unwrap();
    let actor = actor_user("alice");
    let first = db
        .store
        .create_cvm(&actor, create_request("oversell-first-0001"))
        .await
        .unwrap();
    let mut request = create_request("oversell-second-0001");
    request.resources = Resources {
        vcpus: 2,
        memory_bytes: 256 * mib,
        root_disk_bytes: 4 * gib,
    };
    let mut second = db.store.create_cvm(&actor, request).await.unwrap();
    assert_ne!(first.desired.id, second.desired.id);
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&second))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    second = db
        .store
        .command_cvm(
            &actor,
            &second.desired.id,
            CvmCommand {
                request_id: "oversell-stop-0001".into(),
                expected_generation: second.desired.generation,
                change: CvmChange::Stop,
            },
        )
        .await
        .unwrap();
    let mut stopped = evidence(&second);
    stopped.runtime_id = None;
    stopped.interfaces.clear();
    stopped.network = None;
    stopped.network_absent = true;
    stopped.volume_retained = true;
    publish_cvm_intent(&db).await;
    db.store.cvm_record_report("n1", stopped).await.unwrap();
    let configure = |id: &str, cpu, memory| CvmCommand {
        request_id: id.into(),
        expected_generation: second.desired.generation,
        change: CvmChange::Configure {
            name: "oversold".into(),
            vcpus: cpu,
            memory_bytes: memory,
        },
    };
    supersede_cvm_fixture(&db).await;
    assert!(db
        .store
        .command_cvm(
            &actor,
            &second.desired.id,
            configure("oversell-user-cpu-denied", 6, 256 * mib)
        )
        .await
        .is_err());
    assert!(db
        .store
        .command_cvm(
            &actor,
            &second.desired.id,
            configure("oversell-user-memory-denied", 2, gib)
        )
        .await
        .is_err());
    let resized = db
        .store
        .command_cvm(
            &actor,
            &second.desired.id,
            configure("oversell-configure-0001", 4, 512 * mib),
        )
        .await
        .unwrap();
    assert_eq!(resized.desired.spec.resources.vcpus, 4);
    assert_eq!(resized.desired.spec.resources.memory_bytes, 512 * mib);
    supersede_cvm_fixture(&db).await;
    let mut disk_request = create_request("oversell-user-disk-denied");
    disk_request.resources.root_disk_bytes = 16 * gib;
    assert!(db.store.create_cvm(&actor, disk_request).await.is_err());
    let usage = db
        .store
        .cvm_workspace(&system_admin())
        .await
        .unwrap()
        .usage
        .into_iter()
        .find(|u| u.user_id == "alice")
        .unwrap();
    assert_eq!(usage.vcpus, 5);
    assert_eq!(usage.memory_bytes, 640 * mib);
    assert_eq!(usage.disk_bytes, 7 * gib);
}
