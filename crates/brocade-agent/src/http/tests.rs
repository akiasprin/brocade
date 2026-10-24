use super::*;
use std::{
    io::{BufRead, BufReader},
    net::{Shutdown, TcpListener},
    sync::mpsc,
    thread,
};

fn accept(listener: &TcpListener) -> (TcpStream, SocketAddr) {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                stream.set_nonblocking(false).unwrap();
                return (stream, peer);
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("test listener did not accept a request: {error}"),
        }
    }
}
fn warm_control_client() {
    client::shared(
        client::TrafficClass::Control,
        None,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
}

pub(crate) fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (headers.to_ascii_lowercase(), body)
}
pub(crate) fn response_from_wire(raw: &[u8]) -> Result<HttpResponse, String> {
    exchange(raw, "GET", false)
}
fn exchange(raw: &[u8], method: &str, keep_open: bool) -> Result<HttpResponse, String> {
    client::shared(
        client::TrafficClass::Public,
        Some(AddressFamily::V4),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let raw = raw.to_vec();
    let (release, wait) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        read_request(&mut stream);
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let _ = stream.write_all(&raw);
        if keep_open {
            let _ = wait.recv_timeout(Duration::from_secs(10));
        }
    });
    let result = HttpClient::new(&origin).unwrap().request_public_on_family(
        AddressFamily::V4,
        method,
        "/fixture",
        Duration::from_secs(10),
    );
    let _ = release.send(());
    server.join().unwrap();
    result
}
fn bounded_request(
    client: &HttpClient,
    method: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<HttpResponse, String> {
    warm_control_client();
    client.request_inner(
        method,
        "/fixture?secret=do-not-log",
        Some("synthetic-node-token"),
        RequestBody {
            bytes: body,
            content_type: "application/json",
        },
        &[],
        RequestTransport {
            family: None,
            timeout,
        },
    )
}

#[test]
fn framed_responses_finish_without_waiting_for_connection_close() {
    for (wire, method, body, status) in [
        (&b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"[..], "GET", "{}", 200),
        (&b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;foo=bar\r\nhello\r\n0\r\n\r\n"[..], "POST", "hello", 200),
        (&b"HTTP/1.1 204 No Content\r\n\r\n"[..], "GET", "", 204),
        (&b"HTTP/1.1 304 Not Modified\r\n\r\n"[..], "GET", "", 304),
        (&b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n"[..], "HEAD", "", 200),
    ] {
        let response=exchange(wire,method,true).unwrap();
        assert_eq!(response.status,status);assert_eq!(response.body,body);
    }
}
#[test]
fn interim_responses_and_trailers_never_override_final_policy_headers() {
    for header in [
        "Transfer-Encoding: chunked",
        "transfer-encoding: Chunked",
        "Transfer-Encoding:  chunked ",
    ] {
        let raw = format!("HTTP/1.1 200 OK\r\n{header}\r\n\r\n5\r\nhello\r\n0\r\n\r\n");
        assert_eq!(response_from_wire(raw.as_bytes()).unwrap().body, "hello");
    }

    let response=response_from_wire(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nX-Early: ignored\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-Policy: original\r\n\r\n2\r\n{}\r\n0\r\nX-Policy: replaced\r\n\r\n").unwrap();
    assert_eq!(response.body, "{}");
    assert_eq!(response.header("X-POLICY"), Some("original"));
    assert_eq!(response.header("x-early"), None);
}
#[test]
fn malformed_and_truncated_bodies_never_become_successful_responses() {
    for raw in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n"[..],
        &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}"[..],
        &b"HTTP/1.1 zzz OK\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\n\r\n\xff"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n{}"[..],
    ] {
        assert!(
            response_from_wire(raw).is_err(),
            "accepted malformed response"
        );
    }
}
#[test]
fn body_limit_is_independent_of_framing_and_headers_have_a_separate_limit() {
    let mut raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_HTTP_RESPONSE_BYTES}\r\n\r\n")
        .into_bytes();
    raw.resize(raw.len() + MAX_HTTP_RESPONSE_BYTES, b'x');
    assert_eq!(
        response_from_wire(&raw).unwrap().body.len(),
        MAX_HTTP_RESPONSE_BYTES
    );
    let over = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_HTTP_RESPONSE_BYTES + 1
    );
    assert!(response_from_wire(over.as_bytes())
        .unwrap_err()
        .contains("body_exceeds_limit"));
    let headers = format!(
        "HTTP/1.1 200 OK\r\nX-Large: {}\r\nContent-Length: 0\r\n\r\n",
        "x".repeat(MAX_HTTP_HEADER_BYTES)
    );
    assert!(response_from_wire(headers.as_bytes()).is_err());
}
#[test]
fn utf8_lengths_are_bytes_and_content_encoding_is_not_implicitly_decoded() {
    assert_eq!(
        response_from_wire("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n你好".as_bytes())
            .unwrap()
            .body,
        "你好"
    );
    assert_eq!(
        response_from_wire(b"HTTP/1.0 200 OK\r\n\r\nhello")
            .unwrap()
            .body,
        "hello"
    );
    assert_eq!(
        response_from_wire(
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}"
        )
        .unwrap()
        .body,
        "{}"
    );
}
#[test]
fn redirects_and_error_statuses_return_to_the_application_without_replay() {
    for status in [302, 409, 429, 503] {
        let raw=format!("HTTP/1.1 {status} Test\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 2\r\n\r\n{{}}");
        let response = exchange(raw.as_bytes(), "POST", false).unwrap();
        assert_eq!(response.status, status);
    }
}
#[test]
fn truncated_post_response_is_sent_exactly_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        let (_, body) = read_request(&mut stream);
        assert_eq!(body, b"{}");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}")
            .unwrap();
        stream.shutdown(Shutdown::Both).unwrap();
        listener.set_nonblocking(true).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(matches!(listener.accept(),Err(error) if error.kind()==io::ErrorKind::WouldBlock));
    });
    let error = HttpClient::new(&origin)
        .unwrap()
        .request_public("POST", "/", Some("{}"))
        .unwrap_err();
    server.join().unwrap();
    assert!(error.contains("phase=body"));
    assert!(error.contains("status=200"));
    assert!(error.contains("received_bytes=2"));
    assert!(error.contains("content_length=10"));
}
#[test]
fn one_deadline_covers_headers_and_all_body_reads_and_closes_the_socket() {
    warm_control_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        read_request(&mut stream);
        thread::sleep(Duration::from_millis(60));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
            .unwrap();
        for _ in 0..8 {
            thread::sleep(Duration::from_millis(35));
            if stream.write_all(b"x").is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    let error = bounded_request(
        &HttpClient::new(&origin).unwrap(),
        "GET",
        b"",
        Duration::from_millis(160),
    )
    .unwrap_err();
    let elapsed = start.elapsed();
    server.join().unwrap();
    assert!(elapsed < Duration::from_millis(300), "{elapsed:?}: {error}");
    assert!(error.contains("timeout"), "{error}");
    assert!(error.contains("phase=body"));
    assert!(!error.contains("do-not-log"));
    assert!(!error.contains("synthetic-node-token"));
}
#[test]
fn slow_optional_requests_do_not_block_control_requests() {
    client::shared(
        client::TrafficClass::Public,
        Some(AddressFamily::V4),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (entered, wait) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        read_request(&mut stream);
        entered.send(()).unwrap();
        let _ = resume.recv_timeout(Duration::from_secs(5));
    });
    let client = HttpClient::new(&origin).unwrap();
    let stalled = thread::spawn(move || {
        client.request_public_on_family(AddressFamily::V4, "GET", "/", Duration::from_secs(5))
    });
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        response_from_wire(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap()
            .status,
        200
    );
    let _ = release.send(());
    assert!(stalled.join().unwrap().is_err());
    server.join().unwrap();
}
#[test]
fn public_requests_do_not_send_node_credentials_and_binary_requests_preserve_bytes() {
    for public in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/base/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = accept(&listener);
            let request = read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 409 Conflict\r\nContent-Length: 2\r\nX-Policy: yes\r\n\r\n{}")
                .unwrap();
            request
        });
        let client = HttpClient::new(&origin).unwrap();
        let response = if public {
            client.request_public("GET", "/intel", None)
        } else {
            client.request_bytes_with_headers(
                "POST",
                "/catalogue",
                "synthetic-node-token",
                &[0, 0x9f, 0xff, 7],
                "application/gzip",
                &[("X-Extra", "yes".to_owned())],
            )
        }
        .unwrap();
        let (headers, body) = server.join().unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(response.header("X-POLICY"), Some("yes"));
        if public {
            assert!(!headers.contains("authorization:"));
            assert!(!headers.contains("x-brocade-protocol-version:"));
        } else {
            assert!(headers.starts_with("post /base/catalogue "));
            assert!(headers.contains("authorization: bearer synthetic-node-token"));
            assert!(headers.contains("content-type: application/gzip"));
            assert!(headers.contains("x-extra: yes"));
            assert_eq!(body, [0, 0x9f, 0xff, 7]);
        }
    }
}
#[test]
fn family_requests_never_fall_back_to_the_other_address_family() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = HttpClient::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    assert!(client
        .request_public_on_family(AddressFamily::V6, "GET", "/", Duration::from_millis(100))
        .unwrap_err()
        .contains("no IPv6 addresses"));
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(),Err(error) if error.kind()==io::ErrorKind::WouldBlock));
}

