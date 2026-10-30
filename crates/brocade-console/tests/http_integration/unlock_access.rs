use super::*;

async fn request_as(
    app: &Router,
    method: &str,
    path: &str,
    credential: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((name, value)) = credential {
        request = request.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()));
    (status, body)
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn unlock_reports_allow_users_and_enabled_guests_but_keep_scope_and_writes_closed() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let revision = seed_subscription_serving(&db).await;
    let results = json!(brocade_deployment::unlock_probe::LEGACY_UNLOCK_SERVICES
        .into_iter()
        .map(|service| json!({
            "service": service, "status": "available", "code": "web_available", "region": "HK"
        }))
        .collect::<Vec<_>>());
    sqlx::query(
        "INSERT INTO unlock_probe_jobs (
             scope_key, node_id, app_id, chain_id, configuration_revision, protocol,
             job_id, state, trigger, address_family, completed_at, results
         ) VALUES
             ('node:n1:v4', 'n1', NULL, NULL, NULL, NULL,
              'node-report', 'complete', 'scheduled', 'v4', now(), $1),
             ('chain:app-a1b2/chn-b2c3-d4e5:v4', 'n1', 'app-a1b2', 'chn-b2c3-d4e5', $2, 'vless',
              'chain-report', 'complete', 'scheduled', 'v4', now(), $1)",
    )
    .bind(&results)
    .bind(i64::try_from(revision).unwrap())
    .execute(db.pool())
    .await
    .unwrap();
    let (app, admin) = admin_app(&db).await;
    const SUMMARY: &str = "/chains/unlock-probes?limit=1";
    const REPORTS: [&str; 2] = [
        "/nodes/n1/unlock-probe?family=v4",
        "/apps/app-a1b2/chains/chn-b2c3-d4e5/unlock-probe?family=v4",
    ];
    const CAPABILITIES: [&str; 2] = [
        "/nodes/n1/unlock-probe/capabilities",
        "/apps/app-a1b2/chains/chn-b2c3-d4e5/unlock-probe/capabilities",
    ];
    for path in REPORTS.into_iter().chain(CAPABILITIES).chain([SUMMARY]) {
        assert_eq!(
            request_as(&app, "GET", path, None).await.0,
            StatusCode::UNAUTHORIZED,
            "closed guest access: {path}"
        );
    }
    assert_eq!(
        put_json(&app, &admin, "/visitor-access", json!({"enabled": true}))
            .await
            .0,
        StatusCode::OK
    );

    let login = post_json(&app, &admin, "/users/platform.acme/alice/login", json!({})).await;
    assert_eq!(login.0, StatusCode::CREATED);
    let user_cookie = login_cookie(&app, "alice", login.1["password"].as_str().unwrap()).await;
    let direct = post_json(
        &app,
        &admin,
        "/users/platform.acme/alice/direct-login",
        json!({}),
    )
    .await;
    assert_eq!(direct.0, StatusCode::CREATED);
    let response = app
        .clone()
        .oneshot(
            Request::post("/auth/direct-login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"uuid": direct.1["uuid"], "token": direct.1["token"]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let direct_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    // Objects outside the existing account/visitor scope must remain undiscoverable.
    sqlx::query("INSERT INTO tenants(id,name) VALUES('other','Other')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO nodes(id,name,tenant_id,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers) SELECT 'n2','Other','other','10.66.0.2','private-2','public-2',wg_listen_port,api_port,overlay,egress_allowed,dns_kind,dns_servers FROM nodes WHERE id='n1'")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO chains(id,app_id,tenant_id,name,position) VALUES('chn-c3d4-e5f6','app-a1b2','other','Other',1)")
        .execute(db.pool()).await.unwrap();
    let (status, first) = request_as(
        &app,
        "GET",
        SUMMARY,
        Some(("authorization", &format!("Bearer {admin}"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["chains"].as_array().unwrap().len(), 1);
    assert_eq!(first["chains"][0]["probes"]["v4"]["results"], results);
    assert_eq!(first["chains"][0]["probes"]["v4"]["family"], "v4");
    let cursor = first["next_cursor"].as_str().unwrap();
    let next = format!("{SUMMARY}&after={cursor}");
    let (status, second) = request_as(
        &app,
        "GET",
        &next,
        Some(("authorization", &format!("Bearer {admin}"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["chains"][0]["chain_id"], "chn-c3d4-e5f6");
    assert!(second["chains"][0]["probes"]["v4"].is_null());
    assert!(second["next_cursor"].is_null());
    for path in [
        "/chains/unlock-probes?limit=0",
        "/chains/unlock-probes?limit=101",
        "/chains/unlock-probes?limit=invalid",
    ] {
        assert!(
            request_as(
                &app,
                "GET",
                path,
                Some(("authorization", &format!("Bearer {admin}")))
            )
            .await
            .0
            .is_client_error(),
            "{path}"
        );
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM unlock_probe_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for credential in [
        None,
        Some(("cookie", user_cookie.as_str())),
        Some(("cookie", direct_cookie.as_str())),
    ] {
        let (status, page) = request_as(&app, "GET", SUMMARY, credential).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["chains"].as_array().unwrap().len(), 1);
        assert_eq!(page["chains"][0]["chain_id"], "chn-b2c3-d4e5");
        assert_eq!(page["chains"][0]["probes"]["v4"]["results"], results);
        assert!(
            page["next_cursor"].is_null(),
            "hidden chains must not affect pagination"
        );
        let (status, v6) = request_as(&app, "GET", "/chains/unlock-probes", credential).await;
        assert_eq!(status, StatusCode::OK, "{v6}");
        assert!(
            v6["chains"][0]["probes"]["v6"].is_null(),
            "never substitute v4 for v6"
        );
        assert_eq!(
            request_as(&app, "POST", SUMMARY, credential).await.0,
            StatusCode::FORBIDDEN
        );
        for path in REPORTS {
            let (status, body) = request_as(&app, "GET", path, credential).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert_eq!(body["state"], "complete");
            assert_eq!(body["results"], results);
            assert_eq!(body["family"], "v4");
            assert_eq!(
                request_as(&app, "POST", path, credential).await.0,
                StatusCode::FORBIDDEN,
                "manual probe: {path}"
            );
        }
        for path in CAPABILITIES {
            let (status, body) = request_as(&app, "GET", path, credential).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert!(body.get("ipv6_available").is_some());
        }
        for path in [
            "/nodes/n1/unlock-probe?family=v6",
            "/apps/app-a1b2/chains/chn-b2c3-d4e5/unlock-probe?family=v6",
            "/apps/app-a1b2/chains/chn-b2c3-d4e5/unlock-probe?automatic=true",
        ] {
            assert_eq!(
                request_as(&app, "GET", path, credential).await,
                (StatusCode::OK, Value::Null),
                "empty family/automatic report: {path}"
            );
        }
        for path in [
            "/nodes/missing/unlock-probe",
            "/nodes/n2/unlock-probe",
            "/nodes/n2/unlock-probe/capabilities",
            "/apps/app-a1b2/chains/chn-c3d4-e5f6/unlock-probe",
            "/apps/app-a1b2/chains/chn-c3d4-e5f6/unlock-probe/capabilities",
        ] {
            assert_eq!(
                request_as(&app, "GET", path, credential).await.0,
                StatusCode::NOT_FOUND,
                "scope: {path}"
            );
        }
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM unlock_probe_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after, "readers must never create probe work");
    assert_eq!(
        request_as(
            &app,
            "GET",
            REPORTS[0],
            Some(("authorization", "Bearer invalid-token"))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED,
        "invalid credentials must not fall back to guest access"
    );
    assert_eq!(
        put_json(&app, &admin, "/visitor-access", json!({"enabled": false}))
            .await
            .0,
        StatusCode::OK
    );
    for path in REPORTS.into_iter().chain(CAPABILITIES).chain([SUMMARY]) {
        assert_eq!(
            request_as(&app, "GET", path, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request_as(&app, "GET", path, Some(("cookie", &user_cookie)))
                .await
                .0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn chain_unlock_summary_keeps_both_families_and_their_previous_results_independent() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let revision = seed_subscription_serving(&db).await;
    let results = |region: &str| {
        json!(brocade_deployment::unlock_probe::LEGACY_UNLOCK_SERVICES.into_iter()
        .map(|service| json!({"service": service, "status": "available", "code": "web_available", "region": region})).collect::<Vec<_>>())
    };
    let v4 = results("HK");
    let v6 = results("JP");
    sqlx::query("INSERT INTO unlock_probe_jobs (
        scope_key,node_id,app_id,chain_id,configuration_revision,protocol,job_id,state,trigger,address_family,completed_at,claimed_at,results,previous_results,previous_completed_at
    ) VALUES
        ('chain:app-a1b2/chn-b2c3-d4e5:v4','n1','app-a1b2','chn-b2c3-d4e5',$1,'vless','report-v4','complete','scheduled','v4',now(),NULL,$2,'[]',NULL),
        ('chain:app-a1b2/chn-b2c3-d4e5:v6','n1','app-a1b2','chn-b2c3-d4e5',$1,'vless','report-v6','running','scheduled','v6',NULL,now()-interval '1 hour','[]',$3,now()-interval '2 hours')")
        .bind(i64::try_from(revision).unwrap()).bind(&v4).bind(&v6).execute(db.pool()).await.unwrap();
    let (app, admin) = admin_app(&db).await;
    let authorization = format!("Bearer {admin}");
    let credential = Some(("authorization", authorization.as_str()));
    let (status, page) =
        request_as(&app, "GET", "/chains/unlock-probes?limit=100", credential).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let probes = &page["chains"][0]["probes"];
    assert_eq!(probes["v4"]["family"], "v4");
    assert_eq!(probes["v4"]["state"], "complete");
    assert_eq!(probes["v4"]["results"], v4);
    assert_eq!(probes["v6"]["family"], "v6");
    assert_eq!(probes["v6"]["state"], "expired");
    assert_eq!(probes["v6"]["previous_results"], v6);
    assert!(probes["v6"]["results"].as_array().unwrap().is_empty());
    assert!(probes["auto"].is_null());
    for family in ["v4", "v6"] {
        let path = format!("/apps/app-a1b2/chains/chn-b2c3-d4e5/unlock-probe?family={family}");
        let (status, detail) = request_as(&app, "GET", &path, credential).await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(
            detail, probes[family],
            "list and detail share each family's report"
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM unlock_probe_jobs")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 2, "batch reads must not create work");
}
