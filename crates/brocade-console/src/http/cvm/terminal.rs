//! Authenticated WebSocket boundaries for the independent terminal transport.
use super::*;
use crate::cvm::terminal::{Link, Packet, SessionGuard};
use brocade_deployment::cvm::terminal as wire;
use futures_util::SinkExt;
use yawc::{Frame, IncomingUpgrade, OpCode, Options, WebSocket};

fn options() -> Options {
    Options::default()
        .with_max_payload_read(wire::MAX_DATA + 1)
        .with_max_read_buffer(wire::MAX_DATA + 1)
        .with_backpressure_boundary(64 * 1024)
}
fn unavailable(message: impl Into<String>) -> ApiError {
    StoreError::Unavailable(message.into()).into()
}
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return bearer_token(headers).is_some();
    };
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let authority = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or("")),
        None => url.host_str().unwrap_or("").to_owned(),
    };
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| host.eq_ignore_ascii_case(&authority))
}
async fn send<S>(socket: &mut WebSocket<S>, packet: Packet) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = match packet {
        Packet::Data(p) => Frame::binary(p),
        Packet::Control(p) => Frame::text(p),
    };
    tokio::time::timeout(Duration::from_secs(5), socket.send(frame))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
async fn flush<S>(socket: &mut WebSocket<S>) -> Result<(), ()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        std::future::poll_fn(|cx| socket.poll_ready_unpin(cx)).await?;
        socket.flush().await
    })
    .await
    .map_err(|_| ())?
    .map_err(|_: yawc::WebSocketError| ())
}
fn protocol(headers: &HeaderMap) -> ApiResult<()> {
    if headers
        .get(wire::VERSION_HEADER)
        .is_none_or(|v| v != wire::VERSION)
    {
        return Err(StoreError::Unsupported("Guest terminal protocol v2 required".into()).into());
    }
    Ok(())
}

#[derive(Default, Deserialize)]
pub(in crate::http) struct Create {
    #[serde(default)]
    terminal_id: Option<String>,
    #[serde(default)]
    new_session: bool,
}
pub(in crate::http) async fn create(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    request: Option<Json<Create>>,
) -> ApiResult<Response> {
    let request = request.map(|Json(value)| value).unwrap_or_default();
    if !same_origin(&headers) {
        return Err(ApiError::Forbidden);
    }
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    if request
        .terminal_id
        .as_deref()
        .is_some_and(|id| !wire::valid_session(id))
    {
        return Err(StoreError::InvalidData("invalid terminal session identity".into()).into());
    }
    let (node, target) = state.store.cvm_terminal_target(&actor, &id).await?;
    let policy = state.store.cvm_terminal_policy(&node).await?;
    if !policy.enabled {
        return Err(ApiError::Forbidden);
    }
    let (session, ticket) = state
        .terminals
        .create_attachment(
            actor.operator_id(),
            &node,
            target,
            request.terminal_id,
            request.new_session,
            policy.revision,
        )
        .map_err(unavailable)?;
    eprintln!(
        "cvm terminal issued session={session} operator={} instance={id} node={node}",
        actor.operator_id()
    );
    Ok(no_store_json(
        json!({"session_id":session,"ticket":ticket,"expires_in_seconds":30,
            "max_sessions_per_node":wire::MAX_PER_NODE,"history_bytes":wire::HISTORY_BYTES}),
    ))
}

pub(in crate::http) async fn node_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let node = agent_node(&state, &headers).await?;
    let policy = state.store.cvm_terminal_policy(&node.node_id).await?;
    Ok(no_store_json(policy))
}

pub(in crate::http) async fn policy(
    State(state): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    Json(request): Json<brocade_store::cvm::TerminalPolicyUpdate>,
) -> ApiResult<Response> {
    if !same_origin(&headers) {
        return Err(ApiError::Forbidden);
    }
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let enabled = request.enabled;
    state
        .store
        .set_cvm_terminal_policy(&actor, &node, request)
        .await?;
    if !enabled {
        state.terminals.close_node_sessions(&node)
    }
    Ok(no_store_json(state.store.cvm_workspace(&actor).await?))
}

