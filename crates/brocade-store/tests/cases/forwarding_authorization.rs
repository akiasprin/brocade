use super::*;
use brocade_store::ForwardingEntitlementWrite;

fn entitlement(enabled: bool) -> ForwardingEntitlementWrite {
    ForwardingEntitlementWrite {
        enabled,
        tcp_allowed: true,
        udp_allowed: true,
        listen_port_start: 40_000,
        listen_port_end: 49_999,
        max_rules: 4,
        allow_private_targets: false,
    }
}

async fn fixture(db: &TestPg) {
    db.store.migrate().await.unwrap();
    insert_minimal_fixture(db.pool()).await;
    sqlx::query(
        "INSERT INTO tenants (id, name)
         VALUES ('platform.acme.child', 'Child'), ('platform.other', 'Other')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    insert_basic_node(
        db.pool(),
        "n-child",
        "platform.acme.child",
        "10.66.0.2",
        51821,
    )
    .await;
    insert_basic_node(db.pool(), "n-other", "platform.other", "10.66.0.3", 51822).await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_entitlement_requires_access_to_both_user_and_node() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    let actor = tenant_admin("platform.acme");

    for node in ["n1", "n-child"] {
        db.store
            .set_forwarding_entitlement(&actor, "platform.acme", "alice", node, entitlement(true))
            .await
            .unwrap();
    }
    let forbidden = db
        .store
        .set_forwarding_entitlement(
            &actor,
            "platform.acme",
            "alice",
            "n-other",
            entitlement(true),
        )
        .await;
    assert!(matches!(forbidden, Err(StoreError::Forbidden(_))));
    assert!(!sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM forwarding_entitlements WHERE node_id = 'n-other')
             OR EXISTS (SELECT 1 FROM forwarding_node_state WHERE node_id = 'n-other')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap());

    // A system administrator may explicitly share a node across tenant boundaries. Such an
    // existing grant must not let the user's tenant administrator overwrite that node's policy.
    let before = db
        .store
        .set_forwarding_entitlement(
            &system_admin(),
            "platform.acme",
            "alice",
            "n-other",
            entitlement(true),
        )
        .await
        .unwrap();
    let forbidden_update = db
        .store
        .set_forwarding_entitlement(
            &actor,
            "platform.acme",
            "alice",
            "n-other",
            entitlement(false),
        )
        .await;
    assert!(matches!(forbidden_update, Err(StoreError::Forbidden(_))));
    assert_eq!(
        db.store
            .forwarding_overview(&system_admin(), "platform.acme", "alice")
            .await
            .unwrap(),
        before,
        "a denied policy update must not change grants or node generations"
    );
    assert!(matches!(
        db.store
            .set_forwarding_entitlement(
                &actor,
                "platform.acme",
                "alice",
                "missing-node",
                entitlement(true),
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn forwarding_entitlement_rechecks_node_scope_after_a_concurrent_transfer() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    fixture(&db).await;
    let mut transfer = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE nodes SET tenant_id = 'platform.other' WHERE id = 'n1'")
        .execute(&mut *transfer)
        .await
        .unwrap();
    let store = db.store.clone();
    let grant = tokio::spawn(async move {
        store
            .set_forwarding_entitlement(
                &tenant_admin("platform.acme"),
                "platform.acme",
                "alice",
                "n1",
                entitlement(true),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM pg_stat_activity
                     WHERE datname = current_database() AND pid <> pg_backend_pid()
                       AND wait_event_type = 'Lock'
                       AND query LIKE '%FROM users u CROSS JOIN nodes n%'
                )",
            )
            .fetch_one(db.pool())
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the grant must wait for the concurrent node transfer");
    transfer.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), grant)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(StoreError::Forbidden(_))));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM forwarding_entitlements")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
}
