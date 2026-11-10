//! Independent reverse Guest terminal connections. No input is replayed on reconnect.
use crate::{http::HttpClient, options::Options};
use brocade_deployment::{cvm::terminal as wire, protocol::AGENT_PROTOCOL_VERSION};
use futures_util::SinkExt;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use yawc::{Frame, OpCode, Options as SocketOptions, WebSocket};

static POLICY_REFRESH_RUNNING: AtomicBool = AtomicBool::new(false);
struct PolicyRefreshGuard;
impl Drop for PolicyRefreshGuard {
    fn drop(&mut self) {
        POLICY_REFRESH_RUNNING.store(false, Ordering::Release);
    }
}

pub(crate) fn spawn(options: Options) -> Result<(), String> {
    if options.apply_mode != crate::options::ApplyMode::Linux {
        return Ok(());
    }
    thread::Builder::new()
        .name("cvm-terminal".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(_) => {
                    eprintln!("cvm terminal runtime unavailable");
                    return;
                }
            };
            runtime.block_on(async {
                let mut delay = 5;
                loop {
                    if super::backend::current().is_some() {
                        if let Ok(socket) =
                            connect(&options, "/agent/v1/cvm/terminal/control").await
                        {
                            delay = 5;
                            control(&options, socket).await;
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    delay = (delay * 2).min(60);
                }
            });
        })
        .map_err(|e| format!("cannot spawn Guest terminal worker: {e}"))?;
    Ok(())
}

async fn connect(options: &Options, path: &str) -> Result<WebSocket<crate::http::Stream>, String> {
    let server = options.server.trim_end_matches('/');
    let address = if let Some(host) = server.strip_prefix("https://") {
        format!("wss://{host}{path}")
    } else if let Some(host) = server.strip_prefix("http://") {
        format!("ws://{host}{path}")
    } else {
        return Err("invalid terminal server URL".into());
    };
    let url = address
        .parse()
        .map_err(|_| "invalid terminal WebSocket URL")?;
    let request = yawc::HttpRequest::builder()
        .header("authorization", format!("Bearer {}", options.token))
        .header(wire::VERSION_HEADER, wire::VERSION)
        .header(
            "x-brocade-protocol-version",
            AGENT_PROTOCOL_VERSION.to_string(),
        );
    let deadline = Instant::now() + Duration::from_secs(10);
    let stream = HttpClient::new(&options.server)?
        .connect_stream(deadline)
        .await?;
    let settings = SocketOptions::default()
        .with_max_payload_read(wire::MAX_DATA + 1)
        .with_max_read_buffer(wire::MAX_DATA + 1)
        .with_backpressure_boundary(64 * 1024);
    tokio::time::timeout_at(
        deadline.into(),
        WebSocket::handshake_with_request(url, stream, settings, request),
    )
    .await
    .map_err(|_| "terminal handshake timed out")?
    .map_err(|_| "terminal handshake failed".into())
}
async fn send<S>(socket: &mut WebSocket<S>, frame: Frame) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), socket.send(frame))
        .await
        .map_err(|_| "terminal write timed out")?
        .map_err(|_| "terminal write failed".into())
}
async fn flush<S>(socket: &mut WebSocket<S>) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        std::future::poll_fn(|cx| socket.poll_ready_unpin(cx)).await?;
        socket.flush().await
    })
    .await
    .map_err(|_| "terminal control timed out")?
    .map_err(|_| "terminal control failed".into())
}
async fn control(options: &Options, mut socket: WebSocket<crate::http::Stream>) {
    let mut sessions = tokio::task::JoinSet::new();
    let mut policy_refresh = tokio::task::JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    let mut seen = Instant::now();
    loop {
        tokio::select! {
            _=heartbeat.tick()=>{
                if seen.elapsed()>Duration::from_secs(60)||send(&mut socket,Frame::ping(Vec::new())).await.is_err(){break}
                if policy_refresh.is_empty() && POLICY_REFRESH_RUNNING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                let options = options.clone();
                let guard = PolicyRefreshGuard;
                policy_refresh.spawn(refresh_policy(options, guard));
                }
            },
            result=policy_refresh.join_next(),if !policy_refresh.is_empty()=>{
                if !matches!(result, Some(Ok(Ok(())))) { eprintln!("persistent terminal policy refresh unavailable"); }
            },
            _=sessions.join_next(),if !sessions.is_empty()=>{},
            frame=socket.next_frame()=>{
                let Ok(frame)=frame else{break};seen=Instant::now();
                match frame.opcode(){
                    OpCode::Ping=>if flush(&mut socket).await.is_err(){break},
                    OpCode::Pong=>{},
                    OpCode::Text if frame.payload().len()<=wire::MAX_CONTROL=>{
                        let Ok(offer)=serde_json::from_slice::<wire::Offer>(frame.payload()) else{break};
                        if !wire::valid_session(&offer.session_id)||sessions.len()>=wire::MAX_ATTACHMENTS_PER_NODE{break}
                        let options=options.clone();sessions.spawn(async move{session(&options,offer).await});
                    },
                    _=>break,
                }
            }
        }
    }
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
}
async fn refresh_policy(options: Options, guard: PolicyRefreshGuard) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _guard = guard;

        let response = HttpClient::new(&options.server)?.request_timeout(
            "GET",
            "/agent/v1/cvm/terminal/policy",
            &options.token,
            None,
            Duration::from_secs(5),
        )?;
        if response.status != 200 {
            return Err("terminal policy unavailable".into());
        }
        let policy: serde_json::Value =
            serde_json::from_str(&response.body).map_err(|_| "invalid terminal policy")?;
        let enabled = policy["enabled"]
            .as_bool()
            .ok_or("terminal policy missing")?;
        let through = if enabled {
            policy["revoked_through"].as_u64()
        } else {
            policy["revision"].as_u64()
        }
        .ok_or("terminal policy revision missing")?;
        if through != 0 {
            super::backend::current()
                .ok_or("CVM executor unavailable")?
                .revoke_persistent_terminals(through)?;
        }
        Ok(())
    })
    .await
    .map_err(|_| "terminal policy refresh task failed")?
}

