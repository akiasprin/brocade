//! The always-connected Agent transport. Browser leases control sampling, not this socket.
use super::*;
use futures_util::SinkExt;
use yawc::{CompressionLevel, Frame, IncomingUpgrade, OpCode, Options, WebSocket};

use brocade_deployment::agent_channel::MAX_FRAME_BYTES;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn websocket_options() -> Options {
    Options::default()
        .with_compression_level(CompressionLevel::default())
        .with_utf8()
        .with_read_buffer_capacity(4 * 1024)
        .with_backpressure_boundary(1024 * 1024)
        // yawc's size limits are exclusive; preserve the existing inclusive 512 KiB bound.
        .with_max_payload_read(MAX_FRAME_BYTES + 1)
        .with_max_read_buffer(MAX_FRAME_BYTES + 1)
}

async fn send_frame<S>(socket: &mut WebSocket<S>, frame: Frame) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if frame.payload().len() > MAX_FRAME_BYTES {
        return Err(());
    }
    tokio::time::timeout(WRITE_TIMEOUT, socket.send(frame))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

async fn flush_control<S>(socket: &mut WebSocket<S>) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Automatic Pong/Close replies are queued until readiness is driven, including the final
    // peer Close. A normal flush alone would leave these behind when the handler exits.
    tokio::time::timeout(WRITE_TIMEOUT, async {
        std::future::poll_fn(|cx| socket.poll_ready_unpin(cx)).await?;
        socket.flush().await
    })
    .await
    .map_err(|_| ())?
    .map_err(|_: yawc::WebSocketError| ())
}

