use super::*;
#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn clash_rules_http_scopes_versions_preview_and_exact_custom_probe() {
    let db = TestPg::start_if_enabled()
        .await
        .expect("explicit PostgreSQL test required");
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    sqlx::query("UPDATE control_state SET reality_dest='borrowed.example.net:443',reality_server_names='[\"borrowed.example.net\"]'").execute(db.pool()).await.unwrap();
    seed_subscription_serving(&db).await;
    let (app, token) = admin_app(&db).await;
    let path = "/users/platform.acme/alice/clash-rules";
    let info = get_json(&app, &token, path).await;
    assert_eq!(info.0, StatusCode::OK);
    assert_eq!(info.1["version"], 0);
    let rules = json!({"custom":[{"id":"one","kind":"ip","value":"10.0.0.0/8","target":"direct"}],"modules":{},"extra":[],"final":"select"});
    let body = json!({"version":0,"rules":rules});
    let saved = put_json(&app, &token, path, body.clone()).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert_eq!(saved.1["version"], 1);
    assert_eq!(
        put_json(&app, &token, path, body).await.0,
        StatusCode::CONFLICT
    );
    let preview = post_json(&app, &token, &format!("{path}/preview"), rules.clone()).await;
    assert_eq!(preview.0, StatusCode::OK);
    assert!(preview.1["yaml"]
        .as_str()
        .unwrap()
        .contains("IP-CIDR,10.0.0.0/8,DIRECT,no-resolve"));
    let hit = post_json(
        &app,
        &token,
        &format!("{path}/probe"),
        json!({"rules":rules,"kind":"ip","value":"10.1.2.3"}),
    )
    .await;
    assert_eq!(hit.0, StatusCode::OK);
    assert_eq!(hit.1["id"], "one");
    assert_eq!(hit.1["layer"], "custom");
    assert_eq!(
        get_json_without_credentials(&app, path).await.0,
        StatusCode::UNAUTHORIZED
    );
    let login = post_json(&app, &token, "/users/platform.acme/alice/login", json!({})).await;
    let cookie = login_cookie(
        &app,
        login.1["operator_id"].as_str().unwrap(),
        login.1["password"].as_str().unwrap(),
    )
    .await;
    let own = get_json_with_cookie(&app, path, &cookie).await;
    assert_eq!(own.0, StatusCode::OK);
    assert_eq!(
        own.1["rules"]["custom"][0]["value"], "10.0.0.0/8",
        "self preferences must not be masked"
    );
    assert_eq!(
        get_json_with_cookie(&app, "/users/platform.acme/bob/clash-rules", &cookie)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
