use super::*;
use serde_json::json;
use std::{
    io::Read,
    net::{SocketAddr, UdpSocket},
    process::{Child, Command, Stdio},
    sync::atomic::AtomicU8,
    time::SystemTime,
};
struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn write_reply(stream: &mut TcpStream, status: u16, grant: &UserBudgetGrant) {
    let body = serde_json::to_vec(grant).unwrap();
    write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(&body).unwrap();
}
fn console(mode: Arc<AtomicU8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let budgets = Arc::new(Mutex::new(BTreeMap::<String, Instant>::new()));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mode = Arc::clone(&mode);
            let budgets = Arc::clone(&budgets);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length < 4096);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: UserBudgetRequest = serde_json::from_slice(&body).unwrap();
                let phase = mode.load(Ordering::SeqCst);
                if phase == 4 {
                    write_reply(&mut stream, 404, &UserBudgetGrant::Retry { after_ms: 100 });
                    return;
                }
                let grant = if phase == 0
                    || phase == 3
                    || (phase == 5 && request.source.contains("#other"))
                {
                    UserBudgetGrant::Unlimited {
                        valid_ms: BUDGET_TTL_MS,
                        managed: phase != 3,
                    }
                } else if phase == 2 {
                    UserBudgetGrant::Suspended {
                        valid_ms: BUDGET_TTL_MS,
                    }
                } else {
                    let key = if request.source.starts_with("fwd:") {
                        "forwarding".to_owned()
                    } else {
                        format!(
                            "access:{}:{}",
                            request.source.split('@').next().unwrap(),
                            if request.source.contains("#other") {
                                "other-app"
                            } else {
                                "main-app"
                            }
                        )
                    };
                    let mut clocks = budgets.lock().unwrap();
                    let next = clocks.entry(key).or_insert_with(Instant::now);
                    let now = Instant::now();
                    if *next > now {
                        UserBudgetGrant::Retry {
                            after_ms: next.duration_since(now).as_millis().clamp(1, 1000) as u64,
                        }
                    } else {
                        let bytes = 100_000;
                        *next = now + Duration::from_secs(1);
                        UserBudgetGrant::Limited {
                            bytes,
                            rate_bps: 800_000,
                            valid_ms: BUDGET_TTL_MS,
                        }
                    }
                };
                write_reply(&mut stream, 200, &grant);
            });
        }
    });
    format!("http://{address}")
}
fn test_service(dir: &Path, server: &str, node: usize, address: SocketAddr) -> Service {
    let token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
    Service {
        options: Options {
            command: "run".into(),
            server: server.into(),
            token: format!("node-{node}"),
            state_dir: dir.into(),
            apply_mode: crate::ApplyMode::StateDir,
            vpngate_stats_window: Duration::from_secs(900),
        },
        token: token.clone(),
        entries: Mutex::new(BTreeMap::new()),
        known: Mutex::new(BTreeSet::new()),
        unknown_checkpoint: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
        control: Mutex::new(Control {
            endpoint: address.to_string(),
            token: token.clone(),
            generation: 0,
        }),
        generation: AtomicU64::new(0),
        policies: Mutex::new(PolicyCache {
            effects: BTreeMap::new(),
            updated: None,
        }),
    }
}
fn local_service(dir: &Path, server: &str, node: usize) -> std::net::SocketAddr {
    std::fs::create_dir_all(dir).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let service = Arc::new(test_service(dir, server, node, address));
    let token = service.token.clone();
    crate::fsutil::atomic_write_private(
        &control_path(dir),
        &serde_json::to_vec(&Control {
            endpoint: address.to_string(),
            token,
            generation: 0,
        })
        .unwrap(),
    )
    .unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let service = Arc::clone(&service);
            thread::spawn(move || {
                let _ = serve(stream, &service);
            });
        }
    });
    address
}
fn connect_vless(port: u16, target: SocketAddr, udp: bool) -> TcpStream {
    connect_vless_user(port, target, udp, 1)
}
fn connect_vless_user(port: u16, target: SocketAddr, udp: bool, identity: u8) -> TcpStream {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut header = vec![0];
    header.extend_from_slice(&[identity; 16]);
    header.extend_from_slice(&[0, if udp { 2 } else { 1 }]);
    header.extend_from_slice(&target.port().to_be_bytes());
    header.push(1);
    let std::net::IpAddr::V4(ip) = target.ip() else {
        panic!("native fixture requires IPv4")
    };
    header.extend_from_slice(&ip.octets());
    stream.write_all(&header).unwrap();
    stream
}
fn tcp_roundtrip(mut stream: TcpStream, bytes: usize, vless: bool) {
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let payload = vec![73u8; bytes];
    stream.write_all(&payload).unwrap();
    if vless {
        let mut header = [0; 2];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(header, [0, 0]);
    }
    let mut response = vec![0; bytes];
    stream.read_exact(&mut response).unwrap();
    assert_eq!(payload, response);
}
#[test]
#[ignore = "requires an explicitly built BROCADE_XRAY_BIN"]
fn user_global_native_tcp_udp_forwarding_and_existing_sessions_share_budget() {
    let binary =
        std::env::var_os("BROCADE_XRAY_BIN").expect("explicit BROCADE_XRAY_BIN is required");
    let root = std::env::temp_dir().join(format!(
        "brocade-user-native-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let addresses = Command::new("hostname")
        .arg("-I")
        .output()
        .expect("hostname -I is required for the native fixture");
    let local_ip = String::from_utf8(addresses.stdout)
        .unwrap()
        .split_whitespace()
        .filter_map(|value| value.parse::<std::net::Ipv4Addr>().ok())
        .find(|ip| ip.is_private())
        .expect("a private IPv4 interface is required for the forwarding fixture");
    let tcp = TcpListener::bind((local_ip, 0)).unwrap();
    let target = tcp.local_addr().unwrap();
    thread::spawn(move || {
        for stream in tcp.incoming().flatten() {
            thread::spawn(move || {
                let mut stream = stream;
                let mut data = [0; 16384];
                while let Ok(size) = stream.read(&mut data) {
                    if size == 0 || stream.write_all(&data[..size]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let udp = UdpSocket::bind(target).unwrap();
    thread::spawn(move || {
        let mut data = [0; 65536];
        while let Ok((size, peer)) = udp.recv_from(&mut data) {
            let _ = udp.send_to(&data[..size], peer);
        }
    });
    let mode = Arc::new(AtomicU8::new(0));
    let server = console(Arc::clone(&mode));
    let mut children = Children(Vec::new());
    let mut ports = Vec::new();
    let mut anytls_ports = Vec::new();
    let mut api_addresses = Vec::new();
    use base64::Engine as _;
    let certificate = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("../http/testdata/localhost.der"))
    );
    let private_key = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("../http/testdata/localhost-key.der"))
    );
    let mut forwards = Vec::new();
    let mut other_ports = Vec::new();
    for node in 0..2 {
        let dir = root.join(node.to_string());
        local_service(&dir, &server, node);
        let port = free_port();
        let api = free_port();
        let forward = free_port();
        let mut config = json!({"log":{"loglevel":"error"},"api":{"tag":"api","listen":format!("127.0.0.1:{api}"),"services":["HandlerService","StatsService"]},"stats":{},"inbounds":[{"tag":"entry","listen":"127.0.0.1","port":port,"protocol":"vless","settings":{"decryption":"none","clients":[{"id":"01010101-0101-0101-0101-010101010101","email":format!("alice@platform#in{node}")},{"id":"02020202-0202-0202-0202-020202020202","email":format!("bob@platform#in{node}")}]}}],"outbounds":[{"tag":"out:forward","protocol":"freedom"}]});
        let anytls = free_port();
        config["inbounds"].as_array_mut().unwrap().push(json!({
            "tag":"anytls", "listen":"127.0.0.1", "port":anytls, "protocol":"anytls",
            "settings":{"users":[{"password":"01010101-0101-0101-0101-010101010101","email":format!("alice@platform#in{node}")}]},
            "streamSettings":{"network":"tcp","security":"tls","tlsSettings":{"certificates":[{"certificate":[certificate],"key":[private_key]}]}}
        }));
        let other_port = free_port();
        config["inbounds"].as_array_mut().unwrap().push(json!({"tag":"other-app","listen":"127.0.0.1","port":other_port,"protocol":"vless","settings":{"decryption":"none","clients":[{"id":"01010101-0101-0101-0101-010101010101","email":format!("alice@platform#other{node}")}]}}));
        other_ports.push(other_port);
        if node == 0 {
            crate::traffic_limit_budget::initialize(&dir).unwrap();
            crate::traffic_limit_budget::configure(false, None).unwrap();
        }
        let file = dir.join("xray.json");
        std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
        let mut command = Command::new(&binary);
        command
            .args(["run", "-config"])
            .arg(&file)
            .env("XRAY_BROCADE_USER_LIMIT_FILE", control_path(&dir))
            .env(
                "xray.anytls.splice",
                std::env::var("BROCADE_E2E_ANYTLS_SPLICE").unwrap_or_else(|_| "on".into()),
            )
            .env_remove("XRAY_BROCADE_MACHINE_LIMIT_FILE")
            .env_remove("XRAY_BROCADE_MACHINE_LIMIT_REQUIRED")
            .stdout(Stdio::from(
                std::fs::File::create(dir.join("xray.log")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("xray-stderr.log")).unwrap(),
            ));
        if node == 0 {
            command
                .env(
                    "XRAY_BROCADE_MACHINE_LIMIT_FILE",
                    crate::traffic_limit_budget::control_path(&dir),
                )
                .env("XRAY_BROCADE_MACHINE_LIMIT_REQUIRED", "1");
        }
        children.0.push(command.spawn().unwrap());
        let address = format!("127.0.0.1:{api}").parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if crate::xray_grpc::query_stats(address, "brocade>>>user_limit>>>supported")
                .is_ok_and(|stats| stats.iter().any(|s| s.value == 1))
            {
                break;
            }
            assert!(Instant::now() < deadline, "Xray user budget not ready");
            thread::sleep(Duration::from_millis(20));
        }
        crate::xray_grpc::add_forwarding_inbound(
            address,
            &format!("fwd:{}", 42 + node),
            brocade_deployment::protocol::ForwardingProtocol::TcpUdp,
            forward,
            &target.ip().to_string(),
            target.port(),
            true,
        )
        .unwrap();
        let anytls_client = free_port();
        let client_path = dir.join("anytls-client.json");
        std::fs::write(&client_path, serde_json::to_vec(&json!({
            "log":{"loglevel":"error"},
            "inbounds":[{"listen":"127.0.0.1","port":anytls_client,"protocol":"dokodemo-door","settings":{"address":target.ip().to_string(),"port":target.port(),"network":"tcp,udp"}}],
            "outbounds":[{"protocol":"anytls","settings":{"address":"127.0.0.1","port":anytls,"password":"01010101-0101-0101-0101-010101010101"},"streamSettings":{"network":"tcp","security":"tls","tlsSettings":{"serverName":"localhost","allowInsecure":true}}}]
        })).unwrap()).unwrap();
        children.0.push(
            Command::new(&binary)
                .args(["run", "-config"])
                .arg(client_path)
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    std::fs::File::create(dir.join("anytls-client.log")).unwrap(),
                ))
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, anytls_client)).is_err() {
            assert!(Instant::now() < deadline, "AnyTLS client not ready");
            thread::sleep(Duration::from_millis(20));
        }
        anytls_ports.push(anytls_client);
        api_addresses.push(address);
        ports.push(port);
        forwards.push(forward);
    }
    crate::identity::self_identity();
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 524288, true);
    assert!(
        now.elapsed() < Duration::from_secs(2),
        "unexhausted traffic was paced"
    );
    mode.store(1, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    thread::scope(|scope| {
        for &port in &ports {
            scope.spawn(move || tcp_roundtrip(connect_vless(port, target, false), 65536, true));
        }
    });
    let tcp_elapsed = now.elapsed();
    assert!(
        tcp_elapsed >= Duration::from_millis(1800),
        "two nodes each received a complete speed allowance: {tcp_elapsed:?}"
    );
    let now = Instant::now();
    thread::scope(|scope| {
        for &port in &anytls_ports {
            scope.spawn(move || {
                tcp_roundtrip(
                    TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap(),
                    65536,
                    false,
                )
            });
        }
    });
    let anytls_elapsed = now.elapsed();
    assert!(
        anytls_elapsed >= Duration::from_millis(1800),
        "AnyTLS bypassed the shared user budget: {anytls_elapsed:?}"
    );
    eprintln!(
        "two-node AnyTLS TCP exact payload 262144 bytes, shared 800 Kbps budget: {anytls_elapsed:?}"
    );
    // The same UUID in another app must not inherit this app's exhausted state.
    mode.store(5, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    tcp_roundtrip(connect_vless(other_ports[0], target, false), 524288, true);
    assert!(
        now.elapsed() < Duration::from_secs(2),
        "app A exhaustion slowed app B"
    );
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 32768, true);
    assert!(
        now.elapsed() >= Duration::from_millis(500),
        "app B availability released app A's limit"
    );
    mode.store(1, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    thread::scope(|scope| {
        for port in [ports[0], other_ports[0]] {
            scope.spawn(move || tcp_roundtrip(connect_vless(port, target, false), 32768, true));
        }
    });
    let apps_elapsed = now.elapsed();
    assert!(
        apps_elapsed < Duration::from_millis(1500),
        "different apps incorrectly shared a user budget: {apps_elapsed:?}"
    );
    let now = Instant::now();
    thread::scope(|scope| {
        for &port in &forwards {
            scope.spawn(move || {
                tcp_roundtrip(
                    TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap(),
                    65536,
                    false,
                )
            });
        }
    });
    assert!(
        now.elapsed() >= Duration::from_millis(1800),
        "forwarding bypassed the shared user budget"
    );
    // UDP on the existing VLESS protocol is charged in both directions without breaking datagrams.
    let now = Instant::now();
    let mut stream = connect_vless(ports[0], target, true);
    for index in 0..32 {
        let payload = vec![index as u8; 1024];
        stream
            .write_all(&(payload.len() as u16).to_be_bytes())
            .unwrap();
        stream.write_all(&payload).unwrap();
        if index == 0 {
            let mut header = [0; 2];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(header, [0, 0]);
        }
        let mut size = [0; 2];
        stream.read_exact(&mut size).unwrap();
        assert_eq!(u16::from_be_bytes(size), 1024);
        let mut response = vec![0; 1024];
        stream.read_exact(&mut response).unwrap();
        assert_eq!(payload, response);
    }
    assert!(
        now.elapsed() >= Duration::from_millis(400),
        "UDP skipped pacing"
    );
    drop(stream);
    mode.store(4, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let mut stream = connect_vless(ports[0], target, false);
    stream.write_all(b"blocked").unwrap();
    let mut response = [0u8; 9];
    assert!(
        stream.read_exact(&mut response).is_err(),
        "old Console released a previously limited user"
    );
    drop(stream);
    mode.store(0, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 8192, true);
    assert!(
        now.elapsed() < Duration::from_secs(1),
        "quota recovery did not remove pacing"
    );
    let mut existing = connect_vless(ports[0], target, false);
    existing.write_all(b"a").unwrap();
    let mut first = [0u8; 3];
    existing.read_exact(&mut first).unwrap();
    mode.store(2, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    existing.write_all(b"b").unwrap();
    let mut byte = [0];
    assert!(
        existing.read_exact(&mut byte).is_err(),
        "suspension left the existing connection passing payload"
    );
    // The machine budget is an outer ceiling shared by all users, including recovered/unrestricted users.
    mode.store(1, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    crate::traffic_limit_budget::configure(true, Some(160_000)).unwrap();
    let now = Instant::now();
    thread::scope(|scope| {
        for (port, identity) in [(ports[0], 1u8), (ports[0], 2), (other_ports[0], 1)] {
            scope.spawn(move || {
                tcp_roundtrip(
                    connect_vless_user(port, target, false, identity),
                    16384,
                    true,
                )
            });
        }
    });
    let machine_elapsed = now.elapsed();
    assert!(
        machine_elapsed >= Duration::from_millis(3900),
        "users/apps bypassed the shared 160 Kbps machine ceiling: {machine_elapsed:?}"
    );
    crate::traffic_limit_budget::configure(true, Some(3_200_000)).unwrap();
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 65536, true);
    assert!(
        now.elapsed() >= Duration::from_millis(1000),
        "raising the machine ceiling released the lower user limit"
    );
    mode.store(0, Ordering::SeqCst);
    crate::traffic_limit_budget::configure(true, Some(160_000)).unwrap();
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 32768, true);
    let recovered_elapsed = now.elapsed();
    assert!(
        recovered_elapsed >= Duration::from_millis(2600),
        "user quota recovery bypassed the active machine limit: {recovered_elapsed:?}"
    );
    mode.store(3, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(BUDGET_TTL_MS + 50));
    let now = Instant::now();
    tcp_roundtrip(connect_vless(ports[0], target, false), 16384, true);
    let canceled_elapsed = now.elapsed();
    assert!(
        canceled_elapsed >= Duration::from_millis(1300),
        "removing the user quota bypassed the active machine limit: {canceled_elapsed:?}"
    );
    let now = Instant::now();
    tcp_roundtrip(
        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, anytls_ports[0])).unwrap(),
        16384,
        false,
    );
    let anytls_machine_elapsed = now.elapsed();
    assert!(
        anytls_machine_elapsed >= Duration::from_millis(1300),
        "AnyTLS bypassed the active machine ceiling: {anytls_machine_elapsed:?}"
    );
    for address in api_addresses {
        let performance =
            crate::xray_grpc::query_stats(address, "anytls>>>performance>>>").unwrap();
        let counter = |suffix: &str| {
            performance
                .iter()
                .find(|s| s.name.ends_with(suffix))
                .map_or(0, |s| s.value)
        };
        eprintln!(
            "AnyTLS performance: authenticated kTLS={}, splice={}, writev={}, machine transfer={anytls_machine_elapsed:?}",
            counter(">>>ktls_authenticated_connections"),
            counter(">>>splice_connections"),
            counter(">>>writev_syscalls")
        );
        if std::env::var_os("BROCADE_REQUIRE_KTLS").is_some() {
            assert!(
                counter(">>>ktls_authenticated_connections") > 0,
                "real kTLS required"
            );
            if std::env::var("BROCADE_E2E_ANYTLS_SPLICE").as_deref() == Ok("off") {
                assert!(
                    counter(">>>writev_syscalls") > 0,
                    "real vectored write required"
                );
            } else {
                assert!(
                    counter(">>>splice_connections") > 0,
                    "real framed splice required"
                );
            }
        }
    }
    eprintln!("independent app budgets: 131072 payload bytes, {apps_elapsed:?}");
    eprintln!(
        "machine ceiling: two users and two apps, 98304 payload bytes, {machine_elapsed:?}; quota recovery with same machine ceiling: {recovered_elapsed:?}; quota removed: {canceled_elapsed:?}"
    );
    eprintln!(
        "two-node user TCP payload 262144 bytes at 800 Kbps: {tcp_elapsed:?}; TCP, UDP, forwarding, app independence, outage and recovery verified"
    );
    drop(children);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn user_global_requirement_survives_corrupt_and_missing_checkpoints() {
    let dir = std::env::temp_dir().join(format!("brocade-user-required-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(required_path(&dir), b"true").unwrap();
    assert!(required_at_start(&dir));
    std::fs::write(required_path(&dir), b"broken").unwrap();
    assert!(required_at_start(&dir));
    std::fs::write(required_path(&dir), b"false").unwrap();
    assert!(!required_at_start(&dir));
    std::fs::write(known_path(&dir), br#"["alice@platform#entry"]"#).unwrap();
    assert!(
        required_at_start(&dir),
        "a stale false support marker cannot override a confirmed managed user"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn user_global_policy_sync_switches_existing_sources_and_acknowledges_each_snapshot() {
    crate::identity::self_identity();
    let dir = std::env::temp_dir().join(format!("brocade-user-sync-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = "alice@platform#entry";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        for (index, effect) in [
            UserTrafficEffect::Unlimited { managed: true },
            UserTrafficEffect::Limited { rate_bps: 8000 },
            UserTrafficEffect::Suspended,
            UserTrafficEffect::Unlimited { managed: false },
        ]
        .into_iter()
        .enumerate()
        {
            let hash = format!("{index:064x}");
            let snapshot = UserTrafficSnapshot {
                sources: vec![UserTrafficAssignment {
                    source: source.into(),
                    effect,
                    revision: index as u64 + 1,
                }],
                sha256: hash.clone(),
            };
            for acknowledgment in [false, true] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(if acknowledgment { "POST " } else { "GET " }));
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if acknowledgment {
                    assert_eq!(serde_json::from_slice::<String>(&body).unwrap(), hash);
                    stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    let body = serde_json::to_vec(&snapshot).unwrap();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    stream.write_all(&body).unwrap();
                }
            }
        }
    });
    let service = test_service(&dir, &format!("http://{address}"), 0, address);
    synchronize(&service).unwrap();
    assert!(matches!(
        assigned(&service, source).unwrap(),
        Some(Admission::Open)
    ));
    assert!(required_at_start(&dir));
    synchronize(&service).unwrap();
    assert!(assigned(&service, source).unwrap().is_none());
    synchronize(&service).unwrap();
    assert!(matches!(
        assigned(&service, source).unwrap(),
        Some(Admission::Suspended)
    ));
    synchronize(&service).unwrap();
    assert!(matches!(
        assigned(&service, source).unwrap(),
        Some(Admission::Pass)
    ));
    assert!(!required_at_start(&dir));
    assert_eq!(service.generation.load(Ordering::Acquire), 4);
    worker.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