pub(super) async fn agent_realtime(
    State(state): State<AppState>,
    headers: HeaderMap,
    websocket: IncomingUpgrade,
) -> ApiResult<Response> {
    let token = bearer_token(&headers)
        .ok_or(ApiError::Unauthorized)?
        .to_owned();
    let node = authenticate_agent(&state.store, &headers).await?;
    require_active_agent(&node)?;
    let node_id = node.node_id;
    let rpc = headers
        .get(brocade_deployment::agent_channel::CAPABILITY_HEADER)
        .is_some_and(|value| value == "1");
    let rpc_context = if rpc {
        state
            .realtime
            .ensure_change_listener(state.store.clone(), state.shutdown.clone());
        let mut authenticated = HeaderMap::new();
        for name in ["authorization", "x-brocade-protocol-version", "user-agent"] {
            if let Some(value) = headers.get(name) {
                authenticated.insert(name, value.clone());
            }
        }
        Some((agent_routes().with_state(state.clone()), authenticated))
    } else {
        None
    };
    let store = state.store.clone();
    let shutdown = state.shutdown.clone();
    let (response, upgrade) = websocket
        .upgrade(websocket_options())
        .map_err(|_| StoreError::InvalidData("invalid realtime WebSocket upgrade".to_owned()))?;
    tokio::spawn(async move {
        if let Ok(Ok(socket)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await {
            serve_agent_realtime(
                state.realtime,
                store,
                node_id,
                token,
                shutdown,
                socket,
                rpc_context,
            )
            .await;
        }
    });
    Ok(response.map(Body::new))
}

async fn serve_agent_realtime<S>(
    realtime: crate::realtime::RealtimeService,
    store: PgStore,
    node_id: String,
    token: String,
    shutdown: crate::lifecycle::ShutdownSignal,
    mut socket: WebSocket<S>,
    rpc_context: Option<(Router, HeaderMap)>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let mut session = realtime.register_agent(node_id.clone()).await;
    let session_id = session.session;
    let initial = session.commands.borrow_and_update().clone();
    let Ok(initial) = serde_json::to_string(&initial) else {
        realtime.unregister_agent(&node_id, session_id).await;
        return;
    };
    if send_frame(&mut socket, Frame::text(initial)).await.is_err() {
        realtime.unregister_agent(&node_id, session_id).await;
        return;
    }
    use brocade_deployment::agent_channel::{
        AgentChannelRequest, AgentChannelResponse, WakeDomain, MAX_IN_FLIGHT,
    };
    let mut changes = session.changes.clone();
    let mut seen_changes = *changes.borrow_and_update();
    let mut tasks = tokio::task::JoinSet::new();
    if rpc_context.is_some() {
        for response in [
            AgentChannelResponse::Ready { version: 1 },
            AgentChannelResponse::Wake {
                domains: WakeDomain::ALL.to_vec(),
            },
        ] {
            let Ok(text) = serde_json::to_string(&response) else {
                return;
            };
            if send_frame(&mut socket, Frame::text(text)).await.is_err() {
                realtime.unregister_agent(&node_id, session_id).await;
                return;
            }
        }
    }
    // `interval` ticks immediately. That first empty Ping can race the demand command sent when a
    // subscriber arrives, making a freshly connected Agent observe control traffic out of order.
    // The initial command above already proves the socket is writable, so the first heartbeat is
    // due only after one complete idle interval.
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reauthenticate = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(60),
        Duration::from_secs(60),
    );
    reauthenticate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = shutdown.requested() => {
                let _ = send_frame(&mut socket, Frame::close(yawc::close::CloseCode::Normal, b"console shutdown")).await;
                break;
            }
            changed = session.commands.changed() => {
                if changed.is_err() { break; }
                let command = session.commands.borrow_and_update().clone();
                let Ok(text) = serde_json::to_string(&command) else { break };
                if send_frame(&mut socket, Frame::text(text)).await.is_err() {
                    break;
                }
            }
            changed = changes.changed(), if rpc_context.is_some() => {
                if changed.is_err() { break; }
                // A brief debounce folds a transaction burst without delaying existing RPC replies.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let latest = *changes.borrow_and_update();
                let domains = WakeDomain::ALL.into_iter().filter(|domain| latest[*domain as usize] != seen_changes[*domain as usize]).collect();
                seen_changes = latest;
                let Ok(text) = serde_json::to_string(&AgentChannelResponse::Wake { domains }) else { break };
                if send_frame(&mut socket, Frame::text(text)).await.is_err() { break; }
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                let Some(Ok(response)) = result else { break };
                let Ok(text) = serde_json::to_string(&response) else { break };
                if send_frame(&mut socket, Frame::text(text)).await.is_err() { break; }
            }
            incoming = socket.next_frame() => {
                match incoming {
                    Ok(frame) if frame.opcode() == OpCode::Text => {
                        if let Ok(request) = serde_json::from_slice::<AgentChannelRequest>(frame.payload()) {
                            let Some((router, authenticated)) = &rpc_context else { break };
                            if tasks.len() >= MAX_IN_FLIGHT { break; }
                            let router = router.clone();
                            let authenticated = authenticated.clone();
                            tasks.spawn(super::agent_rpc::dispatch(router, authenticated, request));
                            continue;
                        }
                        let sample = match serde_json::from_slice::<AgentRealtimeSample>(frame.payload()) {
                            Ok(sample) => sample,
                            Err(_) => break,
                        };
                        match realtime.record_sample(&node_id, session_id, sample).await {
                            Ok(()) | Err(crate::realtime::RecordSampleError::Inactive) => {}
                            Err(_) => break,
                        }
                    }
                    Ok(frame) if frame.opcode() == OpCode::Ping => {
                        if flush_control(&mut socket).await.is_err() { break; }
                    }
                    Ok(frame) if frame.opcode() == OpCode::Pong => {}
                    Ok(frame) if frame.opcode() == OpCode::Close => {
                        let _ = flush_control(&mut socket).await;
                        break;
                    }
                    Err(_) => {
                        let _ = flush_control(&mut socket).await;
                        break;
                    }
                    // This endpoint has one deliberately narrow JSON text protocol.
                    Ok(_) => break,
                }
            }
            _ = heartbeat.tick() => {
                if send_frame(&mut socket, Frame::ping(Vec::new())).await.is_err()
                {
                    break;
                }
            }
            _ = reauthenticate.tick() => {
                // A WebSocket outlives the request that authenticated it. Re-check periodically so
                // revoking a token or retiring a node also closes an already-open live channel.
                let still_allowed = store
                    .authenticate_node_token(&token)
                    .await
                    .is_ok_and(|node| {
                        node.is_some_and(|node| {
                            node.node_id == node_id
                                && node.lifecycle_phase == NodeLifecyclePhase::Active
                        })
                    });
                if !still_allowed {
                    break;
                }
            }
        }
    }
    realtime.unregister_agent(&node_id, session_id).await;
}
