use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn node_notifications_http_defaults_persists_and_restricts_writes() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    let (app, token) = admin_app(&db).await;
    sqlx::raw_sql("INSERT INTO tenants(id,name) VALUES('platform','Platform') ON CONFLICT DO NOTHING;
        INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,dns_kind)
        VALUES('n1','platform','Node 1','n1.example.net','10.66.0.1','private','public',51820,'system');")
        .execute(db.pool()).await.unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        get_json(&app, &token, "/nodes/agent-state").await.1["nodes"][0]
            ["public_ip_change_notifications"],
        true
    );
    let anonymous = app
        .clone()
        .oneshot(
            Request::put("/nodes/n1/notifications")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"public_ip_changes":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!anonymous.status().is_success());
    assert_eq!(post_json(&app,&token,"/admin/operators",json!({
        "id":"notice-reader","display_name":"Reader","role":"readonly","tenant_scope":"platform",
        "password":"notice-reader-test-password"
    })).await.0,StatusCode::CREATED);
    let reader = post_json(
        &app,
        &token,
        "/admin/operators/notice-reader/token",
        json!({}),
    )
    .await
    .1["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        put_json(
            &app,
            &reader,
            "/nodes/n1/notifications",
            json!({"public_ip_changes":false})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let malformed = app
        .clone()
        .oneshot(
            Request::put("/nodes/n1/notifications")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"public_ip_changes":"false"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        put_json(
            &app,
            &token,
            "/nodes/missing/notifications",
            json!({"public_ip_changes":false})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for enabled in [false, true] {
        assert_eq!(
            put_json(
                &app,
                &token,
                "/nodes/n1/notifications",
                json!({"public_ip_changes":enabled})
            )
            .await,
            (
                StatusCode::OK,
                json!({"node_id":"n1","public_ip_changes":enabled})
            )
        );
        assert_eq!(
            get_json(&app, &token, "/nodes/agent-state").await.1["nodes"][0]
                ["public_ip_change_notifications"],
            enabled
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT current_revision FROM control_state WHERE id=TRUE")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        before
    );
}