/// Without an explicit port the Host header must not carry one either. With
/// it, a name-based reverse proxy on the far side finds no matching site and
/// the agent connects fine but gets a 404.
#[test]
fn the_default_port_is_left_out_of_the_host_header() {
    let client = HttpClient::new("http://example.test").unwrap();
    assert_eq!(client.port, 80);
    assert_eq!(client.host_header, "example.test");
    assert_eq!(client.prefix, "", "没有路径就没有前缀");

    let client = HttpClient::new("http://[::1]").unwrap();
    assert_eq!(client.host, "::1");
    assert_eq!(client.port, 80);
    assert_eq!(client.host_header, "[::1]");
}

/// A trailing slash on the path prefix must go. Kept, the joined path reads
/// `/base//agent/v1/desired`, which some reverse proxies route elsewhere.
#[test]
fn a_trailing_slash_in_the_prefix_is_trimmed() {
    assert_eq!(HttpClient::new("http://h/base/").unwrap().prefix, "/base");
    assert_eq!(HttpClient::new("http://h/base").unwrap().prefix, "/base");
    assert_eq!(HttpClient::new("http://h/").unwrap().prefix, "");
}

/// A broken port must be reported on the spot. Falling back to 80 sends the
/// agent at a port nobody listens on, and the error reads "cannot connect" —
/// which sends the operator to the network, not to their own URL.
#[test]
fn a_broken_port_is_reported_instead_of_falling_back_to_80() {
    for url in [
        "http://example.test:",
        "http://example.test:http",
        "http://example.test:70000",
        "http://[::1]:70000",
    ] {
        let error = HttpClient::new(url).unwrap_err();
        assert!(
            error.contains("invalid port"),
            "{url} 该报端口无效，实际 {error}"
        );
    }
}

