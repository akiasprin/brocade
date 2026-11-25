use super::*;

fn capacity() -> NetworkQuotaCapacity {
    NetworkQuotaCapacity {
        physical_memory_bytes: 958 * 1024 * 1024,
        host_reserved_memory_bytes: 448 * 1024 * 1024,
        contract: NETWORK_QUOTA_CONTRACT.into(),
        total_states: 262144,
        system_reserved_states: 4096,
        memory_budget_bytes: 510 * 1024 * 1024,
        fixed_memory_bytes: 28 * 1024 * 1024,
        bytes_per_state: 1536,
        model: "network-state-budget-v1".into(),
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_network_capacity_is_automatic_and_shared_by_the_host() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let mut caps = catalogue();
    caps.network_quota = Some(capacity());
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
    let mut request = create_request("host-budget-unassigned-01");
    request.user_id = None;
    let first = db.store.create_cvm(&system_admin(), request).await.unwrap();
    supersede_cvm_fixture(&db).await;
    let second = db
        .store
        .create_cvm(&system_admin(), create_request("host-budget-assigned-01"))
        .await
        .unwrap();
    let desired = db.store.cvm_network_quota_desired("n1").await.unwrap();
    assert_eq!(desired.assignments.len(), 1);
    let group = &desired.assignments[0];
    assert_eq!(group.limits.max_states, capacity().allocatable());
    assert!(
        group.instances.contains(&first.desired.id) && group.instances.contains(&second.desired.id)
    );
    let report = NetworkQuotaReport {
        id: group.id.clone(),
        revision: group.revision,
        applied: true,
        states: 2,
        denied_capacity: 0,
        denied_rate: 0,
        allocation_failures: 0,
        error: None,
    };
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n2", vec![report.clone()])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n1", vec![report.clone()])
            .await
            .unwrap(),
        1
    );
    let mut stale = report;
    stale.revision -= 1;
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n1", vec![stale])
            .await
            .unwrap(),
        0
    );
    let absent:(Option<String>,Option<String>)=sqlx::query_as("SELECT to_regclass('public.cvm_quotas')::text,to_regclass('public.cvm_user_host_network_quotas')::text").fetch_one(db.pool()).await.unwrap();
    assert_eq!(absent, (None, None));
}
