use super::*;
use brocade_deployment::{
    plan::{GrantClient, GrantInbound},
    protocol::XrayConfigUpdatePolicy,
};

fn fixture() -> (String, String) {
    (serde_json::json!({"inbounds":[{"listen":"::","port":30000,"streamSettings":{"network":"hysteria"}}]}).to_string(),
     r#"{"redirects":[{"start":30000,"end":30099,"to":30000}]}"#.to_owned())
}

#[test]
fn invalid_ranges_and_namespace_specific_options_fail_before_mutation() {
    let (config, hops) = fixture();
    let spec = specification(&config, &hops).unwrap().unwrap();
    assert_eq!(spec.listeners, ["[::]:30000"]);
    for invalid in [
        r#"{"redirects":[{"start":0,"end":30099,"to":30000}]}"#,
        r#"{"redirects":[{"start":30000,"end":30099,"to":30000},{"start":30050,"end":30100,"to":30050}]}"#,
    ] {
        assert!(specification(&config, invalid).is_err());
    }
    let explicit = config.replace("::", "192.0.2.1");
    assert!(specification(&explicit, &hops).is_err());
    let missing = config.replace("hysteria", "tcp");
    assert!(specification(&missing, &hops).is_err());
    let mut marked: Value = serde_json::from_str(&config).unwrap();
    marked["inbounds"][0]["streamSettings"]["sockopt"] = serde_json::json!({"mark":7});
    assert!(specification(&marked.to_string(), &hops).is_err());
}

#[test]
fn udp_port_conflicts_allow_only_loopback_or_verified_socket_inodes() {
    let (config, hops) = fixture();
    let spec = specification(&config, &hops).unwrap().unwrap();
    let row = |address: &str, port: u16, inode: u64| {
        format!("sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n0: {address}:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 {inode}\n")
    };
    let foreign = row("00000000", 30000, 27);
    assert_eq!(
        udp_conflict(&foreign, false, &spec, &BTreeSet::new()).unwrap(),
        Some(30000)
    );
    assert_eq!(
        udp_conflict(&foreign, false, &spec, &BTreeSet::from([27])).unwrap(),
        None
    );
    assert_eq!(
        udp_conflict(&row("0100007F", 30037, 27), false, &spec, &BTreeSet::new()).unwrap(),
        None
    );
    assert_eq!(
        udp_conflict(
            &row("00000000000000000000000000000000", 30037, 28),
            true,
            &spec,
            &BTreeSet::new()
        )
        .unwrap(),
        Some(30037)
    );
    assert_eq!(
        udp_conflict(
            &row("00000000000000000000000001000000", 30037, 28),
            true,
            &spec,
            &BTreeSet::new()
        )
        .unwrap(),
        None
    );
    assert_eq!(
        udp_conflict(&row("00000000", 29999, 27), false, &spec, &BTreeSet::new()).unwrap(),
        None
    );
    assert!(udp_conflict("header\nmalformed", false, &spec, &BTreeSet::new()).is_err());
}

#[test]
fn reserved_link_routes_and_unmanaged_nodes_are_checked_without_mutation() {
    for route in [
        "169.254.254.0/30",
        "169.254.254.1",
        "169.254.254.3/32",
        "fd42:b10c:adf0::/64",
        "fd42:b10c:adf0::99/128",
    ] {
        assert!(private_route_conflict(route).unwrap());
    }
    for route in [
        "default",
        "0.0.0.0/0",
        "::/0",
        "169.254.0.0/16",
        "fd00::/8",
        "192.0.2.0/24",
        "fe80::/64",
        "169.254.253.1/32",
    ] {
        assert!(!private_route_conflict(route).unwrap());
    }
    assert!(private_route_conflict("192.0.2.0/64").is_err());
    assert!(!listener_change(
        Path::new("/does-not-exist"),
        &DesiredArtifact::Unmanaged {
            reason: "not managed".into()
        },
        &DesiredArtifact::Unmanaged {
            reason: "not managed".into()
        }
    )
    .unwrap());
}

#[test]
fn pending_mapping_is_fenced_to_its_xray_configuration() {
    let root = std::env::temp_dir().join(format!("hy2-pending-{}", std::process::id()));
    assert!(!root.exists());
    crate::create_private_dir(&root).unwrap();
    let (config, hops) = fixture();
    fs::write(root.join("xray.json"), &config).unwrap();
    fs::write(root.join("hy2_port_hop.json"), "{}").unwrap();
    let xray = DesiredArtifact::Unmanaged {
        reason: "unchanged config".into(),
    };
    let desired = DesiredArtifact::Present {
        sha256: sha256_hex(hops.as_bytes()),
        content: hops.clone(),
    };
    stage(&root, &desired, &xray).unwrap();
    assert_eq!(hop_content(&root, &config).unwrap(), hops);
    assert_eq!(hop_content(&root, "different config").unwrap(), "{}");
    finish_pending(&root).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("hy2_port_hop.json")).unwrap(),
        hops
    );
    assert!(!root.join(PENDING).exists());
    stage(
        &root,
        &DesiredArtifact::Disabled {
            reason: "no hopping".into(),
        },
        &xray,
    )
    .unwrap();
    finish_pending(&root).unwrap();
    assert!(root.join("hy2_port_hop.disabled").exists());
    assert!(!root.join("hy2_port_hop.json").exists());
    fs::remove_dir_all(root).unwrap();
}

