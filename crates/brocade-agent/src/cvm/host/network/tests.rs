use super::*;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nic() -> AllocatedNic {
    AllocatedNic {
        slot: 7,
        publication_id: "p-test".into(),
        publication_revision: 1,
        provider_node: "remote".into(),
        resource_domain: "test".into(),
        allocation_id: "a-test".into(),
        mac: "02:00:00:00:00:07".into(),
        ipv4: Some("198.51.100.2".parse().unwrap()),
        ipv6: Some("2001:db8:1::2".parse().unwrap()),
        public_ipv4: None,
        public_ipv6: None,
        address_mode: NicAddressMode::SharedNat,
        destinations: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
        transport: TransportKind::Wireguard,
        transport_generation: 1,
        mtu: 1400,
        blocked: false,
        quota_exceeded: false,
        limits: NicLimits::default(),
    }
}
fn desired() -> InstanceDesired {
    let spec = InstanceSpec {
        execution: ExecutionTarget::BareMetal {
            identity: HostIdentity {
                machine: "a".repeat(64),
                root_filesystem: "b".repeat(64),
            },
        },
        routing: ContainerRoutes {
            revision: 1,
            priorities: vec![InterfacePriority {
                target: RouteTarget::interface("nic-business"),
                ipv4: Some(1),
                ipv6: Some(1),
            }],
            routes: vec![
                ContainerRoute {
                    destination: "203.0.113.9/32".parse().unwrap(),
                    target: RouteTarget::interface("nic-business"),
                    service: None,
                },
                ContainerRoute {
                    destination: "2001:db8:2::9/128".parse().unwrap(),
                    target: RouteTarget::interface("nic-business"),
                    service: None,
                },
            ],
            dns: vec![],
        },
        name: "native".into(),
        image: None,
        resources: Resources {
            vcpus: 1,
            memory_bytes: 128 * 1024 * 1024,
            root_disk_bytes: 3 * 1024 * 1024 * 1024,
        },

        login_revision: 0,
        legacy_ssh_public_key: None,
        interfaces: vec![
            InterfaceSpec {
                id: "nic-contact".into(),
                allocated: None,
                connection: InterfaceRequest {
                    source_id: "host".into(),
                    purpose: InterfacePurpose::Contact,
                    default_ipv4: false,
                    default_ipv6: false,
                },
            },
            InterfaceSpec {
                id: "nic-business".into(),
                allocated: Some(nic()),
                connection: InterfaceRequest {
                    source_id: "p-test".into(),
                    purpose: InterfacePurpose::Business,
                    default_ipv4: false,
                    default_ipv6: false,
                },
            },
        ],
    };
    InstanceDesired {
        artifacts: None,
        id: "cvm-native".into(),
        operation_id: "op-native".into(),
        generation: 1,
        spec_sha256: brocade_core::hash::sha256_hex(&serde_json::to_vec(&spec).unwrap()),
        spec,
        power: PowerState::Running,
        delete_data: false,
        restart_epoch: 0,
        reinstall_epoch: 0,
        detached_interfaces: vec![],
    }
}
fn echo(address: &str) -> Result<()> {
    let address = SocketAddr::new(address.parse().unwrap(), 18080);
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(b"native").unwrap();
    let mut reply = [0; 6];
    stream.read_exact(&mut reply).map_err(|e| e.to_string())?;
    assert_eq!(&reply, b"native");
    Ok(())
}
fn transport(root: &Path, desired: &InstanceDesired, phase: BindingPhase) -> NicBinding {
    let binding = NicBinding {
        side: BindingSide::Consumer,
        id: "binding-native".into(),
        interface_id: "nic-business".into(),
        instance_id: desired.id.clone(),
        consumer_node: "native".into(),
        provider_node: "remote".into(),
        generation: 1,
        phase,
        nic: desired.spec.interfaces[1].allocated.clone().unwrap(),
        offer: TransportOffer {
            kind: TransportKind::Wireguard,
            listen_side: TransportListenSide::Provider,
            endpoint: "192.0.2.2".parse().unwrap(),
            port_start: 42000,
            port_end: 42000,
            mtu: 1400,
        },
        port: 42000,
        credential: String::new(),
        peer_credential: String::new(),
        routing: desired.spec.routing.clone(),
        entry_source_ipv4: None,
    };
    atomic_write_private(
        &root.join("cvm-nics-desired.json"),
        &serde_json::to_vec(&NicDesired {
            node_id: "native".into(),
            bindings: vec![binding.clone()],
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    binding
}
fn observe_transport(binding: &NicBinding) {
    super::super::super::nics::readiness::observe(
        binding,
        &super::super::super::nics::LinkObservation {
            configured: true,
            reachable: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
}
#[test]
fn routing_preserves_management_and_reply_paths_before_selecting_business() {
    let d = desired();
    let script = rules(&d, &BTreeSet::from(["192.0.2.0/24".parse().unwrap()]), None).unwrap();
    assert!(
        script.find("ip daddr 192.0.2.0/24 return").unwrap()
            < script.find("ip daddr 203.0.113.9/32").unwrap()
    );
    assert!(
        script.find("ct direction reply return").unwrap()
            < script.find("ip daddr 203.0.113.9/32").unwrap()
    );
    assert!(script.contains("meta mark set 0xb80007 ct mark set meta mark return"));
}
#[test]
fn native_chains_share_route_order_and_fence_output_without_a_policy_table() {
    let mut d = desired();
    let chain = RouteTarget::chain("chain-native");
    d.spec.routing.priorities.push(InterfacePriority {
        target: chain.clone(),
        ipv4: Some(10),
        ipv6: Some(10),
    });
    d.spec.routing.routes.extend([
        ContainerRoute {
            destination: "0.0.0.0/0".parse().unwrap(),
            target: chain.clone(),
            service: None,
        },
        ContainerRoute {
            destination: "::/0".parse().unwrap(),
            target: chain.clone(),
            service: Some(RouteService {
                protocol: L4Protocol::Udp,
                port_start: 53,
                port_end: 53,
            }),
        },
    ]);
    let binding = brocade_cvm_compile::chain_binding(&d.id, "chain-native");
    let script = rules(
        &d,
        &BTreeSet::from(["192.0.2.0/24".parse().unwrap()]),
        Some("system.slice/brocade-agent.service"),
    )
    .unwrap();
    let chain_rule = format!(
        "ip daddr 0.0.0.0/0 meta l4proto {{ tcp, udp }} meta mark set {:#x}",
        binding.mark
    );
    assert!(script.find("ip daddr 203.0.113.9/32").unwrap() < script.find(&chain_rule).unwrap());
    for management in [
        "socket cgroupv2 level 2 \"system.slice/brocade-agent.service\" return",
        "ct direction reply return",
        "ip daddr 192.0.2.0/24 return",
    ] {
        assert!(script.find(management).unwrap() < script.find(&chain_rule).unwrap());
    }
    assert!(script.contains(&format!(
        "chain_guard meta mark {} oifname != \"{}\" counter drop",
        binding.mark, binding.device
    )));
    assert!(script.contains(&format!(
        "iifname \"{}\" ct direction reply ct mark {} meta mark set ct mark",
        binding.device, binding.mark
    )));
    assert!(script.contains("ip6 daddr ::/0 meta l4proto udp th dport 53-53"));
    assert!(script.contains("type filter hook postrouting priority -1"));
    assert!(service_match("system.slice").is_err());
    assert!(service_match("../bad.service").is_err());
    assert!(service_match("system.slice/\nbad.service").is_err());
}

#[test]
fn native_chain_intent_fences_updates_until_published_removal_or_release() {
    let root = std::env::temp_dir().join(format!(
        "brocade-native-chain-upgrade-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    assert!(!super::super::chain_configured(&root).unwrap());
    let path = root.join("cvm-native.json");
    let mut record = Record {
        desired: desired(),
        deleted: false,
    };
    record.desired.spec.routing.routes[0].target = RouteTarget::chain("chain-native");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(super::super::chain_configured(&root).unwrap());
    record.deleted = true;
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(!super::super::chain_configured(&root).unwrap());
    std::fs::write(&path, b"corrupt").unwrap();
    assert!(super::super::chain_configured(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "runs only inside an explicitly enabled private user/mount/network namespace"]
fn native_network_namespace_convergence() {
    assert_eq!(
        std::env::var("BROCADE_RUN_BAREMETAL_NETNS").as_deref(),
        Ok("1")
    );
    for ns in ["user", "net", "mnt"] {
        let parent = std::env::var(format!("BROCADE_TEST_PARENT_NS_{}", ns.to_uppercase()))
            .expect("use the guarded baremetal_netns.py runner");
        assert_ne!(
            std::fs::read_link(format!("/proc/self/ns/{ns}"))
                .unwrap()
                .to_str()
                .unwrap(),
            parent,
            "never execute in the host namespace"
        );
    }
    let mapping = std::fs::read_to_string("/proc/self/uid_map").unwrap();
    assert!(
        mapping.lines().all(
            |l| l.split_whitespace().nth(2).unwrap().parse::<u64>().unwrap() < u64::from(u32::MAX)
        ),
        "test must run in a restricted user namespace"
    );
    let root = std::env::temp_dir().join(format!("brocade-native-network-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let options = crate::options::Options::parse(vec![
        "run".into(),
        "--server".into(),
        "http://192.0.2.1".into(),
        "--token".into(),
        "test-only-token".into(),
        "--state-dir".into(),
        root.to_str().unwrap().into(),
    ])
    .unwrap();
    super::super::super::nics::initialize(&options);
    super::super::initialize(&options);
    ip(&["link", "set", "lo", "up"]).unwrap();
    command(
        "sysctl",
        &[
            "-qw",
            "net.ipv4.conf.all.rp_filter=1",
            "net.ipv4.conf.default.rp_filter=1",
        ],
    )
    .unwrap();
    ip(&["link", "add", "mgmt0", "type", "dummy"]).unwrap();
    ip(&["link", "set", "mgmt0", "up"]).unwrap();
    ip(&["address", "add", "192.0.2.1/24", "dev", "mgmt0"]).unwrap();
    ip(&[
        "-6",
        "address",
        "add",
        "2001:db8::1/64",
        "dev",
        "mgmt0",
        "nodad",
    ])
    .unwrap();
    ip(&["route", "add", "default", "dev", "mgmt0"]).unwrap();
    ip(&["-6", "route", "add", "default", "dev", "mgmt0"]).unwrap();
    let config = Config {
        terminal_user: "root".into(),
        primary_device: "mgmt0".into(),
        protected_destinations: vec!["192.0.2.0/24".parse().unwrap()],
    };
    let mut d = desired();
    assert_eq!(
        delivery(&config, &d).unwrap().as_deref(),
        Some("192.0.2.1"),
        "physical-host delivery must use its own verified configuration alongside Cube"
    );
    let mut stopped = d.clone();
    stopped.power = PowerState::Stopped;
    assert_eq!(delivery(&config, &stopped).unwrap(), None);
    d.spec.routing.routes[0].destination = "0.0.0.0/0".parse().unwrap();
    d.spec.routing.routes[1].destination = "::/0".parse().unwrap();
    let preparing = transport(&root, &d, BindingPhase::Preparing);
    observe_transport(&preparing);
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::WaitingForTransport
    ));
    let active = transport(&root, &d, BindingPhase::Active);
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::WaitingForTransport
    ));
    observe_transport(&active);
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::WaitingForTransport
    ));
    assert!(
        !table_owned().unwrap(),
        "pending default routes must not capture host traffic"
    );
    assert!(!ownership_path(&root).exists());
    assert!(!root.join("output.nft").exists());
    assert!(!ip(&["-j", "rule", "show"]).unwrap().contains("31650"));
    assert!(!ip(&["-6", "-j", "rule", "show"]).unwrap().contains("31650"));
    ip(&["netns", "add", "native-outbound"]).unwrap();
    let br = bridge("nic-business");
    ip(&["link", "add", &br, "type", "bridge"]).unwrap();
    ip(&["link", "set", &br, "alias", "brocade-nic:nic-business"]).unwrap();
    ip(&["link", "set", &br, "up"]).unwrap();
    ip(&[
        "link",
        "add",
        "native-lan",
        "type",
        "veth",
        "peer",
        "name",
        "peer-lan",
    ])
    .unwrap();
    ip(&["link", "set", "native-lan", "master", &br]).unwrap();
    ip(&["link", "set", "native-lan", "up"]).unwrap();
    ip(&["link", "set", "peer-lan", "netns", "native-outbound"]).unwrap();
    for args in [
        vec!["link", "set", "lo", "up"],
        vec!["link", "set", "peer-lan", "up"],
        vec!["address", "add", "169.254.128.1/32", "dev", "peer-lan"],
        vec!["address", "add", "203.0.113.9/32", "dev", "lo"],
        vec![
            "-6",
            "address",
            "add",
            "fe80::1/64",
            "dev",
            "peer-lan",
            "nodad",
        ],
        vec![
            "-6",
            "address",
            "add",
            "2001:db8:2::9/128",
            "dev",
            "lo",
            "nodad",
        ],
        vec!["route", "add", "198.51.100.2/32", "dev", "peer-lan"],
        vec!["-6", "route", "add", "2001:db8:1::2/128", "dev", "peer-lan"],
    ] {
        let mut cmd = vec!["netns", "exec", "native-outbound", "ip"];
        cmd.extend(args);
        ip(&cmd).unwrap();
    }
    let script="import socket,threading,time\ndef serve(family,host):\n s=socket.socket(family);s.bind((host,18080));s.listen()\n while True:\n  c,_=s.accept();c.sendall(c.recv(1024));c.close()\nthreading.Thread(target=serve,args=(socket.AF_INET,'203.0.113.9'),daemon=True).start()\nthreading.Thread(target=serve,args=(socket.AF_INET6,'2001:db8:2::9'),daemon=True).start()\ntime.sleep(120)";
    let _server = Server(
        Command::new("ip")
            .args(["netns", "exec", "native-outbound", "python3", "-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    // Even a ready carrier must not capture traffic if preparing the host-facing
    // device fails. This foreign link forces that failure after policy preparation.
    let business = device("nic-business");
    ip(&["link", "add", &business, "type", "dummy"]).unwrap();
    assert!(converge(&config, &root, &d).is_err());
    assert!(!table_owned().unwrap());
    assert!(!root.join("output.nft").exists());
    assert!(ip(&["route", "show", "default"]).unwrap().contains("mgmt0"));
    ip(&["link", "delete", &business]).unwrap();
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::Applied
    ));
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::Applied
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while echo("203.0.113.9").is_err() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    echo("2001:db8:2::9").unwrap();
    assert!(inspect_empty().is_err());
    let observations = interfaces(&config, &d).unwrap();
    assert_eq!(observations.len(), 2);
    assert!(ip(&["route", "show", "default"]).unwrap().contains("mgmt0"));
    // Lost host links must leave the selected route fenced until their original
    // owned bridge is restored, including during an offline recovery round.
    ip(&["link", "delete", &br]).unwrap();
    assert!(matches!(
        converge(&config, &root, &d).unwrap(),
        Convergence::WaitingForTransport
    ));
    assert!(table_owned().unwrap());
    assert!(echo("203.0.113.9").is_err());
    assert!(ip(&["route", "show", "default"]).unwrap().contains("mgmt0"));
    ip(&["link", "add", &br, "type", "bridge"]).unwrap();
    ip(&["link", "set", &br, "alias", "brocade-nic:nic-business"]).unwrap();
    ip(&["link", "set", &br, "up"]).unwrap();
    ip(&["link", "set", "native-lan", "master", &br]).unwrap();
    converge(&config, &root, &d).unwrap();
    echo("203.0.113.9").unwrap();
    echo("2001:db8:2::9").unwrap();
    // A Store-authorized reallocation retains the logical NIC while replacing
    // its actual addresses, MAC and policy slot, including across retries.
    let mut replacement = d.clone();
    let next = replacement.spec.interfaces[1].allocated.as_mut().unwrap();
    next.slot = 8;
    next.mac = "02:00:00:00:00:08".into();
    next.ipv4 = Some("198.51.100.3".parse().unwrap());
    next.ipv6 = Some("2001:db8:1::3".parse().unwrap());
    let old_rules = std::fs::read(root.join("output.nft")).unwrap();
    let old_ownership = std::fs::read(ownership_path(&root)).unwrap();
    let preparing = transport(&root, &replacement, BindingPhase::Preparing);
    observe_transport(&preparing);
    assert!(matches!(
        converge(&config, &root, &replacement).unwrap(),
        Convergence::WaitingForTransport
    ));
    let active = transport(&root, &replacement, BindingPhase::Active);
    assert!(matches!(
        converge(&config, &root, &replacement).unwrap(),
        Convergence::WaitingForTransport
    ));
    assert_eq!(std::fs::read(root.join("output.nft")).unwrap(), old_rules);
    assert_eq!(std::fs::read(ownership_path(&root)).unwrap(), old_ownership);
    echo("203.0.113.9").unwrap();
    echo("2001:db8:2::9").unwrap();
    observe_transport(&active);
    ip(&[
        "-n",
        "native-outbound",
        "route",
        "add",
        "198.51.100.3/32",
        "dev",
        "peer-lan",
    ])
    .unwrap();
    ip(&[
        "-n",
        "native-outbound",
        "-6",
        "route",
        "add",
        "2001:db8:1::3/128",
        "dev",
        "peer-lan",
    ])
    .unwrap();
    converge(&config, &root, &replacement).unwrap();
    converge(&config, &root, &replacement).unwrap();
    echo("203.0.113.9").unwrap();
    echo("2001:db8:2::9").unwrap();
    assert_eq!(
        interfaces(&config, &replacement).unwrap()[1].mac,
        "02:00:00:00:00:08"
    );
    assert!(ip(&["-j", "rule", "show"])
        .unwrap()
        .find(&(MARK + 7).to_string())
        .is_none());
    let mut deleted = replacement;
    deleted.power = PowerState::Deleted;
    deleted.spec.routing.routes.clear();
    deleted.spec.routing.priorities.clear();
    transport(&root, &deleted, BindingPhase::Withdrawing);
    converge(&config, &root, &deleted).unwrap();
    converge(&config, &root, &deleted).unwrap();
    assert!(link(&device("nic-business")).unwrap().is_none());
    assert!(!table_owned().unwrap());
    inspect_empty().unwrap();
    assert!(ip(&["route", "show", "default"]).unwrap().contains("mgmt0"));
    std::fs::remove_dir_all(root).unwrap();
}
