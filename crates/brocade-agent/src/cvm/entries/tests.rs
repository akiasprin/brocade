use super::*;

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
        port: 40000,
        slot,
        credential: "a".repeat(44),
        peer_credential: "b".repeat(44),
    }
}
fn l4(protocol: EntryProtocol, listen_port: u16, target_port: u16) -> EntryMode {
    EntryMode::L4 {
        protocol,
        listen_port,
        target_port,
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
                id: "ssh".into(),
                address: "203.0.113.12".parse().unwrap(),
                mode: l4(EntryProtocol::TcpUdp, 30022, 22),
                target: Some("192.168.0.7".parse().unwrap()),
                tunnel: Some(remote.id.clone()),
            },
            EntryRule {
                id: "web".into(),
                address: "203.0.113.30".parse().unwrap(),
                mode: EntryMode::L3,
                target: Some("192.168.0.9".parse().unwrap()),
                tunnel: None,
            },
            EntryRule {
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
    let script = plan::ruleset(&desired, &ready).unwrap();
    let dev = plan::device(&remote);
    assert_eq!(dev.len(), 15);
    for rule in [
        "add rule ip brocade_cvm_entries steer ip daddr 203.0.113.12 tcp dport 30022 meta mark set 0xb60003".to_owned(),
        "add rule ip brocade_cvm_entries steer ip daddr 203.0.113.12 udp dport 30022 meta mark set 0xb60003".to_owned(),
        "add rule ip brocade_cvm_entries incoming ip daddr 203.0.113.12 tcp dport 30022 dnat ip to 192.168.0.7:22".to_owned(),
        "add rule ip brocade_cvm_entries incoming ip daddr 203.0.113.30 dnat ip to 192.168.0.9".to_owned(),
        "add rule ip brocade_cvm_entries outgoing oifname \"cube-dev\" ct status dnat ct original ip daddr 203.0.113.30 masquerade".to_owned(),
        format!("add rule ip brocade_cvm_entries outgoing oifname \"{dev}\" ct status dnat snat ip to 240.0.0.13"),
        format!("add rule ip brocade_cvm_entries forward iifname \"{dev}\" ct direction reply accept"),
        format!("add rule ip brocade_cvm_entries forward iifname \"{dev}\" drop"),
        "add rule ip brocade_cvm_entries fence ip daddr 203.0.113.12 tcp dport 30080 ct state new drop".to_owned(),
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
    let script = plan::ruleset(&desired, &BTreeSet::new()).unwrap();
    assert!(script.contains("fence ip daddr 203.0.113.12 tcp dport 30022 ct state new drop"));
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
    let script = plan::ruleset(&desired, &BTreeSet::from([remote.id.as_str()])).unwrap();
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
            id: "ssh".into(),
            address: "203.0.113.12".parse().unwrap(),
            mode: l4(EntryProtocol::TcpUdp, 30022, 22),
            target: Some("192.168.0.7".parse().unwrap()),
            tunnel: Some(entry.id.clone()),
        }],
        deliveries: vec![EntryDelivery {
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
    let script = plan::ruleset(&desired, &ready).unwrap();
    let path = std::env::temp_dir().join(format!("entry-rules-{}.nft", std::process::id()));
    std::fs::write(&path, &script).unwrap();
    let path = path.to_str().unwrap();
    run_command("nft", &["-f", path]).unwrap();
    // Replacing the table is atomic and repeatable.
    run_command("nft", &["-f", path]).unwrap();
    let listed = run_command("nft", &["list", "table", "ip", TABLE]).unwrap();
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
    let report = converge(&root, &desired);
    println!("ENTRY-REPORT {}", serde_json::to_string(&report).unwrap());
}
