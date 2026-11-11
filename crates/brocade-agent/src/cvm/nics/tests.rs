use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(program: &str, args: &[&str]) -> String {
    run_command(program, args).unwrap_or_else(|error| panic!("acceptance command failed: {error}"))
}
fn namespace_ip(ns: &str, args: &[&str]) {
    let mut all = vec!["-n", ns];
    all.extend_from_slice(args);
    command("ip", &all);
}
fn wg_pair() -> (String, String) {
    let private = command("wg", &["genkey"]).trim().to_owned();
    let mut process = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{private}\n").as_bytes())
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(output.status.success());
    (
        private,
        String::from_utf8(output.stdout).unwrap().trim().into(),
    )
}
fn server(ns: &str, port: u16, dual: bool) -> Server {
    let script = format!(
        r#"import http.server,socket,threading
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  body=(self.client_address[0]+' '+self.path).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass
servers=[http.server.ThreadingHTTPServer(('0.0.0.0',{port}),Handler)]
if {dual}:
 class IPv6Server(http.server.ThreadingHTTPServer):
  address_family=socket.AF_INET6
  def server_bind(self):
   self.socket.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1);super().server_bind()
 servers.append(IPv6Server(('::',{port}),Handler))
for http in servers:threading.Thread(target=http.serve_forever,daemon=True).start()
def echo(family,address):
 udp=socket.socket(family,socket.SOCK_DGRAM)
 if family==socket.AF_INET6:udp.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
 udp.bind((address,{udp_port}))
 while True:
  message,peer=udp.recvfrom(65535);udp.sendto(message,peer)
threading.Thread(target=echo,args=(socket.AF_INET,'0.0.0.0'),daemon=True).start()
if {dual}:threading.Thread(target=echo,args=(socket.AF_INET6,'::'),daemon=True).start()
print('ready',flush=True)
threading.Event().wait()
"#,
        dual = if dual { "True" } else { "False" },
        udp_port = port + 1
    );
    let mut child = Command::new("ip")
        .args(["netns", "exec", ns, "python3", "-u", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "ready");
    Server(child)
}
fn curl(ns: &str, address: &str, port: u16, path: &str) -> String {
    let url = if address.contains(':') {
        format!("http://[{address}]:{port}/{path}")
    } else {
        format!("http://{address}:{port}/{path}")
    };
    command(
        "ip",
        &[
            "netns",
            "exec",
            ns,
            "curl",
            "--noproxy",
            "*",
            "-fsS",
            "--max-time",
            "8",
            &url,
        ],
    )
}
fn udp(ns: &str, address: &str, port: u16) {
    let script=format!("import socket; s=socket.socket({},socket.SOCK_DGRAM);s.settimeout(5);s.sendto(b'cvm-ip-packet',('{}',{}));assert s.recv(100)==b'cvm-ip-packet'",if address.contains(':'){"socket.AF_INET6"}else{"socket.AF_INET"},address,port);
    command("ip", &["netns", "exec", ns, "python3", "-c", &script]);
}

fn scheduling_bindings(count: usize) -> Vec<NicBinding> {
    (0..count)
        .map(|index| {
            let card = (index % (NODE_BINDINGS * 2)) / 2;
            NicBinding {
                id: format!("binding-sweep-{}", index / 2),
                interface_id: format!("nic-sweep-{card}"),
                instance_id: format!("cvm-sweep-{card}"),
                consumer_node: "node-sweep".into(),
                provider_node: "node-sweep".into(),
                side: if index % 2 == 0 {
                    BindingSide::Consumer
                } else {
                    BindingSide::Provider
                },
                generation: 1,
                phase: if index < NODE_BINDINGS * 2 {
                    BindingPhase::Active
                } else {
                    BindingPhase::Preparing
                },
                nic: AllocatedNic {
                    slot: card as u16,
                    publication_id: "network-sweep".into(),
                    publication_revision: 1,
                    provider_node: "node-sweep".into(),
                    resource_domain: "domain-sweep".into(),
                    allocation_id: format!("allocation-sweep-{card}"),
                    mac: format!("02:00:00:00:00:{card:02x}"),
                    ipv4: Some(std::net::Ipv4Addr::new(10, 240, 0, card as u8 + 1).into()),
                    ipv6: None,
                    public_ipv4: None,
                    public_ipv6: None,
                    address_mode: NicAddressMode::SharedNat,
                    destinations: vec!["0.0.0.0/0".parse().unwrap()],
                    transport: TransportKind::Wireguard,
                    transport_generation: 1,
                    mtu: 1400,
                    blocked: false,
                    quota_exceeded: false,
                    limits: NicLimits {
                        max_attachments: NODE_BINDINGS as u16,
                        ..Default::default()
                    },
                },
                offer: TransportOffer {
                    kind: TransportKind::Wireguard,
                    listen_side: TransportListenSide::Provider,
                    endpoint: "192.0.2.1".parse().unwrap(),
                    port_start: 42000,
                    port_end: 42001,
                    mtu: 1400,
                },
                port: 42000,
                credential: String::new(),
                peer_credential: String::new(),
                routing: ContainerRoutes::default(),
                entry_source_ipv4: None,
            }
        })
        .collect()
}