#[derive(Deserialize)]
struct Attach {
    ticket: String,
}

pub(in crate::http) async fn browser(
    State(state): State<AppState>,
    Path((id, session)): Path<(String, String)>,
    headers: HeaderMap,
    upgrade: IncomingUpgrade,
) -> ApiResult<Response> {
    if !wire::valid_session(&session) || !same_origin(&headers) {
        return Err(ApiError::Forbidden);
    }
    let actor = require_admin_context(&state, &headers, AdminPermission::Read).await?;
    let (node, target) = state.store.cvm_terminal_target(&actor, &id).await?;
    let operator = actor.operator_id().to_owned();
    let (response, upgrade) = upgrade
        .upgrade(options())
        .map_err(|_| unavailable("invalid terminal upgrade"))?;
    tokio::spawn(async move {
        let Ok(Ok(mut socket)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await
        else {
            return;
        };
        let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_secs(5), socket.next_frame()).await
        else {
            return;
        };
        if frame.opcode() != OpCode::Text || frame.payload().len() > wire::MAX_CONTROL {
            return;
        }
        let Ok(attach) = serde_json::from_slice::<Attach>(frame.payload()) else {
            return;
        };
        let Ok(link) = state
            .terminals
            .browser(&session, &attach.ticket, &operator, &node, &target)
        else {
            return;
        };
        let _guard = SessionGuard {
            service: state.terminals.clone(),
            id: session.clone(),
        };
        let mut validate = tokio::time::interval(Duration::from_secs(2));
        let shutdown = state.shutdown.clone();
        let check = async move {
            loop {
                validate.tick().await;
                let valid = async {
                    let authentication = authenticate_admin(&state, &headers).await.ok()?;
                    if authentication.admin.operator_id != operator {
                        return None;
                    }
                    let actor = AdminContext::from_authenticated(&authentication.admin);
                    let (actual_node, actual) =
                        state.store.cvm_terminal_target(&actor, &id).await.ok()?;
                    (actual_node == node && actual == target).then_some(())
                };
                if tokio::time::timeout(Duration::from_secs(5), valid)
                    .await
                    .ok()
                    .flatten()
                    .is_none()
                {
                    break;
                }
            }
        };
        tokio::select! {
            _=relay(&mut socket,link,true)=>{},
            _=check=>{},
            _=shutdown.requested()=>{},
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            socket.send(Frame::close(
                yawc::close::CloseCode::Normal,
                b"terminal closed",
            )),
        )
        .await;
        eprintln!("cvm terminal closed session={session}");
    });
    Ok(response.map(Body::new))
}

