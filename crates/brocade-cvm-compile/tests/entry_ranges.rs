use brocade_cvm_compile::entry_network;
use brocade_deployment::cvm::*;
use std::{collections::BTreeSet, process::Command};

fn desired() -> EntryDesired {
    EntryDesired {
        node_id: "entry-node".into(),
        generation: 1,
        rules: vec![EntryRule {
            reserved_ports: Default::default(),
            host_local: true,
            id: "range-entry".into(),
            address: "198.51.100.10".parse().unwrap(),
            mode: EntryMode::L4Range {
                protocol: EntryProtocol::TcpUdp,
                listen_start: 30000,
                listen_end: 30002,
                target_start: 40000,
                target_end: 40002,
            },
            target: Some("192.0.2.10".parse().unwrap()),
            tunnel: None,
        }],
        deliveries: vec![],
        tunnels: vec![],
    }
}

#[test]
fn frozen_range_products_validate_ports_without_resolving_late_bound_addresses() {
    let mut frozen = desired();
    frozen.rules[0].target = Some(std::net::Ipv4Addr::UNSPECIFIED);
    assert!(frozen.validate().is_err());
    assert!(entry_network::ruleset(&frozen, &BTreeSet::new(), &BTreeSet::new()).is_ok());
    frozen.rules[0].mode = EntryMode::L4Range {
        protocol: EntryProtocol::Tcp,
        listen_start: 30000,
        listen_end: 30002,
        target_start: 80,
        target_end: 81,
    };
    assert!(entry_network::ruleset(&frozen, &BTreeSet::new(), &BTreeSet::new()).is_err());
}

#[test]
#[ignore = "requires Linux user/network namespaces, Python, ip and nft"]
fn real_tcp_udp_range_translation_fencing_and_withdrawal() {
    let directory =
        std::env::temp_dir().join(format!("brocade-entry-range-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let mut active = desired();
    active.rules[0].mode = EntryMode::L4Range {
        protocol: EntryProtocol::TcpUdp,
        listen_start: 30000,
        listen_end: 34095,
        target_start: 40000,
        target_end: 44095,
    };
    let mut blocked = active.clone();
    blocked.rules[0].target = None;
    for (name, plan) in [
        ("active.nft", active),
        ("blocked.nft", blocked),
        ("empty.nft", EntryDesired::default()),
    ] {
        let script = entry_network::ruleset(&plan, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        std::fs::write(directory.join(name), script).unwrap();
    }
    let output = Command::new("timeout")
        .args(["60", "unshare", "-Urn", "python3"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/entry_ranges_netns.py"
        ))
        .arg(&directory)
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("18 port/protocol checks passed"));
}
