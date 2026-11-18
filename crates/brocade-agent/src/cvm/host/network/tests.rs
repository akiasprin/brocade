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
        ssh_public_key: String::new(),
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
        artifact_mode: Default::default(),
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
#[test]
fn routing_preserves_management_and_reply_paths_before_selecting_business() {
    let d = desired();
    let script = rules(&d, &BTreeSet::from(["192.0.2.0/24".parse().unwrap()])).unwrap();
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
    let config = Config {
        terminal_user: "root".into(),
        primary_device: "mgmt0".into(),
        protected_destinations: vec!["192.0.2.0/24".parse().unwrap()],
    };
    let d = desired();
    converge(&config, &root, &d).unwrap();
    converge(&config, &root, &d).unwrap();
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
    assert!(converge(&config, &root, &d).is_err());
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
    converge(&config, &root, &deleted).unwrap();
    converge(&config, &root, &deleted).unwrap();
    assert!(link(&device("nic-business")).unwrap().is_none());
    assert!(!table_owned().unwrap());
    inspect_empty().unwrap();
    assert!(ip(&["route", "show", "default"]).unwrap().contains("mgmt0"));
    std::fs::remove_dir_all(root).unwrap();
}
