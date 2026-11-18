use super::*;
use std::time::Duration;
fn tunnel(side: EntryTunnelSide, slot: u16) -> EntryTunnel {
    EntryTunnel {
        id: format!("entry-tunnel-{slot}"),
        side,
        entry_node: "hk".into(),
        target_node: "jp".into(),
        offer: TransportOffer {
            listen_side: TransportListenSide::Provider,
            kind: TransportKind::Wireguard,
            endpoint: "203.0.113.12".parse().unwrap(),
            port_start: 40000,
            port_end: 40010,
            mtu: 1420,
        },
        wireguard_transport: EntryWireguardTransport::Udp,
        port: 40000,
        slot,
        credential: "a".repeat(44),
        peer_credential: "b".repeat(44),
    }
}
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "entry-logs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(p.join("cvm-entries")).unwrap();
    p
}
fn desired() -> EntryDesired {
    EntryDesired {
        node_id: "local".into(),
        generation: 2,
        tunnels: vec![tunnel(EntryTunnelSide::Target, 1)],
        ..Default::default()
    }
}
fn report(d: &EntryDesired, connected: bool) -> EntryNodeReport {
    EntryNodeReport {
        generation: d.generation,
        tunnels: vec![EntryTunnelReport {
            id: d.tunnels[0].id.clone(),
            connected,
            error: None,
        }],
        ..Default::default()
    }
}
#[test]
fn redacts_json_credentials_and_pem_lines_before_truncating() {
    let secret = "0123456789abcdef".repeat(4);
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{secret}\n-----END PRIVATE KEY-----");
    let values = secrets(&[serde_json::json!({"packet_key":secret}).to_string(), pem]);
    let text = redact(&format!("error: {secret} token\n\u{1b}[31m"), &values);
    assert!(!text.contains(&secret));
    assert!(text.contains("[已隐藏]"));
    assert!(!text.contains('\u{1b}'));
    assert!(clip(&"中文".repeat(400), 500).len() <= 500);
}
#[test]
fn wireguard_errors_keep_the_same_safe_cause_for_entry_and_egress() {
    let cases = [
        ("fopen: Permission denied", "配置访问权限被拒绝"),
        ("PERMISSION DENIED", "配置访问权限被拒绝"),
        ("failed to execute wg (os error 13)", "配置访问权限被拒绝"),
        (
            "Unable to modify interface: Operation not permitted",
            "系统拒绝网络配置权限",
        ),
        ("failed (os error 1)", "系统拒绝网络配置权限"),
        ("command timed out after 20s", "配置命令超时"),
        ("failed (os error 110)", "配置命令超时"),
        (
            "Unable to modify interface: Address already in use",
            "监听地址或端口已被占用",
        ),
        (
            "Unable to access interface: No such device",
            "网卡或网络命名空间不存在",
        ),
        (
            "Cannot open network namespace example: No such file or directory",
            "网卡或网络命名空间不存在",
        ),
        ("fopen: No such file or directory", "程序或配置文件不存在"),
        ("wg: command not found", "程序或配置文件不存在"),
        (
            "Key is not the correct length or format: key-canary",
            "密钥格式无效",
        ),
        ("unrecognized failure", "配置命令失败"),
    ];
    for (raw, cause) in cases {
        let entry = wireguard_error("WireGuard 入口通道配置", raw);
        let egress = wireguard_error("WireGuard 出口网卡配置", raw);
        assert!(entry.contains(cause), "{entry}");
        assert!(egress.contains(cause), "{egress}");
        assert_eq!(
            entry.split_once('：').unwrap().1,
            egress.split_once('：').unwrap().1
        );
    }
}
#[test]
fn wireguard_errors_never_echo_keys_even_for_unknown_failures() {
    let secret = "wireguard-private-key-canary";
    for raw in [
        format!("Key is not the correct length or format: {secret}"),
        format!("wg set private-key /private/{secret}: Permission denied"),
        format!("unknown failure: {secret}\n\u{1b}[31m{}", "x".repeat(20000)),
    ] {
        for stage in ["WireGuard 入口通道配置", "WireGuard 出口网卡配置"] {
            let safe = wireguard_error(stage, &raw);
            assert!(safe.starts_with(stage));
            assert!(!safe.contains(secret));
            assert!(!safe.contains("/private/"));
            assert!(!safe.chars().any(char::is_control));
            assert!(safe.len() < 500);
        }
    }
}
#[test]
fn wireguard_command_failure_reaches_reports_and_logs_without_raw_output() {
    let secret = "wireguard-private-key-canary";
    let root = root();
    let script = root.join("wg-failure.sh");
    std::fs::write(
        &script,
        "echo 'wireguard-private-key-canary' >&2; echo 'fopen: Permission denied' >&2; exit 1\n",
    )
    .unwrap();
    let raw = crate::command::run_command("sh", &[script.to_str().unwrap()]).unwrap_err();
    assert!(raw.contains(secret));
    assert!(raw.contains("fopen: Permission denied"));
    let d = desired();
    let mut entry = report(&d, false);
    entry.tunnels[0].error = Some(wireguard_error("WireGuard 入口通道配置", &raw));
    finish(&root, &d, &mut entry);
    entry.validate().unwrap();
    assert!(entry
        .diagnostics
        .as_ref()
        .unwrap()
        .events
        .iter()
        .any(|event| event.message.contains("配置访问权限被拒绝")));
    let binding = NicBindingReport {
        local_block: None,
        traffic: None,
        id: "binding-test".into(),
        side: BindingSide::Consumer,
        generation: 3,
        phase: BindingPhase::Active,
        configured: false,
        reachable: None,
        rx_bytes: 0,
        tx_bytes: 0,
        error: Some(wireguard_error("WireGuard 出口网卡配置", &raw)),
    };
    let (phase, detail) = binding_summary(&binding, &[]);
    assert_eq!(phase, NetworkResourcePhase::Failed);
    assert!(detail.contains("WireGuard 出口网卡配置：配置访问权限被拒绝"));
    for encoded in [
        serde_json::to_string(&entry).unwrap(),
        serde_json::to_string(&binding).unwrap(),
    ] {
        assert!(encoded.contains("配置访问权限被拒绝"));
        assert!(!encoded.contains(secret));
        assert!(!encoded.contains("wg-failure.sh"));
        assert!(!encoded.contains("fopen:"));
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn drains_oversized_and_unterminated_lines_without_retaining_prefixes() {
    let input = format!("{}secret\nok\nlast", "a".repeat(20000));
    let mut lines = Vec::new();
    drain(input.as_bytes(), |s| lines.push(s.to_owned()));
    assert_eq!(lines, ["[日志行过长，已省略]", "ok", "last"]);
}
#[test]
fn bounded_ring_is_private_persistent_and_survives_reload() {
    use std::os::unix::fs::PermissionsExt;
    let root = root();
    let d = desired();
    begin(&root, &d);
    for i in 0..300 {
        record(
            &root,
            Some(&d.tunnels[0].id),
            NetworkLogSource::Agent,
            NetworkLogLevel::Info,
            &format!("event {i}"),
        );
    }
    let mut r = report(&d, true);
    finish(&root, &d, &mut r);
    assert_eq!(r.diagnostics.as_ref().unwrap().events.len(), CAPACITY);
    assert!(r.diagnostics.as_ref().unwrap().dropped_events > 0);
    assert_eq!(
        std::fs::metadata(path(&root)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    registry().lock().unwrap().remove(&root);
    let mut next = report(&d, true);
    finish(&root, &d, &mut next);
    let previous = r.diagnostics.unwrap();
    let current = next.diagnostics.unwrap();
    assert_eq!(&current.events[..CAPACITY - 1], &previous.events[1..]);
    assert!(current
        .events
        .last()
        .unwrap()
        .message
        .contains("规则应用已确认"));
    assert_eq!(current.dropped_events, previous.dropped_events + 1);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn wait_times_out_and_recovers_using_the_same_tunnel_identity() {
    let root = root();
    let d = desired();
    begin(&root, &d);
    let mut first = report(&d, false);
    finish(&root, &d, &mut first);
    assert_eq!(
        first.diagnostics.unwrap().resources[0].phase,
        NetworkResourcePhase::Waiting
    );
    registry()
        .lock()
        .unwrap()
        .get_mut(&root)
        .unwrap()
        .waiting
        .insert(
            d.tunnels[0].id.clone(),
            std::time::Instant::now() - Duration::from_secs(121),
        );
    let mut failed = report(&d, false);
    finish(&root, &d, &mut failed);
    assert!(failed.tunnels[0].error.as_ref().unwrap().contains("120 秒"));
    assert_eq!(
        failed.diagnostics.unwrap().resources[0].phase,
        NetworkResourcePhase::Failed
    );
    let mut recovered = report(&d, true);
    finish(&root, &d, &mut recovered);
    assert!(recovered.tunnels[0].error.is_none());
    assert_eq!(
        recovered.diagnostics.unwrap().resources[0].phase,
        NetworkResourcePhase::Connected
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn child_pipe_combines_stdout_stderr_and_sanitizes_split_reads() {
    let root = root();
    let secret = "a-secret-credential-123456789";
    let mut c = Command::new("sh");
    c.args([
        "-c",
        "printf 'one\\n'; printf '%s\\n' \"$TEST_CREDENTIAL\" >&2",
    ]);
    c.env("TEST_CREDENTIAL", secret);
    capture(
        &mut c,
        &root,
        "tunnel-test",
        NetworkLogSource::Phantun,
        secrets(&[secret.into()]),
    );
    assert!(c.spawn().unwrap().wait().unwrap().success());
    drop(c);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let data = registry()
            .lock()
            .unwrap()
            .get(&root)
            .map(|r| r.data.clone());
        if let Some(data) = data.filter(|d| d.events.len() >= 2) {
            assert_eq!(data.events[0].message, "one");
            assert_eq!(data.events[1].message, "[已隐藏]");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn logger_disk_failure_does_not_fail_network_report() {
    let root = root();
    let d = desired();
    std::fs::create_dir(path(&root)).unwrap();
    let mut r = report(&d, true);
    finish(&root, &d, &mut r);
    assert!(r.error.is_none());
    assert!(r.tunnels[0].connected);
    assert!(r
        .diagnostics
        .unwrap()
        .events
        .iter()
        .any(|e| e.message.contains("历史日志写入失败")));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn entry_reports_preserve_instance_and_egress_progress() {
    let root = root();
    let d = desired();
    observe(
        &root,
        "cvm-native",
        NetworkResourcePhase::Connected,
        "裸机网络配置确认".into(),
    );
    observe(
        &root,
        "binding-egress",
        NetworkResourcePhase::Failed,
        "出口链路配置失败".into(),
    );
    let mut r = report(&d, true);
    finish(&root, &d, &mut r);
    let data = r.diagnostics.unwrap();
    assert_eq!(data.resources.len(), 3);
    assert!(data.resources.iter().any(|p| p.id == "cvm-native"));
    assert!(data
        .resources
        .iter()
        .any(|p| p.id == "binding-egress" && p.phase == NetworkResourcePhase::Failed));
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn shared_progress_window_cannot_make_network_reports_exceed_protocol_bounds() {
    let root = root();
    let d = desired();
    let resources = (0..1024)
        .map(|i| NetworkResourceProgress {
            id: format!("binding-{i}"),
            phase: NetworkResourcePhase::Connected,
            since: 1,
            waiting_seconds: 0,
            message: "confirmed".into(),
        })
        .collect();
    {
        let mut all = registry().lock().unwrap();
        runtime(&mut all, &root).data.resources = resources;
    }
    let mut r = report(&d, true);
    finish(&root, &d, &mut r);
    r.validate().unwrap();
    assert_eq!(r.diagnostics.as_ref().unwrap().resources.len(), 1024);
    assert!(r
        .diagnostics
        .unwrap()
        .resources
        .iter()
        .any(|p| p.id == d.tunnels[0].id));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unconfirmed_egress_cleanup_is_not_reported_as_complete() {
    let mut report = NicBindingReport {
        local_block: None,
        traffic: None,
        id: "binding-test".into(),
        side: BindingSide::Consumer,
        generation: 1,
        phase: BindingPhase::Withdrawing,
        configured: false,
        reachable: None,
        rx_bytes: 0,
        tx_bytes: 0,
        error: None,
    };
    let (phase, message) = binding_summary(&report, &[]);
    assert_eq!(phase, NetworkResourcePhase::Preparing);
    assert!(message.contains("等待确认"));
    report.configured = true;
    assert_eq!(
        binding_summary(&report, &[]).0,
        NetworkResourcePhase::Connected
    );
}
