use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn removed_host_stays_removed_after_inventory_without_retiring_the_machine() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let owner = system_admin();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n WHERE id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    for role in [AdminRole::User, AdminRole::Readonly, AdminRole::TenantAdmin] {
        let actor = AdminContext::new("not-system", role, Some("platform.acme".into()));
        assert!(matches!(
            db.store
                .set_cvm_host_policy(
                    &actor,
                    "n1",
                    HostPolicyUpdate {
                        enabled: false,
                        expected_revision: 0
                    }
                )
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    db.store
        .set_cvm_host_policy(
            &owner,
            "n1",
            HostPolicyUpdate {
                enabled: false,
                expected_revision: 0,
            },
        )
        .await
        .unwrap();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: catalogue(),
                terminal_sessions_used: None,
                error: None,
            },
        )
        .await
        .unwrap();
    let view = db.store.cvm_workspace(&owner).await.unwrap();
    assert!(view.hosts.is_empty());
    assert_eq!(view.available_hosts.len(), 1);
    assert_eq!(view.available_hosts[0].host_revision, 1);
    let user_view = db.store.cvm_workspace(&actor_user("alice")).await.unwrap();
    assert!(user_view.hosts.is_empty() && user_view.available_hosts.is_empty());
    assert!(matches!(
        db.store
            .create_cvm(&system_admin(), create_request("removed-host-create-001"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n WHERE id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    assert!(matches!(
        db.store
            .set_cvm_host_policy(
                &owner,
                "n1",
                HostPolicyUpdate {
                    enabled: true,
                    expected_revision: 0
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    db.store
        .set_cvm_host_policy(
            &owner,
            "n1",
            HostPolicyUpdate {
                enabled: true,
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        db.store.cvm_workspace(&owner).await.unwrap().hosts[0].host_revision,
        2
    );
    db.store
        .create_cvm(&system_admin(), create_request("restored-host-create-01"))
        .await
        .unwrap();
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM cvm_host_policy_events WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(events, 2);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn terminal_occupancy_is_exposed_only_for_fresh_managed_hosts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let mut capabilities = catalogue();
    capabilities.guest_terminal = true;
    capabilities.guest_terminal_persistent = true;
    let mut report = InventoryReport {
        capabilities,
        terminal_sessions_used: Some(4),
        error: None,
    };
    assert!(db
        .store
        .cvm_record_inventory("n1", report.clone())
        .await
        .is_err());
    let owner = system_admin();
    let reader = AdminContext::new("reader", AdminRole::Readonly, Some("platform.acme".into()));
    for used in [0, 2] {
        report.terminal_sessions_used = Some(used);
        db.store
            .cvm_record_inventory("n1", report.clone())
            .await
            .unwrap();
        let view = db.store.cvm_workspace(&owner).await.unwrap();
        let count = view.hosts[0].terminal_sessions.as_ref().unwrap();
        assert_eq!((count.used, count.limit), (u64::from(used), 3));
        let view = db.store.cvm_workspace(&reader).await.unwrap();
        let raw = serde_json::to_value(&view.hosts[0]).unwrap();
        assert!(raw.get("terminal_sessions").is_none());
        assert!(raw["capabilities"].get("terminal_sessions_used").is_none());
    }
    sqlx::query("UPDATE cvm_hosts SET observed_at=now()-interval '151 seconds' WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.cvm_workspace(&owner).await.unwrap().hosts[0]
        .terminal_sessions
        .is_none());
    report.terminal_sessions_used = None;
    db.store.cvm_record_inventory("n1", report).await.unwrap();
    let view = db.store.cvm_workspace(&owner).await.unwrap();
    assert!(view.hosts[0].fresh);
    assert!(view.hosts[0].terminal_sessions.is_none());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn host_removal_waits_for_instances_and_retained_disks() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    prepare(&db).await;
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("occupied-host-create-01"))
        .await
        .unwrap();
    let owner = system_admin();
    assert!(matches!(
        db.store
            .set_cvm_host_policy(
                &owner,
                "n1",
                HostPolicyUpdate {
                    enabled: false,
                    expected_revision: 0
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    sqlx::query("UPDATE cvm_instances SET reserved=false WHERE id=$1")
        .bind(&item.desired.id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE cvm_volumes SET state='retained' WHERE instance_id=$1")
        .bind(&item.desired.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(matches!(
        db.store
            .set_cvm_host_policy(
                &owner,
                "n1",
                HostPolicyUpdate {
                    enabled: false,
                    expected_revision: 0
                }
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    let view = db.store.cvm_workspace(&owner).await.unwrap();
    assert_eq!(view.hosts.len(), 1);
    assert_eq!(view.hosts[0].host_revision, 0);
    assert!(view.available_hosts.is_empty());
}
