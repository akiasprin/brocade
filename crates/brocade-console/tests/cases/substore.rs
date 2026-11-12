use super::*;
use brocade_console::substore;
use brocade_store::{subscription_connector as connector, AdminRole};
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    GenericImage,
};

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1, Docker PostgreSQL and unmodified xream/sub-store"]
async fn substore_native_sync_is_user_scoped_durable_and_preserves_remote_rules() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','4f84aaf8-49ea-4cce-8a93-683ee21ba3c6')").execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO grants(app_id,tenant_id,user_id,ingress_id) VALUES('app-a1b2','platform.acme','bob','ing-b2c3')").execute(db.pool()).await.unwrap();
    seed_subscription_serving(&db).await;
    let actor = AdminContext::system_admin("substore-test");
    let readonly = AdminContext::new("reviewer", AdminRole::Readonly, None);
    let other = AdminContext::new("other", AdminRole::TenantAdmin, Some("other".into()));
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "https://console.example".into(),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Container reaches this disposable listener through the bridge; nothing is sent to user services.
    let origin = format!("http://host.docker.internal:{port}");
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let image = GenericImage::new("xream/sub-store", "latest")
        .with_exposed_port(3000.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_env_var("SUB_STORE_BACKEND_API_PORT", "3000")
        .with_env_var("SUB_STORE_BACKEND_API_HOST", "0.0.0.0")
        .with_host(
            "host.docker.internal",
            testcontainers::core::Host::HostGateway,
        );
    let container = image.start().await.unwrap();
    let native_port = container.get_host_port_ipv4(3000).await.unwrap();
    let endpoint = format!("http://127.0.0.1:{native_port}");
    let http = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if http
                .get(format!("{endpoint}/api/subs"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap();
    for user in ["alice", "bob"] {
        connector::save(
            db.pool(),
            &actor,
            "platform.acme",
            user,
            connector::ConfigWrite {
                endpoint: Some(endpoint.clone()),
                title: format!("Brocade · {user}"),
                auto: true,
                version: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let raw: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(c) FROM subscription_connectors c WHERE user_id='alice'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!raw.to_string().contains(&endpoint));
    let read = connector::view(db.pool(), &readonly, "platform.acme", "alice")
        .await
        .unwrap();
    assert!(!read.writable);
    assert!(connector::view(db.pool(), &other, "platform.acme", "alice")
        .await
        .is_err());
    assert!(connector::save(
        db.pool(),
        &readonly,
        "platform.acme",
        "alice",
        connector::ConfigWrite::default()
    )
    .await
    .is_err());
    let alice = connector::enqueue(
        db.pool(),
        &actor,
        "platform.acme",
        "alice",
        "sync",
        None,
        "manual",
    )
    .await
    .unwrap();
    assert_eq!(
        alice,
        connector::enqueue(
            db.pool(),
            &actor,
            "platform.acme",
            "alice",
            "sync",
            None,
            "double click"
        )
        .await
        .unwrap()
    );
    let (_, _, run) = connector::claim(db.pool(), "test-worker")
        .await
        .unwrap()
        .unwrap();
    substore::execute(
        &db.store,
        &origin,
        "test-worker",
        "platform.acme",
        "alice",
        run,
    )
    .await
    .unwrap();
    let result = connector::run(db.pool(), &actor, "platform.acme", "alice", alice)
        .await
        .unwrap();
    assert_eq!(result.status, "succeeded", "{:?}", result.error);
    assert_eq!(result.output_count, Some(1));
    assert!(result.attempts[0]
        .events
        .iter()
        .any(|e| e.name == "创建来源" && e.http == Some(201)));
    assert!(
        connector::run(db.pool(), &actor, "platform.acme", "bob", alice)
            .await
            .is_err()
    );
    let all: Value = http
        .get(format!("{endpoint}/api/subs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first = &all["data"][0];
    let name = first["name"].as_str().unwrap().to_owned();
    http.patch(format!("{endpoint}/api/sub/{name}"))
        .json(&json!({"process":[{"type":"Flag Operator","args":{"add":true}}]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // A lost worker's lease is reclaimed and the prior attempt remains in the persisted ledger.
    let id = connector::enqueue(
        db.pool(),
        &actor,
        "platform.acme",
        "alice",
        "sync",
        None,
        "restart",
    )
    .await
    .unwrap();
    let (_, _, mut lost) = connector::claim(db.pool(), "lost-worker")
        .await
        .unwrap()
        .unwrap();
    lost.attempts.push(connector::Attempt {
        number: 1,
        status: "running".into(),
        events: vec![],
    });
    connector::progress(db.pool(), "lost-worker", &lost)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE subscription_connector_runs SET lease_until=now()-interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    let (_, _, recovered) = connector::claim(db.pool(), "replacement")
        .await
        .unwrap()
        .unwrap();
    substore::execute(
        &db.store,
        &origin,
        "replacement",
        "platform.acme",
        "alice",
        recovered,
    )
    .await
    .unwrap();
    let r = connector::run(db.pool(), &actor, "platform.acme", "alice", id)
        .await
        .unwrap();
    assert_eq!(r.status, "succeeded");
    assert_eq!(r.attempts.len(), 2);
    assert_eq!(r.attempts[0].status, "interrupted");
    let preserved: Value = http
        .get(format!("{endpoint}/api/sub/{name}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preserved["data"]["process"][0]["type"], "Flag Operator");
    connector::enqueue(
        db.pool(),
        &actor,
        "platform.acme",
        "bob",
        "sync",
        None,
        "manual",
    )
    .await
    .unwrap();
    let (_, _, r) = connector::claim(db.pool(), "bob-worker")
        .await
        .unwrap()
        .unwrap();
    substore::execute(&db.store, &origin, "bob-worker", "platform.acme", "bob", r)
        .await
        .unwrap();
    let all: Value = http
        .get(format!("{endpoint}/api/subs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sources = all["data"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0]["url"], sources[1]["url"]);
    // A serving change wakes the real periodic executor; PATCH retains the existing processors.
    let rotated = "e92032bf-6240-4c22-835d-932084280ad1";
    sqlx::query("UPDATE users SET uuid=$1::uuid WHERE tenant_id='platform.acme' AND id='alice'")
        .bind(rotated)
        .execute(db.pool())
        .await
        .unwrap();
    seed_subscription_serving(&db).await;
    assert!(
        !connector::view(db.pool(), &actor, "platform.acme", "alice")
            .await
            .unwrap()
            .config
            .unwrap()
            .verified
    );
    let shutdown = ShutdownSignal::new();
    substore::spawn(db.store.clone(), origin.clone(), shutdown.clone());
    tokio::time::timeout(Duration::from_secs(20),async {
        loop {
            let view=connector::view(db.pool(),&actor,"platform.acme","alice").await.unwrap();
            let active:i64=sqlx::query_scalar("SELECT count(*) FROM subscription_connector_runs WHERE status IN ('queued','running','retry_wait')").fetch_one(db.pool()).await.unwrap();
            if view.config.is_some_and(|c|c.verified) && active==0 {break;}
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.unwrap();
    shutdown.request();
    let updated: Value = http
        .get(format!("{endpoint}/api/sub/{name}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(updated["data"]["url"].as_str().unwrap().contains(rotated));
    assert_eq!(updated["data"]["process"][0]["type"], "Flag Operator");
    // Cleanup remains possible after the user's ordinary subscription has become unavailable.
    sqlx::query("UPDATE users SET status='disabled' WHERE tenant_id='platform.acme' AND id='bob'")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(connector::enqueue(
        db.pool(),
        &actor,
        "platform.acme",
        "bob",
        "sync",
        None,
        "manual"
    )
    .await
    .is_err());
    let cleanup = connector::enqueue(
        db.pool(),
        &actor,
        "platform.acme",
        "bob",
        "cleanup",
        None,
        "manual",
    )
    .await
    .unwrap();
    let (_, _, run) = connector::claim(db.pool(), "cleanup-worker")
        .await
        .unwrap()
        .unwrap();
    substore::execute(
        &db.store,
        &origin,
        "cleanup-worker",
        "platform.acme",
        "bob",
        run,
    )
    .await
    .unwrap();
    assert_eq!(
        connector::run(db.pool(), &actor, "platform.acme", "bob", cleanup)
            .await
            .unwrap()
            .status,
        "succeeded"
    );
    let remaining: Value = http
        .get(format!("{endpoint}/api/subs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(remaining["data"].as_array().unwrap().len(), 1);
    // Removal wipes secrets but leaves audit history. Reconfiguration uses the tombstone version.
    connector::remove(db.pool(), &actor, "platform.acme", "alice")
        .await
        .unwrap();
    let view = connector::view(db.pool(), &actor, "platform.acme", "alice")
        .await
        .unwrap();
    assert!(view.config.is_none());
    assert!(view.latest.is_some());
    assert!(connector::credentials(db.pool(), "platform.acme", "alice")
        .await
        .unwrap()
        .is_none());
    connector::save(
        db.pool(),
        &actor,
        "platform.acme",
        "alice",
        connector::ConfigWrite {
            endpoint: Some(endpoint),
            title: "Reconnected".into(),
            version: Some(view.version),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1"]
async fn substore_upgrade_from_released_0005_preserves_receipts_and_rolls_back_failed_0006() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    // Apply released bytes, with their original SHA-384 receipts, before running the new migrator.
    sqlx::raw_sql("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY,description TEXT NOT NULL,installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),success BOOLEAN NOT NULL,checksum BYTEA NOT NULL,execution_time BIGINT NOT NULL)").execute(db.pool()).await.unwrap();
    use sha2::{Digest, Sha384};
    for (version, source, name) in [
        (
            1,
            include_str!("../../../brocade-store/migrations/0001_init.sql"),
            "init",
        ),
        (
            2,
            include_str!("../../../brocade-store/migrations/0002_forwarding_rule_results.sql"),
            "forwarding rule results",
        ),
        (
            3,
            include_str!("../../../brocade-store/migrations/0003_unlock_probe_schedule.sql"),
            "unlock probe schedule",
        ),
        (
            4,
            include_str!("../../../brocade-store/migrations/0004_user_app_rate_limits.sql"),
            "user app rate limits",
        ),
        (
            5,
            include_str!("../../../brocade-store/migrations/0005_node_xray_enabled.sql"),
            "node xray enabled",
        ),
    ] {
        sqlx::raw_sql(source).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES($1,$2,true,$3,0)").bind(version as i64).bind(name).bind(Sha384::digest(source.as_bytes()).to_vec()).execute(db.pool()).await.unwrap();
    }
    let before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    let check = db.store.check_database_upgrade().await.unwrap();
    assert!(check.compatible);
    assert_eq!(check.pending_versions, vec![6]);
    // Conflicting existing shape fails atomically: no partial connector table or receipt remains.
    sqlx::query("CREATE TABLE subscription_connector_runs(id integer)")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(db.store.migrate_database_schema().await.is_err());
    let absent: bool = sqlx::query_scalar("SELECT to_regclass('subscription_connectors') IS NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(absent);
    sqlx::query("DROP TABLE subscription_connector_runs")
        .execute(db.pool())
        .await
        .unwrap();
    db.store.migrate().await.unwrap();
    let after: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT version,checksum FROM _sqlx_migrations WHERE version<=5 ORDER BY version",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1"]
async fn substore_http_never_authorizes_another_users_resource_or_task() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    sqlx::query("INSERT INTO users(tenant_id,id,uuid) VALUES('platform.acme','bob','4f84aaf8-49ea-4cce-8a93-683ee21ba3c6')").execute(db.pool()).await.unwrap();
    let actor = AdminContext::system_admin("test");
    let login = db
        .store
        .issue_user_login(&actor, "platform.acme", "alice")
        .await
        .unwrap();
    let session = db
        .store
        .login_admin(brocade_store::AdminLoginRequest {
            operator_id: login.operator_id,
            password: login.password,
        })
        .await
        .unwrap();
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "https://console.example".into(),
    );
    let path = "/users/platform.acme/alice/subscription-connectors/sub-store";
    async fn call(
        app: &Router,
        session: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("cookie", format!("brocade_session={session}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    let token = &session.session.token;
    let own = call(&app, token, "GET", path, Value::Null).await;
    assert_eq!(own.0, StatusCode::OK, "{}", own.1);
    assert_eq!(
        call(
            &app,
            token,
            "GET",
            &path.replace("alice", "bob"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let body = json!({"version":0,"endpoint":"http://127.0.0.1:39999/test-private-path","title":"Own source","auto":false});
    let saved = call(&app, token, "PUT", path, body.clone()).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert!(!saved.1.to_string().contains("test-private-path"));
    assert_eq!(
        call(&app, token, "PUT", &path.replace("alice", "bob"), body)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let queued = call(&app, token, "POST", &format!("{path}/sync"), json!({})).await;
    assert_eq!(queued.0, StatusCode::ACCEPTED, "{}", queued.1);
    let id = queued.1["id"].as_i64().unwrap();
    for (method, suffix) in [
        ("GET", format!("runs/{id}")),
        ("DELETE", format!("runs/{id}")),
        ("POST", format!("runs/{id}/retry")),
    ] {
        assert_eq!(
            call(
                &app,
                token,
                method,
                &format!("{}/{}", path.replace("alice", "bob"), suffix),
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    // A version conflict must preserve the original sealed endpoint, rather than silently overwrite.
    assert_eq!(
        call(
            &app,
            token,
            "PUT",
            path,
            json!({"version":0,"title":"stale","auth":false,"auto":false})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
