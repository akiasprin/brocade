use super::*;
use brocade_deployment::agent_channel::{AgentChannelRequest, AgentChannelResponse, WakeDomain};
use yawc::{CompressionLevel, Frame, OpCode, Options, WebSocket};

type Socket = WebSocket<tokio::net::TcpStream>;
async fn connect(address: std::net::SocketAddr, token: &str) -> Socket {
    WebSocket::handshake_with_request(
        format!("ws://{address}/agent/v1/realtime").parse().unwrap(),
        tokio::net::TcpStream::connect(address).await.unwrap(),
        Options::default().with_compression_level(CompressionLevel::default()),
        yawc::HttpRequest::builder()
            .header("authorization", format!("Bearer {token}"))
            .header(
                "x-brocade-protocol-version",
                brocade_deployment::protocol::AGENT_PROTOCOL_VERSION,
            )
            .header("x-brocade-channel-rpc", "1"),
    )
    .await
    .unwrap()
}
async fn next(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(4), socket.next_frame())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame.opcode(), OpCode::Text);
    serde_json::from_slice(frame.payload()).unwrap()
}
async fn ready(socket: &mut Socket) {
    assert_eq!(next(socket).await["type"], "stop");
    assert_eq!(next(socket).await["type"], "ready");
    let wake = next(socket).await;
    assert_eq!(wake["type"], "wake");
    assert_eq!(
        wake["domains"].as_array().unwrap().len(),
        WakeDomain::ALL.len()
    );
}
async fn rpc(
    socket: &mut Socket,
    id: u64,
    method: &str,
    path: &str,
    body: Value,
    headers: Vec<(String, String)>,
) -> Value {
    socket
        .send(Frame::text(
            serde_json::to_string(&AgentChannelRequest::Rpc {
                id,
                method: method.into(),
                path: path.into(),
                body: if body.is_null() {
                    String::new()
                } else {
                    body.to_string()
                },
                headers,
                timeout_millis: 3000,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    loop {
        let message = next(socket).await;
        if message["type"] == "rpc_result" {
            assert_eq!(message["id"], id);
            return message;
        }
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn agent_channel_rpc_reuses_authentication_and_commit_notifications_and_recovers_on_reconnect(
) {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = db.store.issue_node_token("n1").await.unwrap().token;
    let realtime = RealtimeService::new(Default::default());
    let shutdown = ShutdownSignal::new();
    let app = agent_router_with_origin_realtime_and_shutdown(
        db.store.clone(),
        "http://127.0.0.1:8080".into(),
        realtime,
        shutdown.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut socket = connect(address, &token).await;
    ready(&mut socket).await;
    let result = rpc(
        &mut socket,
        1,
        "GET",
        "/agent/v1/source-access",
        Value::Null,
        vec![],
    )
    .await;
    assert_eq!(result["status"], 200);
    assert_eq!(
        serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["revision"],
        0
    );
    for (id, path) in [
        (2, "/agent/v1/agent-release"),
        (3, "/agent/v1/xray-release"),
    ] {
        assert_eq!(
            rpc(
                &mut socket,
                id,
                "GET",
                path,
                Value::Null,
                vec![("x-brocade-arch".into(), "x86_64".into())]
            )
            .await["status"],
            204
        );
    }
    let report = json!({"observed_at_unix_secs":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),"family":"v4","ip":"1.1.1.1"});
    assert_eq!(
        rpc(
            &mut socket,
            4,
            "POST",
            "/agent/v1/public-ip-observation",
            report,
            vec![]
        )
        .await["status"],
        200
    );
    let observed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_public_ip_state WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(observed, 1);
    assert_eq!(
        rpc(&mut socket, 5, "POST", "/agent/v1/usage", json!({}), vec![]).await["status"],
        400
    );
    assert_eq!(
        rpc(
            &mut socket,
            6,
            "GET",
            "/agent/v1/source-access",
            Value::Null,
            vec![("authorization".into(), "Bearer another-node".into())]
        )
        .await["status"],
        400
    );

    let mut changes = sqlx::postgres::PgListener::connect_with(db.pool())
        .await
        .unwrap();
    changes.listen("brocade_agent_change").await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE source_access_state SET revision=revision+1")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), changes.recv())
            .await
            .is_err()
    );
    sqlx::query("UPDATE source_access_state SET revision=revision+1")
        .execute(db.pool())
        .await
        .unwrap();
    let notification = tokio::time::timeout(Duration::from_secs(3), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(notification.payload()).unwrap()["domain"],
        "source_access"
    );
    loop {
        let message = next(&mut socket).await;
        if let Ok(AgentChannelResponse::Wake { domains }) = serde_json::from_value(message) {
            if domains.contains(&WakeDomain::SourceAccess) {
                break;
            }
        }
    }
    drop(socket);
    let mut socket = connect(address, &token).await;
    ready(&mut socket).await;
    let result = rpc(
        &mut socket,
        7,
        "GET",
        "/agent/v1/source-access",
        Value::Null,
        vec![],
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["revision"],
        1
    );
    sqlx::query("UPDATE node_agent_state SET token_hash=NULL WHERE node_id='n1'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        rpc(
            &mut socket,
            8,
            "GET",
            "/agent/v1/source-access",
            Value::Null,
            vec![]
        )
        .await["status"],
        401
    );
    shutdown.request();
    server.abort();
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn ip_maintenance_rpc_keeps_data_on_http_and_reuses_node_authentication() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = db.store.issue_node_token("n1").await.unwrap().token;
    let shutdown = ShutdownSignal::new();
    let app = agent_router_with_origin_realtime_and_shutdown(
        db.store.clone(),
        "http://127.0.0.1:8080".into(),
        RealtimeService::new(Default::default()),
        shutdown,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut socket = connect(address, &token).await;
    ready(&mut socket).await;
    let desired = rpc(
        &mut socket,
        1,
        "GET",
        "/agent/v1/ip-maintenance/desired",
        Value::Null,
        vec![],
    )
    .await;
    assert_eq!(desired["status"], 200);
    let desired: Value = serde_json::from_str(desired["body"].as_str().unwrap()).unwrap();
    assert_eq!(desired["generation"], 0);
    assert_eq!(desired["policy"]["enabled"], false);
    let report = json!({"generation":0,"run_generation":0,"sequence":1,"agent_instance_id":"rpc-fixture","applied_control_sequence":0,"applied_stop_sequence":0,"applied_session_generation":0,"worker_observed_at_unix_secs":2000000000,"last_started_at_unix_secs":null,"cooldown_until_unix_secs":null,"phase":"disabled","round_id":null,"module":null,"actual_exit_ip":null,"actions_total":0,"action_index":0,"phase_deadline_unix_secs":null,"next_run_unix_secs":null,"daily_requests":0,"daily_response_bytes":0,"actions":[],"evidence":[],"reason":null,"observed_at_unix_secs":2000000000});
    assert_eq!(
        rpc(
            &mut socket,
            2,
            "POST",
            "/agent/v1/ip-maintenance/report",
            report,
            vec![]
        )
        .await["status"],
        204
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ip_maintenance_state WHERE node_id='n1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_ne!(
        rpc(
            &mut socket,
            3,
            "GET",
            "/agent/v1/ip-maintenance/data",
            Value::Null,
            vec![]
        )
        .await["status"],
        200
    );
    server.abort();
    let _ = server.await;
}
