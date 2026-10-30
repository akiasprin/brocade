use super::*;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use yawc::{CompressionLevel, Frame, OpCode, Options, Role, WebSocket};

struct CaptureFirstFrame {
    stream: tokio::net::TcpStream,
    first: Arc<Mutex<Option<u8>>>,
}
impl AsyncRead for CaptureFirstFrame {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.stream).poll_read(cx, buf);
        if let Some(first) = buf.filled().get(before) {
            this.first.lock().unwrap().get_or_insert(*first);
        }
        result
    }
}
impl AsyncWrite for CaptureFirstFrame {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, data)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[tokio::test]
#[ignore = "requires BROCADE_RUN_PG_TESTS=1 and PostgreSQL"]
async fn realtime_default_compression_carries_commands_and_samples_and_limits_inflation() {
    let Some(db) = TestPg::start_if_enabled().await else {
        return;
    };
    db.store.migrate().await.unwrap();
    insert_node(db.pool()).await;
    let token = db.store.issue_node_token("n1").await.unwrap().token;
    let realtime = RealtimeService::new(Default::default());
    let app = agent_router_with_origin_realtime_and_shutdown(
        db.store.clone(),
        "http://127.0.0.1:8080".to_owned(),
        realtime.clone(),
        ShutdownSignal::new(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!("GET /agent/v1/realtime HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Extensions: permessage-deflate\r\nAuthorization: Bearer {token}\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !response.ends_with(b"\r\n\r\n") {
            assert!(response.len() < 4096);
            response.push(stream.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 101"));
    let extensions = response
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-extensions")
                .then(|| value.trim().to_owned())
        })
        .expect("compression must be negotiated");
    assert_eq!(extensions, "permessage-deflate");
    let first = Arc::new(Mutex::new(None));
    let mut socket = WebSocket::from_stream_with_extensions(
        CaptureFirstFrame {
            stream,
            first: first.clone(),
        },
        Role::Client,
        Some(&extensions),
        Options::default().with_compression_level(CompressionLevel::default()),
    )
    .unwrap();
    let stop = socket.next_frame().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<brocade_deployment::protocol::AgentRealtimeCommand>(
            stop.payload()
        )
        .unwrap(),
        brocade_deployment::protocol::AgentRealtimeCommand::Stop
    );
    assert_ne!(
        first.lock().unwrap().unwrap() & 0x40,
        0,
        "the first actual frame must carry RSV1, not merely advertise compression"
    );
    let mut subscription = realtime.subscribe(["n1".to_owned()]).await;
    let command = socket.next_frame().await.unwrap();
    assert!(matches!(
        serde_json::from_slice::<brocade_deployment::protocol::AgentRealtimeCommand>(
            command.payload()
        )
        .unwrap(),
        brocade_deployment::protocol::AgentRealtimeCommand::Start { .. }
    ));
    let sample = brocade_deployment::protocol::AgentRealtimeSample {
        diagnostics_unchanged: false,
        mux_unchanged: false,
        reverse_health: None,
        mux: None,
        vpngate: None,
        user_traffic: None,
        sequence: 1,
        sampled_at_unix_millis: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        elapsed_millis: 1000,
        interface: "eth0".to_owned(),
        rx_bytes_per_sec: 12345,
        tx_bytes_per_sec: 678,
        has_gap: false,
    };
    socket
        .send(Frame::text(serde_json::to_string(&sample).unwrap()))
        .await
        .unwrap();
    let forwarded = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let RealtimeBroadcast::Sample(event) = subscription.events.recv().await.unwrap() {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(forwarded.sample.rx_bytes_per_sec, 12345);
    socket
        .send(Frame::ping(b"compressed-idle-ping".to_vec()))
        .await
        .unwrap();
    let pong = tokio::time::timeout(Duration::from_secs(3), socket.next_frame())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pong.opcode(), OpCode::Pong);
    assert_eq!(pong.payload().as_ref(), b"compressed-idle-ping");
    socket
        .send(Frame::text(" ".repeat(512 * 1024 + 1)))
        .await
        .unwrap();
    let close = tokio::time::timeout(Duration::from_secs(3), socket.next_frame())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(close.opcode(), OpCode::Close);
    assert_eq!(close.close_code(), Some(yawc::close::CloseCode::Size));
    server.abort();
}