async fn session(options: &Options, offer: wire::Offer) {
    let path = format!("/agent/v1/cvm/terminal/{}/stream", offer.session_id);
    let Ok(mut socket) = connect(options, &path).await else {
        return;
    };
    if bridge(&mut socket, &offer).await.is_err() {
        let message = wire::Output::Error {
            message: "Guest 终端连接已结束，请确认运行栈和实例状态后重试".into(),
        };
        if let Ok(text) = serde_json::to_string(&message) {
            let _ = send(&mut socket, Frame::text(text)).await;
        }
    }
}
async fn bridge(
    socket: &mut WebSocket<crate::http::Stream>,
    offer: &wire::Offer,
) -> Result<(), String> {
    let mut terminal = super::backend::current()
        .ok_or("CVM executor is not initialized")?
        .persistent_terminal(offer.clone())?;
    bridge_io(socket, &terminal.input, &mut terminal.output).await
}

async fn bridge_io<S>(
    socket: &mut WebSocket<S>,
    input: &tokio::sync::mpsc::Sender<brocade_cube::TerminalCommand>,
    output: &mut tokio::sync::mpsc::Receiver<brocade_cube::TerminalEvent>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use brocade_cube::{TerminalCommand, TerminalEvent};
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    let mut seen = Instant::now();
    let lifetime = tokio::time::sleep(Duration::from_secs(wire::MAX_SECONDS));
    tokio::pin!(lifetime);
    loop {
        tokio::select! {
            _=&mut lifetime=>return Ok(()),
            _=heartbeat.tick()=>{
                if seen.elapsed()>Duration::from_secs(60){return Err("terminal peer disconnected".into())}
                send(socket,Frame::ping(Vec::new())).await?;
            },
            event=output.recv()=>{
                match event {
                    Some(TerminalEvent::Data(data))=>send(socket,Frame::binary(data)).await?,
                    Some(TerminalEvent::Output(value))=>{
                        let ended=matches!(value,wire::Output::Exit{..}|wire::Output::Error{..});
                        send(socket,Frame::text(serde_json::to_string(&value).map_err(|_|"invalid terminal output")?)).await?;
                        if ended{return Ok(())}
                    },
                    None=>return Ok(()),
                }
            },
            frame=socket.next_frame()=>{
                let frame=frame.map_err(|_|"terminal peer disconnected")?;seen=Instant::now();
                let command=match frame.opcode(){
                    OpCode::Binary if frame.payload().len()<=wire::MAX_DATA=>Some(TerminalCommand::Data(frame.payload().to_vec())),
                    OpCode::Text if frame.payload().len()<=wire::MAX_CONTROL=>{
                        let control:wire::Input=serde_json::from_slice(frame.payload()).map_err(|_|"invalid terminal input")?;
                        if !control.valid(){return Err("invalid terminal input".into())}
                        Some(TerminalCommand::Control(control))
                    },
                    OpCode::Ping=>{flush(socket).await?;None},
                    OpCode::Pong=>None,
                    OpCode::Close=>{let _=flush(socket).await;return Ok(())},
                    _=>return Err("invalid terminal frame".into()),
                };
                if let Some(command)=command {
                    tokio::time::timeout(Duration::from_secs(5),input.send(command)).await
                        .map_err(|_|"Guest terminal input backpressure timed out")?.map_err(|_|"Guest terminal closed")?;
                    // Explicit Close terminates the Shim session. Keep receiving until its
                    // final output and exit receipt; transport disconnect alone only detaches.
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn typed_guest_channel_forwards_input_output_ack_and_close() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut client =
            WebSocket::from_stream(a, yawc::Role::Client, SocketOptions::default()).unwrap();
        let mut peer =
            WebSocket::from_stream(b, yawc::Role::Server, SocketOptions::default()).unwrap();
        let (input, mut commands) = tokio::sync::mpsc::channel(4);
        let (events, mut output) = tokio::sync::mpsc::channel(2);
        let task = tokio::spawn(async move { bridge_io(&mut client, &input, &mut output).await });
        events
            .send(brocade_cube::TerminalEvent::Data(
                "你好 Guest".as_bytes().to_vec(),
            ))
            .await
            .unwrap();
        let frame = loop {
            let frame = peer.next_frame().await.unwrap();
            if frame.opcode() == OpCode::Ping {
                flush(&mut peer).await.unwrap();
            } else {
                break frame;
            }
        };
        assert_eq!(frame.payload(), "你好 Guest".as_bytes());
        peer.send(Frame::binary(vec![3])).await.unwrap();
        assert!(
            matches!(commands.recv().await.unwrap(),brocade_cube::TerminalCommand::Data(v) if v==vec![3])
        );
        peer.send(Frame::text(r#"{"type":"ack","bytes":12}"#))
            .await
            .unwrap();
        assert!(matches!(
            commands.recv().await.unwrap(),
            brocade_cube::TerminalCommand::Control(wire::Input::Ack { bytes: 12 })
        ));
        peer.send(Frame::text(r#"{"type":"close"}"#)).await.unwrap();
        assert!(matches!(
            commands.recv().await.unwrap(),
            brocade_cube::TerminalCommand::Control(wire::Input::Close)
        ));
        events
            .send(brocade_cube::TerminalEvent::Output(wire::Output::Exit {
                code: 0,
            }))
            .await
            .unwrap();
        assert!(task.await.unwrap().is_ok());
        assert!(events.is_closed());
    }
}