/// https means TLS, port 443, and no 443 in the Host header. A name-based
/// reverse proxy matches on that header; carrying the port finds no site,
/// and the symptom is a connection that works and 404s on everything.
#[test]
fn https_urls_get_tls_and_port_443() {
    let client = HttpClient::new("https://example.test").unwrap();
    assert!(client.tls);
    assert_eq!(client.port, 443);
    assert_eq!(client.host_header, "example.test");

    let client = HttpClient::new("https://example.test:8443").unwrap();
    assert!(client.tls);
    assert_eq!(client.port, 8443);
    assert_eq!(client.host_header, "example.test:8443", "非默认端口要带上");

    let client = HttpClient::new("http://example.test").unwrap();
    assert!(!client.tls, "http 不该被悄悄升级成 TLS");
    assert_eq!(client.port, 80);

    // 443 is not the default under http, and 80 is not the default under
    // https, so both must keep their port in the header.
    assert_eq!(
        HttpClient::new("http://example.test:443")
            .unwrap()
            .host_header,
        "example.test:443"
    );
    assert_eq!(
        HttpClient::new("https://example.test:80")
            .unwrap()
            .host_header,
        "example.test:80"
    );
}

/// A URL with no scheme is an error, not something to default to http.
/// Defaulted, a slip like `relay.example:8081` parses as host `relay.example` on port
/// 8081 over plaintext: the token goes out in the clear and everything about
/// it looks normal.
#[test]
fn a_url_without_a_scheme_is_rejected() {
    for url in [
        "relay.example:8081",
        "example.test",
        "ftp://example.test",
        "",
    ] {
        let error = HttpClient::new(url).unwrap_err();
        assert!(
            error.contains("must start with http:// or https://"),
            "{url} 实际 {error}"
        );
    }
}

