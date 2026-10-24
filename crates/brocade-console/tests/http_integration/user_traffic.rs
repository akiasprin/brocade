use super::*;

async fn agent_json(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    protocol: u32,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("x-brocade-protocol-version", protocol.to_string())
                .header("content-type", "application/json")
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    )
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn user_app_policy_http_checks_scope_protocol_permissions_and_exhaustion() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let (admin, token) = admin_app(&db).await;
    let agent = agent_router(db.store.clone());
    let node = db.store.issue_node_token("n1").await.unwrap();
    let policy_path = "/users/platform.acme/alice/traffic-policy";
    let policy =
        json!({"quota_bytes":"1024","exhausted_action":{"kind":"throttle","rate_bps":8000}});
    assert_eq!(
        put_json(
            &admin,
            &token,
            &format!("{policy_path}/access"),
            policy.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "access writes must explicitly name an app"
    );
    let unsupported = put_json(
        &admin,
        &token,
        &format!("{policy_path}/access/app-a1b2"),
        policy.clone(),
    )
    .await;
    assert!(unsupported.0.is_client_error(), "{unsupported:?}");
    let old = agent_json(
        &agent,
        &node.token,
        "GET",
        "/agent/v1/user-traffic/policies",
        43,
        None,
    )
    .await;
    assert!(old.0.is_client_error(), "{old:?}");
    let ready = agent_json(
        &agent,
        &node.token,
        "POST",
        "/agent/v1/user-traffic/support",
        44,
        Some(json!(true)),
    )
    .await;
    assert_eq!(ready.0, StatusCode::OK, "{ready:?}");
    // Agent protocol metadata normally arrives with polling; report it explicitly in this route fixture.
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) VALUES('n1',44) ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=44")
        .execute(db.pool()).await.unwrap();
    let saved = put_json(
        &admin,
        &token,
        &format!("{policy_path}/access/app-a1b2"),
        policy.clone(),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{saved:?}");
    assert_eq!(saved.1[0]["app_id"], "app-a1b2");
    assert_eq!(saved.1[0]["exhausted"], false);
    let app_quota = get_json(&admin, &token, "/quotas").await;
    assert_eq!(app_quota.1["quotas"][0]["app_id"], "app-a1b2");
    assert_eq!(app_quota.1["quotas"][0]["limit_bytes"], 1024);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM retired_forwarding_traffic_policies")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0,
        "access quota must have only its existing app row"
    );
    assert_eq!(saved.1.as_array().unwrap().len(), 1);
    assert_eq!(saved.1[0]["kind"], "access");
    let snapshot = agent_json(
        &agent,
        &node.token,
        "GET",
        "/agent/v1/user-traffic/policies",
        44,
        None,
    )
    .await;
    assert_eq!(snapshot.0, StatusCode::OK);
    assert_eq!(
        snapshot.1["sources"][0]["source"],
        "alice@platform.acme#ing-b2c3"
    );
    let acknowledged = agent_json(
        &agent,
        &node.token,
        "POST",
        "/agent/v1/user-traffic/policies/ack",
        44,
        Some(snapshot.1["sha256"].clone()),
    )
    .await;
    assert_eq!(acknowledged.0, StatusCode::NO_CONTENT, "{acknowledged:?}");
    let request = json!({"source":"alice@platform.acme#ing-b2c3","request_id":"00000000000000000000000000000001","bytes":16384});
    let normal = agent_json(
        &agent,
        &node.token,
        "POST",
        "/agent/v1/user-traffic/budget",
        44,
        Some(request.clone()),
    )
    .await;
    assert_eq!(normal.0, StatusCode::OK);
    assert_eq!(normal.1["mode"], "unlimited");
    sqlx::query("INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes) SELECT 'platform.acme','alice','app-a1b2',starts_at,starts_at+interval '1 day',512,512 FROM user_traffic_cycle(1,now())")
        .execute(db.pool()).await.unwrap();
    db.store.enforce_user_traffic_policies().await.unwrap();
    let limited = agent_json(
        &agent,
        &node.token,
        "POST",
        "/agent/v1/user-traffic/budget",
        44,
        Some(request),
    )
    .await;
    assert_eq!(limited.0, StatusCode::OK);
    assert_eq!(limited.1["mode"], "limited");
    assert_eq!(limited.1["rate_bps"], 8000);
    let created = post_json(&admin, &token, "/admin/operators", json!({"id":"traffic-reader","display_name":"Reader","role":"readonly","tenant_scope":"platform.acme","password":"traffic-test-password-only"})).await;
    assert_eq!(created.0, StatusCode::CREATED);
    let reader = post_json(
        &admin,
        &token,
        "/admin/operators/traffic-reader/token",
        json!({}),
    )
    .await;
    let reader = reader.1["token"].as_str().unwrap();
    assert_eq!(
        get_json(&admin, reader, policy_path).await.0,
        StatusCode::OK
    );
    assert_eq!(
        put_json(
            &admin,
            reader,
            &format!("{policy_path}/access/app-a1b2"),
            policy
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