fn observed(binding: &NicBinding) -> NicBindingReport {
    NicBindingReport {
        id: binding.id.clone(),
        side: binding.side,
        generation: binding.generation,
        phase: binding.phase,
        configured: true,
        reachable: Some(true),
        rx_bytes: 0,
        tx_bytes: 0,
        error: None,
        traffic: None,
        local_block: None,
    }
}

#[test]
fn failed_endpoint_does_not_starve_other_offline_bindings() {
    let bindings = scheduling_bindings(NODE_BINDINGS * 2);
    let mut visited = Vec::new();
    let error = visit_bindings(&bindings, |binding| {
        visited.push((binding.id.clone(), binding.side));
        if visited.len() == 1 {
            Err("first endpoint failed".into())
        } else if visited.len() == REPORT_BATCH_SIZE + 1 {
            Err("another endpoint failed".into())
        } else {
            Ok(())
        }
    });
    assert_eq!(error.unwrap_err(), "first endpoint failed");
    assert_eq!(
        visited,
        bindings
            .iter()
            .map(|b| (b.id.clone(), b.side))
            .collect::<Vec<_>>()
    );
}

#[test]
fn complete_capacity_sweep_reports_every_endpoint_in_bounded_batches() {
    for count in [
        0,
        1,
        REPORT_BATCH_SIZE,
        REPORT_BATCH_SIZE + 1,
        NODE_BINDINGS * 2,
        NODE_BINDINGS * 4,
    ] {
        let bindings = scheduling_bindings(count);
        let mut published = Vec::new();
        reconcile_reports(&bindings, observed, |reports| {
            assert!(!reports.is_empty() && reports.len() <= REPORT_BATCH_SIZE);
            published.extend(reports.into_iter().map(|r| (r.id, r.side)));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            published,
            bindings
                .iter()
                .map(|b| (b.id.clone(), b.side))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn failed_report_does_not_skip_later_endpoints_or_the_partial_batch() {
    let bindings = scheduling_bindings(REPORT_BATCH_SIZE * 2 + 3);
    let mut visited = Vec::new();
    let mut batches = Vec::new();
    let error = reconcile_reports(
        &bindings,
        |binding| {
            visited.push((binding.id.clone(), binding.side));
            observed(binding)
        },
        |reports| {
            batches.push(reports.len());
            if batches.len() == 1 {
                Err("report unavailable".into())
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(error.unwrap_err(), "report unavailable");
    assert_eq!(batches, [REPORT_BATCH_SIZE, REPORT_BATCH_SIZE, 3]);
    assert_eq!(visited.len(), bindings.len());
}

#[test]
#[ignore = "requires BROCADE_RUN_CVM_NIC_NETNS=1 in a private user/mount/network namespace"]
fn offline_restore_recreates_held_fences_without_bindings() {
    assert_eq!(
        std::env::var("BROCADE_RUN_CVM_NIC_NETNS").as_deref(),
        Ok("1")
    );
    assert_ne!(
        std::fs::read_link("/proc/self/ns/net")
            .unwrap()
            .to_string_lossy(),
        std::env::var("BROCADE_CVM_TEST_PARENT_NETNS").unwrap()
    );
    assert_eq!(effective_uid(), 0);
    let root = std::env::temp_dir().join(format!("brocade-held-fences-{}", std::process::id()));
    assert!(!root.exists());
    private_dir(&root).unwrap();
    private_dir(&root.join("cvm-nics")).unwrap();
    let desired = NicDesired {
        fences: vec![NicResourceFence {
            interface_id: "nic-held".into(),
            address: "198.51.100.20".parse().unwrap(),
            protocol: Some(L4Protocol::Tcp),
            port_start: 55080,
            port_end: 55080,
        }],
        ..Default::default()
    };
    atomic_write_private(&desired_path(&root), &serde_json::to_vec(&desired).unwrap()).unwrap();
    let options = Options {
        command: "run".into(),
        server: "http://127.0.0.1:1".into(),
        token: "offline-test-token".into(),
        state_dir: root.clone(),
        apply_mode: ApplyMode::Linux,
        vpngate_stats_window: Duration::from_secs(900),
    };
    for _ in 0..2 {
        restore_saved(&options).unwrap();
        let raw = command(
            "nft",
            &["-j", "list", "table", "inet", "brocade_cvm_nic_fences"],
        );
        let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let rules = state["nftables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.get("rule"))
            .collect::<Vec<_>>();
        assert_eq!(rules.len(), 2);
        for rule in rules {
            let body = rule.to_string();
            assert!(
                body.contains("198.51.100.20") && body.contains("55080") && body.contains("drop")
            );
        }
        command(
            "nft",
            &["delete", "table", "inet", "brocade_cvm_nic_fences"],
        );
    }
    assert!(snapshot(&root).unwrap().bindings.is_empty());
    assert_eq!(snapshot(&root).unwrap().fences, desired.fences);
    std::fs::remove_dir_all(root).unwrap();
}

fn fixture(root: &Path, kind: TransportKind) -> NicDesired {
    let mut nic = AllocatedNic {
        slot: 3,
        publication_id: "network-acceptance".into(),
        publication_revision: 1,
        provider_node: "acceptance-node".into(),
        resource_domain: "acceptance-domain".into(),
        allocation_id: "allocation-acceptance".into(),
        mac: "02:01:02:03:04:05".into(),
        ipv4: Some("10.120.0.2".parse().unwrap()),
        ipv6: Some("fd12:3456::2".parse().unwrap()),
        public_ipv4: Some("198.51.100.20".parse().unwrap()),
        public_ipv6: Some("2001:db8::20".parse().unwrap()),
        address_mode: NicAddressMode::SharedNat,
        destinations: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
        transport: kind,
        transport_generation: 1,
        mtu: 1400,
        blocked: false,
        quota_exceeded: false,
        limits: NicLimits {
            max_attachments: 64,
            ..Default::default()
        },
    };
    let mode =
        std::env::var("BROCADE_CVM_TEST_ADDRESS_MODE").unwrap_or_else(|_| "shared_nat".into());
    nic.address_mode = match mode.as_str() {
        "shared_nat" => NicAddressMode::SharedNat,
        "dedicated_nat" => NicAddressMode::DedicatedNat,
        "dedicated_routed" => NicAddressMode::DedicatedRouted,
        _ => panic!("unknown address mode"),
    };
    if nic.address_mode != NicAddressMode::SharedNat {
        nic.public_ipv4 = Some("203.0.113.20".parse().unwrap());
        nic.public_ipv6 = Some("2001:db8:ffff::20".parse().unwrap());
    }
    if nic.address_mode == NicAddressMode::DedicatedRouted {
        nic.ipv4 = nic.public_ipv4;
        nic.ipv6 = nic.public_ipv6;
    }
    let ((consumer_key, consumer_peer), (provider_key, provider_peer)) = if kind
        == TransportKind::Wireguard
    {
        let (c, cp) = wg_pair();
        let (p, pp) = wg_pair();
        ((c, pp), (p, cp))
    } else {
        let certificate = xray::certificate(root).unwrap();
        let peer = serde_json::json!({"packet_key":"12".repeat(32),"certificate_pem":certificate})
            .to_string();
        (
            ("e26a8cf9-982b-4cb3-8efb-9cfd321647a0".into(), peer.clone()),
            ("e26a8cf9-982b-4cb3-8efb-9cfd321647a0".into(), peer),
        )
    };
    let base = NicBinding {
        routing: ContainerRoutes {
            revision: 1,
            priorities: vec![InterfacePriority {
                interface_id: "nic-acceptance".into(),
                ipv4: Some(10),
                ipv6: Some(10),
            }],
            routes: vec![
                ContainerRoute {
                    destination: "0.0.0.0/0".parse().unwrap(),
                    interface_id: "nic-acceptance".into(),
                    service: None,
                },
                ContainerRoute {
                    destination: "::/0".parse().unwrap(),
                    interface_id: "nic-acceptance".into(),
                    service: None,
                },
            ],
            dns: vec![],
        },
        side: BindingSide::Consumer,
        id: "binding-acceptance".into(),
        interface_id: "nic-acceptance".into(),
        instance_id: "cvm-acceptance".into(),
        consumer_node: "acceptance-node".into(),
        provider_node: "acceptance-node".into(),
        generation: 1,
        phase: BindingPhase::Preparing,
        nic,
        offer: TransportOffer {
            listen_side: if std::env::var("BROCADE_CVM_TEST_LISTEN_SIDE").as_deref()
                == Ok("consumer")
            {
                TransportListenSide::Consumer
            } else {
                TransportListenSide::Provider
            },
            kind,
            endpoint: "198.51.100.20".parse().unwrap(),
            port_start: 30000,
            port_end: 30100,
            mtu: 1400,
        },
        port: 30001,
        credential: consumer_key,
        peer_credential: consumer_peer,
        entry_source_ipv4: None,
    };
    let mut provider = base.clone();
    provider.side = BindingSide::Provider;
    provider.credential = provider_key;
    provider.peer_credential = provider_peer;
    NicDesired {
        node_id: "acceptance-node".into(),
        generation: 1,
        bindings: vec![base, provider],
        ..Default::default()
    }
}

#[test]
#[ignore = "requires BROCADE_RUN_CVM_NIC_NETNS=1 in a private user/mount/network namespace"]
fn real_ip_backend() {
    assert_eq!(
        std::env::var("BROCADE_RUN_CVM_NIC_NETNS").as_deref(),
        Ok("1")
    );
    let parent = std::env::var("BROCADE_CVM_TEST_PARENT_NETNS")
        .expect("private namespace wrapper is required");
    assert_ne!(
        std::fs::read_link("/proc/self/ns/net")
            .unwrap()
            .to_string_lossy(),
        parent
    );
    assert_eq!(effective_uid(), 0);
    let kind = match std::env::var("BROCADE_CVM_TEST_TRANSPORT")
        .unwrap()
        .as_str()
    {
        "wireguard" => TransportKind::Wireguard,
        "xray" => TransportKind::Xray,
        _ => panic!("unknown acceptance backend"),
    };
    let root = std::env::temp_dir().join(format!("brocade-nic-acceptance-{}", std::process::id()));
    assert!(!root.exists());
    private_dir(&root).unwrap();
    private_dir(&root.join("cvm-nics")).unwrap();
    ROOT.set(root.clone()).unwrap();
    command("ip", &["link", "set", "lo", "up"]);
    command("ip", &["address", "add", "198.51.100.20/32", "dev", "lo"]);
    command(
        "ip",
        &[
            "-6",
            "address",
            "add",
            "2001:db8::20/128",
            "dev",
            "lo",
            "nodad",
        ],
    );
    command("ip", &["netns", "add", "bcn-test-wan"]);
    command(
        "ip",
        &[
            "link",
            "add",
            "wan0",
            "type",
            "veth",
            "peer",
            "name",
            "eth0",
            "netns",
            "bcn-test-wan",
        ],
    );
    command("ip", &["link", "set", "wan0", "up"]);
    command("ip", &["address", "add", "203.0.113.1/24", "dev", "wan0"]);
    command(
        "ip",
        &[
            "-6",
            "address",
            "add",
            "2001:db8:ffff::1/64",
            "dev",
            "wan0",
            "nodad",
        ],
    );
    namespace_ip("bcn-test-wan", &["link", "set", "lo", "up"]);
    namespace_ip("bcn-test-wan", &["link", "set", "eth0", "up"]);
    namespace_ip(
        "bcn-test-wan",
        &["address", "add", "203.0.113.10/24", "dev", "eth0"],
    );
    namespace_ip(
        "bcn-test-wan",
        &[
            "-6",
            "address",
            "add",
            "2001:db8:ffff::10/64",
            "dev",
            "eth0",
            "nodad",
        ],
    );
    namespace_ip(
        "bcn-test-wan",
        &["route", "add", "default", "via", "203.0.113.1"],
    );
    namespace_ip(
        "bcn-test-wan",
        &["-6", "route", "add", "default", "via", "2001:db8:ffff::1"],
    );
    command(
        "sysctl",
        &[
            "-qw",
            "net.ipv6.conf.all.forwarding=0",
            "net.ipv6.conf.wan0.accept_ra=1",
        ],
    );
    let mut desired = fixture(&root, kind);
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut previous_error = String::new();
    loop {
        atomic_write_private(&desired_path(&root), &serde_json::to_vec(&desired).unwrap()).unwrap();
        let mut ready = true;
        for binding in &desired.bindings {
            match apply(&root, binding, &desired) {
                Ok(state) => ready &= state.configured && state.reachable == Some(true),
                Err(error) => {
                    if error != previous_error {
                        eprintln!("backend preparing: {error}");
                        previous_error = error;
                    }
                    ready = false;
                }
            }
        }
        if ready {
            break;
        }
        if std::time::Instant::now() >= deadline {
            for b in &desired.bindings {
                let ns = host::namespace(b);
                eprintln!(
                    "routing: {:?}",
                    host::ns_ip(&ns, &["-j".into(), "rule".into(), "show".into()])
                );
                eprintln!(
                    "routes: {:?}",
                    host::ns_ip(
                        &ns,
                        &[
                            "-j".into(),
                            "route".into(),
                            "show".into(),
                            "table".into(),
                            "all".into()
                        ]
                    )
                );
                if kind == TransportKind::Wireguard {
                    eprintln!(
                        "handshake: {:?}",
                        host::ns_command(
                            &ns,
                            &[
                                "wg".into(),
                                "show".into(),
                                host::tunnel(b),
                                "latest-handshakes".into()
                            ]
                        )
                    );
                }
                eprintln!(
                    "links: {:?}",
                    host::ns_ip(
                        &ns,
                        &["-j".into(), "-s".into(), "link".into(), "show".into()]
                    )
                );
            }
            eprintln!("firewall: {}", command("nft", &["list", "ruleset"]));
            panic!("transport did not become ready");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert_eq!(
        command("sysctl", &["-n", "net.ipv6.conf.wan0.accept_ra"]).trim(),
        "2"
    );
    for binding in &mut desired.bindings {
        binding.phase = BindingPhase::Active;
        binding.generation += 1;
    }
    atomic_write_private(&desired_path(&root), &serde_json::to_vec(&desired).unwrap()).unwrap();
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    let card = desired.bindings[0].nic.clone();
    let ipv4 = card.ipv4.unwrap().to_string();
    let ipv6 = card.ipv6.unwrap().to_string();
    let public4 = card.public_ipv4.unwrap().to_string();
    let public6 = card.public_ipv6.unwrap().to_string();
    let bridge = host::layout(&desired.bindings[0]).bridge;
    command("ip", &["netns", "add", "bcn-test-guest"]);
    command(
        "ip",
        &[
            "link",
            "add",
            "guest-peer",
            "type",
            "veth",
            "peer",
            "name",
            "eth0",
            "netns",
            "bcn-test-guest",
        ],
    );
    command("ip", &["link", "set", "guest-peer", "master", &bridge]);
    command("ip", &["link", "set", "guest-peer", "up"]);
    namespace_ip("bcn-test-guest", &["link", "set", "lo", "up"]);
    namespace_ip(
        "bcn-test-guest",
        &[
            "link",
            "set",
            "eth0",
            "address",
            "02:01:02:03:04:05",
            "mtu",
            "1400",
            "up",
        ],
    );
    namespace_ip(
        "bcn-test-guest",
        &["address", "add", &format!("{ipv4}/32"), "dev", "eth0"],
    );
    namespace_ip(
        "bcn-test-guest",
        &[
            "-6",
            "address",
            "add",
            &format!("{ipv6}/128"),
            "dev",
            "eth0",
            "nodad",
        ],
    );
    namespace_ip(
        "bcn-test-guest",
        &[
            "route",
            "add",
            "default",
            "via",
            "169.254.128.1",
            "dev",
            "eth0",
            "onlink",
        ],
    );
    namespace_ip(
        "bcn-test-guest",
        &[
            "-6", "route", "add", "default", "via", "fe80::1", "dev", "eth0", "onlink",
        ],
    );
    let _wan = server("bcn-test-wan", 9090, true);
    let _guest = server("bcn-test-guest", 8080, true);
    assert_eq!(
        probe_service(&desired.bindings[0].interface_id, &ipv4, 8080)
            .unwrap()
            .unwrap()
            .0,
        Reachability::Reachable
    );
    assert!(probe_service(&desired.bindings[0].interface_id, "10.120.0.99", 8080).is_err());
    assert_eq!(
        curl("bcn-test-guest", "203.0.113.10", 9090, "outbound4"),
        format!("{public4} /outbound4")
    );
    assert_eq!(
        curl("bcn-test-guest", "2001:db8:ffff::10", 9090, "outbound6"),
        format!("{public6} /outbound6")
    );
    udp("bcn-test-guest", "203.0.113.10", 9091);
    udp("bcn-test-guest", "2001:db8:ffff::10", 9091);
    command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "ping",
            "-c",
            "1",
            "-W",
            "3",
            "203.0.113.10",
        ],
    );
    command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "ping",
            "-6",
            "-c",
            "1",
            "-W",
            "3",
            "2001:db8:ffff::10",
        ],
    );
    // NICs only carry egress now; nothing on the public side reaches the Guest
    // through a NIC (inbound access is an entry).
    for (address, port) in [(&public4, 50080), (&public4, 8080)] {
        let inbound = Command::new("ip")
            .args([
                "netns",
                "exec",
                "bcn-test-wan",
                "curl",
                "--noproxy",
                "*",
                "-fsS",
                "--max-time",
                "2",
                &format!("http://{address}:{port}/inbound"),
            ])
            .output()
            .unwrap();
        assert!(!inbound.status.success(), "a NIC accepted inbound traffic");
    }
    command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "ping",
            "-M",
            "do",
            "-s",
            "1372",
            "-c",
            "1",
            "-W",
            "3",
            "203.0.113.10",
        ],
    );
    command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "ping",
            "-6",
            "-M",
            "do",
            "-s",
            "1352",
            "-c",
            "1",
            "-W",
            "3",
            "2001:db8:ffff::10",
        ],
    );
    command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "ping",
            "-M",
            "dont",
            "-s",
            "4000",
            "-c",
            "1",
            "-W",
            "3",
            "203.0.113.10",
        ],
    );
    let sample = meter::read(&desired.bindings[0]).unwrap();
    assert!(sample.uploaded_bytes > 0 && sample.downloaded_bytes > 0);
    // A source-bound socket cannot bypass container destination routing.
    let routing = desired.bindings[0].routing.clone();
    for binding in &mut desired.bindings {
        binding.routing.routes.clear();
        binding.routing.revision += 1;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    let no_route = Command::new("ip")
        .args([
            "netns",
            "exec",
            "bcn-test-guest",
            "curl",
            "--noproxy",
            "*",
            "-fsS",
            "--interface",
            &ipv4,
            "--max-time",
            "2",
            "http://203.0.113.10:9090/no-route",
        ])
        .output()
        .unwrap();
    assert!(
        !no_route.status.success(),
        "source binding bypassed destination routes"
    );
    // A port rule selecting another NIC drops only that service; the rest of the
    // prefix keeps using this NIC.
    for binding in &mut desired.bindings {
        binding.routing = routing.clone();
        binding.routing.priorities.push(InterfacePriority {
            interface_id: "nic-elsewhere".into(),
            ipv4: Some(20),
            ipv6: None,
        });
        binding.routing.routes.push(ContainerRoute {
            destination: "203.0.113.10/32".parse().unwrap(),
            interface_id: "nic-elsewhere".into(),
            service: Some(RouteService {
                protocol: L4Protocol::Udp,
                port_start: 9091,
                port_end: 9091,
            }),
        });
        binding.routing.revision += 1;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    assert_eq!(
        curl("bcn-test-guest", "203.0.113.10", 9090, "service-route"),
        format!("{public4} /service-route")
    );
    let elsewhere = command(
        "ip",
        &[
            "netns",
            "exec",
            "bcn-test-guest",
            "python3",
            "-c",
            "import socket\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(2);s.sendto(b'probe',('203.0.113.10',9091))\ntry:\n s.recv(100);print('reached')\nexcept socket.timeout:\n print('blocked')",
        ],
    );
    assert_eq!(
        elsewhere.trim(),
        "blocked",
        "a flow selected for another NIC left through this one"
    );
    // Symmetric egress: a whole-IP entry address on the provider becomes the source.
    if card.address_mode != NicAddressMode::DedicatedRouted {
        command("ip", &["address", "add", "198.51.100.21/32", "dev", "lo"]);
        for binding in &mut desired.bindings {
            binding.routing = routing.clone();
            binding.entry_source_ipv4 = Some("198.51.100.21".parse().unwrap());
            binding.generation += 1;
        }
        for binding in &desired.bindings {
            apply(&root, binding, &desired).unwrap();
        }
        assert_eq!(
            curl("bcn-test-guest", "203.0.113.10", 9090, "symmetric"),
            "198.51.100.21 /symmetric"
        );
    }
    for binding in &mut desired.bindings {
        binding.routing = routing.clone();
        binding.entry_source_ipv4 = None;
        binding.generation += 1;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    // A replacement of the data path may fail, but it must never expose the
    // namespace's ordinary uplink as a business default.
    for binding in &mut desired.bindings {
        binding.nic.blocked = true;
        binding.generation += 1;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    let blocked = std::process::Command::new("ip")
        .args([
            "netns",
            "exec",
            "bcn-test-guest",
            "curl",
            "--noproxy",
            "*",
            "-fsS",
            "--max-time",
            "2",
            "http://203.0.113.10:9090/blocked",
        ])
        .output()
        .unwrap();
    assert!(
        !blocked.status.success(),
        "revoked NIC retained business connectivity"
    );
    for binding in &mut desired.bindings {
        binding.nic.blocked = false;
        binding.generation += 1;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    assert_eq!(
        curl("bcn-test-guest", "203.0.113.10", 9090, "restored"),
        format!("{public4} /restored")
    );
    // Local quota enforcement does not depend on a successful Console request.
    for binding in &mut desired.bindings {
        binding.nic.limits.monthly_bytes = Some(1);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    desired.budgets.insert(
        "nic-acceptance".into(),
        NicTrafficBudget {
            period_start: brocade_deployment::traffic_limit::utc_month_period(now).0 as u64 * 1000,
            used_bytes: 0,
            sample: None,
        },
    );
    let (limited, reasons) = meter::effective(&root, &desired);
    assert!(limited.bindings[0].nic.quota_exceeded);
    assert!(reasons.contains_key("nic-acceptance"));
    for binding in &limited.bindings {
        apply(&root, binding, &limited).unwrap();
    }
    let blocked = Command::new("ip")
        .args([
            "netns",
            "exec",
            "bcn-test-guest",
            "curl",
            "--noproxy",
            "*",
            "-fsS",
            "--max-time",
            "2",
            "http://203.0.113.10:9090/offline-quota",
        ])
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    for binding in &mut desired.bindings {
        binding.nic.limits.monthly_bytes = None;
    }
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    assert_eq!(
        curl("bcn-test-guest", "203.0.113.10", 9090, "quota-raised"),
        format!("{public4} /quota-raised")
    );
    command("ip", &["link", "delete", "guest-peer"]);
    for binding in &mut desired.bindings {
        binding.phase = BindingPhase::Withdrawing;
        binding.generation += 1;
    }
    atomic_write_private(&desired_path(&root), &serde_json::to_vec(&desired).unwrap()).unwrap();
    for binding in &desired.bindings {
        apply(&root, binding, &desired).unwrap();
    }
    assert!(!Path::new("/run/netns")
        .join(host::namespace(&desired.bindings[0]))
        .exists());
    assert!(!Path::new("/run/netns")
        .join(host::namespace(&desired.bindings[1]))
        .exists());
    eprintln!("real {kind:?}: dual-stack TCP/UDP/ICMP, public source NAT, no NIC inbound, port-selected egress, symmetric entry source, revoke/recover and namespace cleanup passed");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires an explicitly owned disposable KVM host and Console; private stop marker"]
fn kvm_network_service() {
    assert_eq!(std::env::var("BROCADE_RUN_CVM_KVM").as_deref(), Ok("1"));
    let options = crate::options::Options::parse(vec!["run".into()]).unwrap();
    assert_eq!(options.apply_mode, crate::options::ApplyMode::Linux);
    let _controller = controller_lock(&options.state_dir).unwrap();
    initialize(&options);
    let stop = options.state_dir.join("nic-service.stop");
    assert!(!stop.exists());
    let start = std::time::Instant::now();
    let mut errors = [String::new(), String::new()];
    while !stop.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(900),
            "isolated NIC acceptance did not finish"
        );
        for (outcome, previous) in [round(&options), super::super::entries::round(&options)]
            .into_iter()
            .zip(&mut errors)
        {
            match outcome {
                Ok(()) => previous.clear(),
                Err(next) => {
                    if next != *previous {
                        eprintln!("isolated network convergence: {next}");
                        *previous = next;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn consumer_forward_rules_follow_route_selection_order() {
    let route = |destination: &str, interface: &str, service: Option<(L4Protocol, u16, u16)>| {
        ContainerRoute {
            destination: destination.parse().unwrap(),
            interface_id: interface.into(),
            service: service.map(|(protocol, port_start, port_end)| RouteService {
                protocol,
                port_start,
                port_end,
            }),
        }
    };
    let routing = ContainerRoutes {
        revision: 1,
        priorities: vec![
            InterfacePriority {
                interface_id: "home".into(),
                ipv4: Some(10),
                ipv6: None,
            },
            InterfacePriority {
                interface_id: "ntt".into(),
                ipv4: Some(20),
                ipv6: None,
            },
        ],
        routes: vec![
            route("0.0.0.0/0", "home", None),
            route("0.0.0.0/0", "ntt", Some((L4Protocol::Tcp, 22, 22))),
            route("0.0.0.0/0", "ntt", Some((L4Protocol::Tcp, 1, 1024))),
            route("192.168.10.0/24", "ntt", None),
        ],
        dns: vec![],
    };
    let rules = host::route_rules(&routing, "ntt", true);
    assert_eq!(
        rules,
        vec![
            "iifname \"lan\" ip daddr 192.168.10.0/24 accept",
            "iifname \"lan\" ip daddr 0.0.0.0/0 meta l4proto tcp th dport 22-22 accept",
            "iifname \"lan\" ip daddr 0.0.0.0/0 meta l4proto tcp th dport 1-1024 accept",
            "iifname \"lan\" ip daddr 0.0.0.0/0 drop",
        ]
    );
    // The same order decides for every NIC, so exactly one accepts each flow.
    let home = host::route_rules(&routing, "home", true);
    assert_eq!(
        home.last().unwrap(),
        "iifname \"lan\" ip daddr 0.0.0.0/0 accept"
    );
    assert!(home[1].ends_with("drop") && home[2].ends_with("drop"));
    for (address, flow, wanted) in [
        ("192.168.10.5", Some((L4Protocol::Udp, 53)), "ntt"),
        ("203.0.113.9", Some((L4Protocol::Tcp, 22)), "ntt"),
        ("203.0.113.9", Some((L4Protocol::Udp, 22)), "home"),
        ("203.0.113.9", None, "home"),
    ] {
        assert_eq!(
            routing.select_flow(address.parse().unwrap(), flow),
            Some(wanted)
        );
    }
}