/// The TLS configuration builds, and twice asking yields the same one.
/// Rebuilt per request, all ~150 trust anchors would be re-parsed every 15
/// seconds for nothing.
#[test]
fn the_tls_config_is_built_once_and_reused() {
    let first = tls_config().expect("信任锚和 provider 都在二进制里，不该建不起来");
    let second = tls_config().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second), "该是同一份");
}

/// A URL with no host must not pass. An empty host survives all the way to
/// connect, where the error talks about resolution failure — several layers
/// away from "your URL is missing a host".
#[test]
fn an_empty_host_is_rejected_early() {
    for url in ["http://", "http://:8080", "http://[]:8080", "http://]bad"] {
        assert!(HttpClient::new(url).is_err(), "{url} 不该被接受");
    }
}

/// An unclosed bracket is an error too. Taking everything before `]` as the
/// host would connect somewhere whose name does not match.
#[test]
fn an_unclosed_bracket_is_rejected() {
    let error = HttpClient::new("http://[::1:8080").unwrap_err();
    assert!(error.contains("bracketed IPv6"), "实际 {error}");
}

mod tls;

#[test]
fn an_invalid_bearer_header_does_not_expose_the_token_or_url_query() {
    let error = HttpClient::new("http://127.0.0.1:1")
        .unwrap()
        .request(
            "GET",
            "/private?key=hidden-query",
            "hidden-token\r\ninvalid",
            None,
        )
        .unwrap_err();
    assert!(error.contains("builder"), "{error}");
    for secret in ["hidden-query", "hidden-token", "/private"] {
        assert!(!error.contains(secret), "{error}");
    }
}

#[test]
fn valid_utf8_response_header_values_preserve_legacy_compatibility() {
    let response = response_from_wire(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Location: 香港\r\n\r\n{}".as_bytes(),
    )
    .unwrap();
    assert_eq!(response.header("x-location"), Some("香港"));
}

#[test]
fn control_requests_reuse_one_connection_after_framed_responses() {
    warm_control_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        for (index, response) in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n"[..],
            &b"HTTP/1.1 204 No Content\r\n\r\n"[..],
        ]
        .into_iter()
        .enumerate()
        {
            let (headers, body) = read_request(&mut stream);
            assert!(!headers.contains("connection: close"));
            assert!(headers.contains(&format!("x-sequence: {index}")));
            assert_eq!(body, b"{}");
            stream.write_all(response).unwrap();
        }
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    });
    let client = HttpClient::new(&origin).unwrap();
    for index in 0..3 {
        let response = client
            .request_with_headers(
                "POST",
                "/",
                "test-node",
                Some("{}"),
                &[("X-Sequence", index.to_string())],
            )
            .unwrap();
        assert_eq!(response.status, if index == 2 { 204 } else { 200 });
    }
    server.join().unwrap();
}

#[test]
fn a_pooled_post_with_a_truncated_response_is_never_replayed() {
    warm_control_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = accept(&listener);
        read_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap();
        let (_, body) = read_request(&mut stream);
        assert_eq!(body, b"mutation");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}")
            .unwrap();
        stream.shutdown(Shutdown::Both).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    });
    let client = HttpClient::new(&origin).unwrap();
    assert_eq!(
        client
            .request("GET", "/", "test-node", None)
            .unwrap()
            .status,
        200
    );
    assert!(client
        .request("POST", "/", "test-node", Some("mutation"))
        .unwrap_err()
        .contains("phase=body"));
    server.join().unwrap();
}

#[test]
fn readiness_wait_wakes_on_data_and_respects_the_sample_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = accept(&listener);
    let mut stream = Stream::Plain(DeadlineTcpStream {
        tcp: client,
        deadline: None,
    });
    stream.finish_handshake().unwrap();
    let started = Instant::now();
    stream
        .wait_for_io(false, Some(Duration::from_millis(25)))
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(20));
    peer.write_all(b"ready").unwrap();
    stream.wait_for_io(false, None).unwrap();
    let mut bytes = [0; 5];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"ready");
}
