use super::*;

#[test]
fn legacy_private_entry_checkpoint_can_be_read_before_current_state_is_persisted() {
    let root = std::env::temp_dir().join(format!("entry-checkpoint-compat-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let legacy = br#"{"node_id":"hk","generation":25,"rules":[{"id":"e1","address":"203.0.113.12","mode":{"kind":"l3"},"target":null,"tunnel":null}],"deliveries":[],"tunnels":[]}"#;
    atomic_write_private(&desired_path(&root), legacy).unwrap();
    let previous = snapshot(&root).unwrap();
    assert_eq!(previous.generation, 25);
    assert!(!previous.rules[0].host_local);
    assert_eq!(std::fs::read(desired_path(&root)).unwrap(), legacy);

    let next = EntryDesired {
        generation: 26,
        rules: Vec::new(),
        ..previous
    };
    persist(&root, &next).unwrap();
    assert_eq!(snapshot(&root).unwrap(), next);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn wireguard_listener_inventory_classifies_real_tool_failure_without_echoing_secrets() {
    let failure = run_command(
        "sh",
        &[
            "-c",
            "printf 'private-input-canary fopen: Permission denied\\n' >&2; exit 1",
        ],
    );
    let error = wireguard_listener_ports(failure).unwrap_err();
    assert!(error.contains("WireGuard 入口传输端口检查"));
    assert!(error.contains("AppArmor"));
    assert!(!error.contains("private-input-canary"));
    assert!(!error.contains("fopen"));
    assert_eq!(
        wireguard_listener_ports(Ok("wg0 51820\nwg1 0\nwg2 51820\n".into())).unwrap(),
        BTreeSet::from([(L4Protocol::Udp, 51820)])
    );
}

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
fn l4(protocol: EntryProtocol, listen_port: u16, target_port: u16) -> EntryMode {
    EntryMode::L4Range {
        protocol,
        listen_start: listen_port,
        listen_end: listen_port,
        target_start: target_port,
        target_end: target_port,
    }
}

#[test]
fn entry_tunnel_clamps_both_syn_directions_before_accept_without_raising_mss() {
    for kind in [TransportKind::Wireguard, TransportKind::Xray] {
        for side in [EntryTunnelSide::Entry, EntryTunnelSide::Target] {
            for (mtu, mss) in [(1280, 1240), (1420, 1380)] {
                let mut remote = tunnel(side, 3);
                remote.offer.kind = kind;
                remote.offer.mtu = mtu;
                let desired = EntryDesired {
                    node_id: if side == EntryTunnelSide::Entry {
                        "hk"
                    } else {
                        "jp"
                    }
                    .into(),
                    generation: 1,
                    rules: Vec::new(),
                    deliveries: Vec::new(),
                    tunnels: vec![remote.clone()],
                };
                let script = plan::ruleset(&desired, &BTreeSet::new(), &BTreeSet::new()).unwrap();
                let dev = plan::device(&remote);
                for direction in ["iifname", "oifname"] {
                    let guard = format!("{direction} \"{dev}\" meta l4proto tcp tcp flags & (syn | rst) == syn tcp option maxseg size > {mss} tcp option maxseg size set {mss}");
                    let position = script.find(&guard).expect("guard both SYN and SYN/ACK");
                    if let Some(accept) =
                        script.find(&format!("iifname \"{dev}\" ct direction reply accept"))
                    {
                        assert!(
                            position < accept,
                            "a terminal accept must not bypass the MSS guard"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn entry_machine_marks_translates_and_fences_by_entry() {
    let remote = tunnel(EntryTunnelSide::Entry, 3);
    let desired = EntryDesired {
        node_id: "hk".into(),
        generation: 4,
        rules: vec![
            EntryRule {
                reserved_ports: Default::default(),
                host_local: false,
                id: "ssh".into(),
                address: "203.0.113.12".parse().unwrap(),
                mode: l4(EntryProtocol::TcpUdp, 30022, 22),
                target: Some("192.168.0.7".parse().unwrap()),
                tunnel: Some(remote.id.clone()),
            },
            EntryRule {
                reserved_ports: Default::default(),
                host_local: false,
                id: "web".into(),
                address: "203.0.113.30".parse().unwrap(),
                mode: EntryMode::L3,
                target: Some("192.168.0.9".parse().unwrap()),
                tunnel: None,
            },
            EntryRule {
                reserved_ports: Default::default(),
                host_local: false,
                id: "stopped".into(),
                address: "203.0.113.12".parse().unwrap(),
                mode: l4(EntryProtocol::Tcp, 30080, 80),
                target: None,
                tunnel: None,
            },
        ],
        deliveries: vec![],
        tunnels: vec![remote.clone()],
    };
    desired.validate().unwrap();
    let ready = BTreeSet::from([remote.id.as_str()]);
    let script = plan::ruleset(&desired, &ready, &BTreeSet::new()).unwrap();
    let dev = plan::device(&remote);
    assert_eq!(dev.len(), 15);
    for rule in [
        "add rule ip brocade_cvm_entries steer ip daddr 203.0.113.12 tcp dport 30022 ct direction original meta mark set 0xb60003".to_owned(),
        "add rule ip brocade_cvm_entries steer ip daddr 203.0.113.12 udp dport 30022 ct direction original meta mark set 0xb60003".to_owned(),
        "add rule ip brocade_cvm_entries incoming ip daddr 203.0.113.12 tcp dport 30022 ct direction original dnat ip to 192.168.0.7:22".to_owned(),
        "add rule ip brocade_cvm_entries incoming ip daddr 203.0.113.30 ct direction original dnat ip to 192.168.0.9".to_owned(),
        "add rule ip brocade_cvm_entries outgoing oifname \"cube-dev\" ct status dnat ct original ip daddr 203.0.113.30 masquerade".to_owned(),
        format!("add rule ip brocade_cvm_entries outgoing oifname \"{dev}\" ct status dnat snat ip to 240.0.0.13"),
        format!("add rule ip brocade_cvm_entries forward iifname \"{dev}\" ct direction reply accept"),
        format!("add rule ip brocade_cvm_entries forward iifname \"{dev}\" drop"),
        "add rule ip brocade_cvm_entries fence ip daddr 203.0.113.12 tcp dport 30080 ct direction original ct state new drop".to_owned(),
    ] {
        assert!(script.contains(&rule), "missing {rule}\n{script}");
    }
    assert!(script.contains(&format!("steer iifname \"{dev}\" meta mark set 0xb60003")));
    assert_eq!(
        plan::targets(&desired, &remote),
        BTreeSet::from(["192.168.0.7".parse().unwrap()])
    );
    // The whole-IP entry never marks: it is delivered on this machine.
    assert!(!script.contains("ip daddr 203.0.113.30 meta mark"));
    // A tunnel that failed to come up fences its entries instead of leaking.
    let script = plan::ruleset(&desired, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    assert!(script.contains(
        "fence ip daddr 203.0.113.12 tcp dport 30022 ct direction original ct state new drop"
    ));
    assert!(!script.contains("dnat ip to 192.168.0.7"));
    assert!(plan::needs_gateway(&desired));
}

#[test]
fn container_host_accepts_only_listed_deliveries_from_its_peer() {
    let remote = tunnel(EntryTunnelSide::Target, 3);
    let desired = EntryDesired {
        node_id: "jp".into(),
        generation: 2,
        rules: vec![],
        deliveries: vec![EntryDelivery {
            reserved_ports: Default::default(),
            host_local: false,
            id: "ssh".into(),
            instance_id: "cvm-web".into(),
            mode: l4(EntryProtocol::Tcp, 30022, 22),
            target: "192.168.0.7".parse().unwrap(),
            tunnel: remote.id.clone(),
        }],
        tunnels: vec![remote.clone()],
    };
    desired.validate().unwrap();
    let dev = plan::device(&remote);
    let script = plan::ruleset(
        &desired,
        &BTreeSet::from([remote.id.as_str()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        plan::addresses(&remote).unwrap().0.to_string(),
        "240.0.0.14"
    );
    for rule in [
        format!("forward iifname \"{dev}\" ip saddr 240.0.0.13 ip daddr 192.168.0.7 tcp dport 22 ct direction original accept"),
        format!("forward iifname \"{dev}\" ip saddr 240.0.0.13 ip daddr 192.168.0.7 ct state related accept"),
        format!("forward iifname \"{dev}\" drop"),
        format!("outgoing iifname \"{dev}\" oifname \"cube-dev\" masquerade"),
        format!("input iifname \"{dev}\" drop"),
    ] {
        assert!(script.contains(&rule), "missing {rule}\n{script}");
    }
    assert!(!script.contains("meta mark set"));
    assert!(plan::needs_gateway(&desired));
    assert!(!plan::needs_gateway(&EntryDesired::default()));
}

/// Real kernel acceptance of the generated ruleset (syntax and hooks), in a private
/// namespace: `BROCADE_RUN_CVM_ENTRY_NETNS=1 unshare -rn cargo test ... -- --ignored`.
#[test]
#[ignore = "requires BROCADE_RUN_CVM_ENTRY_NETNS=1 inside a private network namespace"]
fn generated_rules_load_in_a_real_kernel() {
    assert_eq!(
        std::env::var("BROCADE_RUN_CVM_ENTRY_NETNS").as_deref(),
        Ok("1")
    );
    let entry = tunnel(EntryTunnelSide::Entry, 3);
    let desired = EntryDesired {
        node_id: "hk".into(),
        generation: 1,
        rules: vec![EntryRule {
            reserved_ports: Default::default(),
            host_local: false,
            id: "ssh".into(),
            address: "203.0.113.12".parse().unwrap(),
            mode: l4(EntryProtocol::TcpUdp, 30022, 22),
            target: Some("192.168.0.7".parse().unwrap()),
            tunnel: Some(entry.id.clone()),
        }],
        deliveries: vec![EntryDelivery {
            reserved_ports: Default::default(),
            host_local: false,
            id: "web".into(),
            instance_id: "cvm-web".into(),
            mode: EntryMode::L3,
            target: "192.168.0.9".parse().unwrap(),
            tunnel: "entry-tunnel-4".into(),
        }],
        tunnels: vec![entry.clone(), {
            let mut host = tunnel(EntryTunnelSide::Target, 4);
            host.entry_node = "jp".into();
            host.target_node = "hk".into();
            host
        }],
    };
    let ready = desired.tunnels.iter().map(|t| t.id.as_str()).collect();
    let script = plan::ruleset(&desired, &ready, &BTreeSet::new()).unwrap();
    let path = std::env::temp_dir().join(format!("entry-rules-{}.nft", std::process::id()));
    std::fs::write(&path, &script).unwrap();
    let path = path.to_str().unwrap();
    run_command("nft", &["-f", path]).unwrap();
    // Replacing the table is atomic and repeatable.
    run_command("nft", &["-f", path]).unwrap();
    let listed = run_command("nft", &["list", "table", "ip", "brocade_cvm_entries"]).unwrap();
    assert!(listed.contains("dnat to 192.168.0.7:22"));
    std::fs::remove_file(path).unwrap();
}

/// One convergence of a fixture desired state in the current network namespace.
/// Driven by `crates/brocade-agent/tests/entries_netns.py`; never touches the host.
#[test]
#[ignore = "driven by tests/entries_netns.py inside private namespaces"]
fn converge_fixture() {
    let fixture = std::env::var("BROCADE_ENTRY_FIXTURE").expect("fixture path");
    let root = PathBuf::from(std::env::var("BROCADE_ENTRY_ROOT").expect("state root"));
    let desired: EntryDesired =
        serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    if !desired.node_id.is_empty() {
        desired.validate().unwrap();
    }
    ROOT.set(root.clone()).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    persist(&root, &desired).unwrap();
    if std::env::var_os("BROCADE_ENTRY_TEST_NOTRACK").is_some() {
        crate::conntrack::apply_content(
            r#"{"inbounds":[{"tag":"fixture","listen":"0.0.0.0","port":22,"protocol":"vless"}]}"#,
        )
        .unwrap();
    }
    let report = converge(&root, &desired);
    println!("ENTRY-REPORT {}", serde_json::to_string(&report).unwrap());
}

#[test]
fn native_host_deliveries_use_input_and_never_require_a_cube_gateway() {
    let remote = tunnel(EntryTunnelSide::Target, 3);
    let desired = EntryDesired {
        node_id: "jp".into(),
        generation: 1,
        rules: vec![],
        deliveries: vec![EntryDelivery {
            reserved_ports: Default::default(),
            host_local: true,
            id: "host-ssh".into(),
            instance_id: "cvm-host".into(),
            mode: l4(EntryProtocol::Tcp, 30022, 22),
            target: "192.0.2.10".parse().unwrap(),
            tunnel: remote.id.clone(),
        }],
        tunnels: vec![remote.clone()],
    };
    assert!(!plan::needs_gateway(&desired));
    let dev = plan::device(&remote);
    let script = plan::ruleset(
        &desired,
        &BTreeSet::from([remote.id.as_str()]),
        &BTreeSet::new(),
    )
    .unwrap();
    let allowed=format!("input iifname \"{dev}\" ip saddr 240.0.0.13 ip daddr 192.0.2.10 tcp dport 22 ct direction original accept");
    assert!(script.contains(&allowed), "{script}");
    assert!(
        script.find(&allowed).unwrap()
            < script
                .find(&format!("input iifname \"{dev}\" drop"))
                .unwrap()
    );
    assert!(!script.contains("input iifname \"{dev}\" accept"));
}

#[test]
fn host_l3_forwards_all_application_ports_but_never_captures_transport_or_host_replies() {
    let remote = tunnel(EntryTunnelSide::Entry, 3);
    let desired = EntryDesired {
        node_id: "hk".into(),
        generation: 1,
        rules: vec![EntryRule {
            reserved_ports: Default::default(),
            host_local: false,
            id: "whole".into(),
            address: "203.0.113.12".parse().unwrap(),
            mode: EntryMode::L3,
            target: Some("192.0.2.10".parse().unwrap()),
            tunnel: Some(remote.id.clone()),
        }],
        tunnels: vec![remote.clone()],
        ..Default::default()
    };
    let ports = BTreeSet::from([
        (L4Protocol::Udp, 51820),
        (L4Protocol::Udp, 55001),
        (L4Protocol::Tcp, 43001),
    ]);
    for ready in [BTreeSet::new(), BTreeSet::from([remote.id.as_str()])] {
        let script = plan::ruleset(&desired, &ready, &ports).unwrap();
        for chain in ["steer", "incoming", "fence"] {
            for (protocol, port) in [
                ("udp", 40000),
                ("udp", 51820),
                ("udp", 55001),
                ("tcp", 43001),
            ] {
                let exemption = format!("{chain} {protocol} dport {port} return");
                assert!(script.contains(&exemption));
                if let Some(selector) = script.find(&format!("{chain} ip daddr 203.0.113.12")) {
                    assert!(script.find(&exemption).unwrap() < selector);
                }
            }
        }
        assert!(!script.contains("tcp dport 22 return"));
        for line in script
            .lines()
            .filter(|l| l.contains("ip daddr 203.0.113.12"))
        {
            assert!(line.contains("ct direction original"), "{line}");
        }
    }
}

#[test]
fn native_entry_reply_source_is_the_delivery_address_and_never_a_transit_address() {
    let remote = tunnel(EntryTunnelSide::Target, 3);
    let mut desired = EntryDesired {
        node_id: "jp".into(),
        generation: 1,
        tunnels: vec![remote.clone()],
        deliveries: vec![EntryDelivery {
            reserved_ports: Default::default(),
            host_local: true,
            id: "native".into(),
            instance_id: "baremetal".into(),
            mode: EntryMode::L3,
            target: "192.0.2.10".parse().unwrap(),
            tunnel: remote.id.clone(),
        }],
        ..Default::default()
    };
    assert_eq!(
        plan::reply_source(&desired, &remote).unwrap(),
        Some("192.0.2.10".parse().unwrap())
    );
    let mut duplicate = desired.deliveries[0].clone();
    duplicate.target = "192.0.2.11".parse().unwrap();
    desired.deliveries.push(duplicate);
    assert!(plan::reply_source(&desired, &remote).is_err());
    desired
        .deliveries
        .iter_mut()
        .for_each(|d| d.host_local = false);
    assert_eq!(plan::reply_source(&desired, &remote).unwrap(), None);
}

/// Long-lived execution is required for TUN-backed Xray and Phantun workers.
#[test]
#[ignore = "requires an explicitly selected isolated KVM/network namespace"]
fn serve_fixture() {
    assert_eq!(std::env::var("BROCADE_RUN_CVM_KVM").as_deref(), Ok("1"));
    let fixture = PathBuf::from(std::env::var("BROCADE_ENTRY_FIXTURE").expect("fixture"));
    let root = PathBuf::from(std::env::var("BROCADE_ENTRY_ROOT").expect("root"));
    ROOT.set(root.clone()).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let certificate = super::super::nics::xray::certificate(&root).unwrap();
    atomic_write_private(&root.join("certificate.public.pem"), certificate.as_bytes()).unwrap();
    let stop = root.join("stop");
    assert!(!stop.exists());
    if std::env::var_os("BROCADE_ENTRY_TEST_NOTRACK").is_some() {
        crate::conntrack::apply_content(
            r#"{"inbounds":[{"tag":"fixture","listen":"0.0.0.0","port":22,"protocol":"vless"}]}"#,
        )
        .unwrap();
    }
    let mut backbone = String::new();
    let start = std::time::Instant::now();
    loop {
        if let Ok(content) = std::fs::read_to_string(root.join("backbone.json")) {
            if content != backbone {
                atomic_write_private(&root.join("phantun.json"), content.as_bytes()).unwrap();
                crate::phantun::apply_phantun(&root, &content, None).unwrap();
                backbone = content;
                atomic_write_private(
                    &root.join("backbone.applied"),
                    brocade_core::hash::sha256_hex(backbone.as_bytes()).as_bytes(),
                )
                .unwrap();
            }
        }

        assert!(
            start.elapsed() < Duration::from_secs(900),
            "fixture lifetime exhausted"
        );
        let desired: EntryDesired =
            serde_json::from_slice(&std::fs::read(&fixture).unwrap()).unwrap();
        desired.validate().unwrap();
        persist(&root, &desired).unwrap();
        let report = converge(&root, &desired);
        atomic_write_private(
            &root.join("report.json"),
            &serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        if stop.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let report = converge(&root, &EntryDesired::default());
    assert!(report.error.is_none(), "{report:?}");
}

#[test]
fn tcp_entry_carriers_exempt_their_transport_before_l3_forwarding() {
    for (kind, encapsulation) in [
        (TransportKind::Xray, EntryWireguardTransport::Udp),
        (TransportKind::Wireguard, EntryWireguardTransport::Phantun),
    ] {
        let mut t = tunnel(EntryTunnelSide::Entry, 3);
        t.offer.kind = kind;
        t.wireguard_transport = encapsulation;
        let desired = EntryDesired {
            node_id: "hk".into(),
            generation: 1,
            rules: vec![EntryRule {
                reserved_ports: Default::default(),
                host_local: true,
                id: "whole".into(),
                address: "203.0.113.12".parse().unwrap(),
                mode: EntryMode::L3,
                target: Some("192.0.2.10".parse().unwrap()),
                tunnel: Some(t.id.clone()),
            }],
            deliveries: vec![],
            tunnels: vec![t],
        };
        let script = plan::ruleset(
            &desired,
            &BTreeSet::from([desired.tunnels[0].id.as_str()]),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(
            script.find("incoming tcp dport 40000 return").unwrap()
                < script.find("dnat ip to 192.0.2.10").unwrap()
        );
        assert!(!script.contains("incoming udp dport 40000 return"));
    }
}

#[test]
fn port_ranges_map_each_endpoint_and_fence_every_port_until_delivery_is_ready() {
    let remote = tunnel(EntryTunnelSide::Entry, 3);
    let mode = EntryMode::L4Range {
        protocol: EntryProtocol::TcpUdp,
        listen_start: 30000,
        listen_end: 30002,
        target_start: 40000,
        target_end: 40002,
    };
    let desired = EntryDesired {
        node_id: "hk".into(),
        generation: 1,
        rules: vec![EntryRule {
            reserved_ports: Default::default(),
            host_local: false,
            id: "range-entry".into(),
            address: "203.0.113.12".parse().unwrap(),
            mode: mode.clone(),
            target: Some("192.168.0.7".parse().unwrap()),
            tunnel: Some(remote.id.clone()),
        }],
        deliveries: vec![],
        tunnels: vec![remote.clone()],
    };
    let script = plan::ruleset(
        &desired,
        &BTreeSet::from([remote.id.as_str()]),
        &BTreeSet::new(),
    )
    .unwrap();
    for protocol in ["tcp", "udp"] {
        for offset in 0..3 {
            assert!(script.contains(&format!(
                "{protocol} dport {} ct direction original dnat ip to 192.168.0.7:{}",
                30000 + offset,
                40000 + offset
            )));
        }
    }
    let blocked = plan::ruleset(&desired, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    assert!(!blocked.contains("dnat ip to"));
    for port in 30000..=30002 {
        assert!(blocked.contains(&format!(
            "tcp dport {port} ct direction original ct state new drop"
        )));
    }
    let target = EntryDesired {
        node_id: "jp".into(),
        generation: 1,
        rules: vec![],
        deliveries: vec![EntryDelivery {
            reserved_ports: Default::default(),
            host_local: false,
            id: "range-entry".into(),
            instance_id: "cvm-web".into(),
            mode,
            target: "192.168.0.7".parse().unwrap(),
            tunnel: remote.id.clone(),
        }],
        tunnels: vec![tunnel(EntryTunnelSide::Target, 3)],
    };
    let script = plan::ruleset(&target, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    assert!(script.contains("tcp dport 40000-40002 ct direction original accept"));
    assert!(!script.contains("tcp dport 40003"));
}
