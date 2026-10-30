//! Real framing against an independent raw-deflate peer, including old peers in `super::tests`.
use super::*;
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

fn options(address: std::net::SocketAddr) -> crate::options::Options {
    crate::options::Options {
        command: "run".to_owned(),
        server: format!("http://{address}"),
        token: "loopback-test-token".to_owned(),
        state_dir: std::env::temp_dir(),
        apply_mode: crate::options::ApplyMode::StateDir,
        vpngate_stats_window: Duration::from_secs(900),
    }
}

fn accept_compressed(listener: TcpListener) -> TcpStream {
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    assert_eq!(headers["sec-websocket-extensions"], "permessage-deflate");
    assert_eq!(headers["authorization"], "Bearer loopback-test-token");
    let accept = tungstenite::handshake::derive_accept_key(headers["sec-websocket-key"].as_bytes());
    write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n").unwrap();
    stream
}

fn compress(compressor: &mut Compress, payload: &[u8]) -> Vec<u8> {
    let mut compressed = Vec::with_capacity(payload.len() + 128);
    compressor
        .compress_vec(payload, &mut compressed, FlushCompress::Sync)
        .unwrap();
    assert_eq!(&compressed[compressed.len() - 4..], &[0, 0, 255, 255]);
    compressed.truncate(compressed.len() - 4);
    compressed
}

fn send_frame(peer: &mut TcpStream, flags: u8, payload: &[u8]) {
    let mut header = vec![flags];
    if payload.len() < 126 {
        header.push(payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        header.push(126);
        header.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    peer.write_all(&header).unwrap();
    peer.write_all(payload).unwrap();
}

fn receive_frame(peer: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0; 2];
    peer.read_exact(&mut head).unwrap();
    assert_ne!(head[1] & 128, 0, "Agent frames must be masked");
    let mut size = usize::from(head[1] & 127);
    if size == 126 {
        let mut bytes = [0; 2];
        peer.read_exact(&mut bytes).unwrap();
        size = usize::from(u16::from_be_bytes(bytes));
    } else if size == 127 {
        let mut bytes = [0; 8];
        peer.read_exact(&mut bytes).unwrap();
        size = usize::try_from(u64::from_be_bytes(bytes)).unwrap();
    }
    assert!(size <= MAX_FRAME_BYTES);
    let mut mask = [0; 4];
    peer.read_exact(&mut mask).unwrap();
    let mut data = vec![0; size];
    peer.read_exact(&mut data).unwrap();
    for (index, byte) in data.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    (head[0], data)
}

#[tokio::test]
async fn default_compression_really_compresses_and_keeps_context_in_both_directions() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let payload=serde_json::json!({"counters":vec![serde_json::json!({"user":"loopback-user","up":123456,"down":987654});64]}).to_string();
    let expected = payload.clone();
    let peer = thread::spawn(move || {
        let mut peer = accept_compressed(listener);
        let mut encoder = Compress::new(Compression::default(), false);
        let mut decoder = Decompress::new(false);
        for _ in 0..100 {
            let command = compress(&mut encoder, b"{\"type\":\"stop\"}");
            send_frame(&mut peer, 0xc1, &command);
            let (flags, mut compressed) = receive_frame(&mut peer);
            assert_eq!(flags, 0xc1, "a complete compressed text frame is required");
            assert!(compressed.len() < expected.len() / 2);
            compressed.extend_from_slice(&[0, 0, 255, 255]);
            let mut decoded = Vec::with_capacity(expected.len() + 128);
            decoder
                .decompress_vec(&compressed, &mut decoded, FlushDecompress::Sync)
                .unwrap();
            assert_eq!(decoded, expected.as_bytes());
        }
        send_frame(&mut peer, 0x88, &1000_u16.to_be_bytes());
        assert_eq!(receive_frame(&mut peer).0 & 15, 8);
    });
    let mut socket = connect(&options(address)).await.unwrap();
    for _ in 0..100 {
        let frame = socket.next_frame().await.unwrap();
        assert_eq!(frame.payload().as_ref(), b"{\"type\":\"stop\"}");
        send_sample(&mut socket, payload.clone()).await.unwrap();
    }
    assert_eq!(socket.next_frame().await.unwrap().opcode(), OpCode::Close);
    flush_control(&mut socket).await.unwrap();
    peer.join().unwrap();
}

#[tokio::test]
async fn compressed_input_cannot_bypass_the_existing_message_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let mut peer = accept_compressed(listener);
        let payload = vec![b'x'; MAX_FRAME_BYTES + 1];
        let compressed = compress(&mut Compress::new(Compression::default(), false), &payload);
        assert!(compressed.len() < MAX_FRAME_BYTES / 100);
        send_frame(&mut peer, 0xc1, &compressed);
        let (flags, reply) = receive_frame(&mut peer);
        assert_eq!(flags & 15, 8);
        assert_eq!(u16::from_be_bytes([reply[0], reply[1]]), 1009);
    });
    let mut socket = connect(&options(address)).await.unwrap();
    assert!(matches!(
        socket.next_frame().await,
        Err(yawc::WebSocketError::FrameTooLarge)
    ));
    flush_control(&mut socket).await.unwrap();
    peer.join().unwrap();
}
