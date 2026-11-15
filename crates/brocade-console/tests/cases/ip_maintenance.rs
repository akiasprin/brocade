use super::*;
use axum::http::Method;
use brocade_deployment::ip_maintenance::{data::MaintenanceData, IpMaintenancePolicy};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_http_negotiates_auth_and_invalidates_committed_reports() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = admin_token(&db).await;
    let issued = db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(68))
        .await
        .unwrap();
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "https://console.example".into(),
    );
    let path = "/nodes/n1/ip-maintenance";
    let anonymous = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let (status, catalog) = get_json(&app, &token, "/ip-maintenance/regions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog.as_array().unwrap().len(), 67);
    for version in [67, 68] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/agent/v1/ip-maintenance/desired")
                    .header("authorization", format!("Bearer {}", issued.token))
                    .header("x-brocade-protocol-version", version.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().is_success(), version == 68);
    }
    let response = app
        .clone()
        .oneshot(
            Request::get("/agent/v1/ip-maintenance/data")
                .header("authorization", format!("Bearer {}", issued.token))
                .header("x-brocade-protocol-version", "68")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice::<MaintenanceData>(&bytes)
        .unwrap()
        .validate()
        .unwrap();
    let (status, initial) = get_json(&app, &token, path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["default_policy"]["interval_minutes"], 45);
    assert_eq!(initial["desired"]["policy"]["interval_minutes"], 45);
    let mut policy = serde_json::to_value(IpMaintenancePolicy::default()).unwrap();
    policy["interval_minutes"] = json!(20);
    let update = json!({"expected_generation":0,"policy":policy});
    assert_eq!(
        put_json(&app, &token, path, update.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put_json(&app, &token, path, update).await.0,
        StatusCode::CONFLICT
    );

    let response = app
        .clone()
        .oneshot(
            Request::get("/realtime/observations/events")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    let ready = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(std::str::from_utf8(&ready)
        .unwrap()
        .contains("event: ready"));
    let report = json!({"generation":1,"run_generation":1,"agent_instance_id":"http-fixture","applied_control_sequence":0,"applied_stop_sequence":0,"applied_session_generation":0,"worker_observed_at_unix_secs":2000000000,"last_started_at_unix_secs":null,"cooldown_until_unix_secs":null,"sequence":1,"phase":"waiting","round_id":null,"module":null,
        "actual_exit_ip":null,"actions_total":0,"action_index":0,"phase_deadline_unix_secs":null,
        "next_run_unix_secs":null,"daily_requests":0,"daily_response_bytes":0,"actions":[],"evidence":[],
        "reason":null,"observed_at_unix_secs":2000000000});
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::post("/agent/v1/ip-maintenance/report")
                    .header("authorization", format!("Bearer {}", issued.token))
                    .header("x-brocade-protocol-version", "68")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&report).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let changed = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(&changed).unwrap();
    assert!(text.contains("ip_maintenance"), "{text}");
    assert!(
        !text.contains("n1"),
        "observation hints must not expose node identities"
    );
    let (status, view) = get_json(&app, &token, path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["report"]["sequence"], 1);
    assert_eq!(view["default_policy"]["interval_minutes"], 45);
    assert_eq!(view["desired"]["policy"]["interval_minutes"], 20);
    assert_eq!(view["desired"]["report_sequence_floor"], 1);

    // The public surface retains the common response mask and never grants control operations.
    sqlx::query(
        "INSERT INTO tenants(id,name) VALUES('platform','Platform') ON CONFLICT DO NOTHING",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        put_json(&app, &token, "/visitor-access", json!({"enabled":true}))
            .await
            .0,
        StatusCode::OK
    );
    let mut round = report;
    round["sequence"] = json!(2);
    round["phase"] = json!("complete");
    round["round_id"] = json!("public-round");
    round["module"] = json!("google");
    round["actual_exit_ip"] = json!("1.1.1.1");
    round["actions_total"] = json!(1);
    round["action_index"] = json!(1);
    round["actions"] = json!([{"index":1,"kind":"local_site","host":"www.mhlw.go.jp","status":200,"error":null,"response_bytes":2048}]);
    round["evidence"] = json!([{"source":"google_jump","country":"JP","status":200}]);
    db.store
        .report_ip_maintenance("n1", serde_json::from_value(round).unwrap())
        .await
        .unwrap();
    for route in [
        "/ip-maintenance/regions",
        path,
        "/nodes/n1/ip-maintenance/runs",
        "/nodes/n1/ip-maintenance/runs/public-round",
    ] {
        let (status, body) = get_json_without_credentials(&app, route).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
        let text = body.to_string();
        assert!(
            !text.contains("1.1.1.1") && !text.contains("www.mhlw.go.jp"),
            "addresses must be masked: {route}"
        );
    }
    let (_, history) = get_json_without_credentials(&app, "/nodes/n1/ip-maintenance/runs").await;
    assert_eq!(history["runs"][0]["actions"][0]["status"], 200);
    assert_eq!(history["runs"][0]["evidence"][0]["country"], "JP");
    for (method, route) in [
        (Method::PUT, path),
        (Method::POST, "/nodes/n1/ip-maintenance/run"),
        (Method::POST, "/nodes/n1/ip-maintenance/stop"),
        (Method::POST, "/nodes/n1/ip-maintenance/session-reset"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(route)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{route}");
    }
    assert_eq!(
        get_json_without_credentials(&app, "/nodes/missing/ip-maintenance")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        db.store
            .ip_maintenance_desired("n1")
            .await
            .unwrap()
            .generation,
        1
    );
    db.store
        .record_node_poll_with_protocol("n1", None, Some(69))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::post("/nodes/n1/ip-maintenance/session-reset")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"expected_generation":1,"request_id":"reset-history"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (status, reset) = get_json(&app, &token, path).await;
    assert_eq!(status, StatusCode::OK);
    assert!(reset["session_reset_supported"].as_bool().unwrap());
    assert!(reset["report"].is_null());
    assert_eq!(reset["desired"]["generation"], 2);
    assert_eq!(reset["desired"]["session_generation"], 1);
    assert_eq!(reset["desired"]["policy"]["enabled"], false);
    let (status, history) =
        get_json_without_credentials(&app, "/nodes/n1/ip-maintenance/runs").await;
    assert_eq!(status, StatusCode::OK);
    assert!(history["runs"].as_array().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_dual_desired_is_not_serialized_to_legacy_agents() {
    use brocade_deployment::ip_maintenance::MaintenanceFamily;
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = admin_token(&db).await;
    let issued = db.store.issue_node_token("n1").await.unwrap();
    db.store
        .record_node_poll_with_protocol("n1", None, Some(70))
        .await
        .unwrap();
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "https://console.example".into(),
    );
    let policy = IpMaintenancePolicy {
        family: MaintenanceFamily::Dual,
        source_ip: Some("1.1.1.1".parse().unwrap()),
        source_ipv6: Some("2606:4700::1111".parse().unwrap()),
        ..Default::default()
    };
    assert_eq!(
        put_json(
            &app,
            &token,
            "/nodes/n1/ip-maintenance",
            json!({"expected_generation":0,"policy":policy})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    for version in [68, 69, 70] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/agent/v1/ip-maintenance/desired")
                    .header("authorization", format!("Bearer {}", issued.token))
                    .header("x-brocade-protocol-version", version.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().is_success(), version == 70);
        if version == 70 {
            let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
            let desired: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(desired["policy"]["family"], "dual");
        }
    }
    sqlx::query(
        "INSERT INTO tenants(id,name) VALUES('platform','Platform') ON CONFLICT DO NOTHING",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        put_json(&app, &token, "/visitor-access", json!({"enabled":true}))
            .await
            .0,
        StatusCode::OK
    );
    for (sequence, family, ip) in [(1, "v4", "1.1.1.1"), (2, "v6", "2606:4700::1111")] {
        let report = json!({"family":family,"generation":1,"run_generation":1,"sequence":sequence,"agent_instance_id":"dual-http-fixture","applied_control_sequence":0,"applied_stop_sequence":0,"applied_session_generation":0,"worker_observed_at_unix_secs":2000000000,"last_started_at_unix_secs":null,"cooldown_until_unix_secs":null,"phase":"waiting","round_id":null,"module":null,"actual_exit_ip":ip,"actions_total":0,"action_index":0,"phase_deadline_unix_secs":null,"next_run_unix_secs":null,"daily_requests":0,"daily_response_bytes":0,"actions":[],"evidence":[],"reason":null,"observed_at_unix_secs":2000000000});
        db.store
            .report_ip_maintenance("n1", serde_json::from_value(report).unwrap())
            .await
            .unwrap();
    }
    let (status, visitor) = get_json_without_credentials(&app, "/nodes/n1/ip-maintenance").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visitor["family_reports"].as_array().unwrap().len(), 2);
    for ip in ["1.1.1.1", "2606:4700::1111"] {
        assert!(
            !visitor.to_string().contains(ip),
            "both policy bindings and per-family receipts must be masked"
        );
    }
}
