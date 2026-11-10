use super::*;

fn limits(max_states: u32) -> NetworkQuotaLimits {
    NetworkQuotaLimits {
        max_states,
        new_per_second: 1000,
        burst: 2000,
    }
}
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
async fn preview(db: &TestPg, user: &str, maximum: u32) -> CvmNetworkQuotaPreview {
    db.store
        .cvm_network_quota_preview(
            &system_admin(),
            CvmNetworkQuotaPreviewRequest {
                tenant_id: "platform.acme".into(),
                user_id: user.into(),
                node_id: "n1".into(),
                limits: limits(maximum),
            },
        )
        .await
        .unwrap()
}
fn change(p: &CvmNetworkQuotaPreview) -> CvmHostNetworkQuotaChange {
    CvmHostNetworkQuotaChange {
        node_id: p.node_id.clone(),
        expected_revision: p.revision,
        budget_revision: p.budget_revision,
        limits: p.requested.clone(),
    }
}
async fn grant(db: &TestPg, user: &str, maximum: u32) {
    let p = preview(db, user, maximum).await;
    let mut q = quota();
    q.user_id = user.into();
    q.host_network_quotas = vec![change(&p)];
    db.store.set_cvm_quota(&system_admin(), q).await.unwrap();
}
fn report(a: &NetworkQuotaAssignment, states: u64) -> NetworkQuotaReport {
    NetworkQuotaReport {
        id: a.id.clone(),
        revision: a.revision,
        applied: true,
        states,
        denied_capacity: 0,
        denied_rate: 0,
        allocation_failures: 0,
        error: None,
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_network_quota_serializes_grants_holds_shrinks_and_protects_memory() {
    let Some(db) = TestPg::start_if_enabled_with_tag("18").await else {
        return;
    };
    prepare(&db).await;
    sqlx::query(
        "INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob',gen_random_uuid())",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let mut caps = catalogue();
    caps.network_quota = Some(capacity());
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
    let a = preview(&db, "alice", 150000).await;
    let b = preview(&db, "bob", 150000).await;
    let mut qa = quota();
    qa.host_network_quotas = vec![change(&a)];
    let mut qb = quota();
    qb.user_id = "bob".into();
    qb.host_network_quotas = vec![change(&b)];
    let admin = system_admin();
    let (ra, rb) = tokio::join!(
        db.store.set_cvm_quota(&admin, qa),
        db.store.set_cvm_quota(&admin, qb)
    );
    assert_ne!(
        ra.is_ok(),
        rb.is_ok(),
        "stale concurrent previews must not oversell"
    );
    let owner = if ra.is_ok() { "alice" } else { "bob" };
    let other = if owner == "alice" { "bob" } else { "alice" };
    let desired = db.store.cvm_network_quota_desired("n1").await.unwrap();
    assert_eq!(desired.assignments.len(), 1);
    let original = desired.assignments[0].clone();
    assert!(db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("quota-unconfirmed-create")
        )
        .await
        .is_err());
    grant(&db, owner, 10000).await;
    let a = db
        .store
        .cvm_network_quota_desired("n1")
        .await
        .unwrap()
        .assignments
        .remove(0);
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n1", vec![report(&original, 0)])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n1", vec![report(&a, 20000)])
            .await
            .unwrap(),
        1
    );
    assert_eq!(preview(&db, owner, 10000).await.held_states, 150000);
    assert_eq!(preview(&db, other, 150000).await.other_allocated, 150000);
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n1", vec![report(&a, 10000)])
            .await
            .unwrap(),
        1
    );
    assert_eq!(preview(&db, owner, 10000).await.held_states, 10000);
    assert_eq!(
        db.store
            .cvm_record_network_quotas("n2", vec![report(&a, 0)])
            .await
            .unwrap(),
        0
    );
    assert!(db
        .store
        .cvm_network_quota_preview(
            &actor_user(owner),
            CvmNetworkQuotaPreviewRequest {
                tenant_id: "platform.acme".into(),
                user_id: other.into(),
                node_id: "n1".into(),
                limits: limits(1)
            }
        )
        .await
        .is_err());
    assert!(db
        .store
        .cvm_host_network_quotas(&actor_user(other))
        .await
        .unwrap()
        .is_empty());
    // A committed network grant constrains subsequent Guest memory, and a
    // committed Guest allocation invalidates an earlier network preview.
    grant(&db, "alice", 100000).await;
    let desired = db.store.cvm_network_quota_desired("n1").await.unwrap();
    db.store
        .cvm_record_network_quotas(
            "n1",
            desired.assignments.iter().map(|a| report(a, 0)).collect(),
        )
        .await
        .unwrap();
    let stale = preview(&db, "alice", 100001).await;
    let vm = db
        .store
        .create_cvm(
            &actor_user("alice"),
            create_request("quota-memory-guest-create"),
        )
        .await
        .unwrap();
    let mut q = quota();
    q.max_instances = 2;
    q.host_network_quotas = vec![change(&stale)];
    assert!(db.store.set_cvm_quota(&system_admin(), q).await.is_err());
    let saved = db.store.cvm_workspace(&system_admin()).await.unwrap();
    assert_eq!(
        saved
            .quotas
            .iter()
            .find(|q| q.user_id == "alice")
            .unwrap()
            .max_instances,
        1,
        "global quota changes must roll back with stale network budget"
    );
    let desired = db.store.cvm_network_quota_desired("n1").await.unwrap();
    assert!(desired
        .assignments
        .iter()
        .any(|a| a.instances.contains(&vm.desired.id)));
    let after = preview(&db, "alice", 100001).await;
    assert!(after.maximum < stale.maximum);
    let memory = saved.hosts[0].capabilities.allocatable_memory_bytes;
    assert!(
        memory < capacity().memory_budget_bytes,
        "network grants must reserve Guest memory budget"
    );
}
