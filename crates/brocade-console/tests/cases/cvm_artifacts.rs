use super::*;
use brocade_deployment::cvm::*;
use brocade_store::cvm::CvmCreate;
async fn agent(
    app: &Router,
    token: &str,
    path: &str,
    version: u32,
    body: Option<Value>,
) -> axum::response::Response {
    let mut r = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("x-brocade-protocol-version", version.to_string());
    if body.is_some() {
        r = r.header("content-type", "application/json");
    }
    app.clone()
        .oneshot(
            r.body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_artifacts_http_negotiates_manifests_and_requires_password_capability() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let admin = admin_token(&db).await;
    std::env::set_var(
        brocade_store::secrets::SECRET_KEY_ENV,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    sqlx::query("INSERT INTO node_agent_state(node_id,agent_protocol_version) VALUES('n1',77) ON CONFLICT(node_id) DO UPDATE SET agent_protocol_version=77")
        .execute(db.pool()).await.unwrap();
    let actor = AdminContext::system_admin("artifact-http-test");
    let node = db.store.issue_node_token("n1").await.unwrap().token;
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "https://console.example".into(),
    );
    let input: CvmArtifactInput = serde_json::from_str(include_str!(
        "../../../brocade-cvm-compile/tests/golden/input.json"
    ))
    .unwrap();
    let mut caps = input.capabilities.clone();
    caps.managed_networks = true;
    let report = InventoryReport {
        capabilities: caps,
        terminal_sessions_used: None,
        error: None,
    };
    let response = agent(
        &app,
        &node,
        "/agent/v1/cvm/inventory",
        brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
        Some(serde_json::to_value(report).unwrap()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await["missing_image_manifests"],
        json!([input.image_manifest.sha256])
    );
    let response = agent(
        &app,
        &node,
        "/agent/v1/cvm/image-manifests",
        brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
        Some(serde_json::to_value(&input.image_manifest).unwrap()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let item = db
        .store
        .create_cvm(
            &actor,
            CvmCreate {
                request_id: "artifact-http-create-00001".into(),
                tenant_id: "platform.acme".into(),
                user_id: Some("alice".into()),
                node_id: "n1".into(),
                name: "artifact-http".into(),
                image_id: "debian".into(),
                resources: input.spec.resources,
                interfaces: input
                    .spec
                    .interfaces
                    .into_iter()
                    .map(|n| n.connection)
                    .collect(),
            },
        )
        .await
        .unwrap();
    let path = format!("/cvm/{}/artifacts", item.desired.id);
    let (status, body) = get_json(&app, &admin, &path).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["current"]["files"].as_array().unwrap().len(), 4);
    assert_eq!(
        get_json_without_credentials(&app, &path).await.0,
        StatusCode::UNAUTHORIZED
    );
    publish_cvm_intent(&db).await;
    let response = agent(
        &app,
        &node,
        "/agent/v1/cvm/desired",
        brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let current = response_json(response).await;
    assert_eq!(current["page_offset"], 0);
    assert!(current["instances"][0].get("artifact_mode").is_none());
    assert_eq!(
        current["instances"][0]["artifacts"]["files"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}
