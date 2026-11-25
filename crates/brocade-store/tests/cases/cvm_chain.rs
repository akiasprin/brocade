use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_route_match_reuses_staged_compile_without_persisting_and_scopes_admins() {
    use brocade_store::cvm::CvmRouteMatchRequest;

    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    prepare(&db).await;
    let mut caps = catalogue();
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
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("route-match-create-0001"))
        .await
        .unwrap();
    let id = &item.desired.id;
    let before = db.store.cvm_instance(&system_admin(), id).await.unwrap();
    let request = |protocol, port, routes| {
        serde_json::from_value::<CvmRouteMatchRequest>(
            json!({"destination":"203.0.113.42","protocol":protocol,"port":port,"routes":routes}),
        )
        .unwrap()
    };
    let admin = AdminContext::new(
        "tenant-manager",
        AdminRole::TenantAdmin,
        Some("platform.acme".into()),
    );
    assert_eq!(
        serde_json::to_value(
            db.store
                .match_cvm_route(
                    &admin,
                    id,
                    request("other", None::<u16>, serde_json::Value::Null)
                )
                .await
                .unwrap()
        )
        .unwrap(),
        json!({"kind":"default"})
    );
    let mut staged = json!({"revision":before.desired.spec.routing.revision,"dns":[],
        "priorities":[{"chain_id":"chn-a1b2-c3d4","ipv4":7}],
        "routes":[{"destination":"203.0.113.0/24","chain_id":"chn-a1b2-c3d4"},{"destination":"0.0.0.0/0","chain_id":"chn-a1b2-c3d4"}]});
    let forbidden = AdminContext::new(
        "other-manager",
        AdminRole::TenantAdmin,
        Some("platform.other".into()),
    );
    for actor in [&forbidden, &actor_user("alice")] {
        assert!(matches!(
            db.store
                .match_cvm_route(actor, id, request("tcp", Some(443), staged.clone()))
                .await,
            Err(StoreError::Forbidden(_))
        ));
    }
    // An unpublished instance still supports an actual rollback-only compile of its draft table.
    let result = db
        .store
        .match_cvm_route(&admin, id, request("tcp", Some(443), staged.clone()))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"kind":"route","route_index":0,"priority":7,
        "route":{"destination":"203.0.113.0/24","chain_id":"chn-a1b2-c3d4"}})
    );
    staged["revision"] = json!(before.desired.spec.routing.revision + 1);
    assert!(matches!(
        db.store
            .match_cvm_route(&admin, id, request("tcp", Some(443), staged))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let after = db.store.cvm_instance(&system_admin(), id).await.unwrap();
    assert_eq!(after.desired, before.desired);
    assert_eq!(after.revision_id, before.revision_id);
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_operations WHERE instance_id=$1 AND request_id LIKE 'route-match-%' AND request_id <> 'route-match-create-0001'")
        .bind(id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(leaked, 0);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_chain_uses_owner_grant_requires_capability_and_freezes_native_guard_receipts() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    prepare(&db).await;
    let actor = actor_user("alice");
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("chain-primary-create-0001"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let routing: ContainerRoutes = serde_json::from_value(json!({"revision":0,"priorities":[{"chain_id":"chn-a1b2-c3d4","ipv4":10,"ipv6":10}],"routes":[{"destination":"0.0.0.0/0","chain_id":"chn-a1b2-c3d4"}],"dns":[]})).unwrap();
    let request = RoutesWrite {
        request_id: "chain-primary-routes-0001".into(),
        expected_generation: item.desired.generation,
        expected_revision: 0,
        routes: routing.clone(),
    };
    let unavailable = db
        .store
        .cvm_chain_catalog(&actor, Some(&item.desired.id))
        .await
        .unwrap();
    assert!(!unavailable.supported);
    assert_eq!(
        unavailable.reason.as_deref(),
        Some("此宿主尚不支持线路出口，请管理员检查并更新宿主组件。")
    );
    assert!(matches!(
        db.store
            .set_cvm_routes(&system_admin(), &item.desired.id, request.clone())
            .await,
        Err(StoreError::Unsupported(_))
    ));
    let mut caps = catalogue();
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
    assert!(db
        .store
        .cvm_chain_catalog(&actor_user("bob"), Some(&item.desired.id))
        .await
        .is_err());
    let catalog = db
        .store
        .cvm_chain_catalog(&actor, Some(&item.desired.id))
        .await
        .unwrap();
    assert!(catalog.supported);
    assert!(catalog.reason.is_none());
    sqlx::query("UPDATE cvm_hosts SET observed_at=now()-interval '151 seconds' WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    let stale = db
        .store
        .cvm_chain_catalog(&actor, Some(&item.desired.id))
        .await
        .unwrap();
    assert!(!stale.supported);
    assert_eq!(
        stale.reason.as_deref(),
        Some("尚未收到宿主的最新状态，请等待恢复连接后重试。")
    );
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: catalogue(),
                terminal_sessions_used: None,
                error: Some("fixture inventory unavailable".into()),
            },
        )
        .await
        .unwrap();
    let failed = db
        .store
        .cvm_chain_catalog(&actor, Some(&item.desired.id))
        .await
        .unwrap();
    assert!(!failed.supported);
    assert_eq!(
        failed.reason.as_deref(),
        Some("宿主容器服务异常，请管理员在「宿主」中查看并处理。")
    );
    let mut caps = catalogue();
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
    assert!(catalog
        .chains
        .iter()
        .any(|c| c.id == "chn-a1b2-c3d4" && c.authorized && !c.ipv6));
    let mut ipv6 = request.clone();
    ipv6.request_id = "chain-ipv6-refused-0001".into();
    ipv6.routes.routes[0].destination = "::/0".parse().unwrap();
    assert!(matches!(
        db.store
            .set_cvm_routes(&system_admin(), &item.desired.id, ipv6)
            .await,
        Err(StoreError::Unsupported(_))
    ));
    let saved = db
        .store
        .set_cvm_routes(&system_admin(), &item.desired.id, request)
        .await
        .unwrap();
    assert_eq!(saved.desired.spec.interfaces, item.desired.spec.interfaces);
    let allocations: i64 = sqlx::query_scalar("SELECT count(*) FROM cvm_nic_allocations")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(allocations, 0);
    assert!(
        db.store
            .cvm_desired("n1")
            .await
            .unwrap()
            .chain_clients
            .is_empty(),
        "saving must not move the approved configuration"
    );
    publish_cvm_intent(&db).await;
    let desired = db.store.cvm_desired("n1").await.unwrap();
    assert_eq!(desired.chain_clients.len(), 1);
    let client = &desired.chain_clients[0];
    assert_eq!(
        (&client.tenant_id[..], &client.user_id[..]),
        ("platform.acme", "alice")
    );
    assert!(client
        .content
        .contains("2d2304da-f114-4574-8d44-625afdb1db5c"));
    assert!(!client
        .redacted
        .contains("2d2304da-f114-4574-8d44-625afdb1db5c"));
    assert_eq!(
        client.guard,
        brocade_cvm_compile::host_network::chain_guard(client, false)
    );
    let mut report = evidence(&saved);
    report.network.as_mut().unwrap().routing_revision = saved.desired.spec.routing.revision;
    report.network.as_mut().unwrap().chain_egress_configured = true;
    report.chain_egress = vec![ChainClientReceipt {
        chain_id: client.chain_id.clone(),
        sha256: client.sha256.clone(),
        guard_sha256: "0".repeat(64),
        configured: true,
        error: None,
    }];
    db.store
        .cvm_record_report("n1", report.clone())
        .await
        .unwrap();
    let active: i64 =
        sqlx::query_scalar("SELECT id FROM deployments WHERE active=TRUE AND kind='config'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        target_status(db.pool(), active, "n1").await,
        "converging",
        "an unrelated native guard digest cannot complete this release"
    );
    report.chain_egress[0].guard_sha256 = brocade_core::hash::sha256_hex(client.guard.as_bytes());
    db.store.cvm_record_report("n1", report).await.unwrap();
    assert_eq!(target_status(db.pool(), active, "n1").await, "succeeded");
    // A lost runtime/network observation must not lock the user out of repairing routes.
    let mut unknown = evidence(&saved);
    unknown.status = ApplyStatus::Unknown;
    unknown.error = Some("controlled network loss".into());
    db.store.cvm_record_report("n1", unknown).await.unwrap();
    let mut repaired_routes = saved.desired.spec.routing.clone();
    repaired_routes.priorities[0].ipv4 = Some(20);
    let repair = RoutesWrite {
        request_id: "chain-repair-unknown-0001".into(),
        expected_generation: saved.desired.generation,
        expected_revision: saved.desired.spec.routing.revision,
        routes: repaired_routes,
    };
    let repaired = db
        .store
        .set_cvm_routes(&system_admin(), &saved.desired.id, repair)
        .await
        .unwrap();
    assert!(repaired.desired.generation > saved.desired.generation);
    assert_eq!(
        repaired.desired.spec.interfaces,
        saved.desired.spec.interfaces
    );
    supersede_cvm_fixture(&db).await;
    db.store
        .upsert_grant(
            &system_admin(),
            CreateGrantRequest {
                app_id: "app-main".into(),
                tenant_id: "platform.acme".into(),
                user_id: "alice".into(),
                ingress_id: "ing-a1b2".into(),
                enabled: false,
                note: None,
            },
        )
        .await
        .unwrap();
    let catalog = db
        .store
        .cvm_chain_catalog(&actor, Some(&item.desired.id))
        .await
        .unwrap();
    let used = catalog
        .chains
        .iter()
        .find(|c| c.id == "chn-a1b2-c3d4")
        .unwrap();
    assert!(!used.authorized && used.used_by.contains(&item.desired.id));
    let r = sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
        .fetch_one(db.pool())
        .await
        .unwrap() as u64;
    let preview = db
        .store
        .plan_deployment_artifacts(&system_admin(), r, false)
        .await
        .unwrap();
    let host = preview.iter().find(|t| t.node_id == "n1").unwrap();
    let raw = host.desired_structure["cvm"]["chain_clients"][0]["redacted"]
        .as_str()
        .unwrap();
    assert!(!raw.contains("2d2304da-f114-4574-8d44-625afdb1db5c"));
    publish_cvm_intent(&db).await;
    let revoked = db.store.cvm_desired("n1").await.unwrap();
    assert!(revoked.chain_clients[0].blocked);
    assert_eq!(
        revoked.chain_clients[0].binding, client.binding,
        "revocation keeps the terminal route identity, never restores NAT"
    );
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_chain_release_groups_an_ordinary_ingress_without_a_cube_action() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    prepare(&db).await;
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||'{\"id\":\"n2\",\"name\":\"Ordinary ingress\",\"public_ipv4\":\"n2.example.net\",\"overlay_addr\":\"10.66.0.2\",\"wg_public_key\":\"wg-public-two\"}'::jsonb)).* FROM nodes n WHERE id='n1'").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE ingresses SET node_id='n2' WHERE id='ing-a1b2'")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE steps SET node_id='n2' WHERE chain_id='chn-a1b2-c3d4'")
        .execute(db.pool())
        .await
        .unwrap();
    store_current_model_snapshot(db.pool(), &db.store).await;
    let mut caps = catalogue();
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
    let item = db
        .store
        .create_cvm(&system_admin(), create_request("chain-wave-create-0001"))
        .await
        .unwrap();
    publish_cvm_intent(&db).await;
    db.store
        .cvm_record_report("n1", evidence(&item))
        .await
        .unwrap();
    supersede_cvm_fixture(&db).await;
    let routes=serde_json::from_value(json!({"revision":0,"routes":[{"destination":"0.0.0.0/0","chain_id":"chn-a1b2-c3d4"}],"priorities":[{"chain_id":"chn-a1b2-c3d4","ipv4":10}],"dns":[]})).unwrap();
    db.store
        .set_cvm_routes(
            &system_admin(),
            &item.desired.id,
            RoutesWrite {
                request_id: "chain-wave-routes-0001".into(),
                expected_generation: item.desired.generation,
                expected_revision: 0,
                routes,
            },
        )
        .await
        .unwrap();
    let revision =
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap() as u64;
    let plan = db
        .store
        .plan_deployment(&system_admin(), revision)
        .await
        .unwrap();
    let host = plan.targets.iter().find(|t| t.node_id == "n1").unwrap();
    let ingress = plan.targets.iter().find(|t| t.node_id == "n2").unwrap();
    assert!(host.prerequisites.contains(&"n2".to_owned()));
    assert!(ingress.prerequisites.contains(&"n1".to_owned()));
    assert!(!ingress
        .actions
        .contains(&brocade_deployment::plan::PlannedAction::SyncCvm));
}
