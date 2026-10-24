use super::*;

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn retired_forwarding_total_write_endpoint_cannot_create_a_policy() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let (app, token) = admin_app(&db).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/users/platform.acme/alice/traffic-policy/forwarding")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"quota_bytes":"1024","exhausted_action":{"kind":"suspend"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "removed write endpoint returned {}",
        response.status()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM retired_forwarding_traffic_policies")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