pub(in crate::http) async fn agent_stream(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    upgrade: IncomingUpgrade,
) -> ApiResult<Response> {
    protocol(&headers)?;
    let node = agent_node(&state, &headers).await?;
    if !wire::valid_session(&session) {
        return Err(ApiError::Forbidden);
    }
    let link = state
        .terminals
        .agent(&session, &node.node_id)
        .map_err(unavailable)?;
    let guard = SessionGuard {
        service: state.terminals.clone(),
        id: session,
    };
    let (response, upgrade) = upgrade
        .upgrade(options())
        .map_err(|_| unavailable("invalid terminal upgrade"))?;
    tokio::spawn(async move {
        let _guard = guard;
        let Ok(Ok(mut socket)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await
        else {
            return;
        };
        tokio::select! {_=relay(&mut socket,link,false)=>{},_=state.shutdown.requested()=>{}}
    });
    Ok(response.map(Body::new))
}

async fn relay<S>(socket: &mut WebSocket<S>, mut link: Link, browser: bool)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let lifetime = tokio::time::sleep(Duration::from_secs(wire::MAX_SECONDS));
    tokio::pin!(lifetime);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    let mut seen = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _=&mut lifetime=>break,
            _=link.cancelled.changed()=>break,
            _=heartbeat.tick()=>{
                if seen.elapsed()>Duration::from_secs(60){break}
                if !matches!(tokio::time::timeout(Duration::from_secs(5),socket.send(Frame::ping(Vec::new()))).await,Ok(Ok(()))){break}
            },
            packet=link.receive.recv()=>{
                let Some(packet)=packet else{break};if send(socket,packet).await.is_err(){break}
            },
            frame=socket.next_frame()=>{
                let Ok(frame)=frame else{break};seen=tokio::time::Instant::now();
                let packet=match frame.opcode(){
                    OpCode::Binary if !frame.payload().is_empty()&&frame.payload().len()<=wire::MAX_DATA=>Packet::Data(frame.payload().to_vec()),
                    OpCode::Text if frame.payload().len()<=wire::MAX_CONTROL=>{
                        let valid=if browser {serde_json::from_slice::<wire::Input>(frame.payload()).is_ok_and(|i|i.valid())}
                            else {serde_json::from_slice::<wire::Output>(frame.payload()).is_ok()};
                        if !valid{break}
                        let Ok(text)=String::from_utf8(frame.payload().to_vec()) else{break};Packet::Control(text)
                    },
                    OpCode::Ping=>{if flush(socket).await.is_err(){break};continue},
                    OpCode::Pong=>continue,
                    _=>break,
                };
                let finished=!browser && matches!(&packet,Packet::Control(text) if matches!(serde_json::from_str::<wire::Output>(text),Ok(wire::Output::Exit{..}|wire::Output::Error{..})));
                if !matches!(tokio::time::timeout(Duration::from_secs(5),link.send.send(packet)).await,Ok(Ok(()))){break}
                if finished {
                    // Let the browser drain the final data/exit before the Agent's
                    // disconnect cancels its peer. The browser closes after rendering.
                    let _=tokio::time::timeout(Duration::from_secs(5),link.cancelled.changed()).await;
                    break;
                }
            }
        }
    }
}

pub(in crate::http) async fn control(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: IncomingUpgrade,
) -> ApiResult<Response> {
    protocol(&headers)?;
    let node = agent_node(&state, &headers).await?;
    let (response, upgrade) = upgrade
        .upgrade(options())
        .map_err(|_| unavailable("invalid terminal control upgrade"))?;
    tokio::spawn(async move {
        let Ok(Ok(mut socket)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await
        else {
            return;
        };
        let Ok((registration, mut offers)) = state.terminals.register(&node.node_id) else {
            return;
        };
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        let mut seen = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _=state.shutdown.requested()=>break,
                offer=offers.recv()=>{
                    let Some(offer)=offer else{break};
                    let Ok(text)=serde_json::to_string(&offer) else{break};
                    if send(&mut socket,Packet::Control(text)).await.is_err(){break}
                },
                _=tick.tick()=>{
                    state.terminals.expire();
                    if seen.elapsed()>Duration::from_secs(45){break}
                    let valid=async {agent_node(&state,&headers).await.is_ok_and(|n|n.node_id==node.node_id)};
                    if !tokio::time::timeout(Duration::from_secs(5),valid).await.unwrap_or(false){break}
                    if !matches!(tokio::time::timeout(Duration::from_secs(5),socket.send(Frame::ping(Vec::new()))).await,Ok(Ok(()))){break}
                },
                frame=socket.next_frame()=>{
                    match frame{
                        Ok(f) if f.opcode()==OpCode::Pong=>seen=tokio::time::Instant::now(),
                        Ok(f) if f.opcode()==OpCode::Ping=>{if flush(&mut socket).await.is_err(){break}},
                        _=>break,
                    }
                }
            }
        }
        state.terminals.unregister(&node.node_id, &registration);
    });
    Ok(response.map(Body::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_origin_must_match_authority() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("console.example:8080"),
        );
        for origin in [
            "https://evil.example",
            "null",
            "https://console.example:8080/path",
        ] {
            headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
            assert!(!same_origin(&headers))
        }
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://console.example:8080"),
        );
        assert!(same_origin(&headers));
    }
}
