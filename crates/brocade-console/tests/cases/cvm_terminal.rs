use super::*;
use brocade_deployment::{cvm::terminal as wire, cvm::*};
use brocade_store::cvm::{CvmCreate, CvmQuota};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn socket(address: std::net::SocketAddr, path: &str, token: &str, terminal: bool) -> Socket {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    if terminal {
        request.headers_mut().insert(
            "x-brocade-protocol-version",
            CVM_PROTOCOL_VERSION.to_string().parse().unwrap(),
        );
        request
            .headers_mut()
            .insert(wire::VERSION_HEADER, wire::VERSION.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn browser_socket(address: std::net::SocketAddr, path: &str, cookie: &str) -> Socket {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    request
        .headers_mut()
        .insert("origin", format!("http://{address}").parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn next_message(socket: &mut Socket) -> Message {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match message {
            Message::Ping(p) => socket.send(Message::Pong(p)).await.unwrap(),
            Message::Pong(_) => {}
            other => return other,
        }
    }
}

async fn expect_disconnect(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(p))) => {
                    let _ = socket.send(Message::Pong(p)).await;
                }
                Some(Ok(Message::Pong(_))) => {}
                other => panic!("unexpected data after revocation: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn cvm_terminal_http_websocket_relay_revocation_and_lifecycle() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_usage_model(db.pool()).await;
    let admin = admin_token(&db).await;
    let actor = AdminContext::system_admin("test-admin");
    let mut caps_value = serde_json::to_value(Capabilities::default()).unwrap();
    let fields = json!({"backend":"terminal-test","managed_networks":true,"guest_terminal":true,"guest_terminal_persistent":true,"create":true,"delete":true,"max_interfaces":1,
        "allocatable_vcpus":2,"allocatable_memory_bytes":536870912,"allocatable_disk_bytes":6000000000u64,
        "images":[{"id":"debian","name":"Debian","version":"13","architecture":"x86_64","sha256":"a".repeat(64),"boot_mode":"cube-oci"}],
        "resource_limits":{"min_vcpus":1,"max_vcpus":2,"min_memory_bytes":134217728,"max_memory_bytes":536870912,"memory_step_bytes":67108864,"min_root_disk_bytes":3000000000u64,"max_root_disk_bytes":6000000000u64,"root_disk_step_bytes":1000000000},
        "network_sources":[{"id":"public","name":"Public","kind":"public","description":"mapped"}]});
    caps_value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let caps: Capabilities = serde_json::from_value(caps_value).unwrap();
    db.store
        .cvm_record_inventory(
            "n1",
            InventoryReport {
                capabilities: caps,
                error: None,
            },
        )
        .await
        .unwrap();
    db.store
        .set_cvm_quota(
            &actor,
            CvmQuota {
                host_network_quotas: vec![],
                tenant_id: "platform.acme".into(),
                user_id: "alice".into(),
                enabled: true,
                allowed_nodes: vec!["n1".into()],
                max_instances: 1,
                max_vcpus: 1,
                max_memory_bytes: 134217728,
                max_disk_bytes: 3000000000,
                max_interfaces: 1,
            },
        )
        .await
        .unwrap();
    let instance=db.store.create_cvm(&actor,CvmCreate{request_id:"terminal-http-create-00001".into(),tenant_id:"platform.acme".into(),user_id:"alice".into(),node_id:"n1".into(),name:"terminal".into(),image_id:"debian".into(),resources:brocade_deployment::cvm::Resources{vcpus:1,memory_bytes:134217728,root_disk_bytes:3000000000},
        ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEiBcpQEzqzjADp8VXvOigHnqLh5L5xP76cwoCD8YMK5".into(),interfaces:vec![InterfaceRequest{source_id:"public".into(),purpose:InterfacePurpose::Contact,default_ipv4:false,default_ipv6:false}]}).await.unwrap();
    let d = &instance.desired;
    let id = d.id.clone();
    let report:InstanceReport=serde_json::from_value(json!({"id":id,"operation_id":d.operation_id,"generation":d.generation,"spec_sha256":d.spec_sha256,"status":"applied","power":"running","runtime_id":"owned-runtime","volume_id":"owned-volume","access_address":"192.0.2.1",
        "interfaces":[{"id":d.spec.interfaces[0].id,"device":"eth0","mac":"02:00:00:00:00:01","addresses":["192.0.2.1/24"],"attached":true}],"restart_epoch":d.restart_epoch,"reinstall_epoch":d.reinstall_epoch,"detached_interfaces":[],"network_absent":false,"volume_retained":false,"volume_deleted":false,"error":null})).unwrap();
    assert!(db.store.cvm_record_report("n1", report).await.unwrap());
    let app = merged_router(
        db.store.clone(),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        "http://127.0.0.1".into(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn({
        let app = app.clone();
        async move { axum::serve(listener, app).await.unwrap() }
    });
    let node = db.store.issue_node_token("n1").await.unwrap().token;
    for version in [None, Some(CVM_PROTOCOL_VERSION - 1)] {
        let mut request = format!("ws://{address}/agent/v1/cvm/terminal/control")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {node}").parse().unwrap());
        request
            .headers_mut()
            .insert(wire::VERSION_HEADER, wire::VERSION.parse().unwrap());
        if let Some(version) = version {
            request.headers_mut().insert(
                "x-brocade-protocol-version",
                version.to_string().parse().unwrap(),
            );
        }
        assert!(matches!(
            tokio_tungstenite::connect_async(request).await,
            Err(tokio_tungstenite::tungstenite::Error::Http(_))
        ));
    }
    let mut control = socket(address, "/agent/v1/cvm/terminal/control", &node, true).await;
    // The first ping proves registration has completed before a session is issued.
    let initial = tokio::time::timeout(Duration::from_secs(5), control.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Ping(p) = initial {
        control.send(Message::Pong(p)).await.unwrap()
    } else {
        panic!("expected control heartbeat")
    }
    let policy_path = "/cvm/hosts/n1/terminal";
    let enabled = app
        .clone()
        .oneshot(
            Request::put(policy_path)
                .header("authorization", format!("Bearer {admin}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true,"expected_revision":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    let path = format!("/cvm/{id}/terminal");
    let issued = post_json(&app, &admin, "/users/platform.acme/alice/login", json!({})).await;
    assert_eq!(issued.0, StatusCode::CREATED);
    let cookie = login_cookie(&app, "alice", issued.1["password"].as_str().unwrap()).await;
    let readings = app
        .clone()
        .oneshot(
            Request::get("/cvm/readings")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(readings.status(), StatusCode::OK);
    assert_eq!(readings.headers()["cache-control"], "no-store");
    let readings = response_json(readings).await;
    assert_eq!(readings["instances"].as_array().unwrap().len(), 1);
    assert_eq!(readings["instances"][0]["id"], id);
    assert_eq!(readings["instances"][0]["state"], "unavailable");
    assert!(!readings.to_string().contains("owned-runtime"));
    let request = |origin: &str| {
        Request::post(&path)
            .header("cookie", &cookie)
            .header("host", address.to_string())
            .header("origin", origin)
            .body(Body::empty())
            .unwrap()
    };
    let forbidden = app
        .clone()
        .oneshot(request("https://other.example"))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(request(&format!("http://{address}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let created = (StatusCode::OK, response_json(response).await);
    let session = created.1["session_id"].as_str().unwrap();
    let ticket = created.1["ticket"].as_str().unwrap();
    let browser_path = format!("/cvm/{id}/terminal/{session}/stream");
    let mut browser = browser_socket(address, &browser_path, &cookie).await;
    browser
        .send(Message::text(json!({"ticket":ticket}).to_string()))
        .await
        .unwrap();
    let offer: wire::Offer = serde_json::from_str(
        next_message(&mut control)
            .await
            .into_text()
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert_eq!(offer.target.runtime_id, "owned-runtime");
    assert_eq!(offer.session_id, session);
    let mut guest = socket(
        address,
        &format!("/agent/v1/cvm/terminal/{session}/stream"),
        &node,
        true,
    )
    .await;
    guest
        .send(Message::text(r#"{"type":"ready"}"#))
        .await
        .unwrap();
    assert_eq!(
        next_message(&mut browser).await.into_text().unwrap(),
        r#"{"type":"ready"}"#
    );
    guest
        .send(Message::binary("你好 from Guest".as_bytes().to_vec()))
        .await
        .unwrap();
    assert_eq!(
        next_message(&mut browser).await.into_data(),
        "你好 from Guest".as_bytes()
    );
    browser.send(Message::binary(vec![3])).await.unwrap();
    assert_eq!(next_message(&mut guest).await.into_data().as_ref(), &[3]);
    browser
        .send(Message::text(r#"{"type":"resize","cols":110,"rows":37}"#))
        .await
        .unwrap();
    assert!(next_message(&mut guest)
        .await
        .into_text()
        .unwrap()
        .contains("110"));
    // A consumed ticket cannot attach a second browser or steal the running PTY.
    let mut replay = browser_socket(address, &browser_path, &cookie).await;
    replay
        .send(Message::text(json!({"ticket":ticket}).to_string()))
        .await
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(5), replay.next())
        .await
        .is_ok());
    // Change desired power while the transport remains alive: it must terminate both ends.
    sqlx::query(
        "UPDATE cvm_instances SET desired=jsonb_set(desired,'{power}','\"stopped\"') WHERE id=$1",
    )
    .bind(&id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        next_message(&mut browser).await,
        Message::Close(_)
    ));
    let closed = tokio::time::timeout(Duration::from_secs(5), guest.next())
        .await
        .unwrap();
    assert!(
        closed.is_none()
            || closed
                .as_ref()
                .is_some_and(|r| r.is_err() || matches!(r, Ok(Message::Close(_))))
    );
    assert_eq!(
        post_json(&app, &admin, &path, json!({})).await.0,
        StatusCode::CONFLICT
    );
    sqlx::query(
        "UPDATE cvm_instances SET desired=jsonb_set(desired,'{power}','\"running\"') WHERE id=$1",
    )
    .bind(&id)
    .execute(db.pool())
    .await
    .unwrap();
    // The peer can disconnect immediately after Exit; queued final bytes must drain.
    let final_session = post_json(&app, &admin, &path, json!({})).await.1;
    let final_id = final_session["session_id"].as_str().unwrap();
    let mut final_browser = socket(
        address,
        &format!("/cvm/{id}/terminal/{final_id}/stream"),
        &admin,
        false,
    )
    .await;
    final_browser
        .send(Message::text(
            json!({"ticket":final_session["ticket"]}).to_string(),
        ))
        .await
        .unwrap();
    let _ = next_message(&mut control).await;
    let mut final_guest = socket(
        address,
        &format!("/agent/v1/cvm/terminal/{final_id}/stream"),
        &node,
        true,
    )
    .await;
    final_guest
        .send(Message::binary(b"final tail".to_vec()))
        .await
        .unwrap();
    final_guest
        .send(Message::text(r#"{"type":"exit","code":7}"#))
        .await
        .unwrap();
    final_guest.close(None).await.unwrap();
    assert_eq!(
        next_message(&mut final_browser).await.into_data().as_ref(),
        b"final tail"
    );
    assert_eq!(
        next_message(&mut final_browser).await.into_text().unwrap(),
        r#"{"type":"exit","code":7}"#
    );
    final_browser.close(None).await.unwrap();
    // Disabling the host policy terminates both peers and forbids new sessions.
    let policy_session = post_json(&app, &admin, &path, json!({})).await.1;
    let policy_id = policy_session["session_id"].as_str().unwrap();
    let mut policy_browser = socket(
        address,
        &format!("/cvm/{id}/terminal/{policy_id}/stream"),
        &admin,
        false,
    )
    .await;
    policy_browser
        .send(Message::text(
            json!({"ticket":policy_session["ticket"]}).to_string(),
        ))
        .await
        .unwrap();
    let _ = next_message(&mut control).await;
    let mut policy_guest = socket(
        address,
        &format!("/agent/v1/cvm/terminal/{policy_id}/stream"),
        &node,
        true,
    )
    .await;
    policy_guest
        .send(Message::text(r#"{"type":"ready"}"#))
        .await
        .unwrap();
    let _ = next_message(&mut policy_browser).await;
    for (enabled, expected_revision) in [(false, 1), (true, 2)] {
        let response = app
            .clone()
            .oneshot(
                Request::put(policy_path)
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"enabled":enabled,"expected_revision":expected_revision})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if !enabled {
            assert!(matches!(
                next_message(&mut policy_browser).await,
                Message::Close(_)
            ));
            expect_disconnect(&mut policy_guest).await;
            assert_eq!(
                post_json(&app, &admin, &path, json!({})).await.0,
                StatusCode::FORBIDDEN
            );
        }
    }
    let created = post_json(&app, &admin, &path, json!({})).await;
    assert_eq!(created.0, StatusCode::OK);
    let session = created.1["session_id"].as_str().unwrap();
    let mut browser = socket(
        address,
        &format!("/cvm/{id}/terminal/{session}/stream"),
        &admin,
        false,
    )
    .await;
    browser
        .send(Message::text(
            json!({"ticket":created.1["ticket"]}).to_string(),
        ))
        .await
        .unwrap();
    let _ = next_message(&mut control).await;
    assert!(db
        .store
        .revoke_admin_token(&actor, "test-admin")
        .await
        .unwrap());
    assert!(matches!(
        next_message(&mut browser).await,
        Message::Close(_)
    ));
    server.abort();
}
