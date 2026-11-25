//! Explicit browser interoperability test; no public networking or Guest required.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "requires frontend npm dependencies and Playwright Chromium"]
async fn chromium_terminal_adapter_round_trip() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let mut peer = loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (header, length) = loop {
                let mut data = [0u8; 4096];
                let n = socket.read(&mut data).await.unwrap();
                assert!(n > 0 && request.len() + n <= wire::MAX_SIGNAL);
                request.extend_from_slice(&data[..n]);
                if let Some(at) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..at]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break (at + 4, length);
                }
            };
            assert!(length <= wire::MAX_SDP);
            while request.len() < header + length {
                let mut data = [0u8; 4096];
                let n = socket.read(&mut data).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&data[..n]);
            }
            let value = if request.starts_with(b"POST /offer ") {
                let sdp = String::from_utf8(request[header..header + length].to_vec()).unwrap();
                let (peer, answer) = Peer::answer(
                    &Config::parse("127.0.0.1:0").unwrap(),
                    &wire::RtcOffer {
                        sdp,
                        stun_servers: vec![],
                    },
                )
                .await
                .unwrap();
                Some((peer, answer))
            } else {
                None
            };
            let body = value
                .as_ref()
                .map(|(_, answer)| answer.as_str())
                .unwrap_or("<!doctype html><title>RTC test</title>");
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            if let Some((peer, _)) = value {
                break peer;
            }
        };
        let mut echoed = 0;
        let mut pending: Option<(bool, Vec<u8>)> = None;
        loop {
            if let Some((binary, ref data)) = pending {
                if peer.write(binary, data).unwrap() {
                    pending = None;
                }
            }
            match peer.next().await.unwrap() {
                PeerEvent::Open => {
                    pending = Some((false, br#"{"type":"transport","kind":"webrtc"}"#.to_vec()))
                }
                PeerEvent::Data { binary, data } => {
                    assert!(pending.is_none());
                    echoed += 1;
                    pending = Some((binary, data));
                }
                PeerEvent::Closed => break,
            }
        }
        assert_eq!(echoed, 2);
    });
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let browser = tokio::task::spawn_blocking(move || {
        std::process::Command::new("node")
            .arg(root.join("crates/brocade-agent/tests/terminal_rtc_browser.mjs"))
            .arg(format!("http://{address}"))
            .output()
            .unwrap()
    });
    let output = tokio::time::timeout(Duration::from_secs(40), browser)
        .await
        .unwrap()
        .unwrap();
    if !output.status.success() {
        serving.abort();
    }
    assert!(
        output.status.success(),
        "browser failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
