//! The exact embedded Xray must carry a remote-domain request through the selected ingress.
use brocade_console::http::EMBEDDED_XRAYS;
use brocade_deployment::protocol::{E2eProbeSecurity, E2eProbeTarget, E2eProbeTls};
use brocade_probe::{start_chain_client, ProbeCancellation, ProbeOptions};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
#[test]
fn embedded_xray_chain_client_routes_remote_domains_and_reaps_only_its_own_process() {
    chain_client_reaches_remote_domain(false, None);
}

#[test]
fn ipv4_vless_transport_reaches_an_ipv6_only_remote_domain() {
    chain_client_reaches_remote_domain(true, None);
}

#[test]
fn connection_evidence_survives_tls_vision_anytls_and_hysteria() {
    for mode in ["tls", "vision", "anytls", "hysteria"] {
        chain_client_reaches_remote_domain(true, Some(mode));
    }
}

fn chain_client_reaches_remote_domain(ipv6: bool, traced: Option<&str>) {
    eprintln!("transport={traced:?} ipv6={ipv6}");
    let (_, bytes, _) = EMBEDDED_XRAYS
        .iter()
        .find(|(arch, ..)| *arch == std::env::consts::ARCH)
        .expect("host Xray must be embedded");
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("xray");
    fs::write(&binary, bytes).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["chain.test.invalid".into()]).unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    fs::write(&cert_path, cert.cert.pem()).unwrap();
    fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
    let endpoint = TcpListener::bind(if ipv6 { "[::1]:0" } else { "127.0.0.1:0" }).unwrap();
    let target_port = endpoint.local_addr().unwrap().port();
    endpoint.set_nonblocking(true).unwrap();
    let http = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match endpoint.accept() {
                Ok((s, _)) => break s,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("chain did not reach endpoint: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut data = Vec::new();
        let mut byte = [0];
        while !data.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            data.push(byte[0]);
        }
        assert!(String::from_utf8(data)
            .unwrap()
            .contains("Host: service.chain.invalid"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nloc=JP\n")
            .unwrap();
    });
    let ingress_port = port();
    let uuid = "1bd59ca0-a8f4-4f86-9c0e-23b4f9b08a24";
    let server_log = dir.path().join("server.log");
    let mut config = json!({"log":{"loglevel":"debug","error":server_log,"access":"none"},"dns":{"hosts":{"service.chain.invalid":if ipv6 { "::1" } else { "127.0.0.1" }}},"inbounds":[{"listen":"127.0.0.1","port":ingress_port,"protocol":"vless","settings":{"clients":[{"id":uuid}],"decryption":"none"},"streamSettings":{"network":"tcp","security":"tls","tlsSettings":{"certificates":[{"certificateFile":cert_path,"keyFile":key_path}]}}}],"outbounds":[{"protocol":"freedom","settings":{"domainStrategy":if ipv6 { "ForceIPv6" } else { "ForceIPv4" }}}]});
    if let Some(mode) = traced {
        config["inbounds"][0]["settings"]["clients"][0]["email"] = json!("probe#v6:fixture");
        config["outbounds"][0]["tag"] = json!(brocade_core::model::probe_family_tag(
            "out:forward",
            brocade_core::model::IpFamily::V6
        ));
        config["outbounds"].as_array_mut().unwrap().push(
            json!({"tag":"out:forward","protocol":"freedom","settings":{"domainStrategy":"UseIP"}}),
        );
        match mode {
            "vision" => {
                config["inbounds"][0]["settings"]["clients"][0]["flow"] = json!("xtls-rprx-vision")
            }
            "anytls" => {
                config["inbounds"][0]["protocol"] = json!("anytls");
                config["inbounds"][0]["settings"] =
                    json!({"users":[{"password":uuid,"email":"probe#v6:fixture"}]});
            }
            "hysteria" => {
                config["inbounds"][0]["protocol"] = json!("hysteria");
                config["inbounds"][0]["settings"] =
                    json!({"version":2,"clients":[{"auth":uuid,"email":"probe#v6:fixture"}]});
                config["inbounds"][0]["streamSettings"]["network"] = json!("hysteria");
                config["inbounds"][0]["streamSettings"]["hysteriaSettings"] = json!({"version":2});
                config["inbounds"][0]["streamSettings"]["tlsSettings"]["alpn"] = json!(["h3"]);
            }
            _ => {}
        }
    }
    let profile =
        brocade_deployment::unlock_probe::UnlockProbeProfile::from_config(&config).unwrap();
    let identity = dir.path().join("node-id");
    fs::write(&identity, "exit-fixture").unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
    let config_path = dir.path().join("server.json");
    fs::write(&config_path, config.to_string()).unwrap();
    let _server = Process(
        Command::new(&binary)
            .arg("-config")
            .arg(&config_path)
            .env(
                brocade_deployment::unlock_probe::PROBE_IDENTITY_ENV,
                &identity,
            )
            .env_remove(brocade_deployment::unlock_probe::PROBE_FILE_ENV)
            .env(
                brocade_deployment::unlock_probe::PROBE_PROFILE_ENV,
                serde_json::to_string(&profile).unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if if traced == Some("hysteria") {
            std::net::UdpSocket::bind(("127.0.0.1", ingress_port)).is_err()
        } else {
            TcpStream::connect(("127.0.0.1", ingress_port)).is_ok()
        } {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let mut target = E2eProbeTarget {
        app_id: Some("app-test".into()),
        chain_id: "chain-test".into(),
        chain_name: "Test".into(),
        ingress_id: "ingress-test".into(),
        dial_host: "127.0.0.1".into(),
        port: ingress_port,
        uuid: uuid.into(),
        security: E2eProbeSecurity::Tls(E2eProbeTls {
            server_name: "chain.test.invalid".into(),
            pinned_peer_cert_sha256: Some(brocade_core::hash::sha256_hex(cert.cert.der().as_ref())),
            flow: None,
        }),
        xhttp: None,
        expected_exit_ips: vec![],
    };
    if let Some(mode) = traced {
        let pin = Some(brocade_core::hash::sha256_hex(cert.cert.der().as_ref()));
        match mode {
            "vision"=>if let E2eProbeSecurity::Tls(tls)=&mut target.security {tls.flow=Some("xtls-rprx-vision".into());},
            "anytls"=>target.security=E2eProbeSecurity::AnyTls{settings:brocade_deployment::protocol::E2eProbeAnyTls{server_name:"chain.test.invalid".into(),pinned_peer_cert_sha256:pin,idle_session_check_interval_secs:None,idle_session_timeout_secs:None,min_idle_session:Some(0)},reality:None},
            "hysteria"=>target.security=E2eProbeSecurity::Hysteria2(serde_json::from_value(json!({"server_name":"chain.test.invalid","pinned_peer_cert_sha256":pin,"congestion":"bbr","disable_path_mtu_discovery":false})).unwrap()),
            _=>{},
        }
    }
    let runtime = dir.path().join("clients");
    fs::create_dir(&runtime).unwrap();
    let options =
        ProbeOptions::new(&binary, ProbeCancellation::default()).with_runtime_dir(&runtime);
    let client = if traced.is_some() {
        brocade_probe::start_traced_chain_client(&target, &options, &["gemini.service".into()])
            .unwrap()
    } else {
        start_chain_client(&target, &options).unwrap()
    };
    let socks = client.socks_port();
    let mut stream = TcpStream::connect(("127.0.0.1", socks)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(&[5, 1, 0]).unwrap();
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).unwrap();
    assert_eq!(greeting, [5, 0]);
    let host = b"service.chain.invalid";
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host);
    request.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&request).unwrap();
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0);
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: service.chain.invalid\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    if let Err(error) = stream.read_to_string(&mut response) {
        let logs = fs::read_dir(&runtime)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "log" || ext == "jsonl")
            })
            .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
            .collect::<Vec<_>>();
        panic!(
            "transport {traced:?}: {error}; response={response:?}; client={logs:?}; server={}",
            fs::read_to_string(&server_log).unwrap_or_default()
        );
    }
    assert!(response.ends_with("loc=JP\n"));
    drop(stream);
    if traced.is_some() {
        let data = fs::read_to_string(client.trace_path().unwrap()).unwrap();
        let records = data
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            2,
            "connection start and completion required for {traced:?}"
        );
        assert_eq!(records[0]["id"], records[1]["id"]);
        assert_eq!(records[1]["exit"], "exit-fixture");
        assert_eq!(records[1]["family"], "v6");
        assert_eq!(records[1]["connected"], true);
        assert_eq!(records[1]["complete"], true);
        assert_eq!(records[1]["lookups"][0]["steps"][0]["tag"], "xray.hosts");
    }
    drop(client);
    http.join().unwrap();
    assert!(TcpStream::connect(("127.0.0.1", socks)).is_err());
    assert!(
        if traced == Some("hysteria") {
            std::net::UdpSocket::bind(("127.0.0.1", ingress_port)).is_err()
        } else {
            TcpStream::connect(("127.0.0.1", ingress_port)).is_ok()
        },
        "the serving ingress must survive client cleanup"
    );
    assert_eq!(
        fs::read_dir(&runtime).unwrap().count(),
        0,
        "credential-bearing client files must be removed"
    );
}