fn wait_for_ack(root: &Path, phase: &str) {
    // Unique names avoid cross-machine 9p cache aliasing between phases.
    write_json(
        root,
        &format!("phase-{phase}.json"),
        &serde_json::json!({"phase":phase}),
    )
    .unwrap();
    let path = root.join(format!("ack-{phase}"));
    let start = std::time::Instant::now();
    // TCG is a functional ABI/kernel check, not a hardware performance gate.
    // Only the lab coordinator budget changes; production command, helper and
    // startup deadlines continue to execute from the actual release binary.
    let budget = if std::env::var_os("BROCADE_HY2_TCG").is_some() {
        240
    } else {
        90
    };
    loop {
        if fs::read_to_string(&path).ok().as_deref() == Some(phase) {
            fs::remove_file(&path).unwrap();
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(budget),
            "lab coordinator did not acknowledge {phase}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn restart_lab_xray(path: &Path, api: std::net::SocketAddr, grants: &[GrantInbound]) {
    if let Some(agent) = std::env::var_os("BROCADE_HY2_RELEASE_AGENT") {
        crate::command::run_command_with_timeout(
            agent.to_str().unwrap(),
            &[
                "repair",
                "--state-dir",
                path.parent().unwrap().to_str().unwrap(),
            ],
            Duration::from_secs(30),
        )
        .unwrap();
    } else {
        crate::apply_xray_with_grants(path, api, Some(grants)).unwrap();
    }
}

fn with_readonly_host_udp_timeouts(action: impl FnOnce()) {
    let settings = [
        ("/proc/sys/net/netfilter/nf_conntrack_udp_timeout", "73"),
        (
            "/proc/sys/net/netfilter/nf_conntrack_udp_timeout_stream",
            "137",
        ),
    ];
    let previous: Vec<_> = settings
        .iter()
        .map(|(path, _)| fs::read_to_string(path).unwrap())
        .collect();
    // The caller already verified the disposable mount/network namespace. Bind
    // only these two host settings read-only so other owned settings still work.
    for (path, value) in settings {
        fs::write(path, value).unwrap();
        run("mount", &["--bind", path, path]).unwrap();
        run("mount", &["-o", "remount,bind,ro", path]).unwrap();
    }
    action();
    for ((path, value), previous) in settings.iter().zip(previous) {
        assert_eq!(fs::read_to_string(path).unwrap().trim(), *value);
        run("umount", &[path]).unwrap();
        fs::write(path, previous).unwrap();
    }
}

#[test]
#[ignore = "requires hy2-netns/production.py in an owned Docker/KVM network namespace"]
fn native_restart_migration_recovery_and_reconcile() {
    use std::net::{TcpListener, UdpSocket};
    let ancestor =
        std::env::var("BROCADE_HY2_LAB_ANCESTOR_FD").expect("private lab namespace guard required");
    assert_ne!(
        fs::read_link(format!("/proc/self/fd/{ancestor}")).unwrap(),
        fs::read_link("/proc/self/ns/net").unwrap()
    );
    assert!(
        run("ip", &["-j", "link", "show", "wan"]).is_ok(),
        "requires the disposable lab WAN"
    );
    let output = PathBuf::from(std::env::var_os("BROCADE_HY2_LAB_RESULTS").unwrap());
    // Runtime journals require the guest's native filesystem; a 9p evidence
    // mount does not implement the allocation/mmap contract used in production.
    let root = PathBuf::from(std::env::var_os("BROCADE_HY2_LAB_STATE").unwrap());
    assert!(!root.exists());
    crate::create_private_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let stopped = crate::terminate_processes(
                &crate::serving_xray_processes_in(&self.0),
                crate::XRAY_TERM_GRACE,
                crate::XRAY_KILL_GRACE,
            );
            if stopped.is_ok() {
                let _ = cleanup(&self.0);
                let _ = hy2_port_hop::disable();
            }
        }
    }
    let _cleanup = Cleanup(root.clone());
    crate::source_access::initialize(&root).unwrap();
    let source = PathBuf::from(std::env::var_os("BROCADE_TEST_XRAY").unwrap());
    let binary = root.join("xray");
    fs::copy(source, &binary).unwrap();
    std::env::set_var("BROCADE_XRAY_BIN", &binary);
    std::env::set_var(
        "BROCADE_TEST_LOG_SINK",
        format!(
            "cat >> {}",
            crate::shell_quote(&output.join("xray.log").display().to_string())
        ),
    );
    let api = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let inputs = PathBuf::from(std::env::var_os("BROCADE_HY2_LAB_INPUTS").unwrap());
    let mut config = serde_json::json!({
        "log":{"loglevel":"info"},"stats":{},
        "api":{"tag":"api","listen":api.to_string(),"services":["HandlerService","StatsService"]},
        "policy":{"levels":{"0":{"statsUserUplink":true,"statsUserDownlink":true}}},
        "inbounds":[],"outbounds":[{"tag":"direct","protocol":"freedom"}]
    });
    for (tag, listen) in [("hy4", "0.0.0.0"), ("hy6", "::")] {
        // Both families use different logical ports to avoid dual-stack bind overlap.
        let port = if tag == "hy4" { 30000 } else { 30100 };
        config["inbounds"].as_array_mut().unwrap().push(serde_json::json!({
            "tag":tag,"listen":listen,"port":port,"protocol":"hysteria","settings":{"version":2,"users":[]},
            "streamSettings":{"network":"hysteria","security":"tls","hysteriaSettings":{"version":2},
                "tlsSettings":{"certificates":[{"certificateFile":inputs.join("cover.pem"),"keyFile":inputs.join("cover.key")} ]}}
        }));
    }
    let content = config.to_string();
    let path = root.join("xray.json");
    crate::write_private(&path, &content).unwrap();
    let hops=serde_json::json!({"redirects":[{"start":30000,"end":30099,"to":30000},{"start":30100,"end":30199,"to":30100}]}).to_string();
    crate::write_private(&root.join("hy2_port_hop.json"), &hops).unwrap();
    let grants: Vec<GrantInbound> = ["hy4", "hy6"]
        .into_iter()
        .map(|tag| GrantInbound {
            tag: tag.into(),
            clients: vec![GrantClient {
                email: format!("{tag}@lab.test"),
                uuid: "hy2-netns-disposable-test".into(),
                flow: None,
                level: 0,
            }],
        })
        .collect();
    write_json(&root, "grants.desired.json", &grants).unwrap();
    crate::write_private(&root.join("usage-generation"), "1").unwrap();
    run(
        "sysctl",
        &[
            "-qw",
            "net.ipv4.ip_local_reserved_ports=31000,32000,33000,34000,35000,36000",
        ],
    )
    .unwrap();
    // A different process may still own the public logical port even though a
    // namespace bind would succeed. No Xray or network mutation is allowed.
    let foreign = UdpSocket::bind("0.0.0.0:30000").unwrap();
    let error = crate::apply_xray_with_grants(&path, api, Some(&grants)).unwrap_err();
    assert!(error.contains("非受管 UDP socket 冲突"), "{error}");
    assert!(crate::serving_xray_processes_in(&root).is_empty());
    assert!(!managed(&root).unwrap());
    foreign
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .send_to(b"foreign-still-alive", "127.0.0.1:30000")
        .unwrap();
    let mut bytes = [0; 64];
    let (n, _) = foreign.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..n], b"foreign-still-alive");
    drop(foreign);
    // Loading the actual legacy NAT supplies a positive conntrack control even
    // when the host had not loaded it before this private lab started.
    hy2_port_hop::apply_content(&hops).unwrap();
    with_readonly_host_udp_timeouts(|| {
        crate::apply_xray_with_network(&path, api, Some(&grants), true).unwrap();
    });
    assert!(!managed(&root).unwrap());
    let prior = crate::serving_xray_process_in(&root).unwrap();
    let foreign = UdpSocket::bind("0.0.0.0:30037").unwrap();
    let error = crate::apply_xray_with_grants(&path, api, Some(&grants)).unwrap_err();
    assert!(error.contains("非受管 UDP socket 冲突"), "{error}");
    assert_eq!(crate::serving_xray_process_in(&root), Some(prior));
    assert!(hy2_port_hop::matches_content(&hops));
    drop(foreign);
    crate::run_shell_with_timeout(
        "set -eu\nip route add 169.254.0.0/16 dev wan scope link\nip -4 route show table main | while IFS= read -r route; do ip -4 route add table 10001 $route; done\nip -4 rule add priority 10001 from 198.18.0.1/32 lookup 10001\n",
        Duration::from_secs(5),
    ).unwrap();
    run(
        "ip",
        &[
            "route",
            "add",
            "table",
            "10001",
            "198.19.0.0/16",
            "dev",
            "wan",
        ],
    )
    .unwrap();
    run(
        "ip",
        &[
            "neigh",
            "add",
            PEER_IP,
            "lladdr",
            "02:00:00:00:00:02",
            "nud",
            "permanent",
            "dev",
            "wan",
        ],
    )
    .unwrap();
    let error = crate::apply_xray_with_grants(&path, api, Some(&grants)).unwrap_err();
    assert!(error.contains("外部邻居"), "{error}");
    assert_eq!(crate::serving_xray_process_in(&root), Some(prior));
    run("ip", &["neigh", "delete", PEER_IP, "dev", "wan"]).unwrap();
    nft("add table ip hy2_lab_scope\nadd chain ip hy2_lab_scope post { type nat hook postrouting priority srcnat; policy accept; }\nadd rule ip hy2_lab_scope post ip saddr 198.18.0.1 masquerade\n").unwrap();
    nft("add rule ip hy2_lab_scope post ip saddr 169.254.240.2 masquerade\nadd table ip hy2_lab_accept\nadd chain ip hy2_lab_accept forward { type filter hook forward priority filter; policy drop; }\nadd rule ip hy2_lab_accept forward ct state established,related accept\n").unwrap();
    wait_for_ack(&output, "legacy-ready");
    restart_lab_xray(&path, api, &grants);
    assert!(active(&root));
    let state = read_state(&root).unwrap().unwrap();
    assert_eq!(state.phase, "active");
    let original_ephemeral = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    run(
        "sysctl",
        &["-qw", "net.ipv4.ip_local_port_range=30000 30299"],
    )
    .unwrap();
    for _ in 0..64 {
        let udp = UdpSocket::bind("0.0.0.0:0").unwrap();
        let tcp = TcpListener::bind("0.0.0.0:0").unwrap();
        assert!(udp.local_addr().unwrap().port() >= 30200);
        assert!(tcp.local_addr().unwrap().port() >= 30200);
    }
    fs::write("/proc/sys/net/ipv4/ip_local_port_range", original_ephemeral).unwrap();
    let reserved = run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"]).unwrap();
    assert!([31000, 32000, 33000, 34000, 35000, 36000]
        .iter()
        .all(|port| reserved.contains(&port.to_string())));
    wait_for_ack(&output, "isolated-ready");
    let before = crate::serving_xray_process_in(&root).unwrap();
    reconcile(&root).unwrap();
    assert_eq!(crate::serving_xray_process_in(&root), Some(before));
    assert_eq!(
        read_state(&root).unwrap().unwrap().namespace_inode,
        state.namespace_inode
    );
    nft(&format!("delete table ip6 {TABLE}")).unwrap();
    let wanted = DesiredArtifact::Present {
        content: hops.clone(),
        sha256: sha256_hex(hops.as_bytes()),
    };
    assert!(!matches(&root, &wanted));
    reconcile(&root).unwrap();
    assert!(matches(&root, &wanted));
    assert_eq!(crate::serving_xray_process_in(&root), Some(before));
    let transport = read_state(&root).unwrap().unwrap();
    run(
        "tc",
        &["qdisc", "delete", "dev", &transport.interface, "clsact"],
    )
    .unwrap();
    assert!(!matches(&root, &wanted));
    reconcile(&root).unwrap();
    assert!(matches(&root, &wanted));
    assert_eq!(crate::serving_xray_process_in(&root), Some(before));
    wait_for_ack(&output, "reconciled-ready");
    assert!(!needs_recovery(&root, &content).unwrap());
    let prior_namespace = read_state(&root).unwrap().unwrap();
    // Keep the old namespace alive until comparison so its inode cannot be
    // reused immediately by the kernel when the named mount is replaced.
    let prior_namespace_fd =
        fs::File::open(format!("/run/netns/{}", prior_namespace.namespace)).unwrap();
    run("ip", &["link", "delete", &prior_namespace.interface]).unwrap();
    assert!(needs_recovery(&root, &content).unwrap());
    crate::apply_xray_with_grants(&path, api, Some(&grants)).unwrap();
    assert!(!needs_recovery(&root, &content).unwrap());
    assert_ne!(
        read_state(&root).unwrap().unwrap().namespace_inode,
        prior_namespace.namespace_inode
    );
    drop(prior_namespace_fd);
    wait_for_ack(&output, "link-recovered-ready");
    let before = crate::serving_xray_process_in(&root).unwrap();
    restart_lab_xray(&path, api, &grants);
    let after = crate::serving_xray_process_in(&root).unwrap();
    assert_ne!(after, before);
    assert!(active(&root));
    wait_for_ack(&output, "restarted-ready");
    let unmanaged = DesiredArtifact::Unmanaged {
        reason: "unchanged xray config".into(),
    };
    let disabled = DesiredArtifact::Disabled {
        reason: "disable hopping".into(),
    };
    assert!(listener_change(&root, &unmanaged, &disabled).unwrap());
    stage(&root, &disabled, &unmanaged).unwrap();
    let unchanged = DesiredArtifact::Present {
        content: content.clone(),
        sha256: sha256_hex(content.as_bytes()),
    };
    // Same configuration bytes must not cause rollback to reuse a failed
    // pending range: restore the previously committed hopping artifact instead.
    std::env::set_var("BROCADE_TEST_HY2_FAIL_AFTER_PREPARE", "1");
    let failed = crate::converge_linux_xray(
        &root,
        &unchanged,
        true,
        Some((97, 1)),
        XrayConfigUpdatePolicy::Restart,
        Some(&grants),
    );
    std::env::remove_var("BROCADE_TEST_HY2_FAIL_AFTER_PREPARE");
    assert!(failed.unwrap_err().contains("已恢复上一份配置"));
    assert_eq!(
        fs::read_to_string(root.join("hy2_port_hop.json")).unwrap(),
        hops
    );
    assert!(!root.join(PENDING).exists());
    wait_for_ack(&output, "failed-change-recovered");
    stage(&root, &disabled, &unmanaged).unwrap();
    crate::converge_linux_xray(
        &root,
        &unchanged,
        true,
        Some((98, 1)),
        XrayConfigUpdatePolicy::Restart,
        Some(&grants),
    )
    .unwrap();
    assert!(!managed(&root).unwrap());
    assert!(root.join("hy2_port_hop.disabled").exists());
    wait_for_ack(&output, "single-port-ready");
    let reenabled = DesiredArtifact::Present {
        content: hops.clone(),
        sha256: sha256_hex(hops.as_bytes()),
    };
    assert!(listener_change(&root, &unmanaged, &reenabled).unwrap());
    stage(&root, &reenabled, &unmanaged).unwrap();
    crate::converge_linux_xray(
        &root,
        &unchanged,
        true,
        Some((98, 2)),
        XrayConfigUpdatePolicy::Restart,
        Some(&grants),
    )
    .unwrap();
    assert!(active(&root));
    assert!(!root.join("hy2_port_hop.disabled").exists());
    wait_for_ack(&output, "re-enabled-ready");
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    config["api"]["listen"] = reserved.local_addr().unwrap().to_string().into();
    let desired = DesiredArtifact::Present {
        content: config.to_string(),
        sha256: sha256_hex(config.to_string().as_bytes()),
    };
    let error = crate::converge_linux_xray(
        &root,
        &desired,
        true,
        Some((99, 1)),
        XrayConfigUpdatePolicy::Restart,
        Some(&grants),
    )
    .unwrap_err();
    assert!(error.contains("已恢复上一份配置"), "{error}");
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
    assert!(!managed(&root).unwrap());
    assert_eq!(
        run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"])
            .unwrap()
            .trim(),
        "31000,32000,33000,34000,35000,36000"
    );
    wait_for_ack(&output, "rollback-ready");
    crate::accounting::stop(&root).unwrap();
    // Public interfaces and global IPv6 switches stay disabled. Only the owned
    // transport link can opt in to the IPv6 needed by its internal topology.
    run(
        "sysctl",
        &[
            "-qw",
            "net.ipv6.conf.all.disable_ipv6=1",
            "net.ipv6.conf.default.disable_ipv6=1",
        ],
    )
    .unwrap();
    let mut v4: Value = serde_json::from_str(&content).unwrap();
    v4["inbounds"]
        .as_array_mut()
        .unwrap()
        .retain(|i| i["tag"] == "hy4");
    let v4_grants = vec![grants[0].clone()];
    write_json(&root, "grants.desired.json", &v4_grants).unwrap();
    crate::write_private(&path, &v4.to_string()).unwrap();
    crate::write_private(
        &root.join("hy2_port_hop.json"),
        r#"{"redirects":[{"start":30000,"end":30099,"to":30000}]}"#,
    )
    .unwrap();
    restart_lab_xray(&path, api, &v4_grants);
    for key in ["all", "default", "wan"] {
        assert_eq!(
            fs::read_to_string(format!("/proc/sys/net/ipv6/conf/{key}/disable_ipv6"))
                .unwrap()
                .trim(),
            "1"
        );
    }
    assert!(active(&root));
    wait_for_ack(&output, "ipv4-only-ready");
    let mut legacy_bytes_verified = None;
    let mut old_agent_verified = None;
    if inputs.join("legacy-xray").is_file() {
        let next = root.join("xray.compatibility-next");
        fs::copy(inputs.join("legacy-xray"), &next).unwrap();
        fs::rename(&next, &binary).unwrap();
        with_readonly_host_udp_timeouts(|| {
            restart_lab_xray(&path, api, &v4_grants);
        });
        assert!(!managed(&root).unwrap());
        assert_eq!(
            run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"])
                .unwrap()
                .trim(),
            "31000,32000,33000,34000,35000,36000"
        );
        wait_for_ack(&output, "downgrade-ready");
        legacy_bytes_verified = Some(true);
        if inputs.join("legacy-agent").is_file() {
            crate::command::run_command_with_timeout(
                inputs.join("legacy-agent").to_str().unwrap(),
                &["repair", "--state-dir", root.to_str().unwrap()],
                Duration::from_secs(30),
            )
            .unwrap();
            assert!(!managed(&root).unwrap());
            wait_for_ack(&output, "old-agent-ready");
            old_agent_verified = Some(true);
        }
        fs::copy(inputs.join("xray"), &next).unwrap();
        fs::rename(&next, &binary).unwrap();
        restart_lab_xray(&path, api, &v4_grants);
        assert!(active(&root));
        wait_for_ack(&output, "re-upgraded-ready");
    }
    crate::accounting::stop(&root).unwrap();
    let reserved = run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"]).unwrap();
    fs::write(
        "/proc/sys/net/ipv4/ip_local_reserved_ports",
        format!("{},32900\n", reserved.trim()),
    )
    .unwrap();
    let external = run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"]).unwrap();
    cleanup(&root).unwrap();
    assert_eq!(
        run("sysctl", &["-n", "net.ipv4.ip_local_reserved_ports"]).unwrap(),
        external
    );
    write_json(&output,"native-result.json",&serde_json::json!({"passed":true,"legacy_migrated":true,"custom_source_policy_coexists":true,"existing_nat_coexists":true,"same_pid_reconcile":true,"link_loss_recovered":true,"restart_verified":true,"failed_start_rolled_back":true,"grants_restored":true,"same_config_failure_restored_ranges":true,"disable_reenable_verified":true,"foreign_listener_rejected":true,"foreign_range_rejected_before_stop":true,"ipv4_only_host_verified":true,"reserved_ports_preserved":true,"ephemeral_allocator_avoids_hy2":true,"host_udp_timeouts_preserved":true,"readonly_host_udp_recovery_verified":true,"readonly_host_udp_legacy_cli":legacy_bytes_verified.filter(|_|std::env::var_os("BROCADE_HY2_RELEASE_AGENT").is_some()),"release_agent_cli_restart_verified":std::env::var_os("BROCADE_HY2_RELEASE_AGENT").map(|_|true),"actual_legacy_xray_roundtrip":legacy_bytes_verified,"actual_legacy_agent_roundtrip":old_agent_verified})).unwrap();
}
