//! All keys/certificates here are synthetic loopback fixtures, trusted only in cfg(test).
use super::*;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig, ServerConnection, StreamOwned,
};
fn configuration(version: &'static rustls::SupportedProtocolVersion) -> Arc<ServerConfig> {
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(
                    include_bytes!("../testdata/localhost.der").to_vec(),
                )],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                    include_bytes!("../testdata/localhost-key.der").to_vec(),
                )),
            )
            .unwrap(),
    )
}
fn serve_request(conn: &mut ServerConnection, tcp: &mut TcpStream) -> bool {
    let mut stream = rustls::Stream::new(conn, tcp);
    let mut reader = BufReader::new(&mut stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        if line == "\r\n" {
            return true;
        }
    }
}
#[test]
fn public_diagnostics_advertise_http1_without_changing_control_tls() {
    warm_control_client();
    for public in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        let mut config = (*configuration(&rustls::version::TLS13)).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let server = thread::spawn(move || {
            let (mut tcp, _) = accept(&listener);
            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut conn = ServerConnection::new(Arc::new(config)).unwrap();
            assert!(serve_request(&mut conn, &mut tcp));
            assert_eq!(
                conn.alpn_protocol(),
                public.then_some(b"http/1.1".as_slice())
            );
            rustls::Stream::new(&mut conn, &mut tcp)
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let client = HttpClient::new(&origin).unwrap();
        let response = if public {
            client.request_public_on_family(
                AddressFamily::V4,
                "GET",
                "/fixture",
                Duration::from_secs(5),
            )
        } else {
            bounded_request(&client, "GET", b"", Duration::from_secs(5))
        }
        .unwrap();
        assert_eq!(response.body, "{}");
        server.join().unwrap();
    }
    assert!(tls_config().unwrap().alpn_protocols.is_empty());
}
#[test]
fn public_tls_sessions_are_reused_only_within_the_same_diagnostic_client() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let config = configuration(&rustls::version::TLS13);
    let server = thread::spawn(move || {
        let mut handshakes = Vec::new();
        for _ in 0..3 {
            let (mut tcp, _) = accept(&listener);
            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut conn = ServerConnection::new(config.clone()).unwrap();
            assert!(serve_request(&mut conn, &mut tcp));
            handshakes.push(conn.handshake_kind().unwrap());
            rustls::Stream::new(&mut conn, &mut tcp)
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        }
        handshakes
    });
    let first = diagnostic_family_client(AddressFamily::V4).unwrap();
    let second = diagnostic_family_client(AddressFamily::V4).unwrap();
    for proxy in [&first, &first, &second] {
        let response = HttpClient::new(&origin)
            .unwrap()
            .with_diagnostic_proxy(Some(proxy))
            .request_public_diagnostic(
                "GET",
                "/fixture",
                None,
                "application/json",
                &[],
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(response.body, "{}");
    }
    assert_eq!(
        server.join().unwrap(),
        [
            rustls::HandshakeKind::Full,
            rustls::HandshakeKind::Resumed,
            rustls::HandshakeKind::Full
        ]
    );
}

#[test]
fn http_tls_control_messages_cannot_extend_the_request_deadline() {
    warm_control_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tcp, _) = accept(&listener);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut conn = ServerConnection::new(configuration(&rustls::version::TLS13)).unwrap();
        if !serve_request(&mut conn, &mut tcp) {
            return;
        }
        for _ in 0..20 {
            if conn.refresh_traffic_keys().is_err() {
                return;
            }
            while conn.wants_write() {
                if conn.write_tls(&mut tcp).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    let start = Instant::now();
    let error = bounded_request(
        &HttpClient::new(&origin).unwrap(),
        "GET",
        b"",
        Duration::from_millis(150),
    )
    .unwrap_err();
    let elapsed = start.elapsed();
    server.join().unwrap();
    assert!(error.contains("timeout"), "{error}");
    assert!(elapsed < Duration::from_millis(450), "{elapsed:?}");
}
#[tokio::test]
async fn websocket_upgrade_uses_the_same_absolute_deadline_beneath_tls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut tcp, _) = accept(&listener);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut conn = ServerConnection::new(configuration(&rustls::version::TLS13)).unwrap();
        if !serve_request(&mut conn, &mut tcp) {
            return;
        }
        for _ in 0..20 {
            if conn.refresh_traffic_keys().is_err() {
                return;
            }
            while conn.wants_write() {
                if conn.write_tls(&mut tcp).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    let options = crate::options::Options {
        command: "run".to_owned(),
        server: format!("https://{address}"),
        token: "test-node".to_owned(),
        state_dir: std::env::temp_dir(),
        apply_mode: crate::options::ApplyMode::StateDir,
        vpngate_stats_window: Duration::from_secs(900),
    };
    let start = Instant::now();
    assert!(
        crate::realtime::connect_until(&options, start + Duration::from_millis(150))
            .await
            .is_err()
    );
    let elapsed = start.elapsed();
    server.join().unwrap();
    assert!(elapsed < Duration::from_millis(450), "{elapsed:?}");
}
#[test]
fn tls_handshake_stall_is_bounded_and_the_connection_is_closed() {
    warm_control_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tcp, _) = accept(&listener);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buffer = [0; 8192];
        loop {
            match tcp.read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return,
                Err(error) => panic!("socket remained open: {error}"),
            }
        }
    });
    let started = Instant::now();
    let error = bounded_request(
        &HttpClient::new(&origin).unwrap(),
        "GET",
        b"",
        Duration::from_millis(150),
    )
    .unwrap_err();
    let elapsed = started.elapsed();
    server.join().unwrap();
    assert!(error.contains("timeout"), "{error}");
    assert!(elapsed < Duration::from_millis(450));
}
#[test]
fn framed_tls_eof_is_accepted_but_unframed_or_truncated_tls_eof_is_rejected() {
    for (wire, ok, version) in [
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"[..],
            true,
            &rustls::version::TLS12,
        ),
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"[..],
            true,
            &rustls::version::TLS13,
        ),
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}"[..],
            false,
            &rustls::version::TLS13,
        ),
        (
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n"[..],
            false,
            &rustls::version::TLS13,
        ),
        (
            &b"HTTP/1.1 200 OK\r\n\r\n{}"[..],
            false,
            &rustls::version::TLS13,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut tcp, _) = accept(&listener);
            tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut conn = ServerConnection::new(configuration(version)).unwrap();
            if !serve_request(&mut conn, &mut tcp) {
                return;
            }
            let mut stream = rustls::Stream::new(&mut conn, &mut tcp);
            stream.write_all(wire).unwrap();
            stream.flush().unwrap();
        });
        let result = bounded_request(
            &HttpClient::new(&origin).unwrap(),
            "GET",
            b"",
            Duration::from_secs(2),
        );
        server.join().unwrap();
        assert_eq!(result.is_ok(), ok, "{result:?}");
    }
}
#[test]
fn a_wrong_tls_certificate_name_fails_closed() {
    let listener = TcpListener::bind("127.0.0.2:0").unwrap();
    let origin = format!("https://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut tcp, _) = accept(&listener);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut conn = ServerConnection::new(configuration(&rustls::version::TLS13)).unwrap();
        while conn.is_handshaking() {
            if conn.complete_io(&mut tcp).is_err() {
                break;
            }
        }
    });
    assert!(bounded_request(
        &HttpClient::new(&origin).unwrap(),
        "GET",
        b"",
        Duration::from_secs(2)
    )
    .is_err());
    server.join().unwrap();
}
#[tokio::test]
async fn a_successful_websocket_does_not_keep_the_handshake_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (tcp, _) = accept(&listener);
        tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let conn = ServerConnection::new(configuration(&rustls::version::TLS13)).unwrap();
        let stream = StreamOwned::new(conn, tcp);
        let _socket = tungstenite::accept(stream).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    let options = crate::options::Options {
        command: "run".to_owned(),
        server: format!("https://{address}"),
        token: "test-node".to_owned(),
        state_dir: std::env::temp_dir(),
        apply_mode: crate::options::ApplyMode::StateDir,
        vpngate_stats_window: Duration::from_secs(900),
    };
    let mut socket =
        crate::realtime::connect_until(&options, Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), socket.next_frame())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    server.join().unwrap();
}
